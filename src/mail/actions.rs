//! Aktionen auf Nachrichten (gelesen, markiert, archivieren, Papierkorb, löschen, Spam,
//! verschieben, Kategorie) – lokal und per IMAP auf dem Server. Ist der Server nicht
//! erreichbar, wird nur lokal geändert und das im Ergebnis vermerkt.
//!
//! Verschobene Nachrichten verlieren ihre UID (sie bekommen im Zielordner eine neue) und
//! warten mit `uid = NULL, on_server = 1`, bis der nächste Abgleich sie dort wiederfindet.

use crate::api::{BulkAction, BulkResult, FolderRole, MessageAction};
use crate::config::Config;
use crate::db::models::now;
use crate::error::{AppError, AppResult};
use crate::mail::imap::{self, Session};
use crate::mail::model::{AccountRow, FolderRow};
use crate::mail::{accounts, folders, store};
use sqlx::{FromRow, SqlitePool};
use std::collections::BTreeMap;

#[derive(Debug, Clone, FromRow)]
struct Target {
    id: String,
    account_id: String,
    folder_id: Option<String>,
    uid: Option<i64>,
    on_server: bool,
    path: Option<String>,
}

impl Target {
    /// Liegt die Nachricht mit bekannter UID auf dem Server?
    fn server_uid(&self) -> Option<(String, u32)> {
        match (self.on_server, self.uid, &self.path) {
            (true, Some(uid), Some(path)) => u32::try_from(uid).ok().map(|u| (path.clone(), u)),
            _ => None,
        }
    }
}

/// Ordner einer Rolle finden oder anlegen (mit Sitzung auch auf dem Server).
pub async fn ensure_role(
    pool: &SqlitePool,
    account: &AccountRow,
    role: FolderRole,
    session: Option<&mut Session>,
) -> AppResult<FolderRow> {
    if let Some(f) = folders::by_role(pool, &account.id, role).await? {
        if account.is_imap() && !f.on_server {
            if let Some(s) = session {
                if imap::create(s, &f.path).await.is_ok() {
                    sqlx::query("UPDATE folders SET on_server = 1 WHERE id = ?")
                        .bind(&f.id)
                        .execute(pool)
                        .await?;
                    return folders::get(pool, &f.id).await;
                }
            }
        }
        return Ok(f);
    }
    let existing = folders::for_account(pool, &account.id).await?;
    let delimiter = existing
        .iter()
        .map(|f| f.delimiter.clone())
        .find(|d| !d.is_empty())
        .unwrap_or_else(|| "/".into());
    let name = folders::default_name(role);
    let mut path = if role == FolderRole::Inbox {
        "INBOX".to_string()
    } else {
        folders::encode_mutf7(name)
    };
    // Server mit allen Ordnern unter INBOX (z. B. Courier: „INBOX.Sent“)
    let others: Vec<&FolderRow> = existing.iter().filter(|f| f.path != "INBOX").collect();
    let prefix = format!("INBOX{delimiter}");
    if role != FolderRole::Inbox
        && account.is_imap()
        && !others.is_empty()
        && others.iter().all(|f| f.path.starts_with(&prefix))
    {
        path = format!("{prefix}{path}");
    }
    let mut on_server = false;
    if account.is_imap() && role != FolderRole::Inbox {
        if let Some(s) = session {
            on_server = imap::create(s, &path).await.is_ok();
        }
    }
    let folder =
        folders::create_local(pool, &account.id, &path, &delimiter, role, on_server).await?;
    if folder.role() != role {
        sqlx::query("UPDATE folders SET role = ? WHERE id = ?")
            .bind(role.as_str())
            .bind(&folder.id)
            .execute(pool)
            .await?;
    }
    folders::get(pool, &folder.id).await
}

async fn load(pool: &SqlitePool, user_id: &str, ids: &[String]) -> AppResult<Vec<Target>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        let mut b = sqlx::QueryBuilder::new(
            "SELECT m.id, m.account_id, m.folder_id, m.uid, m.on_server, f.path
             FROM messages m JOIN mail_accounts a ON a.id = m.account_id LEFT JOIN folders f ON f.id = m.folder_id
             WHERE a.user_id = ",
        );
        b.push_bind(user_id).push(" AND m.id IN (");
        let mut sep = b.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        b.push(")");
        out.extend(b.build_query_as::<Target>().fetch_all(pool).await?);
    }
    Ok(out)
}

/// Nachrichten je Server-Ordner gruppieren.
fn by_folder(rows: &[&Target]) -> BTreeMap<String, Vec<u32>> {
    let mut map: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for r in rows {
        if let Some((path, uid)) = r.server_uid() {
            map.entry(path).or_default().push(uid);
        }
    }
    map
}

async fn set_column(pool: &SqlitePool, column: &str, value: bool, ids: &[String]) -> AppResult<()> {
    for chunk in ids.chunks(500) {
        let mut b = sqlx::QueryBuilder::new(format!("UPDATE messages SET {column} = "));
        b.push_bind(value).push(" WHERE id IN (");
        let mut sep = b.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        b.push(")");
        b.build().execute(pool).await?;
    }
    Ok(())
}

/// Aktion ausführen.
pub async fn apply(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    action: &BulkAction,
) -> AppResult<BulkResult> {
    let mut ids = action.ids.clone();
    ids.sort();
    ids.dedup();
    if ids.len() > 10_000 {
        return Err(AppError::bad("Höchstens 10 000 Nachrichten auf einmal."));
    }
    let rows = load(pool, user_id, &ids).await?;
    let mut result = BulkResult {
        failed: (ids.len() - rows.len()) as u32,
        ..BulkResult::default()
    };
    let mut groups: BTreeMap<String, Vec<Target>> = BTreeMap::new();
    for r in rows {
        groups.entry(r.account_id.clone()).or_default().push(r);
    }
    let mut server_errors: Vec<String> = Vec::new();
    for (account_id, rows) in groups {
        let account = accounts::get(pool, user_id, &account_id).await?;
        match apply_account(pool, config, &account, &action.action, &rows).await {
            Ok((updated, failed, err)) => {
                result.updated += updated;
                result.failed += failed;
                if let Some(e) = err {
                    server_errors.push(format!("{}: {e}", account.label()));
                }
            }
            Err(e) => return Err(e),
        }
    }
    if !server_errors.is_empty() {
        result.message = Some(format!(
            "Server nicht erreichbar oder Fehler – nur lokal geändert ({}).",
            server_errors.join("; ")
        ));
    }
    Ok(result)
}

/// Aktion für die Nachrichten eines Postfachs: (geändert, fehlgeschlagen, Serverfehler).
async fn apply_account(
    pool: &SqlitePool,
    config: &Config,
    account: &AccountRow,
    action: &MessageAction,
    rows: &[Target],
) -> AppResult<(u32, u32, Option<String>)> {
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let on_server: Vec<&Target> = rows.iter().filter(|r| r.server_uid().is_some()).collect();

    // Kategorie: nur lokal
    if let MessageAction::SetCategory { category } = action {
        for chunk in ids.chunks(500) {
            let mut b = sqlx::QueryBuilder::new(
                "UPDATE messages SET category_source = 'user', category = ",
            );
            b.push_bind(category.as_str()).push(" WHERE id IN (");
            let mut sep = b.separated(", ");
            for id in chunk {
                sep.push_bind(id);
            }
            b.push(")");
            b.build().execute(pool).await?;
        }
        return Ok((ids.len() as u32, 0, None));
    }

    // Verbindung nur, wenn der Server etwas zu tun bekommt
    let mut server_error: Option<String> = None;
    let mut session: Option<Session> = None;
    if account.is_imap() && !on_server.is_empty() {
        match imap::connect(&imap::Server::for_account(account, config)).await {
            Ok(s) => session = Some(s),
            Err(e) => server_error = Some(e),
        }
    }

    let outcome = run_action(
        pool,
        account,
        action,
        rows,
        &ids,
        &on_server,
        session.as_mut(),
    )
    .await;
    if let Some(s) = session {
        imap::logout(s).await;
    }
    let (updated, failed, err) = outcome?;
    Ok((updated, failed, server_error.or(err)))
}

async fn run_action(
    pool: &SqlitePool,
    account: &AccountRow,
    action: &MessageAction,
    rows: &[Target],
    ids: &[String],
    on_server: &[&Target],
    mut session: Option<&mut Session>,
) -> AppResult<(u32, u32, Option<String>)> {
    let mut server_error: Option<String> = None;
    let flag =
        |name: &str, add: bool| format!("{}FLAGS.SILENT ({name})", if add { "+" } else { "-" });
    let (column, value, query) = match action {
        MessageAction::MarkRead => ("seen", true, flag("\\Seen", true)),
        MessageAction::MarkUnread => ("seen", false, flag("\\Seen", false)),
        MessageAction::Flag => ("flagged", true, flag("\\Flagged", true)),
        MessageAction::Unflag => ("flagged", false, flag("\\Flagged", false)),
        MessageAction::Delete => {
            if let Some(s) = session.as_deref_mut() {
                let caps = imap::caps(s).await;
                for (path, uids) in by_folder(on_server) {
                    let r = async {
                        imap::select(s, &path).await?;
                        imap::expunge(s, caps, &uids).await
                    }
                    .await;
                    if let Err(e) = r {
                        server_error.get_or_insert(e);
                    }
                }
            }
            store::delete(pool, ids).await?;
            return Ok((ids.len() as u32, 0, server_error));
        }
        MessageAction::SetCategory { .. } => unreachable!("oben erledigt"),
        MessageAction::Archive
        | MessageAction::Trash
        | MessageAction::Spam
        | MessageAction::Restore
        | MessageAction::NotSpam
        | MessageAction::Move { .. } => {
            return move_messages(pool, account, action, rows, session).await;
        }
    };
    if let Some(s) = session {
        for (path, uids) in by_folder(on_server) {
            let r = async {
                imap::select(s, &path).await?;
                imap::store(s, &uids, &query).await
            }
            .await;
            if let Err(e) = r {
                server_error.get_or_insert(e);
            }
        }
    }
    set_column(pool, column, value, ids).await?;
    Ok((ids.len() as u32, 0, server_error))
}

async fn move_messages(
    pool: &SqlitePool,
    account: &AccountRow,
    action: &MessageAction,
    rows: &[Target],
    mut session: Option<&mut Session>,
) -> AppResult<(u32, u32, Option<String>)> {
    let target = match action {
        MessageAction::Move { folder_id } => {
            let f = folders::get(pool, folder_id).await?;
            if f.account_id != account.id {
                // Nur innerhalb desselben Postfachs
                return Ok((
                    0,
                    rows.len() as u32,
                    Some(
                        "Nachrichten lassen sich nur innerhalb desselben Postfachs verschieben."
                            .into(),
                    ),
                ));
            }
            f
        }
        MessageAction::Archive => {
            ensure_role(pool, account, FolderRole::Archive, session.as_deref_mut()).await?
        }
        MessageAction::Trash => {
            ensure_role(pool, account, FolderRole::Trash, session.as_deref_mut()).await?
        }
        MessageAction::Spam => {
            ensure_role(pool, account, FolderRole::Junk, session.as_deref_mut()).await?
        }
        _ => ensure_role(pool, account, FolderRole::Inbox, session.as_deref_mut()).await?,
    };
    let moving: Vec<&Target> = rows
        .iter()
        .filter(|r| r.folder_id.as_deref() != Some(target.id.as_str()))
        .collect();
    let mut server_error = None;
    if let Some(s) = session {
        if target.on_server {
            let caps = imap::caps(s).await;
            for (path, uids) in by_folder(&moving) {
                let r = async {
                    imap::select(s, &path).await?;
                    imap::move_to(s, caps, &uids, &target.path).await
                }
                .await;
                if let Err(e) = r {
                    server_error.get_or_insert(e);
                }
            }
        }
    }
    let ts = now();
    let trashed = (target.role() == FolderRole::Trash).then_some(ts);
    let mut tx = pool.begin().await?;
    for r in &moving {
        sqlx::query("UPDATE messages SET folder_id = ?, uid = NULL, trashed_at = ? WHERE id = ?")
            .bind(&target.id)
            .bind(trashed)
            .bind(&r.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok((rows.len() as u32, 0, server_error))
}
