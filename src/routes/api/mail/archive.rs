//! Archiv: Übersicht, Zeitleiste, Export, Import, Wiederherstellen.

use super::messages::QueryParams;
use super::{Body, Download};
use crate::api;
use crate::auth::guards::CurrentUser;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::mail::archive::{self, ExportFormat};
use crate::mail::{accounts, SyncState};
use crate::services::activity;
use rocket::data::{Data, Limits, ToByteUnit};
use rocket::http::ContentType;
use rocket::serde::json::Json;
use rocket::{get, post, routes, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![overview, timeline, export, import, restore]
}

#[get("/archive")]
async fn overview(
    user: CurrentUser,
    pool: &State<SqlitePool>,
    config: &State<Config>,
    sync: &State<SyncState>,
) -> AppResult<Json<api::ArchiveOverview>> {
    Ok(Json(
        archive::overview(pool, config, sync, &user.0.id).await?,
    ))
}

#[get("/archive/timeline?<account>&<year>")]
async fn timeline(
    user: CurrentUser,
    account: Option<&str>,
    year: Option<i32>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<Vec<api::PeriodCount>>> {
    let account = account.filter(|a| !a.is_empty());
    Ok(Json(
        archive::timeline(pool, &user.0.id, account, year).await?,
    ))
}

#[get("/export?<p..>")]
async fn export(
    user: CurrentUser,
    p: QueryParams<'_>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Download> {
    let format = match p.format.unwrap_or("mbox") {
        "mbox" => ExportFormat::Mbox,
        "eml" | "zip" => ExportFormat::Eml,
        other => {
            return Err(AppError::bad(format!(
                "Unbekanntes Format „{other}“ (mbox oder eml)."
            )))
        }
    };
    let query = p.to_query()?;
    let ids = p.ids();
    let export = archive::export(pool, config, &user.0.id, &query, ids.as_deref(), format).await?;
    let file = rocket::tokio::fs::File::open(&export.path).await?;
    // Die offene Datei bleibt lesbar; der Name verschwindet sofort (Unix).
    let _ = std::fs::remove_file(&export.path);
    activity::log(
        pool,
        Some(&user.0.id),
        "export",
        &format!("{} Nachrichten als {}", export.count, export.filename),
    )
    .await;
    Ok(Download {
        body: Body::File(file),
        content_type: match format {
            ExportFormat::Mbox => ContentType::new("application", "mbox"),
            ExportFormat::Eml => ContentType::ZIP,
        },
        filename: export.filename,
        inline: false,
    })
}

#[post("/import?<account>&<folder>", data = "<data>")]
async fn import(
    user: CurrentUser,
    account: &str,
    folder: Option<&str>,
    data: Data<'_>,
    limits: &Limits,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::ImportResult>> {
    let a = accounts::get(pool, &user.0.id, account).await?;
    let limit = limits.get("file").unwrap_or(2.gibibytes());
    let path = archive::tmp_file(config, "upload")?;
    let file = data.open(limit).into_file(&path).await?;
    if !file.is_complete() {
        let _ = rocket::tokio::fs::remove_file(&path).await;
        return Err(AppError::bad(format!(
            "Die Datei ist zu gross (höchstens {limit})."
        )));
    }
    let result = archive::import(pool, &user.0.id, account, folder, &path).await;
    let _ = rocket::tokio::fs::remove_file(&path).await;
    let result = result?;
    activity::log(
        pool,
        Some(&user.0.id),
        "import",
        &format!("{}: {}", a.email, result.message),
    )
    .await;
    Ok(Json(result))
}

#[post("/archive/restore", data = "<req>")]
async fn restore(
    user: CurrentUser,
    req: Json<api::RestoreRequest>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::BulkResult>> {
    let result = archive::restore(pool, config, &user.0.id, &req).await?;
    activity::log(
        pool,
        Some(&user.0.id),
        "archive_restore",
        &format!("{} Nachrichten wiederhergestellt", result.updated),
    )
    .await;
    Ok(Json(result))
}
