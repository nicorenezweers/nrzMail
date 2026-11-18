//! REST-API unter `/api/v1` – Anfragen und Antworten sind die Typen aus `nrzmail_api`
//! (`crate::api`); die Liste aller Endpunkte steht dort in der Moduldoku.

mod admin;
mod ai;
mod auth;
mod mail;

use crate::api;
use rocket::serde::json::Json;
use rocket::{get, routes, Route};

pub fn routes() -> Vec<Route> {
    let mut r = routes![health];
    r.extend(auth::routes());
    r.extend(admin::routes());
    r.extend(mail::routes());
    r.extend(ai::routes());
    r
}

/// Lebenszeichen für Docker-Healthcheck und Monitoring.
#[get("/health")]
pub fn health() -> Json<api::Health> {
    Json(api::Health {
        status: "ok".into(),
        name: "nrzMail".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}
