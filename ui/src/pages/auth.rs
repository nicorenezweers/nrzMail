//! Anmelden, Ersteinrichtung (erstes Konto wird Administrator) und Registrierung.
//! `GET /auth/status` entscheidet, welche Seite gilt.

use crate::Route;
use crate::api;
use crate::components::common::*;
use dioxus::prelude::*;
use nrzmail_api::{AuthStatus, LoginRequest, SetupRequest, User};

/// Gemeinsamer Rahmen der Anmeldeseiten: Logo, Karte, Fusszeile.
#[component]
fn AuthCard(#[props(into)] title: String, #[props(into)] subtitle: String, children: Element) -> Element {
    rsx! {
        div { class: "flex min-h-screen items-center justify-center bg-background p-4",
            div { class: "w-full max-w-sm",
                div { class: "mb-6 flex flex-col items-center gap-3 text-center",
                    img { src: "/static/icon.png", class: "size-12 rounded-xl", alt: "" }
                    div {
                        h1 { class: "text-xl font-semibold tracking-tight", "{title}" }
                        p { class: "mt-1 text-sm text-muted-foreground", "{subtitle}" }
                    }
                }
                div { class: "rounded-xl border bg-card p-6 shadow-sm", {children} }
                p { class: "mt-6 text-center text-xs text-muted-foreground", "nrzMail – deine Mails und dein Archiv, verschlüsselt in einer Datei" }
            }
        }
    }
}

/// Nur Pfade dieser App als Ziel nach der Anmeldung (keine fremden Adressen).
fn safe_next(next: &str) -> String {
    if next.starts_with('/') && !next.starts_with("//") && !next.contains('\\') && !next.starts_with("/login") {
        next.to_string()
    } else {
        "/".into()
    }
}

/// Ganze Seite laden, damit alles frisch startet.
fn go(target: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.location().assign(target);
    }
}

#[component]
pub fn Login(next: String) -> Element {
    let mut username = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let status = use_resource(|| api::get::<AuthStatus>("/auth/status"));
    let nav = use_navigator();
    let target = safe_next(&next);
    if let Some(Ok(s)) = &*status.read() {
        if s.setup_required {
            nav.replace(Route::Setup {});
        } else if s.user.is_some() {
            go(&target);
        }
    }
    let can_register = status.read().as_ref().and_then(|r| r.as_ref().ok()).is_some_and(|s| s.registration_open);
    let version = status.read().as_ref().and_then(|r| r.as_ref().ok()).map(|s| s.version.clone()).unwrap_or_default();
    let submit = move |e: FormEvent| {
        e.prevent_default();
        let target = target.clone();
        spawn(async move {
            busy.set(true);
            error.set(None);
            let body = LoginRequest { username: username().trim().to_string(), password: password() };
            let res = api::post::<User, _>("/auth/login", &body).await;
            busy.set(false);
            match res {
                Ok(_) => go(&target),
                Err(e) if e.status == 401 => error.set(Some("Benutzername oder Passwort stimmt nicht.".into())),
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    rsx! {
        document::Title { "Anmelden – nrzMail" }
        AuthCard { title: "Anmelden", subtitle: "Willkommen zurück bei nrzMail",
            form { class: "grid gap-4", onsubmit: submit,
                if let Some(e) = error() { Callout { text: e, error: true } }
                Field { label: "Benutzername",
                    input { class: INPUT, autocomplete: "username", autofocus: true, required: true, value: "{username}", oninput: move |e| username.set(e.value()) }
                }
                Field { label: "Passwort",
                    input { class: INPUT, r#type: "password", autocomplete: "current-password", required: true, value: "{password}", oninput: move |e| password.set(e.value()) }
                }
                button { class: "{btn(BTN_PRIMARY)} w-full", disabled: busy(), r#type: "submit",
                    if busy() { icons::LoaderCircle { class: "size-4 animate-spin" } }
                    "Anmelden"
                }
                if can_register {
                    p { class: "text-center text-sm text-muted-foreground",
                        "Noch kein Konto? "
                        Link { class: "font-medium text-foreground underline-offset-4 hover:underline", to: Route::Register {}, "Registrieren" }
                    }
                }
            }
        }
        if !version.is_empty() {
            p { class: "fixed bottom-3 left-0 right-0 text-center text-[11px] text-muted-foreground/60", "Version {version}" }
        }
    }
}

#[component]
fn RegisterForm(first: bool) -> Element {
    let mut username = use_signal(String::new);
    let mut display_name = use_signal(String::new);
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut repeat = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let submit = move |e: FormEvent| {
        e.prevent_default();
        if password() != repeat() {
            error.set(Some("Die Passwörter stimmen nicht überein.".into()));
            return;
        }
        spawn(async move {
            busy.set(true);
            error.set(None);
            let body = SetupRequest {
                username: username().trim().to_string(),
                password: password(),
                display_name: display_name().trim().to_string(),
                email: email().trim().to_string(),
            };
            let path = if first { "/auth/setup" } else { "/auth/register" };
            let res = api::post::<User, _>(path, &body).await;
            busy.set(false);
            match res {
                // Gleich weiter zum ersten Postfach
                Ok(_) => go("/settings/postfaecher"),
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    rsx! {
        form { class: "grid gap-4", onsubmit: submit,
            if first {
                Callout { text: "Das erste Konto wird Administrator dieser Instanz." }
            }
            if let Some(e) = error() { Callout { text: e, error: true } }
            Field { label: "Benutzername", hint: "2–64 Zeichen: Buchstaben, Zahlen, Punkt, Minus, Unterstrich, @",
                input { class: INPUT, autocomplete: "username", autofocus: true, required: true, value: "{username}", oninput: move |e| username.set(e.value()) }
            }
            Field { label: "Name",
                input { class: INPUT, autocomplete: "name", placeholder: "z. B. Anna Muster", value: "{display_name}", oninput: move |e| display_name.set(e.value()) }
            }
            Field { label: "E-Mail",
                input { class: INPUT, r#type: "email", autocomplete: "email", value: "{email}", oninput: move |e| email.set(e.value()) }
            }
            Field { label: "Passwort", hint: "Mindestens 8 Zeichen",
                input { class: INPUT, r#type: "password", autocomplete: "new-password", required: true, minlength: 8, value: "{password}", oninput: move |e| password.set(e.value()) }
            }
            Field { label: "Passwort wiederholen",
                input { class: INPUT, r#type: "password", autocomplete: "new-password", required: true, value: "{repeat}", oninput: move |e| repeat.set(e.value()) }
            }
            button { class: "{btn(BTN_PRIMARY)} w-full", disabled: busy(), r#type: "submit",
                if busy() { icons::LoaderCircle { class: "size-4 animate-spin" } }
                if first { "Einrichten" } else { "Konto anlegen" }
            }
            if !first {
                p { class: "text-center text-sm text-muted-foreground",
                    "Schon ein Konto? "
                    Link { class: "font-medium text-foreground underline-offset-4 hover:underline", to: Route::Login { next: String::new() }, "Anmelden" }
                }
            }
        }
    }
}

#[component]
pub fn Setup() -> Element {
    let status = use_resource(|| api::get::<AuthStatus>("/auth/status"));
    let nav = use_navigator();
    // Schon eingerichtet → zur Anmeldung
    if let Some(Ok(s)) = &*status.read()
        && !s.setup_required {
            nav.replace(Route::Login { next: String::new() });
        }
    rsx! {
        document::Title { "Einrichten – nrzMail" }
        AuthCard { title: "nrzMail einrichten", subtitle: "Lege das erste Konto an.",
            RegisterForm { first: true }
        }
    }
}

#[component]
pub fn Register() -> Element {
    let status = use_resource(|| api::get::<AuthStatus>("/auth/status"));
    let state = status.read().as_ref().and_then(|r| r.as_ref().ok()).cloned();
    rsx! {
        document::Title { "Registrieren – nrzMail" }
        AuthCard { title: "Registrieren", subtitle: "Neues Konto bei nrzMail",
            match state {
                None => rsx! { Loading {} },
                Some(s) if s.setup_required => rsx! { RegisterForm { first: true } },
                Some(s) if s.registration_open => rsx! { RegisterForm { first: false } },
                Some(_) => rsx! {
                    div { class: "grid gap-4",
                        Callout { text: "Die Registrierung ist geschlossen. Ein Administrator kann dir ein Konto anlegen." }
                        Link { class: "{btn(BTN_OUTLINE)} w-full", to: Route::Login { next: String::new() }, "Zur Anmeldung" }
                    }
                },
            }
        }
    }
}
