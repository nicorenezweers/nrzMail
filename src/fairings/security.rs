//! Sicherheit für jede Antwort: Header mit einer strengen Content-Security-Policy. Rockets
//! eigenes Shield (u. a. `X-Frame-Options: SAMEORIGIN`) wird beim Start durch ein leeres ersetzt,
//! damit alle Sicherheits-Header von hier kommen; Routen können einzelne davon selbst setzen.
//! Beim Start legt das Fairing ausserdem den Zustand der Anmeldebremse an ([`LoginThrottle`]).
//!
//! Die CSP erlaubt Skripte, Stile und Schrift nur vom eigenen Server: Die Oberfläche ist
//! eingebettet (siehe routes::spa und routes::assets), Skripte nur als Dateien – keine
//! Inline-Skripte. WebAssembly braucht 'wasm-unsafe-eval', Dioxus' `eval` 'unsafe-eval'.
//! Inline-Stile bleiben erlaubt (HTML-Mails bringen ihre eigenen mit).
//!
//! Bilder dürfen auch von fremden Servern kommen: HTML-Mails zeigen externe Bilder, wenn der
//! Benutzer sie lädt (die Mail-API liefert sie bis dahin nur als `data-src`). Ohne Referrer
//! (`Referrer-Policy: same-origin`). Rahmen (Mail-Ansicht, PDF-Anhänge) nur vom eigenen
//! Server oder als `blob:`; nrzMail selbst lässt sich nirgends einbetten.

use crate::auth::throttle::LoginThrottle;
use rocket::fairing::{self, Fairing, Info, Kind};
use rocket::http::Header;
use rocket::shield::Shield;
use rocket::{Build, Request, Response, Rocket};

pub struct SecurityHeaders;

const CSP: &str = "default-src 'self'; \
    script-src 'self' 'wasm-unsafe-eval' 'unsafe-eval'; \
    style-src 'self' 'unsafe-inline'; \
    font-src 'self'; \
    img-src 'self' data: blob: https: http:; \
    frame-src 'self' blob:; \
    worker-src 'self' blob:; \
    connect-src 'self'; \
    object-src 'none'; \
    frame-ancestors 'none'; \
    base-uri 'self'; \
    form-action 'self'";

#[rocket::async_trait]
impl Fairing for SecurityHeaders {
    fn info(&self) -> Info {
        Info {
            name: "Sicherheits-Header",
            kind: Kind::Ignite | Kind::Response,
        }
    }

    async fn on_ignite(&self, rocket: Rocket<Build>) -> fairing::Result {
        Ok(rocket
            .manage(LoginThrottle::default())
            .attach(Shield::new()))
    }

    async fn on_response<'r>(&self, _req: &'r Request<'_>, res: &mut Response<'r>) {
        for (name, value) in [
            ("Content-Security-Policy", CSP),
            ("X-Content-Type-Options", "nosniff"),
            ("X-Frame-Options", "DENY"),
            ("Referrer-Policy", "same-origin"),
            (
                "Permissions-Policy",
                "camera=(), microphone=(), geolocation=()",
            ),
            ("Cross-Origin-Opener-Policy", "same-origin"),
        ] {
            if !res.headers().contains(name) {
                res.set_header(Header::new(name, value));
            }
        }
    }
}
