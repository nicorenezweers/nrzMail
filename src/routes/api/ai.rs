//! KI-API: Status, Anbieter, Einstellungen, Zusammenfassen, Antworten, Schreiben, Chat – `/ai/*`.
//! Einstellungen, Modelle und Test nur für Admins; alles mit Nachrichten nur auf den eigenen.

use crate::ai::settings::{self, AiConfig};
use crate::ai::{self, assistant, client, compose, provider, AiError};
use crate::api;
use crate::auth::guards::{AdminUser, CurrentUser};
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::services::activity;
use rocket::serde::json::Json;
use rocket::{get, post, put, routes, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![
        status,
        providers,
        get_settings,
        put_settings,
        models,
        test,
        summarize,
        reply,
        write,
        chat,
        categorize
    ]
}

type AiJson<T> = Result<Json<T>, AiError>;

/// Ist KI nutzbar? (für alle angemeldeten Benutzer)
#[get("/ai/status")]
async fn status(
    _user: CurrentUser,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::AiStatus>> {
    Ok(Json(AiConfig::load(pool, config).await?.to_status()))
}

#[get("/ai/providers")]
fn providers(_user: CurrentUser) -> Json<Vec<api::AiProvider>> {
    Json(provider::PROVIDERS.iter().map(|p| p.to_api()).collect())
}

#[get("/ai/settings")]
async fn get_settings(
    _admin: AdminUser,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::AiSettings>> {
    Ok(Json(AiConfig::load(pool, config).await?.to_settings()))
}

#[put("/ai/settings", data = "<body>")]
async fn put_settings(
    admin: AdminUser,
    body: Json<api::AiSettingsUpdate>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::AiSettings>> {
    let mut stored = settings::load_stored(pool).await?;
    settings::apply_update(&mut stored, &body, config)?;
    settings::save_stored(pool, &stored).await?;
    let ai = AiConfig::resolve(config, &stored);
    let key = match body.api_key.as_deref().map(str::trim) {
        None => "",
        Some("") => ", Schlüssel entfernt",
        Some(_) => ", neuer Schlüssel",
    };
    activity::log(
        pool,
        Some(&admin.0.id),
        "ai_settings",
        &format!(
            "{} ({}), Modell {}, {}{key}",
            ai.provider.name,
            if ai.enabled { "ein" } else { "aus" },
            ai.model,
            if ai.auto_categorize {
                "einteilen ein"
            } else {
                "einteilen aus"
            },
        ),
    )
    .await;
    Ok(Json(ai.to_settings()))
}

/// Modelle des gewählten Anbieters, mit `?provider=` die eines anderen (dessen Schlüssel und Adresse).
#[get("/ai/models?<provider>")]
async fn models(
    _admin: AdminUser,
    provider: Option<&str>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AiJson<Vec<api::AiModel>> {
    let mut stored = settings::load_stored(pool).await?;
    if let Some(p) = provider.map(str::trim).filter(|p| !p.is_empty()) {
        let p =
            provider::find(p).ok_or_else(|| AppError::bad("Diesen KI-Anbieter gibt es nicht."))?;
        stored.provider = p.id.to_string();
    }
    let ai = AiConfig::resolve(config, &stored);
    Ok(Json(client::models(&ai).await?))
}

/// Verbindung prüfen – mit den mitgeschickten (noch nicht gespeicherten) Einstellungen,
/// ohne Angaben mit den gespeicherten. Leerer bzw. fehlender Schlüssel = der gespeicherte.
#[post("/ai/test", data = "<body>")]
async fn test(
    _admin: AdminUser,
    body: Option<Json<api::AiSettingsUpdate>>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::AiTestResult>> {
    let mut stored = settings::load_stored(pool).await?;
    if let Some(mut update) = body.map(Json::into_inner) {
        if update
            .api_key
            .as_deref()
            .is_some_and(|k| k.trim().is_empty())
        {
            update.api_key = None;
        }
        settings::apply_update(&mut stored, &update, config)?;
    }
    let ai = AiConfig::resolve(config, &stored);
    Ok(Json(client::test(&ai).await))
}

#[post("/ai/summarize", data = "<body>")]
async fn summarize(
    user: CurrentUser,
    body: Json<api::SummarizeRequest>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AiJson<api::AiText> {
    let ai = AiConfig::load(pool, config).await?;
    Ok(Json(
        ai::summarize::summarize(pool, &ai, &user.0, &body).await?,
    ))
}

#[post("/ai/reply", data = "<body>")]
async fn reply(
    user: CurrentUser,
    body: Json<api::DraftReplyRequest>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AiJson<api::AiDraft> {
    let ai = AiConfig::load(pool, config).await?;
    Ok(Json(compose::reply(pool, &ai, &user.0, &body).await?))
}

#[post("/ai/write", data = "<body>")]
async fn write(
    user: CurrentUser,
    body: Json<api::WriteRequest>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AiJson<api::AiDraft> {
    let ai = AiConfig::load(pool, config).await?;
    Ok(Json(compose::write(pool, &ai, &user.0, &body).await?))
}

#[post("/ai/chat", data = "<body>")]
async fn chat(
    user: CurrentUser,
    body: Json<api::ChatRequest>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AiJson<api::ChatReply> {
    let ai = AiConfig::load(pool, config).await?;
    Ok(Json(assistant::chat(pool, &ai, &user.0, &body).await?))
}

#[post("/ai/categorize", data = "<body>")]
async fn categorize(
    user: CurrentUser,
    body: Json<api::CategorizeRequest>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AiJson<api::BulkResult> {
    let ai = AiConfig::load(pool, config).await?;
    Ok(Json(
        ai::categorize::categorize(pool, &ai, &user.0, &body).await?,
    ))
}
