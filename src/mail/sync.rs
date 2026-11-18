//! Abgleich eines IMAP-Postfachs.
//!
//! 1. **Ordner**: LIST → Ordner anlegen/aktualisieren, Rolle aus SPECIAL-USE oder dem Namen;
//!    Sammelordner (`\All`, `\Flagged`, Gmail „Alle Nachrichten“) werden nicht abgeglichen;
//!    verschwundene Ordner bekommen `on_server = 0`.
//! 2. **Bestand** je Ordner: UIDVALIDITY prüfen (geändert → lokale UIDs verwerfen und neu
//!    zuordnen), Flags abgleichen, auf dem Server gelöschte Nachrichten **nicht** löschen,
//!    sondern `on_server = 0, uid = NULL` (sie bleiben im Archiv).
//! 3. **Neu** je Ordner: neue UIDs holen (beim ersten Abgleich höchstens
//!    `initial_sync_limit`, neueste zuerst). Wartende Nachrichten (verschoben, gesendet,
//!    wiederhergestellt: `uid IS NULL`) werden über Message-ID bzw. SHA-256 wiedergefunden
//!    statt doppelt gespeichert.
//!
//! Bestand vor Neu (über alle Ordner), damit eine in einen anderen Ordner verschobene
//! Nachricht zuerst als „weg“ erkannt und dann im neuen Ordner wieder zugeordnet wird.

use crate::api::FolderRole;
use crate::config::Config;
use crate::db::models::{new_id, now};
use crate::error::AppError;
use crate::mail::imap::{self, Listed, Session};
use crate::mail::model::{AccountRow, FolderRow};
use crate::mail::store::{self, Flags, NewMessage};
use crate::mail::{accounts, folders, parse, retention};
use futures::StreamExt;
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};

/// Höchstens so viele Bytes bzw. Nachrichten je Abruf.
const BATCH_BYTES: u64 = 16 * 1024 * 1024;
const BATCH_COUNT: usize = 50;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub new: usize,
    pub updated: usize,
    pub gone: usize,
}

fn db(e: impl Into<AppError>) -> String {
    format!("Datenbank: {}", e.into())
}

/// Postfächer, die fällig sind.
pub async fn due(pool: &SqlitePool, config: &Config) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM mail_accounts WHERE kind = 'imap' AND sync_enabled = 1 AND imap_host <> ''
         AND (last_sync_at IS NULL OR last_sync_at + max(sync_interval_minutes, ?) * 60 <= ?)",
    )
    .bind(config.min_sync_interval_minutes.max(1) as i64)
    .bind(now())
    .fetch_all(pool)
    .await
}

/// Abgleichen und Ergebnis (Zeitpunkt, Fehler) am Postfach festhalten.
pub async fn run(pool: &SqlitePool, config: &Config, account_id: &str) -> Result<Report, String> {
    let account = match accounts::by_id(pool, account_id).await {
        Ok(Some(a)) if a.is_imap() => a,
        Ok(_) => return Ok(Report::default()),
        Err(e) => return Err(db(e)),
    };
    let result = sync_account(pool, config, &account).await;
    match &result {
        Ok(r) if r.new + r.gone + r.updated > 0 => {
            tracing::info!(account = %account.id, new = r.new, updated = r.updated, gone = r.gone, "Abgleich")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(account = %account.id, "Abgleich: {e}"),
    }
    if let Err(e) =
        sqlx::query("UPDATE mail_accounts SET last_sync_at = ?, last_error = ? WHERE id = ?")
            .bind(now())
            .bind(result.as_ref().err())
            .bind(&account.id)
            .execute(pool)
            .await
    {
        tracing::warn!("Abgleich speichern: {e}");
    }
    result
}

pub async fn sync_account(
    pool: &SqlitePool,
    config: &Config,
    account: &AccountRow,
) -> Result<Report, String> {
    let mut session = imap::connect(&imap::Server::for_account(account, config)).await?;
    let result = sync_session(pool, config, account, &mut session).await;
    imap::logout(session).await;
    result
}

async fn sync_session(
    pool: &SqlitePool,
    config: &Config,
    account: &AccountRow,
    session: &mut Session,
) -> Result<Report, String> {
    let listed = imap::list(session).await?;
    let all = reconcile_folders(pool, account, &listed)
        .await
        .map_err(db)?;
    let mut report = Report::default();
    let mut errors: Vec<String> = Vec::new();
    let mut active: Vec<FolderRow> = all
        .into_iter()
        .filter(|f| f.on_server && f.sync_enabled)
        .collect();
    // Posteingang zuerst, dann Gesendet und Entwürfe, Spam und Papierkorb zuletzt – sonst
    // erscheinen neue Mails erst, wenn grosse Archiv-Ordner fertig sind.
    active.sort_by_key(|f| folder_priority(&f.role));

    let mut states = Vec::new();
    for folder in active {
        match sync_existing(pool, session, &folder, &mut report).await {
            Ok(state) => states.push((folder, state)),
            Err(e) => errors.push(format!("{}: {e}", folders::decode_mutf7(&folder.path))),
        }
    }
    for (folder, state) in states {
        if let Err(e) = fetch_new(pool, config, session, account, &folder, state, &mut report).await
        {
            errors.push(format!("{}: {e}", folders::decode_mutf7(&folder.path)));
        }
    }
    if let Err(e) = retention::delete_from_server(pool, account, session).await {
        errors.push(e);
    }
    if errors.is_empty() {
        Ok(report)
    } else {
        Err(errors.join("; "))
    }
}

/// Ordner vom Server übernehmen; je Rolle nur ein Ordner (SPECIAL-USE vor Namen, vom
/// Benutzer gesetzte Rollen bleiben).
async fn reconcile_folders(
    pool: &SqlitePool,
    account: &AccountRow,
    listed: &[Listed],
) -> Result<Vec<FolderRow>, AppError> {
    let existing = folders::for_account(pool, &account.id).await?;
    let by_path: HashMap<&str, &FolderRow> =
        existing.iter().map(|f| (f.path.as_str(), f)).collect();
    let marked: Vec<Listed> = listed
        .iter()
        .map(|l| Listed {
            virtual_folder: l.virtual_folder || folders::is_virtual_name(&l.path),
            ..l.clone()
        })
        .collect();
    let selectable: Vec<&Listed> = marked.iter().filter(|l| l.selectable).collect();

    let mut taken: HashSet<FolderRole> = existing
        .iter()
        .filter(|f| f.role_locked && f.role() != FolderRole::Other)
        .map(FolderRow::role)
        .collect();
    let mut roles: HashMap<&str, FolderRole> = HashMap::new();
    for l in &selectable {
        if l.path.eq_ignore_ascii_case("INBOX") && taken.insert(FolderRole::Inbox) {
            roles.insert(&l.path, FolderRole::Inbox);
        }
    }
    for l in &selectable {
        if let Some(r) = l.special.filter(|_| !l.virtual_folder) {
            if !roles.contains_key(l.path.as_str()) && taken.insert(r) {
                roles.insert(&l.path, r);
            }
        }
    }
    for l in &selectable {
        if roles.contains_key(l.path.as_str()) || l.virtual_folder {
            continue;
        }
        let r = folders::role_from_name(&l.path, &l.delimiter);
        if r != FolderRole::Other && taken.insert(r) {
            roles.insert(&l.path, r);
        }
    }

    let mut tx = pool.begin().await?;
    let mut seen: HashSet<&str> = HashSet::new();
    for l in &selectable {
        seen.insert(&l.path);
        let role = roles.get(l.path.as_str()).copied().unwrap_or_default();
        let decoded = folders::decode_mutf7(&l.path);
        let name = if l.delimiter.is_empty() {
            decoded.clone()
        } else {
            decoded
                .rsplit(l.delimiter.as_str())
                .next()
                .unwrap_or(&decoded)
                .to_string()
        };
        if let Some(f) = by_path.get(l.path.as_str()) {
            sqlx::query(
                "UPDATE folders SET name = ?, delimiter = ?, on_server = 1,
                 role = CASE WHEN role_locked = 1 THEN role ELSE ? END WHERE id = ?",
            )
            .bind(&name)
            .bind(&l.delimiter)
            .bind(role.as_str())
            .bind(&f.id)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO folders (id, account_id, path, name, delimiter, role, sync_enabled, on_server)
                 VALUES (?, ?, ?, ?, ?, ?, ?, 1)",
            )
            .bind(new_id())
            .bind(&account.id)
            .bind(&l.path)
            .bind(&name)
            .bind(&l.delimiter)
            .bind(role.as_str())
            .bind(!l.virtual_folder)
            .execute(&mut *tx)
            .await?;
        }
    }
    // Nicht mehr auf dem Server: Ordner und Nachrichten bleiben lokal
    for f in existing
        .iter()
        .filter(|f| f.on_server && !seen.contains(f.path.as_str()))
    {
        sqlx::query("UPDATE folders SET on_server = 0 WHERE id = ?")
            .bind(&f.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE messages SET on_server = 0, uid = NULL WHERE folder_id = ?")
            .bind(&f.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    folders::for_account(pool, &account.id).await
}

/// Stand eines Ordners nach dem Bestandsabgleich.
struct FolderState {
    validity: Option<u32>,
    reset: bool,
    /// UID → (Flags, Grösse)
    server: HashMap<u32, (Flags, u32)>,
}

async fn collect_fetches(
    session: &mut Session,
    set: &str,
    query: &str,
) -> Result<Vec<async_imap::types::Fetch>, String> {
    let stream = imap::timed("Abruf", session.uid_fetch(set, query)).await?;
    let all: Vec<_> = tokio::time::timeout(imap::COMMAND_TIMEOUT, stream.collect::<Vec<_>>())
        .await
        .map_err(|_| "Abruf: Zeitüberschreitung.".to_string())?;
    all.into_iter()
        .map(|r| r.map_err(|e| format!("Abruf: {e}")))
        .collect()
}

async fn sync_existing(
    pool: &SqlitePool,
    session: &mut Session,
    folder: &FolderRow,
    report: &mut Report,
) -> Result<FolderState, String> {
    let mb = imap::select(session, &folder.path).await?;
    let validity = mb.uid_validity;
    let reset =
        matches!((folder.uid_validity, validity), (Some(old), Some(new)) if old != new as i64);
    if reset {
        tracing::info!(folder = %folder.path, "UIDVALIDITY geändert – Nachrichten werden neu zugeordnet");
        sqlx::query("UPDATE messages SET uid = NULL WHERE folder_id = ? AND uid IS NOT NULL")
            .bind(&folder.id)
            .execute(pool)
            .await
            .map_err(db)?;
        sqlx::query("UPDATE folders SET uid_next = NULL, highest_modseq = NULL WHERE id = ?")
            .bind(&folder.id)
            .execute(pool)
            .await
            .map_err(db)?;
    }
    if folder.uid_validity != validity.map(i64::from) {
        sqlx::query("UPDATE folders SET uid_validity = ? WHERE id = ?")
            .bind(validity.map(i64::from))
            .bind(&folder.id)
            .execute(pool)
            .await
            .map_err(db)?;
    }

    let mut server = HashMap::new();
    if mb.exists > 0 {
        for f in collect_fetches(session, "1:*", "(UID FLAGS RFC822.SIZE)").await? {
            if let Some(uid) = f.uid {
                server.insert(uid, (imap::flags_of(&f), f.size.unwrap_or(0)));
            }
        }
    }

    let local: Vec<(String, i64, bool, bool, bool)> = sqlx::query_as(
        "SELECT id, uid, seen, flagged, answered FROM messages WHERE folder_id = ? AND uid IS NOT NULL",
    )
    .bind(&folder.id)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let mut tx = pool.begin().await.map_err(db)?;
    for (id, uid, seen, flagged, answered) in local {
        let remote = u32::try_from(uid).ok().and_then(|u| server.get(&u));
        match remote {
            None => {
                sqlx::query("UPDATE messages SET on_server = 0, uid = NULL WHERE id = ?")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                report.gone += 1;
            }
            Some((f, _)) if (f.seen, f.flagged, f.answered) != (seen, flagged, answered) => {
                sqlx::query("UPDATE messages SET seen = ?, flagged = ?, answered = ?, on_server = 1 WHERE id = ?")
                    .bind(f.seen)
                    .bind(f.flagged)
                    .bind(f.answered)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                report.updated += 1;
            }
            Some(_) => {}
        }
    }
    tx.commit().await.map_err(db)?;
    Ok(FolderState {
        validity,
        reset,
        server,
    })
}

/// Message-ID aus einem Kopf-Ausschnitt.
fn header_message_id(header: &[u8]) -> Option<String> {
    let mut raw = header.to_vec();
    raw.extend_from_slice(b"\r\n");
    mail_parser::MessageParser::default()
        .parse(&raw)
        .and_then(|m| m.message_id().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

async fn fetch_new(
    pool: &SqlitePool,
    config: &Config,
    session: &mut Session,
    account: &AccountRow,
    folder: &FolderRow,
    state: FolderState,
    report: &mut Report,
) -> Result<(), String> {
    let local: HashSet<u32> = sqlx::query_scalar::<_, i64>(
        "SELECT uid FROM messages WHERE folder_id = ? AND uid IS NOT NULL",
    )
    .bind(&folder.id)
    .fetch_all(pool)
    .await
    .map_err(db)?
    .into_iter()
    .filter_map(|u| u32::try_from(u).ok())
    .collect();
    let initial = folder.uid_next.is_none() || state.reset;
    let mut candidates: Vec<u32> = state
        .server
        .keys()
        .filter(|u| !local.contains(u))
        .copied()
        .collect();
    if initial {
        if config.initial_sync_limit > 0 {
            let mut newest: Vec<u32> = state.server.keys().copied().collect();
            newest.sort_unstable_by(|a, b| b.cmp(a));
            newest.truncate(config.initial_sync_limit as usize);
            let keep: HashSet<u32> = newest.into_iter().collect();
            candidates.retain(|u| keep.contains(u));
        }
        candidates.sort_unstable_by(|a, b| b.cmp(a));
    } else {
        let watermark = folder.uid_next.unwrap_or(0).max(0) as u32;
        candidates.retain(|u| *u >= watermark);
        candidates.sort_unstable();
    }
    let max_uid = state.server.keys().copied().max();

    if !candidates.is_empty() {
        imap::select(session, &folder.path).await?;
        relink_by_message_id(pool, session, folder, &mut candidates, &state, report).await?;
        let mut batch: Vec<u32> = Vec::new();
        let mut bytes = 0u64;
        for (i, uid) in candidates.iter().enumerate() {
            batch.push(*uid);
            bytes += state.server.get(uid).map(|(_, s)| *s as u64).unwrap_or(0);
            if bytes >= BATCH_BYTES || batch.len() >= BATCH_COUNT || i + 1 == candidates.len() {
                let fetches = collect_fetches(
                    session,
                    &imap::uid_set(&batch),
                    "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[])",
                )
                .await?;
                for f in &fetches {
                    let (Some(uid), Some(raw)) = (f.uid, f.body()) else {
                        continue;
                    };
                    let internal = f.internal_date().map(|d| d.timestamp());
                    store_fetched(
                        pool,
                        account,
                        folder,
                        uid,
                        raw,
                        imap::flags_of(f),
                        internal,
                        report,
                    )
                    .await
                    .map_err(db)?;
                }
                batch.clear();
                bytes = 0;
            }
        }
    }

    if state.reset {
        // Nach neuer UIDVALIDITY nicht wiedergefunden: liegt nur noch im Archiv
        sqlx::query("UPDATE messages SET on_server = 0 WHERE folder_id = ? AND uid IS NULL AND on_server = 1")
            .bind(&folder.id)
            .execute(pool)
            .await
            .map_err(db)?;
    }
    let next = max_uid
        .map(|m| i64::from(m) + 1)
        .max(folder.uid_next.filter(|_| !state.reset))
        .unwrap_or(1);
    sqlx::query("UPDATE folders SET uid_validity = ?, uid_next = ?, last_sync_at = ? WHERE id = ?")
        .bind(state.validity.map(i64::from))
        .bind(next)
        .bind(now())
        .bind(&folder.id)
        .execute(pool)
        .await
        .map_err(db)?;
    Ok(())
}

/// Reihenfolge beim Abgleich (kleiner = früher).
fn folder_priority(role: &str) -> u8 {
    match role {
        "inbox" => 0,
        "sent" => 1,
        "drafts" => 2,
        "other" => 3,
        "archive" => 4,
        "junk" => 5,
        "trash" => 6,
        _ => 3,
    }
}

/// Wartende Nachrichten dieses Ordners (`uid IS NULL`) über die Message-ID wiederfinden –
/// ohne die ganze Nachricht noch einmal zu laden.
async fn relink_by_message_id(
    pool: &SqlitePool,
    session: &mut Session,
    folder: &FolderRow,
    candidates: &mut Vec<u32>,
    state: &FolderState,
    report: &mut Report,
) -> Result<(), String> {
    let pending: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, message_id FROM messages WHERE folder_id = ? AND uid IS NULL AND on_server = 1 AND message_id <> ''",
    )
    .bind(&folder.id)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    if pending.is_empty() {
        return Ok(());
    }
    let mut by_mid: HashMap<String, String> = HashMap::new();
    for (id, mid) in pending {
        by_mid.entry(mid).or_insert(id);
    }
    let mut relinked: HashSet<u32> = HashSet::new();
    for chunk in candidates.clone().chunks(200) {
        let fetches = collect_fetches(
            session,
            &imap::uid_set(chunk),
            "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
        )
        .await?;
        for f in &fetches {
            let (Some(uid), Some(header)) = (f.uid, f.header()) else {
                continue;
            };
            let Some(mid) = header_message_id(header) else {
                continue;
            };
            if let Some(id) = by_mid.remove(&mid) {
                let flags = state.server.get(&uid).map(|(f, _)| *f).unwrap_or_default();
                sqlx::query(
                    "UPDATE messages SET uid = ?, on_server = 1, seen = ?, flagged = ?, answered = ? WHERE id = ?",
                )
                .bind(i64::from(uid))
                .bind(flags.seen)
                .bind(flags.flagged)
                .bind(flags.answered)
                .bind(&id)
                .execute(pool)
                .await
                .map_err(db)?;
                relinked.insert(uid);
                report.updated += 1;
            }
        }
        if by_mid.is_empty() {
            break;
        }
    }
    candidates.retain(|u| !relinked.contains(u));
    Ok(())
}

/// Abgeholte Nachricht speichern – oder eine wartende mit gleichen Rohdaten zuordnen.
#[allow(clippy::too_many_arguments)]
async fn store_fetched(
    pool: &SqlitePool,
    account: &AccountRow,
    folder: &FolderRow,
    uid: u32,
    raw: &[u8],
    flags: Flags,
    internal_date: Option<i64>,
    report: &mut Report,
) -> Result<(), AppError> {
    let sha = parse::sha256(raw);
    let mut tx = pool.begin().await?;
    let waiting: Option<String> = sqlx::query_scalar(
        "SELECT id FROM messages WHERE account_id = ? AND sha256 = ? AND uid IS NULL
         ORDER BY (folder_id = ?) DESC, on_server DESC LIMIT 1",
    )
    .bind(&account.id)
    .bind(&sha)
    .bind(&folder.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = waiting {
        sqlx::query(
            "UPDATE messages SET folder_id = ?, uid = ?, on_server = 1, seen = ?, flagged = ?, answered = ?,
             trashed_at = CASE WHEN ? THEN COALESCE(trashed_at, ?) ELSE NULL END WHERE id = ?",
        )
        .bind(&folder.id)
        .bind(i64::from(uid))
        .bind(flags.seen)
        .bind(flags.flagged)
        .bind(flags.answered)
        .bind(folder.role() == FolderRole::Trash)
        .bind(now())
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        report.updated += 1;
    } else {
        store::insert(
            &mut tx,
            NewMessage {
                account,
                folder: Some(folder),
                uid: Some(uid),
                on_server: true,
                raw,
                flags,
                internal_date,
                dedupe: false,
            },
        )
        .await?;
        report.new += 1;
    }
    tx.commit().await?;
    Ok(())
}
