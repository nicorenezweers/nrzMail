//! Ordner: Rollen (Posteingang, Gesendet, …) aus SPECIAL-USE oder dem Namen, Namen in
//! „modified UTF-7“ (IMAP), Ordner lesen, anlegen und ändern.

use crate::api::{self, FolderRole};
use crate::db::models::new_id;
use crate::error::{AppError, AppResult};
use crate::mail::model::FolderRow;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use sqlx::SqlitePool;

/// IMAP-Ordnername (modified UTF-7, RFC 3501 5.1.3) → Text: `Entw&APw-rfe` → `Entwürfe`.
pub fn decode_mutf7(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[pos..]);
            return out;
        };
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else {
            let decoded = STANDARD_NO_PAD
                .decode(encoded.replace(',', "/"))
                .ok()
                .filter(|b| b.len() % 2 == 0)
                .and_then(|b| {
                    let units: Vec<u16> = b
                        .chunks(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    String::from_utf16(&units).ok()
                });
            match decoded {
                Some(d) => out.push_str(&d),
                None => out.push_str(&rest[pos..pos + end + 2]),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Text → IMAP-Ordnername (modified UTF-7).
pub fn encode_mutf7(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push('&');
        out.push_str(&STANDARD_NO_PAD.encode(bytes).replace('/', ","));
        out.push('-');
        pending.clear();
    };
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Rolle aus dem Namen (Deutsch, Englisch, Französisch, Italienisch).
pub fn role_from_name(path: &str, delimiter: &str) -> FolderRole {
    if path.eq_ignore_ascii_case("INBOX") {
        return FolderRole::Inbox;
    }
    let decoded = decode_mutf7(path);
    let last = if delimiter.is_empty() {
        decoded.as_str()
    } else {
        decoded.rsplit(delimiter).next().unwrap_or(&decoded)
    };
    let name = last.trim().to_lowercase();
    let name = name.as_str();
    const SENT: &[&str] = &[
        "sent",
        "sent items",
        "sent mail",
        "sent messages",
        "gesendet",
        "gesendete elemente",
        "gesendete objekte",
        "gesendete nachrichten",
        "envoyés",
        "envoyes",
        "éléments envoyés",
        "messages envoyés",
        "inviati",
        "posta inviata",
        "elementi inviati",
    ];
    const TRASH: &[&str] = &[
        "trash",
        "deleted",
        "deleted items",
        "deleted messages",
        "bin",
        "papierkorb",
        "gelöscht",
        "gelöschte elemente",
        "gelöschte objekte",
        "corbeille",
        "éléments supprimés",
        "cestino",
        "posta eliminata",
        "elementi eliminati",
    ];
    const JUNK: &[&str] = &[
        "spam",
        "junk",
        "junk e-mail",
        "junk email",
        "junk-e-mail",
        "bulk mail",
        "unerwünscht",
        "courrier indésirable",
        "indésirables",
        "posta indesiderata",
    ];
    const DRAFTS: &[&str] = &[
        "drafts",
        "draft",
        "entwürfe",
        "entwurf",
        "brouillons",
        "bozze",
    ];
    const ARCHIVE: &[&str] = &["archive", "archiv", "archives", "archivio", "archivé"];
    for (list, role) in [
        (SENT, FolderRole::Sent),
        (TRASH, FolderRole::Trash),
        (JUNK, FolderRole::Junk),
        (DRAFTS, FolderRole::Drafts),
        (ARCHIVE, FolderRole::Archive),
    ] {
        if list.contains(&name) {
            return role;
        }
    }
    FolderRole::Other
}

/// Sammelordner von Gmail („Alle Nachrichten“, „Markiert“, „Wichtig“) – für Server ohne
/// SPECIAL-USE; sie enthalten nur Kopien und werden nicht abgeglichen.
pub fn is_virtual_name(path: &str) -> bool {
    let decoded = decode_mutf7(path);
    let Some((root, name)) = decoded.split_once('/') else {
        return false;
    };
    matches!(root, "[Gmail]" | "[Google Mail]")
        && matches!(
            name.to_lowercase().as_str(),
            "all mail"
                | "alle nachrichten"
                | "tous les messages"
                | "tutti i messaggi"
                | "starred"
                | "markiert"
                | "suivis"
                | "speciali"
                | "important"
                | "wichtig"
                | "importanti"
        )
}

/// Name für einen neu anzulegenden Ordner einer Rolle.
pub fn default_name(role: FolderRole) -> &'static str {
    match role {
        FolderRole::Inbox => "INBOX",
        FolderRole::Sent => "Gesendet",
        FolderRole::Drafts => "Entwürfe",
        FolderRole::Trash => "Papierkorb",
        FolderRole::Junk => "Spam",
        FolderRole::Archive => "Archiv",
        FolderRole::Other => "Ordner",
    }
}

const COLUMNS: &str =
    "id, account_id, path, name, delimiter, role, uid_validity, uid_next, highest_modseq, \
    sync_enabled, on_server, last_sync_at, role_locked";

pub async fn get(pool: &SqlitePool, id: &str) -> AppResult<FolderRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM folders WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

/// Ordner eines Benutzers (sonst 404).
pub async fn get_owned(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<FolderRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM folders WHERE id = ? AND account_id IN (SELECT id FROM mail_accounts WHERE user_id = ?)"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn for_account(pool: &SqlitePool, account_id: &str) -> AppResult<Vec<FolderRow>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM folders WHERE account_id = ? ORDER BY path"
    )))
    .bind(account_id)
    .fetch_all(pool)
    .await?)
}

/// Ordner mit dieser Rolle (der auf dem Server zuerst).
pub async fn by_role(
    pool: &SqlitePool,
    account_id: &str,
    role: FolderRole,
) -> AppResult<Option<FolderRow>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM folders WHERE account_id = ? AND role = ? ORDER BY on_server DESC, length(path), path LIMIT 1"
    )))
    .bind(account_id)
    .bind(role.as_str())
    .fetch_optional(pool)
    .await?)
}

pub async fn by_path(
    pool: &SqlitePool,
    account_id: &str,
    path: &str,
) -> AppResult<Option<FolderRow>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM folders WHERE account_id = ? AND path = ?"
    )))
    .bind(account_id)
    .bind(path)
    .fetch_optional(pool)
    .await?)
}

/// Lokalen Ordner anlegen (oder den vorhandenen mit diesem Pfad liefern).
pub async fn create_local(
    pool: &SqlitePool,
    account_id: &str,
    path: &str,
    delimiter: &str,
    role: FolderRole,
    on_server: bool,
) -> AppResult<FolderRow> {
    let name = {
        let decoded = decode_mutf7(path);
        if delimiter.is_empty() {
            decoded
        } else {
            decoded
                .rsplit(delimiter)
                .next()
                .unwrap_or(&decoded)
                .to_string()
        }
    };
    sqlx::query(
        "INSERT INTO folders (id, account_id, path, name, delimiter, role, on_server) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (account_id, path) DO NOTHING",
    )
    .bind(new_id())
    .bind(account_id)
    .bind(path)
    .bind(name)
    .bind(delimiter)
    .bind(role.as_str())
    .bind(on_server)
    .execute(pool)
    .await?;
    by_path(pool, account_id, path)
        .await?
        .ok_or(AppError::NotFound)
}

/// Ordner für die API, mit Zählern (alle Postfächer des Benutzers oder eines).
pub async fn list(
    pool: &SqlitePool,
    user_id: &str,
    account: Option<&str>,
) -> AppResult<Vec<api::Folder>> {
    let rows: Vec<FolderRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM folders WHERE account_id IN (SELECT id FROM mail_accounts WHERE user_id = ?)
         AND (? IS NULL OR account_id = ?) ORDER BY account_id, CASE role WHEN 'inbox' THEN 0 ELSE 1 END, path"
    )))
    .bind(user_id)
    .bind(account)
    .bind(account)
    .fetch_all(pool)
    .await?;
    let counts: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT m.folder_id, count(*), sum(CASE WHEN m.seen = 0 THEN 1 ELSE 0 END) FROM messages m
         WHERE m.folder_id IS NOT NULL AND m.account_id IN (SELECT id FROM mail_accounts WHERE user_id = ?)
         AND (? IS NULL OR m.account_id = ?) GROUP BY m.folder_id",
    )
    .bind(user_id)
    .bind(account)
    .bind(account)
    .fetch_all(pool)
    .await?;
    let map: std::collections::HashMap<String, (i64, i64)> = counts
        .into_iter()
        .map(|(id, total, unread)| (id, (total, unread)))
        .collect();
    Ok(rows
        .iter()
        .map(|f| {
            let (total, unread) = map.get(&f.id).copied().unwrap_or((0, 0));
            f.to_api(total, unread)
        })
        .collect())
}

/// Rolle oder Abgleich eines Ordners ändern. Eine gesetzte Rolle gibt es danach nur noch
/// einmal im Postfach, und der Abgleich überschreibt sie nicht mehr.
pub async fn update(
    pool: &SqlitePool,
    user_id: &str,
    id: &str,
    input: &api::FolderUpdate,
) -> AppResult<api::Folder> {
    let folder = get_owned(pool, user_id, id).await?;
    let mut tx = pool.begin().await?;
    if let Some(role) = input.role {
        if role != FolderRole::Other {
            sqlx::query(
                "UPDATE folders SET role = 'other', role_locked = 1 WHERE account_id = ? AND role = ? AND id <> ?",
            )
            .bind(&folder.account_id)
            .bind(role.as_str())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE folders SET role = ?, role_locked = 1 WHERE id = ?")
            .bind(role.as_str())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(enabled) = input.sync_enabled {
        sqlx::query("UPDATE folders SET sync_enabled = ? WHERE id = ?")
            .bind(enabled)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    list(pool, user_id, Some(&folder.account_id))
        .await?
        .into_iter()
        .find(|f| f.id == id)
        .ok_or(AppError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_utf7() {
        assert_eq!(decode_mutf7("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(decode_mutf7("Gel&APY-schte Elemente"), "Gelöschte Elemente");
        assert_eq!(decode_mutf7("A&-B"), "A&B");
        assert_eq!(decode_mutf7("INBOX"), "INBOX");
        assert_eq!(decode_mutf7("kaputt&"), "kaputt&");
        assert_eq!(decode_mutf7("&Jjo-!"), "☺!");
        for s in [
            "Entwürfe",
            "Gelöschte Elemente",
            "A&B",
            "日本語",
            "Ordner/Unterordner",
        ] {
            assert_eq!(decode_mutf7(&encode_mutf7(s)), s);
        }
        assert_eq!(encode_mutf7("Entwürfe"), "Entw&APw-rfe");
    }

    #[test]
    fn gmail_virtual_folders() {
        assert!(is_virtual_name("[Gmail]/Alle Nachrichten"));
        assert!(is_virtual_name("[Google Mail]/All Mail"));
        assert!(!is_virtual_name("[Gmail]/Gesendet"));
        assert!(!is_virtual_name("Alle Nachrichten"));
    }

    #[test]
    fn roles_from_names() {
        assert_eq!(role_from_name("INBOX", "/"), FolderRole::Inbox);
        assert_eq!(role_from_name("inbox", "/"), FolderRole::Inbox);
        assert_eq!(role_from_name("INBOX.Sent", "."), FolderRole::Sent);
        assert_eq!(role_from_name("Gesendete Elemente", "/"), FolderRole::Sent);
        assert_eq!(role_from_name("Entw&APw-rfe", "/"), FolderRole::Drafts);
        assert_eq!(
            role_from_name("Gel&APY-schte Elemente", "/"),
            FolderRole::Trash
        );
        assert_eq!(role_from_name("Papierkorb", "/"), FolderRole::Trash);
        assert_eq!(role_from_name("Junk-E-Mail", "/"), FolderRole::Junk);
        assert_eq!(role_from_name("Cestino", "/"), FolderRole::Trash);
        assert_eq!(role_from_name("Archiv", "/"), FolderRole::Archive);
        assert_eq!(role_from_name("Rechnungen", "/"), FolderRole::Other);
        assert_eq!(role_from_name("Projekte/INBOX", "/"), FolderRole::Other);
    }
}
