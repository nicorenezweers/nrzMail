//! nrzMail – Oberfläche (Dioxus 0.7, WebAssembly) mit Komponenten von rust-ui.
//! Spricht nur mit der JSON-API unter `/api/v1` (Sitzungs-Cookie) und benutzt dafür die
//! gemeinsamen Typen aus `nrzmail_api`.

mod actions;
mod api;
mod components;
mod pages;
mod state;
mod ui;
mod util;

use components::overlay::{ConfirmHost, Toaster};
use dioxus::prelude::*;
use pages::archive::Archive;
use pages::auth::{Login, Register, Setup};
use pages::layout::AppLayout;
use pages::mail::Mail;
use pages::misc::{Home, NotFound, SettingsHome};
use pages::settings::Settings;
use pages::stats::Stats;

static CSS: Asset = asset!("/assets/tailwind.css");

#[derive(Routable, Clone, PartialEq, Debug)]
#[rustfmt::skip]
pub enum Route {
    #[route("/login?:next")]
    Login { next: String },
    #[route("/setup")]
    Setup {},
    #[route("/register")]
    Register {},
    #[layout(AppLayout)]
        #[route("/")]
        Home {},
        // Hauptansicht. Leere Werte = Vorgabe (Posteingang über alle Postfächer).
        #[route("/mail?:..query")]
        Mail { query: MailQuery },
        #[route("/archive")]
        Archive {},
        #[route("/stats")]
        Stats {},
        #[route("/settings")]
        SettingsHome {},
        #[route("/settings/:section")]
        Settings { section: String },
    #[end_layout]
    #[route("/:..segments")]
    NotFound { segments: Vec<String> },
}

impl Route {
    /// Mail-Ansicht; leere Werte fehlen in der Adresse (`/mail?view=inbox&open=…`).
    pub fn mail(view: &str, account: &str, folder: &str, category: &str, q: &str, open: &str) -> Route {
        Route::Mail {
            query: MailQuery {
                view: view.to_string(),
                account: account.to_string(),
                folder: folder.to_string(),
                category: category.to_string(),
                q: q.to_string(),
                open: open.to_string(),
            },
        }
    }

    /// Posteingang (oder eine andere Ansicht) über alle Postfächer.
    pub fn view(view: nrzmail_api::MailView) -> Route {
        Route::mail(view.as_str(), "", "", "", "", "")
    }

    /// Posteingang eines Postfachs.
    pub fn account_inbox(account: &str) -> Route {
        Route::mail("inbox", account, "", "", "", "")
    }

    /// Eine Nachricht öffnen (über „Alle Nachrichten“, damit sie sicher in der Liste steht).
    pub fn message(id: &str) -> Route {
        Route::mail(nrzmail_api::MailView::All.as_str(), "", "", "", "", id)
    }

    /// Ein Ordner eines Postfachs.
    pub fn folder(account: &str, folder: &str) -> Route {
        Route::mail("", account, folder, "", "", "")
    }
}

/// Zustand der Mail-Ansicht in der Adresse, z. B. `/mail?view=inbox&q=rechnung&open=…`.
/// Leere Werte fehlen. Der Router dekodiert die ganze Abfrage, bevor er sie hier zerlegt –
/// darum sind `%`, `&`, `=` und `+` in Werten doppelt kodiert.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MailQuery {
    pub view: String,
    pub account: String,
    pub folder: String,
    pub category: String,
    pub q: String,
    pub open: String,
}

impl MailQuery {
    fn pairs(&self) -> [(&'static str, &String); 6] {
        [
            ("view", &self.view),
            ("account", &self.account),
            ("folder", &self.folder),
            ("category", &self.category),
            ("q", &self.q),
            ("open", &self.open),
        ]
    }
}

impl From<&str> for MailQuery {
    fn from(s: &str) -> Self {
        let unescape = |v: &str| v.replace("%26", "&").replace("%3D", "=").replace("%2B", "+").replace("%25", "%");
        let mut q = MailQuery::default();
        for part in s.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            let v = unescape(v);
            match k {
                "view" => q.view = v,
                "account" => q.account = v,
                "folder" => q.folder = v,
                "category" => q.category = v,
                "q" => q.q = v,
                "open" => q.open = v,
                _ => {}
            }
        }
        q
    }
}

impl std::fmt::Display for MailQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let escape = |v: &str| v.replace('%', "%2525").replace('&', "%2526").replace('=', "%253D").replace('+', "%252B");
        let mut first = true;
        for (k, v) in self.pairs() {
            if v.trim().is_empty() {
                continue;
            }
            if !first {
                f.write_str("&")?;
            }
            first = false;
            write!(f, "{k}={}", escape(v))?;
        }
        Ok(())
    }
}

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    use_hook(|| {
        // Bis an den Rand (Notch, Home-Indikator), Tastatur verkleinert den Inhalt, kein Zoom
        // beim Tippen in Felder; die Ränder halten pt-safe/pb-safe frei.
        document::eval(
            "let m = document.querySelector('meta[name=viewport]'); \
             if (!m) { m = document.createElement('meta'); m.name = 'viewport'; document.head.appendChild(m); } \
             m.setAttribute('content', 'width=device-width, initial-scale=1, viewport-fit=cover, interactive-widget=resizes-content');",
        );
        // iPhone: sichtbare Höhe über der Tastatur für Vollbild-Fenster (Verfassen, Dieter)
        document::eval(
            "const vv = window.visualViewport; if (vv) { const r = document.documentElement.style; \
             const set = () => { r.setProperty('--vvh', vv.height + 'px'); r.setProperty('--vvt', vv.offsetTop + 'px'); }; \
             vv.addEventListener('resize', set); vv.addEventListener('scroll', set); set(); }",
        );
        state::watch_phone();
    });
    rsx! {
        document::Stylesheet { href: CSS }
        document::Link { rel: "icon", r#type: "image/png", href: "/static/favicon-32.png" }
        document::Link { rel: "apple-touch-icon", href: "/static/icon.png" }
        document::Meta { name: "theme-color", content: "#1f1f1f" }
        // Als Web-App auf dem Home-Bildschirm (iPhone/Android): Vollbild, eigenes Symbol
        document::Link { rel: "manifest", href: "/static/manifest.webmanifest" }
        document::Meta { name: "apple-mobile-web-app-capable", content: "yes" }
        document::Meta { name: "mobile-web-app-capable", content: "yes" }
        document::Meta { name: "apple-mobile-web-app-status-bar-style", content: "black" }
        document::Meta { name: "apple-mobile-web-app-title", content: "nrzMail" }
        Router::<Route> {}
        ConfirmHost {}
        Toaster {}
    }
}
