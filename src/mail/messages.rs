//! Nachrichten lesen: Liste (Ansichten, Filter, Suche, Unterhaltungen), Detail mit
//! bereinigtem HTML, Unterhaltung, Anhänge und Kontakte für die Autovervollständigung.
//! Jede Abfrage ist auf die Postfächer des angemeldeten Benutzers beschränkt.

use crate::api::{self, FolderRole, MailView, MessageQuery};
use crate::error::{AppError, AppResult};
use crate::mail::model::{addresses, AttachmentRow, DetailRow, SummaryRow, SUMMARY_COLUMNS};
use crate::mail::search::{self, Search};
use crate::mail::{html, parse, store};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::collections::HashMap;

/// Rolle zu einem Ordnernamen in der Suche (`folder:posteingang`).
fn role_alias(name: &str) -> Option<FolderRole> {
    Some(match name.to_lowercase().as_str() {
        "inbox" | "posteingang" | "eingang" => FolderRole::Inbox,
        "sent" | "gesendet" => FolderRole::Sent,
        "drafts" | "entwürfe" | "entwurf" => FolderRole::Drafts,
        "trash" | "papierkorb" => FolderRole::Trash,
        "junk" | "spam" => FolderRole::Junk,
        "archive" | "archiv" => FolderRole::Archive,
        _ => return None,
    })
}

/// Ansicht, wenn keine angegeben ist: mit Suchbegriff alles, sonst der Posteingang.
pub fn effective_view(q: &MessageQuery, s: &Search) -> MailView {
    q.view.unwrap_or(if s.is_empty() {
        MailView::Inbox
    } else {
        MailView::All
    })
}

/// `FROM … WHERE …` für eine Nachrichten-Abfrage (Benutzer, Ansicht, Filter, Suche, IDs).
pub fn push_scope(
    b: &mut QueryBuilder<Sqlite>,
    user_id: &str,
    q: &MessageQuery,
    s: &Search,
    ids: Option<&[String]>,
) {
    b.push(
        " FROM messages m JOIN mail_accounts a ON a.id = m.account_id \
         LEFT JOIN folders f ON f.id = m.folder_id WHERE a.user_id = ",
    )
    .push_bind(user_id.to_string());

    const ROLE: &str = "COALESCE(f.role, 'other')";
    // Was auf dem Server gelöscht wurde, liegt nur noch im Archiv („Alle Nachrichten“)
    const LIVE: &str = "(m.on_server = 1 OR a.kind = 'import')";
    if let Some(folder) = q.folder.as_ref().filter(|f| !f.is_empty()) {
        b.push(" AND m.folder_id = ").push_bind(folder.clone());
    } else if ids.is_none() {
        let cond = match effective_view(q, s) {
            MailView::Inbox => format!("{ROLE} = 'inbox' AND {LIVE}"),
            MailView::Unread => format!(
                "m.seen = 0 AND {ROLE} NOT IN ('trash', 'junk', 'sent', 'drafts') AND {LIVE}"
            ),
            MailView::Starred => {
                format!("m.flagged = 1 AND {ROLE} NOT IN ('trash', 'junk') AND {LIVE}")
            }
            MailView::Sent => format!(
                "({ROLE} = 'sent' OR (a.kind = 'import' AND m.is_sent = 1 AND {ROLE} NOT IN ('trash', 'junk'))) AND {LIVE}"
            ),
            MailView::Drafts => format!("{ROLE} = 'drafts' AND {LIVE}"),
            MailView::Archive => format!("{ROLE} = 'archive' AND {LIVE}"),
            MailView::Junk => format!("{ROLE} = 'junk' AND {LIVE}"),
            MailView::Trash => format!("{ROLE} = 'trash' AND {LIVE}"),
            MailView::All => format!("{ROLE} NOT IN ('trash', 'junk')"),
        };
        b.push(" AND ").push(cond);
    }
    if let Some(ids) = ids {
        if ids.is_empty() {
            b.push(" AND 0");
        } else {
            b.push(" AND m.id IN (");
            let mut sep = b.separated(", ");
            for id in ids {
                sep.push_bind(id.clone());
            }
            b.push(")");
        }
    }
    if let Some(account) = q.account.as_ref().filter(|a| !a.is_empty()) {
        b.push(" AND m.account_id = ").push_bind(account.clone());
    }
    if let Some(c) = q.category {
        b.push(" AND m.category = ").push_bind(c.as_str());
    }
    for (flag, value) in [
        ("m.seen = 0", q.unread.or(s.unread)),
        ("m.flagged = 1", q.flagged.or(s.flagged)),
        ("m.has_attachments = 1", q.attachments.or(s.has_attachment)),
        ("m.is_sent = 1", s.sent),
        ("m.answered = 1", s.answered),
    ] {
        match value {
            Some(true) => {
                b.push(" AND ").push(flag);
            }
            Some(false) => {
                b.push(" AND NOT (").push(flag).push(")");
            }
            None => {}
        }
    }
    for after in [q.after, s.after].into_iter().flatten() {
        b.push(" AND m.date >= ").push_bind(after);
    }
    for before in [q.before, s.before].into_iter().flatten() {
        b.push(" AND m.date < ").push_bind(before);
    }
    if !s.folders.is_empty() {
        b.push(" AND (");
        for (i, name) in s.folders.iter().enumerate() {
            if i > 0 {
                b.push(" OR ");
            }
            b.push("lower(f.name) = lower(")
                .push_bind(name.clone())
                .push(") OR lower(f.path) = lower(")
                .push_bind(name.clone())
                .push(")");
            if let Some(role) = role_alias(name) {
                b.push(" OR f.role = ").push_bind(role.as_str());
            }
        }
        b.push(")");
    }
    if !s.accounts.is_empty() {
        b.push(" AND (");
        for (i, name) in s.accounts.iter().enumerate() {
            if i > 0 {
                b.push(" OR ");
            }
            let like = format!("%{}%", name.to_lowercase());
            b.push("lower(a.name) LIKE ")
                .push_bind(like.clone())
                .push(" OR lower(a.email) LIKE ")
                .push_bind(like);
        }
        b.push(")");
    }
    for (list, op) in [(&s.categories, "IN"), (&s.not_categories, "NOT IN")] {
        if !list.is_empty() {
            b.push(format!(" AND m.category {op} ("));
            let mut sep = b.separated(", ");
            for c in list {
                sep.push_bind(c.as_str());
            }
            b.push(")");
        }
    }
    if let Some(expr) = s.fts_match() {
        b.push(" AND m.num IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ")
            .push_bind(expr)
            .push(")");
    }
    if let Some(expr) = s.fts_exclude() {
        b.push(" AND m.num NOT IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ")
            .push_bind(expr)
            .push(")");
    }
}

/// Eine Seite der Nachrichtenliste.
pub async fn list(
    pool: &SqlitePool,
    user_id: &str,
    q: &MessageQuery,
) -> AppResult<api::MessageList> {
    let s = search::parse(q.q.as_deref().unwrap_or(""));
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let threads = q.threads == Some(true);

    let mut b: QueryBuilder<Sqlite> = QueryBuilder::new("");
    if threads {
        b.push("WITH filtered AS (SELECT m.id, m.thread_id, m.date");
        push_scope(&mut b, user_id, q, &s, None);
        b.push(
            "), ranked AS (SELECT id, date, ROW_NUMBER() OVER (PARTITION BY thread_id ORDER BY date DESC, id DESC) AS rn FROM filtered) ",
        );
        b.push(format!("SELECT {SUMMARY_COLUMNS} FROM messages m WHERE m.id IN (SELECT id FROM ranked WHERE rn = 1 ORDER BY date DESC, id DESC LIMIT "));
        b.push_bind(limit).push(" OFFSET ").push_bind(offset);
        b.push(") ORDER BY m.date DESC, m.id DESC");
    } else {
        b.push(format!("SELECT {SUMMARY_COLUMNS}"));
        push_scope(&mut b, user_id, q, &s, None);
        b.push(" ORDER BY m.date DESC, m.id DESC LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
    }
    let rows: Vec<SummaryRow> = b.build_query_as().fetch_all(pool).await?;

    let mut c: QueryBuilder<Sqlite> = QueryBuilder::new(if threads {
        "SELECT count(DISTINCT m.thread_id)"
    } else {
        "SELECT count(*)"
    });
    push_scope(&mut c, user_id, q, &s, None);
    let total: i64 = c.build_query_scalar().fetch_one(pool).await?;

    Ok(api::MessageList {
        items: rows.iter().map(SummaryRow::to_api).collect(),
        total,
        offset,
        limit,
    })
}

async fn summary(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<SummaryRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SUMMARY_COLUMNS} FROM messages m JOIN mail_accounts a ON a.id = m.account_id WHERE m.id = ? AND a.user_id = ?"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn attachments(pool: &SqlitePool, message_id: &str) -> AppResult<Vec<AttachmentRow>> {
    Ok(sqlx::query_as(
        "SELECT id, message_id, filename, content_type, size, content_id, inline, part_index
         FROM attachments WHERE message_id = ? ORDER BY part_index",
    )
    .bind(message_id)
    .fetch_all(pool)
    .await?)
}

/// Nachricht mit bereinigtem HTML und Anhängen (markiert nicht als gelesen).
pub async fn detail(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<api::MessageDetail> {
    let summary = summary(pool, user_id, id).await?;
    let d: DetailRow = sqlx::query_as(
        "SELECT m.id, m.cc_addrs, m.bcc_addrs, m.reply_to, m.message_id, m.in_reply_to, m.text_body, m.html_body,
                m.list_unsubscribe, m.ai_summary, a.email AS account_email, f.name AS folder_name
         FROM messages m JOIN mail_accounts a ON a.id = m.account_id LEFT JOIN folders f ON f.id = m.folder_id
         WHERE m.id = ? AND a.user_id = ?",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;
    let atts = attachments(pool, id).await?;
    let cids: HashMap<String, String> = atts
        .iter()
        .filter(|a| !a.content_id.is_empty())
        .map(|a| (a.content_id.clone(), a.id.clone()))
        .collect();
    let cleaned = (!d.html_body.trim().is_empty()).then(|| html::clean(&d.html_body, &cids));
    Ok(api::MessageDetail {
        summary: summary.to_api(),
        cc: addresses(&d.cc_addrs),
        bcc: addresses(&d.bcc_addrs),
        reply_to: addresses(&d.reply_to),
        message_id_header: d.message_id,
        in_reply_to: d.in_reply_to,
        text_body: d.text_body,
        has_remote_images: cleaned.as_ref().is_some_and(|c| c.has_remote_images),
        html_body: cleaned.map(|c| c.html),
        attachments: atts.iter().map(AttachmentRow::to_api).collect(),
        list_unsubscribe: (!d.list_unsubscribe.is_empty()).then_some(d.list_unsubscribe),
        ai_summary: d.ai_summary.filter(|s| !s.trim().is_empty()),
        account_email: d.account_email,
        folder_name: d.folder_name,
    })
}

/// Unterhaltung, älteste zuerst (ohne Papierkorb und Spam, ausser alles liegt dort).
pub async fn thread(pool: &SqlitePool, user_id: &str, thread_id: &str) -> AppResult<api::Thread> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT m.id, COALESCE(f.role, 'other') FROM messages m JOIN mail_accounts a ON a.id = m.account_id
         LEFT JOIN folders f ON f.id = m.folder_id
         WHERE m.thread_id = ? AND a.user_id = ? ORDER BY m.date, m.id",
    )
    .bind(thread_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Err(AppError::NotFound);
    }
    let visible: Vec<&String> = rows
        .iter()
        .filter(|(_, role)| role != "trash" && role != "junk")
        .map(|(id, _)| id)
        .collect();
    let ids: Vec<&String> = if visible.is_empty() {
        rows.iter().map(|(id, _)| id).collect()
    } else {
        visible
    };
    let mut messages = Vec::with_capacity(ids.len());
    for id in ids {
        messages.push(detail(pool, user_id, id).await?);
    }
    let subject = messages
        .first()
        .map(|m| m.summary.subject.clone())
        .unwrap_or_default();
    Ok(api::Thread {
        id: thread_id.to_string(),
        subject,
        messages,
    })
}

/// Rohdaten mit Betreff (für den Download als `.eml`).
pub async fn raw(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<(String, Vec<u8>)> {
    let s = summary(pool, user_id, id).await?;
    Ok((s.subject, store::raw(pool, id).await?))
}

/// Anhang mit Inhalt (aus den Rohdaten).
pub async fn attachment(
    pool: &SqlitePool,
    user_id: &str,
    id: &str,
) -> AppResult<(AttachmentRow, Vec<u8>)> {
    let att: AttachmentRow = sqlx::query_as(
        "SELECT t.id, t.message_id, t.filename, t.content_type, t.size, t.content_id, t.inline, t.part_index
         FROM attachments t JOIN messages m ON m.id = t.message_id JOIN mail_accounts a ON a.id = m.account_id
         WHERE t.id = ? AND a.user_id = ?",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;
    let raw = store::raw(pool, &att.message_id).await?;
    let index = u32::try_from(att.part_index).map_err(|_| AppError::NotFound)?;
    let data = parse::part_contents(&raw, index).ok_or(AppError::NotFound)?;
    Ok((att, data))
}

/// Bekannte Adressen: Absender empfangener und Empfänger gesendeter Nachrichten, die
/// häufigsten und neuesten zuerst.
pub async fn contacts(
    pool: &SqlitePool,
    user_id: &str,
    q: &str,
    limit: i64,
) -> AppResult<Vec<api::Contact>> {
    let like = format!("%{}%", q.trim().to_lowercase());
    // Ein Name aus der neuesten Nachricht mit Namen: SQLite nimmt bei genau einem max()
    // die übrigen Spalten aus derselben Zeile; Zeilen mit Namen zählen dafür später.
    const NAMED: i64 = 10_000_000_000;
    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "WITH c(name, address, date) AS (
            SELECT m.from_name, lower(m.from_address), m.date FROM messages m JOIN mail_accounts a ON a.id = m.account_id
            WHERE a.user_id = ?1 AND m.is_sent = 0 AND m.from_address <> ''
            UNION ALL
            SELECT COALESCE(json_extract(j.value, '$.name'), ''), lower(json_extract(j.value, '$.address')), m.date
            FROM messages m JOIN mail_accounts a ON a.id = m.account_id, json_each(m.to_addrs) j
            WHERE a.user_id = ?1 AND m.is_sent = 1
            UNION ALL
            SELECT COALESCE(json_extract(j.value, '$.name'), ''), lower(json_extract(j.value, '$.address')), m.date
            FROM messages m JOIN mail_accounts a ON a.id = m.account_id, json_each(m.cc_addrs) j
            WHERE a.user_id = ?1 AND m.is_sent = 1
         )
         SELECT address, name, count(*) AS n, max(date + CASE WHEN name <> '' THEN ?2 ELSE 0 END) AS k
         FROM c WHERE address IS NOT NULL AND address <> '' AND (address LIKE ?3 OR lower(name) LIKE ?3)
         GROUP BY address ORDER BY n DESC, k DESC LIMIT ?4",
    )
    .bind(user_id)
    .bind(NAMED)
    .bind(&like)
    .bind(limit.clamp(1, 100))
    .fetch_all(pool)
    .await?;
    let own: Vec<String> =
        sqlx::query_scalar("SELECT lower(email) FROM mail_accounts WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter(|(address, ..)| !own.contains(address))
        .map(|(address, name, count, k)| api::Contact {
            name,
            address,
            count,
            last_seen: if k >= NAMED { k - NAMED } else { k },
        })
        .collect())
}

/// Für Export und Aktionen: IDs der Nachrichten zu einer Abfrage (oder den angegebenen IDs).
pub async fn select_ids(
    pool: &SqlitePool,
    user_id: &str,
    q: &MessageQuery,
    ids: Option<&[String]>,
) -> AppResult<Vec<(String, String, i64)>> {
    let s = search::parse(q.q.as_deref().unwrap_or(""));
    let mut b: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT m.id, m.from_address, m.date");
    push_scope(&mut b, user_id, q, &s, ids);
    b.push(" ORDER BY m.date, m.id");
    Ok(b.build_query_as().fetch_all(pool).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sql(q: &MessageQuery) -> String {
        let s = search::parse(q.q.as_deref().unwrap_or(""));
        let mut b: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT m.id");
        push_scope(&mut b, "u1", q, &s, None);
        b.sql().as_str().to_string()
    }

    #[test]
    fn query_language_becomes_sql() {
        let q = MessageQuery {
            view: Some(MailView::Unread),
            q: Some(
                "rechnung has:attachment -werbung category:newsletter folder:archiv after:2024"
                    .into(),
            ),
            ..MessageQuery::default()
        };
        let s = sql(&q);
        for part in [
            "a.user_id = ?",
            "m.seen = 0",
            "NOT IN ('trash', 'junk', 'sent', 'drafts')",
            "m.has_attachments = 1",
            "m.category IN (?)",
            "lower(f.name) = lower(?)",
            "f.role = ?",
            "m.date >= ?",
            "m.num IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)",
            "m.num NOT IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)",
        ] {
            assert!(s.contains(part), "{part} fehlt in {s}");
        }
        // Mit Suchbegriff ohne Ansicht: alles (auch nur noch lokal), ohne Papierkorb/Spam
        let s = sql(&MessageQuery {
            q: Some("x".into()),
            ..MessageQuery::default()
        });
        assert!(
            s.contains("NOT IN ('trash', 'junk')") && !s.contains("on_server = 1"),
            "{s}"
        );
        // Ohne alles: Posteingang, nur was noch auf dem Server liegt
        let s = sql(&MessageQuery::default());
        assert!(
            s.contains("= 'inbox'") && s.contains("m.on_server = 1 OR a.kind = 'import'"),
            "{s}"
        );
        // Ordner statt Ansicht
        let s = sql(&MessageQuery {
            folder: Some("f1".into()),
            view: Some(MailView::Trash),
            ..MessageQuery::default()
        });
        assert!(
            s.contains("m.folder_id = ?") && !s.contains("'trash'"),
            "{s}"
        );
    }
}
