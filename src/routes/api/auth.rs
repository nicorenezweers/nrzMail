//! Anmeldung und eigenes Konto: `/auth/*` und `/me*`.
//!
//! Ändernde Anfragen ohne Anmeldung (Einrichtung, Registrierung, Anmeldung, Abmeldung) prüfen
//! die Herkunft mit [`SameOrigin`], alle anderen über [`CurrentUser`]. Fehlversuche bei der
//! Anmeldung und beim Ändern des Passworts zählt die Bremse ([`LoginThrottle`]).

use crate::api;
use crate::auth::guards::{CurrentUser, MaybeUser, SameOrigin, UserAgent};
use crate::auth::session;
use crate::auth::throttle::{Limit, LoginThrottle};
use crate::config::Config;
use crate::db::models::User;
use crate::error::{error_response, AppError, AppResult};
use crate::services::{activity, users};
use rocket::http::{ContentType, CookieJar, Header, Status};
use rocket::response::{self, Responder, Response};
use rocket::serde::json::Json;
use rocket::{get, post, put, routes, Request, Route, State};
use sqlx::SqlitePool;
use std::io::Cursor;
use std::net::IpAddr;
use std::time::Duration;

pub fn routes() -> Vec<Route> {
    routes![
        status,
        setup,
        register,
        login,
        logout,
        me,
        update_me,
        change_password,
        preferences,
        update_preferences
    ]
}

/// Wartezeit nach einem Fehlversuch – bremst zusätzlich, ohne Benutzer merklich aufzuhalten.
const FAILURE_DELAY: Duration = Duration::from_millis(300);

/// Fehler der Anmeldung: alle von [`AppError`], dazu „falsche Anmeldedaten“ (401 mit eigener
/// Meldung) und „zu viele Fehlversuche“ (429 mit `Retry-After`).
#[derive(Debug)]
pub enum AuthError {
    App(AppError),
    InvalidCredentials,
    TooManyAttempts(Duration),
}

impl From<AppError> for AuthError {
    fn from(e: AppError) -> Self {
        AuthError::App(e)
    }
}

impl From<sqlx::Error> for AuthError {
    fn from(e: sqlx::Error) -> Self {
        AuthError::App(e.into())
    }
}

impl<'r> Responder<'r, 'static> for AuthError {
    fn respond_to(self, req: &'r Request<'_>) -> response::Result<'static> {
        match self {
            AuthError::App(e) => e.respond_to(req),
            AuthError::InvalidCredentials => error_response(
                req,
                Status::Unauthorized,
                "Benutzername oder Passwort ist falsch.",
            ),
            AuthError::TooManyAttempts(wait) => {
                let minutes = wait.as_secs().div_ceil(60).max(1);
                let message = if minutes == 1 {
                    "Zu viele Fehlversuche – bitte in einer Minute erneut versuchen.".to_string()
                } else {
                    format!("Zu viele Fehlversuche – bitte in {minutes} Minuten erneut versuchen.")
                };
                let body = serde_json::to_string(&api::ApiError {
                    error: api::ErrorBody {
                        code: "too_many_requests".into(),
                        message,
                    },
                })
                .unwrap_or_default();
                Response::build()
                    .status(Status::TooManyRequests)
                    .header(ContentType::JSON)
                    .header(Header::new(
                        "Retry-After",
                        wait.as_secs().max(1).to_string(),
                    ))
                    .sized_body(body.len(), Cursor::new(body))
                    .ok()
            }
        }
    }
}

/// Sitzung anlegen, Cookie setzen, letzte Anmeldung festhalten.
async fn start_session(
    pool: &SqlitePool,
    config: &Config,
    cookies: &CookieJar<'_>,
    ua: &UserAgent,
    user: &User,
) -> AppResult<User> {
    let token = session::create(pool, &user.id, &ua.0).await?;
    session::set_cookie(cookies, token, config.cookie_secure);
    users::touch_login(pool, &user.id).await
}

fn ip_label(ip: Option<IpAddr>) -> String {
    ip.map_or_else(|| "unbekannt".to_string(), |ip| ip.to_string())
}

// ------------------------------------------------------------------ Anmeldung

#[get("/auth/status")]
async fn status(
    user: MaybeUser,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::AuthStatus>> {
    Ok(Json(api::AuthStatus {
        setup_required: users::count(pool).await? == 0,
        registration_open: users::registration_open(pool, config).await?,
        user: user.0.as_ref().map(User::to_api),
        version: env!("CARGO_PKG_VERSION").into(),
    }))
}

/// Erstes Konto anlegen (Administrator) und anmelden – nur solange es keine Benutzer gibt.
#[post("/auth/setup", data = "<body>")]
async fn setup(
    _origin: SameOrigin,
    body: Json<api::SetupRequest>,
    cookies: &CookieJar<'_>,
    ua: UserAgent,
    ip: Option<IpAddr>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::User>> {
    if users::count(pool).await? > 0 {
        return Err(AppError::Conflict("nrzMail ist schon eingerichtet.".into()));
    }
    let user = users::create_first(
        pool,
        &users::NewAccount {
            username: &body.username,
            password: &body.password,
            display_name: &body.display_name,
            email: &body.email,
            is_admin: true,
        },
    )
    .await?;
    let user = start_session(pool, config, cookies, &ua, &user).await?;
    tracing::info!(user = %user.username, "nrzMail eingerichtet");
    activity::log(
        pool,
        Some(&user.id),
        "setup",
        &format!("Erstes Konto {} · IP {}", user.username, ip_label(ip)),
    )
    .await;
    Ok(Json(user.to_api()))
}

/// Selbst registrieren und anmelden – nur wenn die Registrierung offen ist.
#[post("/auth/register", data = "<body>")]
async fn register(
    _origin: SameOrigin,
    body: Json<api::SetupRequest>,
    cookies: &CookieJar<'_>,
    ua: UserAgent,
    ip: Option<IpAddr>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Json<api::User>> {
    if users::count(pool).await? == 0 {
        return Err(AppError::Conflict(
            "nrzMail ist noch nicht eingerichtet – zuerst das erste Konto anlegen.".into(),
        ));
    }
    if !users::registration_open(pool, config).await? {
        return Err(AppError::forbidden(
            "Die Registrierung ist geschlossen – ein Administrator muss dein Konto anlegen.",
        ));
    }
    let user = users::create(
        pool,
        &users::NewAccount {
            username: &body.username,
            password: &body.password,
            display_name: &body.display_name,
            email: &body.email,
            is_admin: false,
        },
    )
    .await?;
    let user = start_session(pool, config, cookies, &ua, &user).await?;
    tracing::info!(user = %user.username, "Konto registriert");
    activity::log(
        pool,
        Some(&user.id),
        "register",
        &format!("IP {}", ip_label(ip)),
    )
    .await;
    Ok(Json(user.to_api()))
}

#[post("/auth/login", data = "<body>")]
#[allow(clippy::too_many_arguments)]
async fn login(
    _origin: SameOrigin,
    body: Json<api::LoginRequest>,
    cookies: &CookieJar<'_>,
    ua: UserAgent,
    ip: Option<IpAddr>,
    throttle: &State<LoginThrottle>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> Result<Json<api::User>, AuthError> {
    let limit = Limit::from_config(config);
    let key = LoginThrottle::key(ip, &body.username);
    if let Some(wait) = throttle.blocked(&key, limit) {
        return Err(AuthError::TooManyAttempts(wait));
    }

    let (user, ok) = users::authenticate(pool, &body.username, &body.password).await?;
    let user = match user {
        Some(user) if ok => user,
        user => {
            let user_id = user.as_ref().map(|u| u.id.as_str());
            let name: String = body.username.trim().chars().take(64).collect();
            tracing::warn!(username = %name, ip = %ip_label(ip), "Anmeldung fehlgeschlagen");
            activity::log(
                pool,
                user_id,
                "login_failed",
                &format!("{name} · IP {}", ip_label(ip)),
            )
            .await;
            if throttle.failure(&key, limit) {
                activity::log(
                    pool,
                    user_id,
                    "login_blocked",
                    &format!(
                        "{name} · IP {} · {} Fehlversuche",
                        ip_label(ip),
                        limit.max_failures
                    ),
                )
                .await;
            }
            tokio::time::sleep(FAILURE_DELAY).await;
            return Err(AuthError::InvalidCredentials);
        }
    };

    throttle.reset(&key);
    let user = start_session(pool, config, cookies, &ua, &user).await?;
    tracing::info!(user = %user.username, "Anmeldung");
    activity::log(
        pool,
        Some(&user.id),
        "login",
        &format!("IP {}", ip_label(ip)),
    )
    .await;
    Ok(Json(user.to_api()))
}

#[post("/auth/logout")]
async fn logout(
    _origin: SameOrigin,
    user: MaybeUser,
    cookies: &CookieJar<'_>,
    pool: &State<SqlitePool>,
) -> AppResult<Status> {
    if let Some(token) = session::token_from(cookies) {
        session::destroy(pool, &token).await?;
    }
    session::clear_cookie(cookies);
    if let Some(user) = user.0 {
        activity::log(pool, Some(&user.id), "logout", "").await;
    }
    Ok(Status::NoContent)
}

// ------------------------------------------------------------------ Eigenes Konto

#[get("/me")]
fn me(user: CurrentUser) -> Json<api::User> {
    Json(user.0.to_api())
}

#[put("/me", data = "<body>")]
async fn update_me(
    user: CurrentUser,
    body: Json<api::UpdateProfile>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::User>> {
    Ok(Json(
        users::update_profile(pool, &user.0, &body).await?.to_api(),
    ))
}

/// Passwort ändern. Alle anderen Sitzungen enden, die laufende bleibt.
#[put("/me/password", data = "<body>")]
async fn change_password(
    user: CurrentUser,
    body: Json<api::ChangePassword>,
    cookies: &CookieJar<'_>,
    throttle: &State<LoginThrottle>,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> Result<Status, AuthError> {
    let limit = Limit::from_config(config);
    let key = format!("passwort|{}", user.0.id);
    if let Some(wait) = throttle.blocked(&key, limit) {
        return Err(AuthError::TooManyAttempts(wait));
    }
    let token = session::token_from(cookies);
    if !users::change_password(pool, &user.0, &body.current, &body.new, token.as_deref()).await? {
        throttle.failure(&key, limit);
        tokio::time::sleep(FAILURE_DELAY).await;
        return Err(AppError::bad("Das aktuelle Passwort stimmt nicht.").into());
    }
    throttle.reset(&key);
    activity::log(pool, Some(&user.0.id), "password_changed", "").await;
    Ok(Status::NoContent)
}

#[get("/me/preferences")]
fn preferences(user: CurrentUser) -> Json<api::Preferences> {
    Json(user.0.preferences())
}

#[put("/me/preferences", data = "<body>")]
async fn update_preferences(
    user: CurrentUser,
    body: Json<api::Preferences>,
    pool: &State<SqlitePool>,
) -> AppResult<Json<api::Preferences>> {
    Ok(Json(users::set_preferences(pool, &user.0, &body).await?))
}
