//! Aufbewahrungsfristen:
//! * `delete_from_server_after_days` je Postfach – ältere Nachrichten auf dem Server löschen
//!   (beim Abgleich, in derselben Verbindung); lokal bleiben sie mit `on_server = 0`.
//! * `archive_retention_days` je Postfach – ältere Nachrichten auch lokal löschen.
//! * `trash_retention_days` (Konfiguration) – Papierkorb nach so vielen Tagen leeren.

use crate::config::Config;
use crate::db::models::now;
use crate::mail::imap::{self, Session};
use crate::mail::model::AccountRow;
use crate::mail::store;
use sqlx::SqlitePool;
use std::collections::BTreeMap;

/// Alte Nachrichten auf dem Server löschen (Entwürfe ausgenommen).
pub async fn delete_from_server(
    pool: &SqlitePool,
    account: &AccountRow,
    session: &mut Session,
) -> Result<usize, String> {
    let Some(days) = account.delete_from_server_after_days.filter(|d| *d > 0) else {
        return Ok(0);
    };
    let cutoff = now() - days * 86_400;
    let rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT m.id, m.uid, f.path FROM messages m JOIN folders f ON f.id = m.folder_id
         WHERE m.account_id = ? AND m.on_server = 1 AND m.uid IS NOT NULL AND m.date < ?
         AND f.on_server = 1 AND f.role <> 'drafts'",
    )
    .bind(&account.id)
    .bind(cutoff)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Aufbewahrung: {e}"))?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut by_path: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
    for (id, uid, path) in rows {
        if let Ok(uid) = u32::try_from(uid) {
            by_path.entry(path).or_default().push((id, uid));
        }
    }
    let caps = imap::caps(session).await;
    let mut deleted = 0;
    for (path, list) in by_path {
        imap::select(session, &path).await?;
        let uids: Vec<u32> = list.iter().map(|(_, u)| *u).collect();
        imap::expunge(session, caps, &uids).await?;
        for chunk in list.chunks(500) {
            let mut b = sqlx::QueryBuilder::new(
                "UPDATE messages SET on_server = 0, uid = NULL WHERE id IN (",
            );
            let mut sep = b.separated(", ");
            for (id, _) in chunk {
                sep.push_bind(id);
            }
            b.push(")");
            b.build()
                .execute(pool)
                .await
                .map_err(|e| format!("Aufbewahrung: {e}"))?;
        }
        deleted += list.len();
    }
    tracing::info!(account = %account.id, deleted, "Aufbewahrung: auf dem Server gelöscht");
    Ok(deleted)
}

fn sql(e: crate::error::AppError) -> sqlx::Error {
    match e {
        crate::error::AppError::Database(e) => e,
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

/// Lokale Fristen (Archiv je Postfach) und der Papierkorb (auch auf dem Server).
/// Gibt die Zahl gelöschter Nachrichten zurück.
pub async fn run_local(pool: &SqlitePool, config: &Config) -> Result<u64, sqlx::Error> {
    let ts = now();
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT m.id FROM messages m JOIN mail_accounts a ON a.id = m.account_id
         WHERE a.archive_retention_days IS NOT NULL AND a.archive_retention_days > 0
         AND m.date < ? - a.archive_retention_days * 86400",
    )
    .bind(ts)
    .fetch_all(pool)
    .await?;
    let mut n = store::delete(pool, &ids).await.map_err(sql)?;

    if config.trash_retention_days > 0 {
        let cutoff = ts - config.trash_retention_days as i64 * 86_400;
        let rows: Vec<(String, String, Option<i64>, bool, String)> = sqlx::query_as(
            "SELECT m.id, m.account_id, m.uid, m.on_server, f.path FROM messages m JOIN folders f ON f.id = m.folder_id
             WHERE f.role = 'trash' AND COALESCE(m.trashed_at, m.received_at) < ?",
        )
        .bind(cutoff)
        .fetch_all(pool)
        .await?;
        let mut by_account: BTreeMap<String, Vec<(String, Option<u32>, String)>> = BTreeMap::new();
        for (id, account, uid, on_server, path) in rows {
            let uid = uid
                .filter(|_| on_server)
                .and_then(|u| u32::try_from(u).ok());
            by_account.entry(account).or_default().push((id, uid, path));
        }
        for (account_id, list) in by_account {
            let on_server: Vec<&(String, Option<u32>, String)> =
                list.iter().filter(|(_, uid, _)| uid.is_some()).collect();
            let mut server_ok = on_server.is_empty();
            if !server_ok {
                if let Ok(Some(account)) = crate::mail::accounts::by_id(pool, &account_id).await {
                    match imap::connect(&imap::Server::for_account(&account, config)).await {
                        Ok(mut s) => {
                            let caps = imap::caps(&mut s).await;
                            let mut by_path: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
                            for (_, uid, path) in &on_server {
                                by_path.entry(path.as_str()).or_default().extend(uid);
                            }
                            server_ok = true;
                            for (path, uids) in by_path {
                                let r = async {
                                    imap::select(&mut s, path).await?;
                                    imap::expunge(&mut s, caps, &uids).await
                                }
                                .await;
                                if let Err(e) = r {
                                    tracing::warn!(account = %account_id, "Papierkorb leeren: {e}");
                                    server_ok = false;
                                }
                            }
                            imap::logout(s).await;
                        }
                        Err(e) => tracing::warn!(account = %account_id, "Papierkorb leeren: {e}"),
                    }
                }
            }
            // Ohne Server nur, was dort ohnehin nicht mehr liegt
            let delete: Vec<String> = list
                .into_iter()
                .filter(|(_, uid, _)| server_ok || uid.is_none())
                .map(|(id, ..)| id)
                .collect();
            n += store::delete(pool, &delete).await.map_err(sql)?;
        }
    }
    if n > 0 {
        tracing::info!(deleted = n, "Aufbewahrung: endgültig gelöscht");
    }
    Ok(n)
}
