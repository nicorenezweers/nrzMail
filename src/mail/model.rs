//! Zeilen der Mail-Tabellen und ihre Umwandlung in die API-Typen.

use crate::api::{self, AccountKind, Address, Category, FolderRole, Security};
use sqlx::FromRow;

/// Zeile aus `mail_accounts` (mit Passwörtern – verlässt den Server nie).
#[derive(Debug, Clone, FromRow)]
pub struct AccountRow {
    pub id: String,
    pub user_id: String,
    pub kind: String,
    pub name: String,
    pub email: String,
    pub display_name: String,
    pub color: String,
    pub signature: String,
    pub imap_host: String,
    pub imap_port: i64,
    pub imap_security: String,
    pub imap_username: String,
    pub imap_password: String,
    pub smtp_host: String,
    pub smtp_port: i64,
    pub smtp_security: String,
    pub smtp_username: String,
    pub smtp_password: String,
    pub sync_enabled: bool,
    pub sync_interval_minutes: i64,
    pub delete_from_server_after_days: Option<i64>,
    pub archive_retention_days: Option<i64>,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Zähler eines Postfachs für die API.
#[derive(Debug, Clone, Copy, Default)]
pub struct AccountCounts {
    pub messages: i64,
    pub unread: i64,
    pub size: i64,
}

impl AccountRow {
    pub fn kind(&self) -> AccountKind {
        AccountKind::parse(&self.kind).unwrap_or_default()
    }

    pub fn is_imap(&self) -> bool {
        self.kind() == AccountKind::Imap
    }

    pub fn imap_security(&self) -> Security {
        Security::parse(&self.imap_security).unwrap_or_default()
    }

    pub fn smtp_security(&self) -> Security {
        Security::parse(&self.smtp_security).unwrap_or_default()
    }

    /// Name in der Seitenleiste, sonst die Adresse.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }

    /// Gmail legt gesendete Nachrichten selbst im Gesendet-Ordner ab.
    pub fn is_gmail(&self) -> bool {
        let h = self.imap_host.to_ascii_lowercase();
        h.ends_with("gmail.com") || h.ends_with("googlemail.com")
    }

    /// Anmeldename für SMTP (leer = der von IMAP).
    pub fn smtp_login(&self) -> (&str, &str) {
        let user = if self.smtp_username.trim().is_empty() {
            self.imap_username.trim()
        } else {
            self.smtp_username.trim()
        };
        let password = if self.smtp_password.is_empty() {
            &self.imap_password
        } else {
            &self.smtp_password
        };
        (user, password)
    }

    pub fn to_api(&self, counts: AccountCounts, syncing: bool) -> api::MailAccount {
        api::MailAccount {
            id: self.id.clone(),
            kind: self.kind(),
            name: self.name.clone(),
            email: self.email.clone(),
            display_name: self.display_name.clone(),
            color: self.color.clone(),
            signature: self.signature.clone(),
            imap_host: self.imap_host.clone(),
            imap_port: port(self.imap_port),
            imap_security: self.imap_security(),
            imap_username: self.imap_username.clone(),
            imap_password_hint: hint(&self.imap_password),
            smtp_host: self.smtp_host.clone(),
            smtp_port: port(self.smtp_port),
            smtp_security: self.smtp_security(),
            smtp_username: self.smtp_username.clone(),
            smtp_password_hint: hint(&self.smtp_password),
            sync_enabled: self.sync_enabled,
            sync_interval_minutes: self.sync_interval_minutes.clamp(0, u32::MAX as i64) as u32,
            delete_from_server_after_days: self
                .delete_from_server_after_days
                .map(|d| d.clamp(0, u32::MAX as i64) as u32),
            archive_retention_days: self
                .archive_retention_days
                .map(|d| d.clamp(0, u32::MAX as i64) as u32),
            last_sync_at: self.last_sync_at,
            last_error: self.last_error.clone().filter(|e| !e.is_empty()),
            syncing,
            message_count: counts.messages,
            unread_count: counts.unread,
            size_bytes: counts.size,
            created_at: self.created_at,
        }
    }
}

fn port(p: i64) -> u16 {
    u16::try_from(p).unwrap_or(0)
}

/// Hinweis auf ein Passwort wie `ab…yz` – nur bei langen Passwörtern Anfang und Ende,
/// sonst Punkte; leer bleibt leer.
pub fn hint(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    match chars.len() {
        0 => String::new(),
        n if n >= 12 => format!(
            "{}…{}",
            chars[..2].iter().collect::<String>(),
            chars[n - 2..].iter().collect::<String>()
        ),
        _ => "••••••".into(),
    }
}

/// Zeile aus `folders`.
#[derive(Debug, Clone, FromRow)]
pub struct FolderRow {
    pub id: String,
    pub account_id: String,
    pub path: String,
    pub name: String,
    pub delimiter: String,
    pub role: String,
    pub uid_validity: Option<i64>,
    pub uid_next: Option<i64>,
    pub highest_modseq: Option<i64>,
    pub sync_enabled: bool,
    pub on_server: bool,
    pub last_sync_at: Option<i64>,
    pub role_locked: bool,
}

impl FolderRow {
    pub fn role(&self) -> FolderRole {
        FolderRole::parse(&self.role).unwrap_or_default()
    }

    pub fn to_api(&self, total: i64, unread: i64) -> api::Folder {
        let (parent_path, depth) = if self.delimiter.is_empty() {
            (None, 0)
        } else {
            let parts: Vec<&str> = self.path.split(self.delimiter.as_str()).collect();
            let depth = parts.len().saturating_sub(1) as u32;
            let parent = (parts.len() > 1).then(|| parts[..parts.len() - 1].join(&self.delimiter));
            (parent, depth)
        };
        api::Folder {
            id: self.id.clone(),
            account_id: self.account_id.clone(),
            path: self.path.clone(),
            name: self.name.clone(),
            parent_path,
            depth,
            role: self.role(),
            total,
            unread,
            sync_enabled: self.sync_enabled,
        }
    }
}

/// Spalten für Listen-Einträge (ohne Text und HTML).
pub const SUMMARY_COLUMNS: &str = "m.id, m.account_id, m.folder_id, m.thread_id, m.subject, m.from_name, \
    m.from_address, m.to_addrs, m.snippet, m.date, m.seen, m.flagged, m.answered, m.has_attachments, \
    m.category, m.size, m.on_server, m.is_sent, \
    (SELECT count(*) FROM messages t LEFT JOIN folders tf ON tf.id = t.folder_id \
     WHERE t.thread_id = m.thread_id AND COALESCE(tf.role, 'other') NOT IN ('trash', 'junk')) AS thread_count";

/// Eintrag der Nachrichtenliste.
#[derive(Debug, Clone, FromRow)]
pub struct SummaryRow {
    pub id: String,
    pub account_id: String,
    pub folder_id: Option<String>,
    pub thread_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_address: String,
    pub to_addrs: String,
    pub snippet: String,
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub has_attachments: bool,
    pub category: String,
    pub size: i64,
    pub on_server: bool,
    pub is_sent: bool,
    pub thread_count: i64,
}

impl SummaryRow {
    pub fn to_api(&self) -> api::MessageSummary {
        api::MessageSummary {
            id: self.id.clone(),
            account_id: self.account_id.clone(),
            folder_id: self.folder_id.clone(),
            thread_id: self.thread_id.clone(),
            thread_count: self.thread_count.clamp(1, u32::MAX as i64) as u32,
            subject: self.subject.clone(),
            from: Address::new(self.from_name.clone(), self.from_address.clone()),
            to: addresses(&self.to_addrs),
            snippet: self.snippet.clone(),
            date: self.date,
            seen: self.seen,
            flagged: self.flagged,
            answered: self.answered,
            has_attachments: self.has_attachments,
            category: Category::parse(&self.category).unwrap_or_default(),
            size: self.size,
            on_server: self.on_server,
            is_sent: self.is_sent,
            otp_code: String::new(),
        }
    }
}

/// Die übrigen Felder für die Detailansicht.
#[derive(Debug, Clone, FromRow)]
pub struct DetailRow {
    pub id: String,
    pub cc_addrs: String,
    pub bcc_addrs: String,
    pub reply_to: String,
    pub message_id: String,
    pub in_reply_to: String,
    pub text_body: String,
    pub html_body: String,
    pub list_unsubscribe: String,
    pub ai_summary: Option<String>,
    pub account_email: String,
    pub folder_name: Option<String>,
}

/// Zeile aus `attachments`.
#[derive(Debug, Clone, FromRow)]
pub struct AttachmentRow {
    pub id: String,
    pub message_id: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub content_id: String,
    pub inline: bool,
    pub part_index: i64,
}

impl AttachmentRow {
    pub fn to_api(&self) -> api::Attachment {
        api::Attachment {
            id: self.id.clone(),
            message_id: self.message_id.clone(),
            filename: self.filename.clone(),
            content_type: self.content_type.clone(),
            size: self.size,
            inline: self.inline,
            content_id: self.content_id.clone(),
        }
    }
}

/// Adressliste aus der JSON-Spalte (kaputtes JSON = leer).
pub fn addresses(json: &str) -> Vec<Address> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Adressliste als JSON für die Datenbank.
pub fn addresses_json(list: &[Address]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hint_never_shows_short_passwords() {
        assert_eq!(hint(""), "");
        assert_eq!(hint("geheim"), "••••••");
        assert_eq!(hint("abcdefghijklmnxyz"), "ab…yz");
    }

    #[test]
    fn folder_tree_position() {
        let f = FolderRow {
            id: "1".into(),
            account_id: "a".into(),
            path: "Archiv/2024/Rechnungen".into(),
            name: "Rechnungen".into(),
            delimiter: "/".into(),
            role: "archive".into(),
            uid_validity: None,
            uid_next: None,
            highest_modseq: None,
            sync_enabled: true,
            on_server: true,
            last_sync_at: None,
            role_locked: false,
        };
        let api = f.to_api(3, 1);
        assert_eq!(api.depth, 2);
        assert_eq!(api.parent_path.as_deref(), Some("Archiv/2024"));
        assert_eq!(api.role, FolderRole::Archive);
    }
}
