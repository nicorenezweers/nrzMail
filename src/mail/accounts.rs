//! Postfächer: anlegen, ändern, löschen (nur eigene), Zähler für die Seitenleiste und
//! „Verbindung testen“ für IMAP und SMTP.

use crate::api::{self, AccountKind, Address, MailAccountInput};
use crate::config::Config;
use crate::db::models::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::mail::model::{AccountCounts, AccountRow};
use crate::mail::{folders, imap, smtp, SyncState};
use sqlx::SqlitePool;
use std::collections::HashMap;

/// Postfach des Benutzers (sonst 404 – fremde Postfächer gibt es nicht).
pub async fn get(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<AccountRow> {
    sqlx::query_as("SELECT * FROM mail_accounts WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)
}

/// Postfach ohne Prüfung des Benutzers (Hintergrund).
pub async fn by_id(pool: &SqlitePool, id: &str) -> AppResult<Option<AccountRow>> {
    Ok(sqlx::query_as("SELECT * FROM mail_accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn rows(pool: &SqlitePool, user_id: &str) -> AppResult<Vec<AccountRow>> {
    Ok(
        sqlx::query_as("SELECT * FROM mail_accounts WHERE user_id = ? ORDER BY created_at, name")
            .bind(user_id)
            .fetch_all(pool)
            .await?,
    )
}

/// Zähler je Postfach: Nachrichten, ungelesene (ohne Papierkorb, Spam, Gesendet, Entwürfe), Grösse.
pub async fn counts(pool: &SqlitePool, user_id: &str) -> AppResult<HashMap<String, AccountCounts>> {
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT m.account_id, count(*),
                COALESCE(sum(CASE WHEN m.seen = 0 AND COALESCE(f.role, 'other') NOT IN ('trash', 'junk', 'sent', 'drafts')
                                   AND (m.on_server = 1 OR a.kind = 'import') THEN 1 ELSE 0 END), 0),
                COALESCE(sum(m.size), 0)
         FROM messages m JOIN mail_accounts a ON a.id = m.account_id LEFT JOIN folders f ON f.id = m.folder_id
         WHERE a.user_id = ? GROUP BY m.account_id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, messages, unread, size)| {
            (
                id,
                AccountCounts {
                    messages,
                    unread,
                    size,
                },
            )
        })
        .collect())
}

pub async fn list(
    pool: &SqlitePool,
    sync: &SyncState,
    user_id: &str,
) -> AppResult<Vec<api::MailAccount>> {
    let counts = counts(pool, user_id).await?;
    Ok(rows(pool, user_id)
        .await?
        .iter()
        .map(|a| {
            a.to_api(
                counts.get(&a.id).copied().unwrap_or_default(),
                sync.is_syncing(&a.id),
            )
        })
        .collect())
}

pub async fn get_api(
    pool: &SqlitePool,
    sync: &SyncState,
    user_id: &str,
    id: &str,
) -> AppResult<api::MailAccount> {
    let a = get(pool, user_id, id).await?;
    let counts = counts(pool, user_id).await?;
    Ok(a.to_api(
        counts.get(&a.id).copied().unwrap_or_default(),
        sync.is_syncing(&a.id),
    ))
}

fn valid_host(h: &str) -> bool {
    let h = h.trim();
    !h.is_empty()
        && h.len() <= 253
        && !h.contains(char::is_whitespace)
        && !h.contains(['/', '@', ':'])
}

/// Eingaben prüfen und bereinigen.
fn validate(i: &MailAccountInput) -> AppResult<MailAccountInput> {
    let mut i = i.clone();
    i.email = i.email.trim().to_string();
    i.name = i.name.trim().chars().take(100).collect();
    i.display_name = i.display_name.trim().chars().take(200).collect();
    i.imap_host = i.imap_host.trim().to_string();
    i.smtp_host = i.smtp_host.trim().to_string();
    i.imap_username = i.imap_username.trim().to_string();
    i.smtp_username = i.smtp_username.trim().to_string();
    i.color = i.color.trim().to_string();
    if !Address::new("", i.email.clone()).is_valid() {
        return Err(AppError::bad("Bitte eine gültige E-Mail-Adresse angeben."));
    }
    if i.color.len() > 32 || i.color.contains([';', '"', '<', '>', '{', '}', '\\']) {
        return Err(AppError::bad("Ungültige Farbe."));
    }
    if i.signature.chars().count() > 20_000 {
        return Err(AppError::bad("Die Signatur ist zu lang."));
    }
    if !(1..=1440).contains(&i.sync_interval_minutes) {
        return Err(AppError::bad("Abgleich alle 1–1440 Minuten."));
    }
    for d in [i.delete_from_server_after_days, i.archive_retention_days]
        .into_iter()
        .flatten()
    {
        if !(1..=36_500).contains(&d) {
            return Err(AppError::bad("Aufbewahrung: 1–36500 Tage oder leer."));
        }
    }
    if i.kind == AccountKind::Imap {
        if !valid_host(&i.imap_host) {
            return Err(AppError::bad(
                "IMAP-Server angeben, z. B. imap.gmail.com (ohne https:// und Port).",
            ));
        }
        if i.imap_port == 0 {
            return Err(AppError::bad("IMAP-Port: 1–65535 (meist 993)."));
        }
        if i.imap_username.is_empty() {
            i.imap_username = i.email.clone();
        }
        if !i.smtp_host.is_empty() {
            if !valid_host(&i.smtp_host) {
                return Err(AppError::bad(
                    "SMTP-Server angeben, z. B. smtp.gmail.com (ohne https:// und Port).",
                ));
            }
            if i.smtp_port == 0 {
                return Err(AppError::bad("SMTP-Port: 1–65535 (meist 465 oder 587)."));
            }
        }
    } else {
        // Nur Archiv: kein Server, keine Fristen für den Server
        i.delete_from_server_after_days = None;
        i.sync_enabled = false;
    }
    Ok(i)
}

/// Neues Postfach.
pub async fn create(
    pool: &SqlitePool,
    user_id: &str,
    input: &MailAccountInput,
) -> AppResult<AccountRow> {
    let i = validate(input)?;
    let imap_password = i.imap_password.clone().unwrap_or_default();
    if i.kind == AccountKind::Imap && imap_password.is_empty() {
        return Err(AppError::bad(
            "Passwort angeben (bei Gmail, iCloud und Outlook ein App-Passwort).",
        ));
    }
    let id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO mail_accounts (id, user_id, kind, name, email, display_name, color, signature,
         imap_host, imap_port, imap_security, imap_username, imap_password,
         smtp_host, smtp_port, smtp_security, smtp_username, smtp_password,
         sync_enabled, sync_interval_minutes, delete_from_server_after_days, archive_retention_days,
         created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(i.kind.as_str())
    .bind(&i.name)
    .bind(&i.email)
    .bind(&i.display_name)
    .bind(&i.color)
    .bind(&i.signature)
    .bind(&i.imap_host)
    .bind(i.imap_port as i64)
    .bind(i.imap_security.as_str())
    .bind(&i.imap_username)
    .bind(&imap_password)
    .bind(&i.smtp_host)
    .bind(i.smtp_port as i64)
    .bind(i.smtp_security.as_str())
    .bind(&i.smtp_username)
    .bind(i.smtp_password.clone().unwrap_or_default())
    .bind(i.sync_enabled)
    .bind(i.sync_interval_minutes as i64)
    .bind(i.delete_from_server_after_days.map(i64::from))
    .bind(i.archive_retention_days.map(i64::from))
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    get(pool, user_id, &id).await
}

/// Postfach ändern. Leere oder fehlende Passwörter bleiben unverändert.
pub async fn update(
    pool: &SqlitePool,
    user_id: &str,
    id: &str,
    input: &MailAccountInput,
) -> AppResult<AccountRow> {
    let old = get(pool, user_id, id).await?;
    if old.kind() != input.kind {
        return Err(AppError::bad(
            "Die Art eines Postfachs lässt sich nicht ändern – bitte ein neues anlegen.",
        ));
    }
    let i = validate(input)?;
    let keep = |new: &Option<String>, old: &str| match new {
        Some(p) if !p.is_empty() => p.clone(),
        _ => old.to_string(),
    };
    sqlx::query(
        "UPDATE mail_accounts SET name = ?, email = ?, display_name = ?, color = ?, signature = ?,
         imap_host = ?, imap_port = ?, imap_security = ?, imap_username = ?, imap_password = ?,
         smtp_host = ?, smtp_port = ?, smtp_security = ?, smtp_username = ?, smtp_password = ?,
         sync_enabled = ?, sync_interval_minutes = ?, delete_from_server_after_days = ?, archive_retention_days = ?,
         last_error = NULL, updated_at = ? WHERE id = ? AND user_id = ?",
    )
    .bind(&i.name)
    .bind(&i.email)
    .bind(&i.display_name)
    .bind(&i.color)
    .bind(&i.signature)
    .bind(&i.imap_host)
    .bind(i.imap_port as i64)
    .bind(i.imap_security.as_str())
    .bind(&i.imap_username)
    .bind(keep(&i.imap_password, &old.imap_password))
    .bind(&i.smtp_host)
    .bind(i.smtp_port as i64)
    .bind(i.smtp_security.as_str())
    .bind(&i.smtp_username)
    .bind(keep(&i.smtp_password, &old.smtp_password))
    .bind(i.sync_enabled)
    .bind(i.sync_interval_minutes as i64)
    .bind(i.delete_from_server_after_days.map(i64::from))
    .bind(i.archive_retention_days.map(i64::from))
    .bind(now())
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?;
    get(pool, user_id, id).await
}

/// Postfach mit allen lokalen Nachrichten, Ordnern und Entwürfen löschen.
pub async fn delete(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<AccountRow> {
    let a = get(pool, user_id, id).await?;
    sqlx::query("DELETE FROM mail_accounts WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(a)
}

/// Verbindung testen: IMAP anmelden und Ordner auflisten, SMTP anmelden.
/// Mit `existing` gelten dessen Passwörter, wo die Eingabe keine hat.
pub async fn test(
    config: &Config,
    input: &MailAccountInput,
    existing: Option<&AccountRow>,
) -> AppResult<api::ConnectionTest> {
    let mut result = api::ConnectionTest::default();
    if input.kind == AccountKind::Import {
        result.imap_ok = true;
        result.smtp_ok = true;
        result.imap_message = "Nur Archiv – kein Server nötig.".into();
        result.smtp_message = result.imap_message.clone();
        return Ok(result);
    }
    let i = validate(input)?;
    let pick = |new: &Option<String>, old: Option<&str>| match new {
        Some(p) if !p.is_empty() => p.clone(),
        _ => old.unwrap_or("").to_string(),
    };
    let row = AccountRow {
        id: String::new(),
        user_id: String::new(),
        kind: i.kind.as_str().into(),
        name: i.name.clone(),
        email: i.email.clone(),
        display_name: i.display_name.clone(),
        color: String::new(),
        signature: String::new(),
        imap_host: i.imap_host.clone(),
        imap_port: i.imap_port as i64,
        imap_security: i.imap_security.as_str().into(),
        imap_username: i.imap_username.clone(),
        imap_password: pick(&i.imap_password, existing.map(|e| e.imap_password.as_str())),
        smtp_host: i.smtp_host.clone(),
        smtp_port: i.smtp_port as i64,
        smtp_security: i.smtp_security.as_str().into(),
        smtp_username: i.smtp_username.clone(),
        smtp_password: pick(&i.smtp_password, existing.map(|e| e.smtp_password.as_str())),
        sync_enabled: false,
        sync_interval_minutes: 5,
        delete_from_server_after_days: None,
        archive_retention_days: None,
        last_sync_at: None,
        last_error: None,
        created_at: 0,
        updated_at: 0,
    };

    let imap_test = async {
        let mut session = imap::connect(&imap::Server::for_account(&row, config)).await?;
        let listed = imap::list(&mut session).await;
        imap::logout(session).await;
        listed
    };
    let smtp_test = async {
        if row.smtp_host.is_empty() {
            return Err("Kein SMTP-Server eingetragen – Senden ist nicht möglich.".to_string());
        }
        smtp::test(&row, config).await
    };
    let (imap_result, smtp_result) = tokio::join!(imap_test, smtp_test);
    match imap_result {
        Ok(list) => {
            result.imap_ok = true;
            result.folders = list
                .iter()
                .filter(|l| l.selectable)
                .map(|l| folders::decode_mutf7(&l.path))
                .collect();
            result.imap_message = format!("Verbunden – {} Ordner gefunden.", result.folders.len());
        }
        Err(e) => result.imap_message = e,
    }
    match smtp_result {
        Ok(()) => {
            result.smtp_ok = true;
            result.smtp_message = "Verbunden und angemeldet.".into();
        }
        Err(e) => result.smtp_message = e,
    }
    Ok(result)
}
