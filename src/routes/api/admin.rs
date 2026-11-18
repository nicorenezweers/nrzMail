//! Verwaltung (nur Admins): Benutzer, Instanz-Einstellungen, Zugriffsprotokoll – `/admin/*`.

use crate::api;
use crate::auth::guards::AdminUser;
use crate::auth::session;
use crate::config::Config;
use crate::error::AppResult;
use crate::services::{activity, settings, users};
use rocket::http::{CookieJar, Status};
use rocket::serde::json::Json;
use rocket::{delete, get, post, put, routes, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![
        list_users,
        create_user,
        update_user,
        delete_user,
        get_settings,
        put_settings,
        list_activity
    ]
}

// ------------------------------------------------------------------ Benutzer

#[get("/admin/users")]
async fn list_users(
    _admin: AdminUser,
    pool: &State<SqlitePool>,
) -> AppResult<Json<Vec<api::User>>> {
    Ok(Json(
        users::list(pool)
            .await?
            .iter()
            .map(|u| u.to_api())
            .collect(),
    ))
}

#[post("/admin/users", data = "<body>")]
async fn create_user(
    admin: AdminUser,
    body: Json<api::NewUser>,
    pool: &State<SqlitePool>,
) -> AppResult<(Status, Json<api::User>)> {
    let user = users::create(
        pool,
        &users::NewAccount {
            username: &body.username,
            password: &body.password,
            display_name: &body.display_name,
            email: &body.email,
            is_admin: body.is_admin,
        },
    )
    .await?;
    let role = if user.is_admin { " (Admin)" } else { "" };
    activity::log(
        pool,
        Some(&admin.0.id),
        "user_created",
        &format!("{}{role}", user.username),
    )
    .await;
    Ok((Status::Created, Json(user.to_api())))
}

/// Benutzer ändern: Anzeigename, E-Mail, Admin-Recht, Sperre, Passwort. Sperren und ein neues
/// Passwort beenden die Sitzungen des Benutzers.
#[put("/admin/users/<id>", data = "<body>")]
async fn update_user(
    admin: AdminUser,
    id: &str,
    body: Json<api::UpdateUser>,
    cookies: &CookieJar<'_>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::User>> {
    let token = session::token_from(cookies);
    let change = users::admin_update(pool, &admin.0, id, &body, token.as_deref()).await?;
    activity::log(pool, Some(&admin.0.id), "user_updated", &change.summary).await;
    Ok(Json(change.user.to_api()))
}

/// Benutzer löschen – mit seinen Postfächern und dem lokalen Archiv.
#[delete("/admin/users/<id>")]
async fn delete_user(admin: AdminUser, id: &str, pool: &State<SqlitePool>) -> AppResult<Status> {
    let user = users::delete(pool, &admin.0, id).await?;
    tracing::info!(user = %user.username, by = %admin.0.username, "Benutzer gelöscht");
    activity::log(pool, Some(&admin.0.id), "user_deleted", &user.username).await;
    Ok(Status::NoContent)
}

// ------------------------------------------------------------------ Einstellungen

#[get("/admin/settings")]
async fn get_settings(
    _admin: AdminUser,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::InstanceSettings>> {
    Ok(Json(api::InstanceSettings {
        registration_open: users::registration_open(pool, config).await?,
    }))
}

#[put("/admin/settings", data = "<body>")]
async fn put_settings(
    admin: AdminUser,
    body: Json<api::InstanceSettings>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::InstanceSettings>> {
    settings::set(pool, settings::REGISTRATION_OPEN, &body.registration_open).await?;
    activity::log(
        pool,
        Some(&admin.0.id),
        "settings_changed",
        if body.registration_open {
            "Registrierung offen"
        } else {
            "Registrierung geschlossen"
        },
    )
    .await;
    get_settings(admin, pool, config).await
}

// ------------------------------------------------------------------ Zugriffsprotokoll

/// Neueste Einträge zuerst; `limit` 1–1000 (Vorgabe 200).
#[get("/admin/activity?<limit>")]
async fn list_activity(
    _admin: AdminUser,
    limit: Option<u32>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<Vec<api::ActivityEntry>>> {
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    Ok(Json(activity::recent(pool, limit).await?))
}
