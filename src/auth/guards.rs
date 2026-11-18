//! Request Guards. Routen verlangen damit eine Anmeldung:
//!
//! * [`CurrentUser`] – angemeldet über das Sitzungs-Cookie, Konto nicht gesperrt.
//! * [`AdminUser`] – zusätzlich Administrator der Instanz.
//! * [`MaybeUser`] – angemeldet oder nicht (z. B. `/auth/status`).
//! * [`SameOrigin`] – keine Anmeldung nötig, aber nur von der eigenen Seite (z. B. `/auth/login`).
//!
//! Schutz gegen CSRF: Das Cookie ist `SameSite=Lax` (fremde Seiten können kein POST mit
//! Cookie auslösen). Zusätzlich werden ändernde Anfragen abgewiesen, deren `Origin` nicht
//! zum eigenen Host passt.

use crate::auth::session;
use crate::db::models::User;
use crate::error::AppError;
use rocket::http::{Method, Status};
use rocket::request::{FromRequest, Outcome, Request};
use sqlx::SqlitePool;

/// Benutzer der Sitzung – je Anfrage nur einmal aus der Datenbank geladen.
async fn session_user(req: &Request<'_>) -> Option<User> {
    req.local_cache_async(async {
        let pool = req.rocket().state::<SqlitePool>()?;
        let token = session::token_from(req.cookies())?;
        session::lookup(pool, &token)
            .await
            .ok()
            .flatten()
            .filter(|u| !u.disabled)
    })
    .await
    .clone()
}

/// Ändernde Anfrage von einer fremden Seite?
pub fn is_cross_origin(req: &Request<'_>) -> bool {
    if matches!(req.method(), Method::Get | Method::Head | Method::Options) {
        return false;
    }
    let Some(origin) = req.headers().get_one("Origin") else {
        return false;
    };
    let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or("");
    // `dx serve` (Entwicklung der Oberfläche) leitet von :8080 weiter und schreibt Host um
    if cfg!(debug_assertions) && origin_host == "localhost:8080" {
        return false;
    }
    let own = [
        req.headers().get_one("X-Forwarded-Host"),
        req.headers().get_one("Host"),
    ];
    !own.iter()
        .flatten()
        .any(|h| h.eq_ignore_ascii_case(origin_host))
}

/// Für ändernde Anfragen ohne Anmeldung (Einrichtung, Anmeldung, Registrierung):
/// weist Anfragen mit fremdem `Origin` ab, wie [`CurrentUser`] es für angemeldete tut.
pub struct SameOrigin;

#[rocket::async_trait]
impl<'r> FromRequest<'r> for SameOrigin {
    type Error = AppError;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        if is_cross_origin(req) {
            Outcome::Error((
                Status::Forbidden,
                AppError::forbidden("Anfrage von einer fremden Seite abgewiesen."),
            ))
        } else {
            Outcome::Success(SameOrigin)
        }
    }
}

pub struct CurrentUser(pub User);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for CurrentUser {
    type Error = AppError;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        if is_cross_origin(req) {
            return Outcome::Error((
                Status::Forbidden,
                AppError::forbidden("Anfrage von einer fremden Seite abgewiesen."),
            ));
        }
        match session_user(req).await {
            Some(user) => Outcome::Success(CurrentUser(user)),
            None => Outcome::Error((Status::Unauthorized, AppError::Unauthorized)),
        }
    }
}

/// Angemeldet, aber optional – für Seiten wie die Anmeldung selbst.
pub struct MaybeUser(pub Option<User>);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for MaybeUser {
    type Error = std::convert::Infallible;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(MaybeUser(session_user(req).await))
    }
}

pub struct AdminUser(pub User);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AdminUser {
    type Error = AppError;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        match CurrentUser::from_request(req).await {
            Outcome::Success(CurrentUser(u)) if u.is_admin => Outcome::Success(AdminUser(u)),
            Outcome::Success(_) => Outcome::Error((
                Status::Forbidden,
                AppError::forbidden("Nur für Administratoren."),
            )),
            Outcome::Error(e) => Outcome::Error(e),
            Outcome::Forward(s) => Outcome::Forward(s),
        }
    }
}

/// Browser-Kennung für die Sitzungsliste.
pub struct UserAgent(pub String);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for UserAgent {
    type Error = std::convert::Infallible;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(UserAgent(
            req.headers()
                .get_one("User-Agent")
                .unwrap_or_default()
                .to_string(),
        ))
    }
}
