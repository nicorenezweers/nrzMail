//! Anmeldung der macOS-App am eingebauten (lokalen) Server ohne Passwort.
//!
//! Die App startet den Server im eigenen Prozess auf `127.0.0.1` und setzt dafür
//! `NRZMAIL_DESKTOP_TOKEN` (bei jedem Start neu, zufällig). Nur wer dieses Token kennt – also
//! das Fenster der App –, wird mit `GET /__desktop/login?token=…` angemeldet. Gibt es noch kein
//! Konto, legt die Route eines an (Name des macOS-Benutzers, zufälliges Passwort). Ohne Token
//! (Server, Docker, Umbrel) antwortet die Route mit 404.

use crate::auth::guards::UserAgent;
use crate::auth::session;
use crate::config::Config;
use crate::db::models::User;
use crate::error::{AppError, AppResult};
use crate::services::{activity, users};
use rocket::http::CookieJar;
use rocket::response::Redirect;
use rocket::{get, routes, Route, State};
use sqlx::SqlitePool;

pub fn routes() -> Vec<Route> {
    routes![login]
}

#[get("/__desktop/login?<token>")]
async fn login(
    token: &str,
    cookies: &CookieJar<'_>,
    ua: UserAgent,
    pool: &State<SqlitePool>,
    config: &State<Config>,
) -> AppResult<Redirect> {
    let expected = config.desktop_token.as_deref().unwrap_or_default();
    if expected.len() < 32 || !constant_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(AppError::NotFound);
    }
    let user = match local_user(pool, config).await? {
        Some(u) => u,
        None => create_local_user(pool, config).await?,
    };
    let token = session::create(pool, &user.id, &ua.0).await?;
    session::set_cookie(cookies, token, false);
    users::touch_login(pool, &user.id).await?;
    Ok(Redirect::to("/"))
}

/// Konto der App: das mit dem Namen des macOS-Benutzers, sonst der erste Administrator.
async fn local_user(pool: &SqlitePool, config: &Config) -> AppResult<Option<User>> {
    let name = username(&config.desktop_user);
    if let Some(u) = users::by_username(pool, &name).await? {
        if !u.disabled {
            return Ok(Some(u));
        }
    }
    Ok(users::list(pool)
        .await?
        .into_iter()
        .find(|u| u.is_admin && !u.disabled))
}

async fn create_local_user(pool: &SqlitePool, config: &Config) -> AppResult<User> {
    let name = username(&config.desktop_user);
    let display: String = config.desktop_full_name.trim().chars().take(100).collect();
    let password = session::new_token();
    let user = users::create_first(
        pool,
        &users::NewAccount {
            username: &name,
            password: &password,
            display_name: &display,
            email: "",
            is_admin: true,
        },
    )
    .await?;
    tracing::info!(user = %user.username, "Lokales Konto der App angelegt");
    activity::log(pool, Some(&user.id), "setup", "Lokales Konto der macOS-App").await;
    Ok(user)
}

/// Gültiger Benutzername aus dem macOS-Namen (sonst „ich“).
fn username(raw: &str) -> String {
    let clean: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'))
        .take(64)
        .collect();
    if clean.chars().count() >= 2 {
        clean
    } else {
        "ich".into()
    }
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        assert_eq!(username("nico"), "nico");
        assert_eq!(username("Nico R"), "NicoR");
        assert_eq!(username(""), "ich");
        assert_eq!(username("ä"), "ich");
    }

    #[test]
    fn compare() {
        assert!(constant_eq(b"abc", b"abc"));
        assert!(!constant_eq(b"abc", b"abd"));
        assert!(!constant_eq(b"abc", b"ab"));
    }
}
