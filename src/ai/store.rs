//! Nachrichten und Postfächer für die KI lesen – direkt per SQL aus dem festen Schema und
//! immer eingeschränkt auf die Postfächer des angemeldeten Benutzers (`mail_accounts.user_id`).
//! Fremde oder unbekannte Nachrichten sind schlicht „nicht gefunden“.

use super::text;
use crate::api;
use crate::error::{AppError, AppResult};
use sqlx::SqlitePool;

/// SQL aus Konstanten und `?`-Platzhaltern (geprüft: nie mit Eingaben zusammengesetzt).
pub fn sql(text: String) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(text)
}

/// Spalten einer Nachricht samt Ordner und Postfach.
pub const SELECT: &str = "SELECT m.id, m.account_id, m.thread_id, m.subject, m.from_name, m.from_address,
        m.to_addrs, m.cc_addrs, m.date, m.snippet, m.text_body, m.html_body, m.seen, m.is_sent,
        m.has_attachments, m.category, m.category_source, m.list_unsubscribe,
        f.role AS folder_role, f.name AS folder_name, a.name AS account_name, a.email AS account_email
    FROM messages m
    JOIN mail_accounts a ON a.id = m.account_id
    LEFT JOIN folders f ON f.id = m.folder_id";

/// Eine Nachricht, wie die KI sie braucht.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Mail {
    pub id: String,
    pub account_id: String,
    pub thread_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_address: String,
    pub to_addrs: String,
    pub cc_addrs: String,
    pub date: i64,
    pub snippet: String,
    pub text_body: String,
    pub html_body: String,
    pub seen: bool,
    pub is_sent: bool,
    pub has_attachments: bool,
    pub category: String,
    pub category_source: String,
    pub list_unsubscribe: String,
    pub folder_role: Option<String>,
    pub folder_name: Option<String>,
    pub account_name: String,
    pub account_email: String,
}

fn addresses(json: &str) -> Vec<api::Address> {
    serde_json::from_str(json).unwrap_or_default()
}

fn list(a: &[api::Address]) -> String {
    a.iter()
        .take(10)
        .map(|a| text::one_line(&a.display(), 120))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Mail {
    pub fn from(&self) -> api::Address {
        api::Address::new(self.from_name.clone(), self.from_address.clone())
    }

    pub fn to(&self) -> Vec<api::Address> {
        addresses(&self.to_addrs)
    }

    pub fn cc(&self) -> Vec<api::Address> {
        addresses(&self.cc_addrs)
    }

    /// Lesbarer Text der Nachricht.
    pub fn body(&self) -> String {
        text::body_text(&self.text_body, &self.html_body)
    }

    /// Text ohne zitierten Verlauf.
    pub fn own_text(&self) -> String {
        text::strip_quotes(&self.body())
    }

    pub fn to_ref(&self) -> api::MessageRef {
        api::MessageRef {
            id: self.id.clone(),
            subject: self.subject.clone(),
            from: self.from(),
            date: self.date,
        }
    }

    /// Ordner für die Anzeige („Posteingang“, „Gesendet“, eigener Name, „nur Archiv“).
    pub fn folder_label(&self) -> String {
        match (self.folder_role.as_deref(), self.folder_name.as_deref()) {
            (Some(role), name) if role != "other" => api::FolderRole::parse(role)
                .map(|r| r.label().to_string())
                .or(name.map(String::from))
                .unwrap_or_default(),
            (_, Some(name)) => name.to_string(),
            _ => "nur im Archiv".into(),
        }
    }

    /// Kopfzeilen: Von, An, Cc, Datum, Betreff (einzeilig, gekürzt).
    pub fn header(&self) -> String {
        let mut h = format!(
            "Von: {}\nAn: {}\n",
            text::one_line(&self.from().display(), 160),
            list(&self.to())
        );
        let cc = self.cc();
        if !cc.is_empty() {
            h.push_str(&format!("Cc: {}\n", list(&cc)));
        }
        h.push_str(&format!(
            "Datum: {} ({})\nBetreff: {}",
            text::datetime(self.date),
            text::weekday_de(self.date),
            text::one_line(&self.subject, 300)
        ));
        if self.has_attachments {
            h.push_str("\nAnhänge: ja");
        }
        h
    }
}

/// Eine Nachricht des Benutzers.
pub async fn message(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<Mail> {
    sqlx::query_as::<_, Mail>(sql(format!("{SELECT} WHERE m.id = ? AND a.user_id = ?")))
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)
}

/// Nachrichten einer Unterhaltung des Benutzers (die neuesten 200), älteste zuerst.
pub async fn thread(pool: &SqlitePool, user_id: &str, thread_id: &str) -> AppResult<Vec<Mail>> {
    let mut list = sqlx::query_as::<_, Mail>(sql(format!(
        "{SELECT} WHERE m.thread_id = ? AND a.user_id = ? ORDER BY m.date DESC, m.received_at DESC LIMIT 200"
    )))
    .bind(thread_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    if list.is_empty() {
        return Err(AppError::NotFound);
    }
    list.reverse();
    Ok(list)
}

/// Nachrichten des Benutzers zu den IDs (unbekannte und fremde fehlen einfach).
pub async fn by_ids(pool: &SqlitePool, user_id: &str, ids: &[String]) -> AppResult<Vec<Mail>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(100) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let mut q = sqlx::query_as::<_, Mail>(sql(format!(
            "{SELECT} WHERE a.user_id = ? AND m.id IN ({marks})"
        )))
        .bind(user_id);
        for id in chunk {
            q = q.bind(id);
        }
        out.extend(q.fetch_all(pool).await?);
    }
    Ok(out)
}

/// Postfach mit dem, was Entwürfe brauchen.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub email: String,
    pub display_name: String,
    pub signature: String,
}

const ACCOUNT: &str = "SELECT id, name, email, display_name, signature FROM mail_accounts";

/// Ein Postfach des Benutzers.
pub async fn account(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<Account> {
    sqlx::query_as::<_, Account>(sql(format!("{ACCOUNT} WHERE id = ? AND user_id = ?")))
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)
}

/// Postfach für „Neue Nachricht“: das bevorzugte aus den Einstellungen, sonst das älteste.
pub async fn default_account(
    pool: &SqlitePool,
    user: &crate::db::models::User,
) -> AppResult<Option<Account>> {
    if let Some(id) = user.preferences().default_account {
        if let Ok(a) = account(pool, &user.id, &id).await {
            return Ok(Some(a));
        }
    }
    Ok(sqlx::query_as::<_, Account>(sql(format!(
        "{ACCOUNT} WHERE user_id = ? ORDER BY created_at ASC LIMIT 1"
    )))
    .bind(&user.id)
    .fetch_optional(pool)
    .await?)
}

/// Wer „ich“ bin: Absendername (Postfach, sonst Benutzer) und Adresse.
pub fn me(user: &crate::db::models::User, account: Option<&Account>) -> api::Address {
    let name = account
        .map(|a| a.display_name.trim())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| user.display_name.trim());
    let name = if name.is_empty() {
        user.username.as_str()
    } else {
        name
    };
    let email = account
        .map(|a| a.email.as_str())
        .unwrap_or(user.email.as_str());
    api::Address::new(name, email)
}
