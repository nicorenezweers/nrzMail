//! Statische Dateien unter `/static/`, ins Programm eingebettet – nrzMail bleibt ein
//! einzelnes Programm und braucht kein Internet für Schrift und Icons.

use rocket::http::{ContentType, Header};
use rocket::response::{self, Responder, Response};
use rocket::{get, routes, Request, Route};
use std::io::Cursor;
use std::path::PathBuf;

pub fn routes() -> Vec<Route> {
    routes![asset, favicon]
}

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

pub struct Asset {
    body: &'static [u8],
    content_type: ContentType,
    immutable: bool,
}

/// Übersetzung der Oberfläche (deutsch → Sprache), z. B. `locales/en.json`.
fn locale(path: &str) -> Option<Asset> {
    let lang = path.strip_prefix("locales/")?.strip_suffix(".json")?;
    LOCALES
        .iter()
        .find(|(l, _)| *l == lang)
        .map(|(_, body)| Asset {
            body,
            content_type: ContentType::JSON,
            immutable: false,
        })
}

fn lookup(path: &str) -> Option<Asset> {
    let png = ContentType::PNG;
    let woff2 = ContentType::new("font", "woff2");
    let (body, content_type, immutable): (&'static [u8], ContentType, bool) = match path {
        "fonts/inter-latin.woff2" => (
            include_bytes!("../../static/fonts/inter-latin.woff2"),
            woff2,
            true,
        ),
        "fonts/inter-latin-ext.woff2" => (
            include_bytes!("../../static/fonts/inter-latin-ext.woff2"),
            woff2,
            true,
        ),
        "icon.png" => (include_bytes!("../../static/icon.png"), png, false),
        "favicon-32.png" => (include_bytes!("../../static/favicon-32.png"), png, false),
        "manifest.webmanifest" => (
            include_bytes!("../../static/manifest.webmanifest"),
            ContentType::new("application", "manifest+json"),
            false,
        ),
        _ => return locale(path),
    };
    Some(Asset {
        body,
        content_type,
        immutable,
    })
}

impl<'r> Responder<'r, 'static> for Asset {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        let cache = if self.immutable {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=3600"
        };
        Response::build()
            .header(self.content_type)
            .header(Header::new("Cache-Control", cache))
            .sized_body(self.body.len(), Cursor::new(self.body))
            .ok()
    }
}

#[get("/static/<path..>")]
fn asset(path: PathBuf) -> Option<Asset> {
    lookup(&path.to_str()?.replace('\\', "/"))
}

#[get("/favicon.ico")]
fn favicon() -> Option<Asset> {
    lookup("favicon-32.png")
}
