//! Zugriffsprotokoll (`activity_log`): wer hat wann was getan – Anmeldungen, Exporte,
//! Löschungen, neue Postfächer. Fehler beim Protokollieren halten die Aktion nicht auf.

use crate::api;
use crate::db::models::now;
use crate::error::AppResult;
use sqlx::SqlitePool;

/// Eintrag schreiben. `action` kurz und maschinenlesbar (`login`, `export`, `account_deleted`, …).
pub async fn log(pool: &SqlitePool, user_id: Option<&str>, action: &str, detail: &str) {
    if let Err(e) = sqlx::query(
        "INSERT INTO activity_log (user_id, action, detail, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(action)
    .bind(detail.chars().take(500).collect::<String>())
    .bind(now())
    .execute(pool)
    .await
    {
        tracing::warn!("Zugriffsprotokoll: {e}");
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    user_id: Option<String>,
    username: Option<String>,
    action: String,
    detail: String,
    created_at: i64,
}

/// Neueste Einträge zuerst, mit Benutzername (fehlt, wenn der Benutzer gelöscht ist).
pub async fn recent(pool: &SqlitePool, limit: u32) -> AppResult<Vec<api::ActivityEntry>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.id, a.user_id, u.username, a.action, a.detail, a.created_at
         FROM activity_log a LEFT JOIN users u ON u.id = a.user_id
         ORDER BY a.id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| api::ActivityEntry {
            id: r.id,
            user_id: r.user_id,
            username: r.username,
            action: r.action,
            detail: r.detail,
            created_at: r.created_at,
        })
        .collect())
}
