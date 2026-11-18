//! Archiv: Übersicht je Postfach, Zeitleiste, Export (mbox oder zip mit .eml), Import
//! (zip, mbox, einzelne .eml – automatisch erkannt) und Wiederherstellen auf den Server.

use crate::api::{self, MessageQuery};
use crate::config::Config;
use crate::db::models::now;
use crate::error::{AppError, AppResult};
use crate::mail::mbox::{self, MboxReader};
use crate::mail::model::{AccountRow, FolderRow};
use crate::mail::store::{self, Flags, NewMessage, Stored};
use crate::mail::{accounts, folders, imap, messages, SyncState};
use sqlx::SqlitePool;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

/// Grösste einzelne Nachricht beim Import.
const MAX_MESSAGE_BYTES: usize = 100 * 1024 * 1024;

/// Ordner für temporäre Dateien (Export, Import).
pub fn tmp_dir(config: &Config) -> AppResult<PathBuf> {
    let dir = config.data_dir.join("tmp");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Eine neue, leere temporäre Datei.
pub fn tmp_file(config: &Config, ext: &str) -> AppResult<PathBuf> {
    Ok(tmp_dir(config)?.join(format!("{}.{ext}", uuid::Uuid::new_v4())))
}

/// Übrig gebliebene temporäre Dateien (älter als einen Tag) löschen, z. B. nach einem Absturz.
pub fn clean_tmp(config: &Config) -> usize {
    let Ok(entries) = std::fs::read_dir(config.data_dir.join("tmp")) else {
        return 0;
    };
    let day = std::time::Duration::from_secs(86_400);
    entries
        .flatten()
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > day)
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

fn period_counts(rows: Vec<(String, i64, i64)>) -> Vec<api::PeriodCount> {
    rows.into_iter()
        .map(|(label, count, size_bytes)| api::PeriodCount {
            label,
            count,
            size_bytes,
        })
        .collect()
}

/// `GET /archive`
pub async fn overview(
    pool: &SqlitePool,
    config: &Config,
    sync: &SyncState,
    user_id: &str,
) -> AppResult<api::ArchiveOverview> {
    let list = accounts::list(pool, sync, user_id).await?;
    let mut out = api::ArchiveOverview::default();
    for account in list {
        let folders = folders::list(pool, user_id, Some(&account.id)).await?;
        let years = period_counts(
            sqlx::query_as(
                "SELECT strftime('%Y', date, 'unixepoch') AS y, count(*), COALESCE(sum(size), 0)
                 FROM messages WHERE account_id = ? GROUP BY y ORDER BY y",
            )
            .bind(&account.id)
            .fetch_all(pool)
            .await?,
        );
        let (count, size, oldest, newest, local_only): (i64, i64, Option<i64>, Option<i64>, i64) =
            sqlx::query_as(
                "SELECT count(*), COALESCE(sum(size), 0), min(date), max(date),
                        COALESCE(sum(CASE WHEN on_server = 0 THEN 1 ELSE 0 END), 0)
                 FROM messages WHERE account_id = ?",
            )
            .bind(&account.id)
            .fetch_one(pool)
            .await?;
        let attachment_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM attachments t JOIN messages m ON m.id = t.message_id
             WHERE m.account_id = ? AND t.inline = 0",
        )
        .bind(&account.id)
        .fetch_one(pool)
        .await?;
        out.total_messages += count;
        out.total_size_bytes += size;
        out.accounts.push(api::ArchiveAccount {
            account,
            folders,
            years,
            message_count: count,
            size_bytes: size,
            attachment_count,
            oldest,
            newest,
            local_only_count: local_only,
        });
    }
    let db = config.database_path();
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    out.database_bytes = [db, PathBuf::from(wal)]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() as i64)
        .sum();
    Ok(out)
}

/// `GET /archive/timeline`: Jahre, mit `year` die Monate dieses Jahres.
pub async fn timeline(
    pool: &SqlitePool,
    user_id: &str,
    account: Option<&str>,
    year: Option<i32>,
) -> AppResult<Vec<api::PeriodCount>> {
    let rows = match year {
        Some(y) => sqlx::query_as(
            "SELECT strftime('%Y-%m', m.date, 'unixepoch') AS p, count(*), COALESCE(sum(m.size), 0)
             FROM messages m JOIN mail_accounts a ON a.id = m.account_id
             WHERE a.user_id = ? AND (? IS NULL OR m.account_id = ?) AND strftime('%Y', m.date, 'unixepoch') = ?
             GROUP BY p ORDER BY p",
        )
        .bind(user_id)
        .bind(account)
        .bind(account)
        .bind(format!("{y:04}"))
        .fetch_all(pool)
        .await?,
        None => sqlx::query_as(
            "SELECT strftime('%Y', m.date, 'unixepoch') AS p, count(*), COALESCE(sum(m.size), 0)
             FROM messages m JOIN mail_accounts a ON a.id = m.account_id
             WHERE a.user_id = ? AND (? IS NULL OR m.account_id = ?) GROUP BY p ORDER BY p",
        )
        .bind(user_id)
        .bind(account)
        .bind(account)
        .fetch_all(pool)
        .await?,
    };
    Ok(period_counts(rows))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Mbox,
    /// zip mit einer .eml je Nachricht
    Eml,
}

/// Fertiger Export in einer temporären Datei.
pub struct Export {
    pub path: PathBuf,
    pub filename: String,
    pub count: usize,
}

/// Sicherer Dateiname (ohne Pfadtrenner und Steuerzeichen, höchstens `max` Zeichen).
pub fn safe_filename(name: &str, max: usize) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    let out: String = cleaned.chars().take(max).collect();
    if out.is_empty() {
        "nachricht".into()
    } else {
        out
    }
}

fn date_label(ts: i64) -> String {
    let d =
        time::OffsetDateTime::from_unix_timestamp(ts).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day())
}

/// `GET /export`: alle Nachrichten der Abfrage (oder die IDs) in eine temporäre Datei.
pub async fn export(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    q: &MessageQuery,
    ids: Option<&[String]>,
    format: ExportFormat,
) -> AppResult<Export> {
    let mut query = q.clone();
    if query.view.is_none() && query.folder.is_none() {
        query.view = Some(api::MailView::All);
    }
    let list = messages::select_ids(pool, user_id, &query, ids).await?;
    let ext = match format {
        ExportFormat::Mbox => "mbox",
        ExportFormat::Eml => "zip",
    };
    let path = tmp_file(config, ext)?;
    let filename = format!("nrzmail-{}.{ext}", date_label(now()));

    // Schreiben in einem eigenen Thread; die Datenbank liest hier.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Vec<u8>, String, i64, String)>(16);
    let file_path = path.clone();
    let writer = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let file = std::fs::File::create(&file_path)?;
        match format {
            ExportFormat::Mbox => {
                let mut w = std::io::BufWriter::new(file);
                while let Some((raw, from, date, _)) = rx.blocking_recv() {
                    mbox::write_message(&mut w, &raw, &from, date)?;
                }
                w.flush()
            }
            ExportFormat::Eml => {
                let mut zip = zip::ZipWriter::new(file);
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .large_file(true);
                let mut used = std::collections::HashSet::new();
                while let Some((raw, _, date, subject)) = rx.blocking_recv() {
                    let base = format!("{}_{}", date_label(date), safe_filename(&subject, 80));
                    let mut name = format!("{base}.eml");
                    let mut n = 2;
                    while !used.insert(name.clone()) {
                        name = format!("{base}_{n}.eml");
                        n += 1;
                    }
                    zip.start_file(name, options)
                        .map_err(std::io::Error::other)?;
                    zip.write_all(&raw)?;
                }
                zip.finish().map_err(std::io::Error::other)?;
                Ok(())
            }
        }
    });

    let mut count = 0;
    let mut failed = None;
    for (id, from, date) in &list {
        let raw = match store::raw(pool, id).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(message = %id, "Export: {e}");
                continue;
            }
        };
        let subject: String = sqlx::query_scalar("SELECT subject FROM messages WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?;
        if tx.send((raw, from.clone(), *date, subject)).await.is_err() {
            failed = Some("Export: Schreiben abgebrochen.");
            break;
        }
        count += 1;
    }
    drop(tx);
    let written = writer
        .await
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    if let Err(e) = written {
        let _ = std::fs::remove_file(&path);
        return Err(AppError::Io(e));
    }
    if let Some(msg) = failed {
        let _ = std::fs::remove_file(&path);
        return Err(AppError::Io(std::io::Error::other(msg)));
    }
    Ok(Export {
        path,
        filename,
        count,
    })
}

/// Eine gelesene Nachricht (Rohdaten, Datum der From_-Zeile) oder warum nicht.
type ArchiveItem = Result<(Vec<u8>, Option<i64>), String>;

/// Rohnachrichten aus einer Datei lesen (zip mit .eml/.mbox, mbox oder eine .eml).
fn read_archive(path: &Path, tx: tokio::sync::mpsc::Sender<ArchiveItem>) -> std::io::Result<()> {
    let mut head = [0u8; 5];
    let n = std::fs::File::open(path)?.read(&mut head)?;
    let head = &head[..n];
    let send = |item: ArchiveItem| tx.blocking_send(item).is_ok();
    let send_mbox = |reader: &mut dyn std::io::BufRead| -> bool {
        for msg in MboxReader::new(reader) {
            let item = msg.map_err(|e| e.to_string()).and_then(|m| {
                if m.raw.len() > MAX_MESSAGE_BYTES {
                    Err("Nachricht zu gross".into())
                } else {
                    Ok((m.raw, m.date))
                }
            });
            if !send(item) {
                return false;
            }
        }
        true
    };
    if head.starts_with(b"PK\x03\x04") {
        let mut zip =
            zip::ZipArchive::new(std::fs::File::open(path)?).map_err(std::io::Error::other)?;
        for i in 0..zip.len() {
            let mut entry = match zip.by_index(i) {
                Ok(e) => e,
                Err(e) => {
                    if !send(Err(e.to_string())) {
                        return Ok(());
                    }
                    continue;
                }
            };
            let name = entry.name().to_lowercase();
            if entry.is_dir()
                || name.contains("__macosx/")
                || name.rsplit('/').next().is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            if name.ends_with(".eml") || name.ends_with(".msg.eml") {
                if entry.size() as usize > MAX_MESSAGE_BYTES {
                    if !send(Err("Nachricht zu gross".into())) {
                        return Ok(());
                    }
                    continue;
                }
                let mut raw = Vec::with_capacity(entry.size() as usize);
                let item = entry
                    .read_to_end(&mut raw)
                    .map(|_| (raw, None))
                    .map_err(|e| e.to_string());
                if !send(item) {
                    return Ok(());
                }
            } else if name.ends_with(".mbox")
                || name.ends_with(".mbx")
                || !name.rsplit('/').next().unwrap_or("").contains('.')
            {
                let mut reader = BufReader::new(entry);
                if !send_mbox(&mut reader) {
                    return Ok(());
                }
            }
        }
        Ok(())
    } else if head.starts_with(b"From ") {
        let mut reader = BufReader::with_capacity(1 << 16, std::fs::File::open(path)?);
        send_mbox(&mut reader);
        Ok(())
    } else {
        let len = std::fs::metadata(path)?.len() as usize;
        if len > MAX_MESSAGE_BYTES {
            send(Err("Nachricht zu gross".into()));
            return Ok(());
        }
        let raw = std::fs::read(path)?;
        send(Ok((raw, None)));
        Ok(())
    }
}

/// Zielordner für den Import (vorhanden über Pfad oder Namen, sonst lokal angelegt).
async fn import_folder(
    pool: &SqlitePool,
    account: &AccountRow,
    name: Option<&str>,
) -> AppResult<FolderRow> {
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Import");
    if name.chars().count() > 200 {
        return Err(AppError::bad("Der Ordnername ist zu lang."));
    }
    for f in folders::for_account(pool, &account.id).await? {
        if f.id == name || f.path == name || f.name.eq_ignore_ascii_case(name) {
            return Ok(f);
        }
    }
    let path = folders::encode_mutf7(name);
    let role = folders::role_from_name(&path, "/");
    // Eine Rolle gibt es je Postfach nur einmal
    let role = if folders::by_role(pool, &account.id, role).await?.is_some() {
        api::FolderRole::Other
    } else {
        role
    };
    folders::create_local(pool, &account.id, &path, "/", role, false).await
}

/// `POST /import`: Nachrichten aus einer hochgeladenen Datei übernehmen.
pub async fn import(
    pool: &SqlitePool,
    user_id: &str,
    account_id: &str,
    folder: Option<&str>,
    path: &Path,
) -> AppResult<api::ImportResult> {
    let account = accounts::get(pool, user_id, account_id).await?;
    let folder = import_folder(pool, &account, folder).await?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ArchiveItem>(32);
    let file = path.to_path_buf();
    let reader = tokio::task::spawn_blocking(move || read_archive(&file, tx));

    let mut result = api::ImportResult::default();
    let mut batch = pool.begin().await?;
    let mut in_batch = 0;
    while let Some(item) = rx.recv().await {
        let (raw, mbox_date) = match item {
            Ok((raw, _)) if raw.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!("Import: {e}");
                result.failed += 1;
                continue;
            }
        };
        let stored = store::insert(
            &mut batch,
            NewMessage {
                account: &account,
                folder: Some(&folder),
                uid: None,
                on_server: false,
                raw: &raw,
                flags: Flags {
                    seen: true,
                    ..Flags::default()
                },
                // Fehlt der Date-Header, gilt das Datum der From_-Zeile
                internal_date: mbox_date,
                dedupe: true,
            },
        )
        .await;
        match stored {
            Ok(Stored::Inserted(_)) => result.imported += 1,
            Ok(Stored::Duplicate(_)) => result.duplicates += 1,
            Err(e) => {
                tracing::warn!("Import: {e}");
                result.failed += 1;
            }
        }
        in_batch += 1;
        if in_batch >= 100 {
            batch.commit().await?;
            batch = pool.begin().await?;
            in_batch = 0;
        }
    }
    batch.commit().await?;
    match reader.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!("Import lesen: {e}");
            if result.imported + result.duplicates == 0 {
                return Err(AppError::bad(format!(
                    "Die Datei lässt sich nicht lesen: {e}"
                )));
            }
            result.failed += 1;
        }
        Err(e) => return Err(AppError::Io(std::io::Error::other(e.to_string()))),
    }
    result.message = format!(
        "{} importiert, {} schon vorhanden, {} fehlerhaft.",
        result.imported, result.duplicates, result.failed
    );
    Ok(result)
}

/// `POST /archive/restore`: Nachrichten wieder auf den Server laden (APPEND in den Zielordner).
pub async fn restore(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    req: &api::RestoreRequest,
) -> AppResult<api::BulkResult> {
    let target = folders::get_owned(pool, user_id, &req.folder_id).await?;
    let account = accounts::get(pool, user_id, &target.account_id).await?;
    if !account.is_imap() {
        return Err(AppError::bad(
            "Wiederherstellen geht nur in einen Ordner eines IMAP-Postfachs.",
        ));
    }
    let mut session = imap::connect(&imap::Server::for_account(&account, config))
        .await
        .map_err(|e| AppError::bad(format!("Server nicht erreichbar: {e}")))?;
    let mut result = api::BulkResult::default();
    let outcome: AppResult<()> = async {
        if !target.on_server {
            imap::create(&mut session, &target.path)
                .await
                .map_err(AppError::bad)?;
            sqlx::query("UPDATE folders SET on_server = 1 WHERE id = ?")
                .bind(&target.id)
                .execute(pool)
                .await?;
        }
        let mut ids = req.ids.clone();
        ids.sort();
        ids.dedup();
        for id in &ids {
            let row: Option<(String, bool, bool)> = sqlx::query_as(
                "SELECT m.account_id, m.seen, m.flagged FROM messages m JOIN mail_accounts a ON a.id = m.account_id
                 WHERE m.id = ? AND a.user_id = ?",
            )
            .bind(id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
            let Some((message_account, seen, flagged)) = row else {
                result.failed += 1;
                continue;
            };
            let raw = store::raw(pool, id).await?;
            let flags = [(seen, "\\Seen"), (flagged, "\\Flagged")]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, f)| *f)
                .collect::<Vec<_>>()
                .join(" ");
            if let Err(e) = imap::append(&mut session, &target.path, &flags, &raw).await {
                result.failed += 1;
                result.message.get_or_insert(e);
                continue;
            }
            if message_account == account.id {
                sqlx::query("UPDATE messages SET folder_id = ?, on_server = 1, uid = NULL, trashed_at = NULL WHERE id = ?")
                    .bind(&target.id)
                    .bind(id)
                    .execute(pool)
                    .await?;
            } else {
                let mut tx = pool.begin().await?;
                let stored = store::insert(
                    &mut tx,
                    NewMessage {
                        account: &account,
                        folder: Some(&target),
                        uid: None,
                        on_server: true,
                        raw: &raw,
                        flags: Flags {
                            seen,
                            flagged,
                            ..Flags::default()
                        },
                        internal_date: None,
                        dedupe: true,
                    },
                )
                .await?;
                if let Stored::Duplicate(existing) = stored {
                    sqlx::query("UPDATE messages SET folder_id = ?, on_server = 1, uid = NULL WHERE id = ? AND uid IS NULL")
                        .bind(&target.id)
                        .bind(existing)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit().await?;
            }
            result.updated += 1;
        }
        Ok(())
    }
    .await;
    imap::logout(session).await;
    outcome?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filenames() {
        assert_eq!(safe_filename("../../etc/passwd", 80), "_.._etc_passwd");
        assert_eq!(
            safe_filename("Re: Offerte <Nr. 5>", 80),
            "Re_ Offerte _Nr. 5_"
        );
        assert_eq!(safe_filename("   ", 80), "nachricht");
        assert_eq!(safe_filename("Grüsse aus Zürich", 6), "Grüsse");
    }
}
