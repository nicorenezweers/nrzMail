//! Nachrichten speichern und lesen: Zeile in `messages`, Rohdaten zlib-komprimiert in
//! `message_raw`, Anhänge als Metadaten, Eintrag im Suchindex – alles in einer Transaktion.
//! Unterhaltungen (Threads) entstehen über `In-Reply-To`/`References`.

use crate::api::FolderRole;
use crate::db::models::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::mail::model::{addresses_json, AccountRow, FolderRow};
use crate::mail::parse::{self, Parsed};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sqlx::{SqliteConnection, SqlitePool};
use std::io::{Read, Write};

/// IMAP-Flags einer Nachricht.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
}

/// Neue Nachricht zum Speichern.
pub struct NewMessage<'a> {
    pub account: &'a AccountRow,
    pub folder: Option<&'a FolderRow>,
    pub uid: Option<u32>,
    pub on_server: bool,
    pub raw: &'a [u8],
    pub flags: Flags,
    /// Empfangszeit vom Server (IMAP INTERNALDATE), falls der Date-Header fehlt
    pub internal_date: Option<i64>,
    /// Gleiche Rohdaten im Postfach → nicht noch einmal speichern
    pub dedupe: bool,
}

/// Ergebnis von [`insert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    Inserted(String),
    /// Gab es schon (ID der vorhandenen Nachricht)
    Duplicate(String),
}

impl Stored {
    pub fn id(&self) -> &str {
        match self {
            Stored::Inserted(id) | Stored::Duplicate(id) => id,
        }
    }
}

pub fn compress(raw: &[u8]) -> AppResult<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::with_capacity(raw.len() / 3), Compression::default());
    enc.write_all(raw)?;
    Ok(enc.finish()?)
}

pub fn decompress(data: &[u8]) -> AppResult<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 3);
    ZlibDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}

/// Unterhaltung finden: über die Vorgänger (References, In-Reply-To), über Antworten, die
/// schon vor der Nachricht da waren, oder über eine Kopie derselben Nachricht.
async fn thread_for(
    conn: &mut SqliteConnection,
    user_id: &str,
    parsed: &Parsed,
) -> AppResult<String> {
    let mut refs: Vec<&str> = parsed
        .references
        .iter()
        .rev()
        .take(30)
        .map(String::as_str)
        .collect();
    if !parsed.in_reply_to.is_empty() {
        refs.push(&parsed.in_reply_to);
    }
    if !parsed.message_id.is_empty() {
        refs.push(&parsed.message_id);
    }
    refs.sort_unstable();
    refs.dedup();
    if !refs.is_empty() {
        let mut b = sqlx::QueryBuilder::new(
            "SELECT m.thread_id FROM messages m JOIN mail_accounts a ON a.id = m.account_id WHERE a.user_id = ",
        );
        b.push_bind(user_id).push(" AND m.message_id IN (");
        let mut sep = b.separated(", ");
        for r in &refs {
            sep.push_bind(*r);
        }
        b.push(") ORDER BY m.date LIMIT 1");
        if let Some(t) = b
            .build_query_scalar::<String>()
            .fetch_optional(&mut *conn)
            .await?
        {
            return Ok(t);
        }
    }
    if !parsed.message_id.is_empty() {
        let child: Option<String> = sqlx::query_scalar(
            "SELECT m.thread_id FROM messages m JOIN mail_accounts a ON a.id = m.account_id
             WHERE a.user_id = ? AND m.in_reply_to = ? LIMIT 1",
        )
        .bind(user_id)
        .bind(&parsed.message_id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(t) = child {
            return Ok(t);
        }
    }
    Ok(new_id())
}

/// Nachricht speichern (Aufrufer hält die Transaktion).
pub async fn insert(conn: &mut SqliteConnection, m: NewMessage<'_>) -> AppResult<Stored> {
    let sha = parse::sha256(m.raw);
    if m.dedupe {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM messages WHERE account_id = ? AND sha256 = ? LIMIT 1",
        )
        .bind(&m.account.id)
        .bind(&sha)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(id) = existing {
            return Ok(Stored::Duplicate(id));
        }
    }
    let parsed = parse::parse(m.raw);
    let role = m.folder.map(FolderRow::role).unwrap_or_default();
    let is_sent = role == FolderRole::Sent
        || (!parsed.from.address.is_empty()
            && parsed
                .from
                .address
                .eq_ignore_ascii_case(m.account.email.trim()));
    let category = parsed.category(is_sent);
    let thread_id = thread_for(conn, &m.account.user_id, &parsed).await?;
    let ts = now();
    let date = sort_date(parsed.date, m.internal_date, ts);
    let id = new_id();
    sqlx::query(
        "INSERT INTO messages (id, account_id, folder_id, uid, on_server, message_id, in_reply_to, refs, thread_id,
         subject, from_name, from_address, to_addrs, cc_addrs, bcc_addrs, reply_to, date, received_at, snippet,
         text_body, html_body, size, seen, flagged, answered, draft, is_sent, has_attachments, category,
         list_unsubscribe, sha256, created_at, trashed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&m.account.id)
    .bind(m.folder.map(|f| f.id.as_str()))
    .bind(m.uid.map(i64::from))
    .bind(m.on_server)
    .bind(&parsed.message_id)
    .bind(&parsed.in_reply_to)
    .bind(parsed.references.join(" "))
    .bind(&thread_id)
    .bind(&parsed.subject)
    .bind(&parsed.from.name)
    .bind(&parsed.from.address)
    .bind(addresses_json(&parsed.to))
    .bind(addresses_json(&parsed.cc))
    .bind(addresses_json(&parsed.bcc))
    .bind(addresses_json(&parsed.reply_to))
    .bind(date)
    .bind(ts)
    .bind(&parsed.snippet)
    .bind(&parsed.text)
    .bind(&parsed.html)
    .bind(m.raw.len() as i64)
    .bind(m.flags.seen || is_sent)
    .bind(m.flags.flagged)
    .bind(m.flags.answered)
    .bind(m.flags.draft || role == FolderRole::Drafts)
    .bind(is_sent)
    .bind(parsed.has_attachments())
    .bind(category.as_str())
    .bind(&parsed.list_unsubscribe)
    .bind(&sha)
    .bind(ts)
    .bind((role == FolderRole::Trash).then_some(ts))
    .execute(&mut *conn)
    .await?;

    sqlx::query("INSERT INTO message_raw (message_id, compressed, data) VALUES (?, 1, ?)")
        .bind(&id)
        .bind(compress(m.raw)?)
        .execute(&mut *conn)
        .await?;

    for a in &parsed.attachments {
        sqlx::query(
            "INSERT INTO attachments (id, message_id, filename, content_type, size, content_id, inline, part_index)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(&id)
        .bind(&a.filename)
        .bind(&a.content_type)
        .bind(a.size)
        .bind(&a.content_id)
        .bind(a.inline)
        .bind(a.part_index as i64)
        .execute(&mut *conn)
        .await?;
    }

    sqlx::query(
        "INSERT INTO messages_fts (rowid, message_id, subject, sender, recipients, body, attachment_names)
         SELECT num, id, ?, ?, ?, ?, ? FROM messages WHERE id = ?",
    )
    .bind(&parsed.subject)
    .bind(format!("{} {}", parsed.from.name, parsed.from.address))
    .bind(parsed.recipients_text())
    .bind(parsed.index_text())
    .bind(parsed.attachment_names())
    .bind(&id)
    .execute(&mut *conn)
    .await?;

    Ok(Stored::Inserted(id))
}

/// Speichern in einer eigenen Transaktion.
pub async fn insert_one(pool: &SqlitePool, m: NewMessage<'_>) -> AppResult<Stored> {
    let mut tx = pool.begin().await?;
    let stored = insert(&mut tx, m).await?;
    tx.commit().await?;
    Ok(stored)
}

/// Zeitpunkt, nach dem die Liste sortiert: der `Date`-Header, ausser er ist offensichtlich
/// falsch. Den setzt der Absender selbst – Spam datiert gern in die Zukunft, falsch gestellte
/// Uhren und lange liegengebliebene Mails rutschen sonst weit nach unten. Weicht er mehr als
/// zwei Tage von der Ankunft beim Server (INTERNALDATE) ab, zählt die Ankunft. Ohne
/// INTERNALDATE (Import) gilt der Header, ausser er liegt mehr als einen Tag in der Zukunft.
pub fn sort_date(header: Option<i64>, internal: Option<i64>, now: i64) -> i64 {
    const TWO_DAYS: i64 = 2 * 86_400;
    match (header, internal) {
        (Some(h), Some(i)) if (h - i).abs() > TWO_DAYS => i,
        (Some(h), _) if h > now + 86_400 => internal.unwrap_or(now),
        (Some(h), _) => h,
        (None, Some(i)) => i,
        (None, None) => now,
    }
}

/// Rohdaten einer Nachricht (der Aufrufer hat den Zugriff schon geprüft).
pub async fn raw(pool: &SqlitePool, message_id: &str) -> AppResult<Vec<u8>> {
    let (compressed, data): (bool, Vec<u8>) =
        sqlx::query_as("SELECT compressed, data FROM message_raw WHERE message_id = ?")
            .bind(message_id)
            .fetch_optional(pool)
            .await?
            .ok_or(AppError::NotFound)?;
    if compressed {
        decompress(&data)
    } else {
        Ok(data)
    }
}

/// Nachrichten endgültig löschen (Rohdaten, Anhänge und Suchindex über Fremdschlüssel/Trigger).
pub async fn delete(pool: &SqlitePool, ids: &[String]) -> AppResult<u64> {
    let mut n = 0;
    for chunk in ids.chunks(500) {
        let mut b = sqlx::QueryBuilder::new("DELETE FROM messages WHERE id IN (");
        let mut sep = b.separated(", ");
        for id in chunk {
            sep.push_bind(id);
        }
        b.push(")");
        n += b.build().execute(pool).await?.rows_affected();
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_round_trip() {
        let raw = b"Subject: x\r\n\r\n".repeat(100);
        let c = compress(&raw).unwrap();
        assert!(c.len() < raw.len());
        assert_eq!(decompress(&c).unwrap(), raw);
    }
}

#[cfg(test)]
mod sort_date_tests {
    use super::sort_date;

    #[test]
    fn wrong_headers_fall_back_to_arrival() {
        let now = 1_800_000_000;
        assert_eq!(sort_date(Some(now - 60), Some(now), now), now - 60);
        // In die Zukunft datiert (Spam)
        assert_eq!(sort_date(Some(now + 30 * 86_400), Some(now - 10), now), now - 10);
        // Uralter Header, eben angekommen
        assert_eq!(sort_date(Some(now - 400 * 86_400), Some(now), now), now);
        // Import ohne INTERNALDATE: Header gilt, ausser Zukunft
        assert_eq!(sort_date(Some(now - 400 * 86_400), None, now), now - 400 * 86_400);
        assert_eq!(sort_date(Some(now + 5 * 86_400), None, now), now);
        assert_eq!(sort_date(None, None, now), now);
    }
}
