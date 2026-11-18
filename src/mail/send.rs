//! Senden und Postausgang: sofort senden (SMTP), danach die Kopie per IMAP APPEND in den
//! Gesendet-Ordner (nicht bei Gmail – das legt sie selbst ab) und lokal speichern, das
//! Original als beantwortet markieren. Entwürfe, geplante (`sendAt`) und fehlgeschlagene
//! Nachrichten liegen in `outbox`; die Hintergrundschleife sendet fällige.

use crate::api::{self, Address, Compose, FolderRole, OutboxItem, OutboxStatus, SendResult};
use crate::config::Config;
use crate::db::models::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::mail::model::AccountRow;
use crate::mail::smtp::{self, Built, Outgoing};
use crate::mail::store::{self, Flags, NewMessage};
use crate::mail::{accounts, actions, imap};
use base64::Engine;
use sqlx::SqlitePool;

/// Grösste Nachricht (Rohdaten inkl. Anhängen).
pub const MAX_BYTES: usize = 25 * 1024 * 1024;

fn decode_attachment(a: &api::OutgoingAttachment) -> AppResult<Vec<u8>> {
    let data = a.data_base64.trim();
    // Auch als Data-URL („data:…;base64,…“)
    let data = data.split_once(";base64,").map(|(_, d)| d).unwrap_or(data);
    let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(clean)
        .map_err(|_| AppError::bad(format!("Anhang „{}“ ist nicht gültig kodiert.", a.filename)))
}

fn check_recipients(c: &Compose) -> AppResult<()> {
    let all: Vec<&Address> = c.to.iter().chain(&c.cc).chain(&c.bcc).collect();
    if all.is_empty() {
        return Err(AppError::bad("Bitte mindestens einen Empfänger angeben."));
    }
    if let Some(bad) = all.iter().find(|a| !a.is_valid()) {
        return Err(AppError::bad(format!(
            "Ungültige Adresse: {}",
            bad.address.trim()
        )));
    }
    Ok(())
}

/// Nachricht zusammenbauen: Antwort-Header, Anhänge (auch weitergeleitete), Grösse prüfen.
pub async fn prepare(
    pool: &SqlitePool,
    account: &AccountRow,
    c: &Compose,
) -> AppResult<(Built, Option<String>)> {
    check_recipients(c)?;
    let domain = account
        .email
        .rsplit_once('@')
        .map(|(_, d)| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "nrzmail.local".into());
    let message_id = format!("<{}@{domain}>", uuid::Uuid::new_v4());

    // Antwort: In-Reply-To und References vom Original
    let mut in_reply_to = None;
    let mut references = None;
    let mut original = None;
    if let Some(orig) = c.in_reply_to.as_deref().filter(|s| !s.is_empty()) {
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT m.id, m.message_id, m.refs FROM messages m JOIN mail_accounts a ON a.id = m.account_id
             WHERE m.id = ? AND a.user_id = ?",
        )
        .bind(orig)
        .bind(&account.user_id)
        .fetch_optional(pool)
        .await?;
        let (id, mid, refs) = row.ok_or(AppError::NotFound)?;
        if !mid.is_empty() {
            let mut list: Vec<String> = refs
                .split_whitespace()
                .map(|r| format!("<{}>", r.trim_matches(['<', '>'])))
                .collect();
            list.push(format!("<{mid}>"));
            in_reply_to = Some(format!("<{mid}>"));
            references = Some(list.join(" "));
        }
        original = Some(id);
    }
    if let Some(fwd) = c.forward_of.as_deref().filter(|s| !s.is_empty()) {
        let ok: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM messages m JOIN mail_accounts a ON a.id = m.account_id WHERE m.id = ? AND a.user_id = ?",
        )
        .bind(fwd)
        .bind(&account.user_id)
        .fetch_optional(pool)
        .await?;
        ok.ok_or(AppError::NotFound)?;
    }

    let mut attachments = Vec::new();
    let mut total = c.text.len() + c.html.as_ref().map_or(0, String::len);
    for a in &c.attachments {
        let data = decode_attachment(a)?;
        total += data.len();
        if total > MAX_BYTES {
            return Err(too_large(total));
        }
        let name = if a.filename.trim().is_empty() {
            "Anhang".to_string()
        } else {
            a.filename.trim().to_string()
        };
        let ctype = if a.content_type.trim().is_empty() {
            "application/octet-stream".to_string()
        } else {
            a.content_type.trim().to_string()
        };
        attachments.push((name, ctype, data));
    }
    for id in &c.forward_attachments {
        let (att, data) = crate::mail::messages::attachment(pool, &account.user_id, id).await?;
        total += data.len();
        if total > MAX_BYTES {
            return Err(too_large(total));
        }
        attachments.push((att.filename, att.content_type, data));
    }

    let outgoing = Outgoing {
        from: Address::new(account.display_name.trim(), account.email.trim()),
        to: c.to.clone(),
        cc: c.cc.clone(),
        bcc: c.bcc.clone(),
        subject: c.subject.trim().to_string(),
        text: c.text.clone(),
        html: c.html.clone().filter(|h| !h.trim().is_empty()),
        message_id,
        in_reply_to,
        references,
        attachments,
    };
    let built = smtp::build(&outgoing).map_err(AppError::bad)?;
    if built.raw_with_bcc.len() > MAX_BYTES {
        return Err(too_large(built.raw_with_bcc.len()));
    }
    Ok((built, original))
}

fn too_large(bytes: usize) -> AppError {
    AppError::bad(format!(
        "Die Nachricht ist mit {:.1} MB zu gross – höchstens 25 MB sind möglich (grosse Dateien besser als Link teilen).",
        bytes as f64 / 1024.0 / 1024.0
    ))
}

/// Senden und alles danach (beantwortet, Gesendet-Kopie). Gibt die lokale ID der Kopie zurück.
pub async fn deliver(
    pool: &SqlitePool,
    config: &Config,
    account: &AccountRow,
    built: &Built,
    original: Option<&str>,
) -> Result<String, String> {
    smtp::send(account, config, built).await?;
    match after_send(pool, config, account, built, original).await {
        Ok(id) => Ok(id),
        Err(e) => {
            // Gesendet ist gesendet – nur protokollieren
            tracing::warn!(account = %account.id, "Nach dem Senden: {e}");
            Ok(String::new())
        }
    }
}

async fn after_send(
    pool: &SqlitePool,
    config: &Config,
    account: &AccountRow,
    built: &Built,
    original: Option<&str>,
) -> AppResult<String> {
    let orig: Option<(String, Option<i64>, bool, Option<String>)> = match original {
        Some(id) => {
            sqlx::query_as(
                "SELECT m.id, m.uid, m.on_server, f.path FROM messages m LEFT JOIN folders f ON f.id = m.folder_id WHERE m.id = ?",
            )
            .bind(id)
            .fetch_optional(pool)
            .await?
        }
        None => None,
    };
    let mut appended = false;
    if account.is_imap() {
        match imap::connect(&imap::Server::for_account(account, config)).await {
            Ok(mut s) => {
                if let Some((_, Some(uid), true, Some(path))) = &orig {
                    if let Ok(uid) = u32::try_from(*uid) {
                        let r = async {
                            imap::select(&mut s, path).await?;
                            imap::store(&mut s, &[uid], "+FLAGS.SILENT (\\Answered)").await
                        }
                        .await;
                        if let Err(e) = r {
                            tracing::warn!("Als beantwortet markieren: {e}");
                        }
                    }
                }
                if !account.is_gmail() {
                    let folder =
                        actions::ensure_role(pool, account, FolderRole::Sent, Some(&mut s)).await?;
                    if folder.on_server {
                        match imap::append(&mut s, &folder.path, "\\Seen", &built.raw_with_bcc)
                            .await
                        {
                            Ok(()) => appended = true,
                            Err(e) => tracing::warn!("Kopie in Gesendet: {e}"),
                        }
                    }
                }
                imap::logout(s).await;
            }
            Err(e) => tracing::warn!("Nach dem Senden (IMAP): {e}"),
        }
    }
    if let Some((id, ..)) = &orig {
        sqlx::query("UPDATE messages SET answered = 1 WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
    }
    let folder = actions::ensure_role(pool, account, FolderRole::Sent, None).await?;
    let stored = store::insert_one(
        pool,
        NewMessage {
            account,
            folder: Some(&folder),
            uid: None,
            // Gmail legt die Kopie selbst ab; der nächste Abgleich findet sie über die Message-ID
            on_server: appended || (account.is_imap() && account.is_gmail()),
            raw: &built.raw_with_bcc,
            flags: Flags {
                seen: true,
                ..Flags::default()
            },
            internal_date: Some(now()),
            dedupe: false,
        },
    )
    .await?;
    Ok(stored.id().to_string())
}

async fn insert_outbox(
    pool: &SqlitePool,
    user_id: &str,
    c: &Compose,
    status: OutboxStatus,
    send_at: Option<i64>,
) -> AppResult<String> {
    let id = new_id();
    let ts = now();
    sqlx::query(
        "INSERT INTO outbox (id, user_id, account_id, status, payload, send_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(&c.account_id)
    .bind(status.as_str())
    .bind(serde_json::to_string(c).unwrap_or_else(|_| "{}".into()))
    .bind(send_at)
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn delete_draft(pool: &SqlitePool, user_id: &str, draft_id: Option<&str>) -> AppResult<()> {
    if let Some(id) = draft_id.filter(|s| !s.is_empty()) {
        sqlx::query(
            "DELETE FROM outbox WHERE id = ? AND user_id = ? AND status IN ('draft', 'failed')",
        )
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// `POST /send`: sofort senden, (mit `sendAt` in der Zukunft) planen oder – mit
/// Sendeverzögerung `undo_seconds` – erst nach dieser Zeit senden, damit es sich noch
/// zurückziehen lässt (`undo`).
pub async fn send(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    c: &Compose,
    undo_seconds: u32,
) -> AppResult<SendResult> {
    let account = accounts::get(pool, user_id, &c.account_id).await?;
    if account.smtp_host.trim().is_empty() {
        return Err(AppError::bad(
            "Für dieses Postfach ist kein SMTP-Server eingetragen – Senden ist nicht möglich.",
        ));
    }
    let ts = now();
    if let Some(at) = c.send_at.filter(|at| *at > ts + 5) {
        check_recipients(c)?;
        let mut stored = c.clone();
        stored.draft_id = None;
        let id = insert_outbox(pool, user_id, &stored, OutboxStatus::Scheduled, Some(at)).await?;
        delete_draft(pool, user_id, c.draft_id.as_deref()).await?;
        return Ok(SendResult {
            status: OutboxStatus::Scheduled,
            outbox_id: id,
            message_id: None,
            undo_until: None,
        });
    }
    let (built, original) = prepare(pool, &account, c).await?;
    let mut stored = c.clone();
    stored.draft_id = None;
    let undo_seconds = undo_seconds.min(api::UNDO_SEND_MAX);
    if undo_seconds > 0 {
        // Intern zurückhalten: geprüft ist sie schon, gesendet wird sie vom Hintergrund
        let at = ts + i64::from(undo_seconds);
        stored.send_at = None;
        let id = insert_outbox(pool, user_id, &stored, OutboxStatus::Scheduled, Some(at)).await?;
        delete_draft(pool, user_id, c.draft_id.as_deref()).await?;
        return Ok(SendResult {
            status: OutboxStatus::Scheduled,
            outbox_id: id,
            message_id: None,
            undo_until: Some(at),
        });
    }
    let outbox_id = insert_outbox(pool, user_id, &stored, OutboxStatus::Sending, None).await?;
    match deliver(pool, config, &account, &built, original.as_deref()).await {
        Ok(local) => {
            sqlx::query("DELETE FROM outbox WHERE id = ?")
                .bind(&outbox_id)
                .execute(pool)
                .await?;
            delete_draft(pool, user_id, c.draft_id.as_deref()).await?;
            Ok(SendResult {
                status: OutboxStatus::Sent,
                outbox_id,
                message_id: (!local.is_empty()).then_some(local),
                undo_until: None,
            })
        }
        Err(e) => {
            sqlx::query(
                "UPDATE outbox SET status = 'failed', error = ?, updated_at = ? WHERE id = ?",
            )
            .bind(&e)
            .bind(now())
            .bind(&outbox_id)
            .execute(pool)
            .await?;
            Err(AppError::bad(format!(
                "Senden fehlgeschlagen: {e} Die Nachricht liegt im Postausgang."
            )))
        }
    }
}

#[derive(sqlx::FromRow)]
struct OutboxRow {
    id: String,
    account_id: String,
    status: String,
    payload: String,
    send_at: Option<i64>,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl OutboxRow {
    fn to_api(&self) -> OutboxItem {
        OutboxItem {
            id: self.id.clone(),
            account_id: self.account_id.clone(),
            status: OutboxStatus::parse(&self.status).unwrap_or_default(),
            compose: serde_json::from_str(&self.payload).unwrap_or_default(),
            send_at: self.send_at,
            error: self.error.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

const OUTBOX_COLUMNS: &str =
    "id, account_id, status, payload, send_at, error, created_at, updated_at";

async fn outbox_row(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<OutboxRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {OUTBOX_COLUMNS} FROM outbox WHERE id = ? AND user_id = ?"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// `GET /outbox`: Entwürfe, geplante und fehlgeschlagene (neueste zuerst).
pub async fn list(
    pool: &SqlitePool,
    user_id: &str,
    status: Option<OutboxStatus>,
) -> AppResult<Vec<OutboxItem>> {
    let rows: Vec<OutboxRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {OUTBOX_COLUMNS} FROM outbox WHERE user_id = ? AND status <> 'sent' AND (? IS NULL OR status = ?)
         ORDER BY updated_at DESC"
    )))
    .bind(user_id)
    .bind(status.map(OutboxStatus::as_str))
    .bind(status.map(OutboxStatus::as_str))
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(OutboxRow::to_api).collect())
}

/// `POST /drafts`
pub async fn create_draft(pool: &SqlitePool, user_id: &str, c: &Compose) -> AppResult<OutboxItem> {
    accounts::get(pool, user_id, &c.account_id).await?;
    let id = insert_outbox(pool, user_id, c, OutboxStatus::Draft, None).await?;
    Ok(outbox_row(pool, user_id, &id).await?.to_api())
}

/// `PUT /drafts/<id>` (auch geplante und fehlgeschlagene werden damit wieder zum Entwurf).
pub async fn update_draft(
    pool: &SqlitePool,
    user_id: &str,
    id: &str,
    c: &Compose,
) -> AppResult<OutboxItem> {
    let row = outbox_row(pool, user_id, id).await?;
    if row.status == "sending" {
        return Err(AppError::Conflict(
            "Die Nachricht wird gerade gesendet.".into(),
        ));
    }
    accounts::get(pool, user_id, &c.account_id).await?;
    sqlx::query(
        "UPDATE outbox SET account_id = ?, status = 'draft', payload = ?, send_at = NULL, error = NULL, updated_at = ?
         WHERE id = ? AND user_id = ?",
    )
    .bind(&c.account_id)
    .bind(serde_json::to_string(c).unwrap_or_else(|_| "{}".into()))
    .bind(now())
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(outbox_row(pool, user_id, id).await?.to_api())
}

/// `DELETE /outbox/<id>`
pub async fn delete(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<()> {
    let row = outbox_row(pool, user_id, id).await?;
    if row.status == "sending" {
        return Err(AppError::Conflict(
            "Die Nachricht wird gerade gesendet.".into(),
        ));
    }
    sqlx::query("DELETE FROM outbox WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// `POST /outbox/<id>/undo`: Senden zurückziehen, solange die Nachricht noch wartet. Sie wird
/// wieder zum Entwurf. Ist sie schon unterwegs oder gesendet, gibt es einen Konflikt.
pub async fn undo(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<OutboxItem> {
    let changed = sqlx::query(
        "UPDATE outbox SET status = 'draft', send_at = NULL, error = NULL, updated_at = ?
         WHERE id = ? AND user_id = ? AND status = 'scheduled'",
    )
    .bind(now())
    .bind(id)
    .bind(user_id)
    .execute(pool)
    .await?
    .rows_affected();
    if changed == 0 {
        return match outbox_row(pool, user_id, id).await {
            Ok(row) if row.status == "draft" => Ok(row.to_api()),
            Ok(_) => Err(AppError::Conflict(
                "Zu spät – die Nachricht wird gerade gesendet.".into(),
            )),
            Err(AppError::NotFound) => Err(AppError::Conflict(
                "Zu spät – die Nachricht ist schon gesendet.".into(),
            )),
            Err(e) => Err(e),
        };
    }
    Ok(outbox_row(pool, user_id, id).await?.to_api())
}

/// Hintergrund: fällige geplante Nachrichten senden.
pub async fn send_due(pool: &SqlitePool, config: &Config) -> AppResult<usize> {
    let due: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, user_id, payload FROM outbox WHERE status = 'scheduled' AND send_at <= ? ORDER BY send_at",
    )
    .bind(now())
    .fetch_all(pool)
    .await?;
    let mut sent = 0;
    for (id, user_id, payload) in due {
        let claimed = sqlx::query(
            "UPDATE outbox SET status = 'sending', updated_at = ? WHERE id = ? AND status = 'scheduled'",
        )
        .bind(now())
        .bind(&id)
        .execute(pool)
        .await?
        .rows_affected();
        if claimed == 0 {
            continue;
        }
        let result: Result<(), String> = async {
            let c: Compose = serde_json::from_str(&payload)
                .map_err(|_| "Gespeicherte Nachricht ist beschädigt.".to_string())?;
            let account = accounts::get(pool, &user_id, &c.account_id)
                .await
                .map_err(|e| e.public_message())?;
            let (built, original) = prepare(pool, &account, &c)
                .await
                .map_err(|e| e.public_message())?;
            deliver(pool, config, &account, &built, original.as_deref()).await?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                sqlx::query("DELETE FROM outbox WHERE id = ?")
                    .bind(&id)
                    .execute(pool)
                    .await?;
                sent += 1;
            }
            Err(e) => {
                tracing::warn!(outbox = %id, "Geplantes Senden: {e}");
                sqlx::query(
                    "UPDATE outbox SET status = 'failed', error = ?, updated_at = ? WHERE id = ?",
                )
                .bind(&e)
                .bind(now())
                .bind(&id)
                .execute(pool)
                .await?;
            }
        }
    }
    Ok(sent)
}

/// Beim Start: was beim letzten Lauf mitten im Senden stecken blieb, als fehlgeschlagen zeigen.
pub async fn recover_stuck(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query(
        "UPDATE outbox SET status = 'failed', error = 'Senden wurde unterbrochen – bitte prüfen und erneut senden.'
         WHERE status = 'sending'",
    )
    .execute(pool)
    .await?;
    Ok(())
}
