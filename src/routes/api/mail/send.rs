//! Senden und Postausgang: `/send`, `/outbox*`, `/drafts*`.

use crate::api::{self, OutboxStatus};
use crate::auth::guards::CurrentUser;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::mail::send;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{delete, get, post, put, routes, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![send_now, outbox, create_draft, update_draft, remove, undo]
}

#[post("/send", data = "<compose>")]
async fn send_now(
    user: CurrentUser,
    compose: Json<api::Compose>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::SendResult>> {
    let delay = user.0.preferences().undo_send_seconds;
    Ok(Json(
        send::send(pool, config, &user.0.id, &compose, delay).await?,
    ))
}

#[get("/outbox?<status>")]
async fn outbox(
    user: CurrentUser,
    status: Option<&str>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<Vec<api::OutboxItem>>> {
    let status = match status.filter(|s| !s.is_empty()) {
        Some(s) => Some(
            OutboxStatus::parse(s)
                .ok_or_else(|| AppError::bad(format!("Unbekannter Status „{s}“.")))?,
        ),
        None => None,
    };
    Ok(Json(send::list(pool, &user.0.id, status).await?))
}

#[post("/drafts", data = "<compose>")]
async fn create_draft(
    user: CurrentUser,
    compose: Json<api::Compose>,
    pool: &State<SqlitePool>,
) -> AppResult<(Status, Json<api::OutboxItem>)> {
    Ok((
        Status::Created,
        Json(send::create_draft(pool, &user.0.id, &compose).await?),
    ))
}

#[put("/drafts/<id>", data = "<compose>")]
async fn update_draft(
    user: CurrentUser,
    id: &str,
    compose: Json<api::Compose>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::OutboxItem>> {
    Ok(Json(
        send::update_draft(pool, &user.0.id, id, &compose).await?,
    ))
}

#[delete("/outbox/<id>")]
async fn remove(user: CurrentUser, id: &str, pool: &State<SqlitePool>) -> AppResult<Status> {
    send::delete(pool, &user.0.id, id).await?;
    Ok(Status::NoContent)
}

#[post("/outbox/<id>/undo")]
async fn undo(
    user: CurrentUser,
    id: &str,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::OutboxItem>> {
    Ok(Json(send::undo(pool, &user.0.id, id).await?))
}
