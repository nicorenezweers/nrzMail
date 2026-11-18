//! Postfächer und Ordner: `/accounts*`, `/folders*`.

use crate::api::{self, AccountKind};
use crate::auth::guards::CurrentUser;
use crate::config::Config;
use crate::error::AppResult;
use crate::mail::{accounts, folders, SyncState};
use crate::services::activity;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{delete, get, post, put, routes, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![
        list,
        create,
        get_one,
        update,
        remove,
        test,
        sync_now,
        sync_all,
        folder_list,
        folder_update
    ]
}

#[get("/accounts")]
async fn list(
    user: CurrentUser,
    pool: &State<SqlitePool>,
    sync: &State<SyncState>,
) -> AppResult<Json<Vec<api::MailAccount>>> {
    Ok(Json(accounts::list(pool, sync, &user.0.id).await?))
}

#[post("/accounts", data = "<input>")]
async fn create(
    user: CurrentUser,
    input: Json<api::MailAccountInput>,
    pool: &State<SqlitePool>,
    sync: &State<SyncState>,
) -> AppResult<(Status, Json<api::MailAccount>)> {
    let a = accounts::create(pool, &user.0.id, &input).await?;
    activity::log(
        pool,
        Some(&user.0.id),
        "account_created",
        &format!("{} ({})", a.email, a.kind),
    )
    .await;
    if a.kind() == AccountKind::Imap && a.sync_enabled {
        sync.request(&a.id);
    }
    Ok((
        Status::Created,
        Json(accounts::get_api(pool, sync, &user.0.id, &a.id).await?),
    ))
}

#[get("/accounts/<id>")]
async fn get_one(
    user: CurrentUser,
    id: &str,
    pool: &State<SqlitePool>,
    sync: &State<SyncState>,
) -> AppResult<Json<api::MailAccount>> {
    Ok(Json(accounts::get_api(pool, sync, &user.0.id, id).await?))
}

#[put("/accounts/<id>", data = "<input>")]
async fn update(
    user: CurrentUser,
    id: &str,
    input: Json<api::MailAccountInput>,
    pool: &State<SqlitePool>,
    sync: &State<SyncState>,
) -> AppResult<Json<api::MailAccount>> {
    let a = accounts::update(pool, &user.0.id, id, &input).await?;
    activity::log(pool, Some(&user.0.id), "account_updated", &a.email).await;
    Ok(Json(accounts::get_api(pool, sync, &user.0.id, id).await?))
}

#[delete("/accounts/<id>")]
async fn remove(user: CurrentUser, id: &str, pool: &State<SqlitePool>) -> AppResult<Status> {
    let a = accounts::delete(pool, &user.0.id, id).await?;
    activity::log(pool, Some(&user.0.id), "account_deleted", &a.email).await;
    Ok(Status::NoContent)
}

/// Verbindung testen; mit `?account=` gelten die gespeicherten Passwörter, wo die Eingabe keine hat.
#[post("/accounts/test?<account>", data = "<input>")]
async fn test(
    user: CurrentUser,
    account: Option<&str>,
    input: Json<api::MailAccountInput>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::ConnectionTest>> {
    let existing = match account.filter(|a| !a.is_empty()) {
        Some(id) => Some(accounts::get(pool, &user.0.id, id).await?),
        None => None,
    };
    Ok(Json(
        accounts::test(config, &input, existing.as_ref()).await?,
    ))
}

#[post("/accounts/<id>/sync")]
async fn sync_now(
    user: CurrentUser,
    id: &str,
    pool: &State<SqlitePool>,
    sync: &State<SyncState>,
) -> AppResult<Status> {
    let a = accounts::get(pool, &user.0.id, id).await?;
    if a.is_imap() {
        sync.request(&a.id);
    }
    Ok(Status::Accepted)
}

/// Alle eigenen Postfächer abgleichen (automatisches Aktualisieren der Oberfläche). Postfächer,
/// die gerade abgleichen oder vor weniger als 30 Sekunden fertig wurden, bleiben aussen vor.
#[post("/sync")]
async fn sync_all(
    user: CurrentUser,
    pool: &State<SqlitePool>,
    sync: &State<SyncState>,
) -> AppResult<Status> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM mail_accounts WHERE user_id = ? AND kind = 'imap' AND sync_enabled = 1
         AND imap_host <> '' AND (last_sync_at IS NULL OR last_sync_at <= ?)",
    )
    .bind(&user.0.id)
    .bind(now - i64::from(api::AUTO_REFRESH_MIN))
    .fetch_all(pool.inner())
    .await?;
    for id in ids.iter().filter(|id| !sync.is_syncing(id)) {
        sync.request(id);
    }
    Ok(Status::Accepted)
}

#[get("/folders?<account>")]
async fn folder_list(
    user: CurrentUser,
    account: Option<&str>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<Vec<api::Folder>>> {
    let account = account.filter(|a| !a.is_empty());
    if let Some(a) = account {
        accounts::get(pool, &user.0.id, a).await?;
    }
    Ok(Json(folders::list(pool, &user.0.id, account).await?))
}

#[put("/folders/<id>", data = "<input>")]
async fn folder_update(
    user: CurrentUser,
    id: &str,
    input: Json<api::FolderUpdate>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::Folder>> {
    Ok(Json(folders::update(pool, &user.0.id, id, &input).await?))
}
