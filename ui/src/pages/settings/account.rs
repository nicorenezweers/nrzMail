//! Eigenes Konto: Profil, Passwort und die persönlichen Einstellungen der Oberfläche.

use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::state::{self, ACCOUNTS, ME, PREFS};
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{ChangePassword, ConversationView, Preferences, UnsubscribeCleanup, UpdateProfile, User};

#[component]
pub fn AccountSettings() -> Element {
    rsx! {
        Profile {}
        Display {}
        Password {}
    }
}

#[component]
fn Profile() -> Element {
    let me = ME();
    let mut name = use_signal(|| me.as_ref().map(|u| u.display_name.clone()).unwrap_or_default());
    let mut email = use_signal(|| me.as_ref().map(|u| u.email.clone()).unwrap_or_default());
    let mut busy = use_signal(|| false);
    let Some(user) = me else { return rsx! { Loading {} } };
    let language = if user.language.is_empty() { "de".to_string() } else { user.language.clone() };
    let save = move |e: FormEvent| {
        e.prevent_default();
        let body = UpdateProfile { display_name: name().trim().to_string(), email: email().trim().to_string(), language: language.clone() };
        spawn(async move {
            busy.set(true);
            match api::put::<User, _>("/me", &body).await {
                Ok(u) => {
                    *ME.write() = Some(u);
                    toast("Profil gespeichert.");
                }
                Err(e) => toast_error(e),
            }
            busy.set(false);
        });
    };
    rsx! {
        Section { title: "Profil", description: "Name und Adresse für die Anmeldung und Benachrichtigungen – Absender von Mails stellst du je Postfach ein.",
            form { class: "grid max-w-xl gap-4", onsubmit: save,
                Field { label: "Benutzername", hint: "Lässt sich nicht ändern.",
                    input { class: INPUT, value: "{user.username}", readonly: true, disabled: true }
                }
                Field { label: "Name",
                    input { class: INPUT, autocomplete: "name", value: "{name}", oninput: move |e| name.set(e.value()) }
                }
                Field { label: "E-Mail",
                    input { class: INPUT, r#type: "email", autocomplete: "email", value: "{email}", oninput: move |e| email.set(e.value()) }
                }
                div { class: "flex items-center gap-3",
                    button { class: btn(BTN_PRIMARY), r#type: "submit", disabled: busy(), "Speichern" }
                    if let Some(t) = user.last_login_at {
                        span { class: "text-xs text-muted-foreground", "Letzte Anmeldung: {util::datetime(t)}" }
                    }
                }
            }
        }
    }
}

/// Wahl für die Sendeverzögerung (höchstens 30 Minuten).
const UNDO_CHOICES: [(u32, &str); 11] = [
    (0, "Aus – sofort senden"),
    (10, "10 Sekunden"),
    (30, "30 Sekunden"),
    (60, "1 Minute (Vorgabe)"),
    (120, "2 Minuten"),
    (300, "5 Minuten"),
    (480, "8 Minuten"),
    (600, "10 Minuten"),
    (900, "15 Minuten"),
    (1200, "20 Minuten"),
    (1800, "30 Minuten"),
];

/// Wahl für das automatische Aktualisieren.
const REFRESH_CHOICES: [(u32, &str); 7] = [
    (0, "Aus – nur im Hintergrund nach Plan"),
    (30, "Alle 30 Sekunden (Vorgabe)"),
    (60, "Jede Minute"),
    (120, "Alle 2 Minuten"),
    (300, "Alle 5 Minuten"),
    (600, "Alle 10 Minuten"),
    (1800, "Alle 30 Minuten"),
];

#[component]
fn Display() -> Element {
    let prefs = PREFS();
    let accounts = ACCOUNTS();
    let save = move |p: Preferences| {
        spawn(async move {
            match state::save_prefs(p).await {
                Ok(()) => toast("Gespeichert."),
                Err(e) => toast_error(e),
            }
        });
    };
    let p = prefs.clone();
    rsx! {
        Section { title: "Darstellung & Verhalten", description: "Gilt nur für dich und auf allen Geräten.",
            div { class: "grid gap-5",
                PrefToggle { label: "Kompakte Liste", hint: "Eine Zeile je Nachricht.", checked: p.compact,
                    onchange: { let p = p.clone(); move |v| save(Preferences { compact: v, ..p.clone() }) } }
                PrefToggle { label: "Vorschau in der Liste", hint: "Anfang des Textes unter dem Betreff.", checked: p.show_snippets,
                    onchange: { let p = p.clone(); move |v| save(Preferences { show_snippets: v, ..p.clone() }) } }
                PrefToggle { label: "Unterhaltungen bündeln", hint: "Antworten erscheinen zusammen mit der ursprünglichen Nachricht.", checked: p.threads,
                    onchange: { let p = p.clone(); move |v| save(Preferences { threads: v, ..p.clone() }) } }
                PrefToggle { label: "Kategorie-Reiter im Posteingang", hint: "Allgemein, Newsletter, Soziale Netzwerke, Benachrichtigungen.", checked: p.category_tabs,
                    onchange: { let p = p.clone(); move |v| save(Preferences { category_tabs: v, ..p.clone() }) } }
                PrefToggle { label: "Posteingang mit „Alle“ öffnen", hint: "Zeigt zuerst alle neuen Nachrichten – auch Newsletter und Benachrichtigungen. Aus: zuerst nur „Allgemein“.", checked: p.inbox_start_all,
                    onchange: { let p = p.clone(); move |v| save(Preferences { inbox_start_all: v, ..p.clone() }) } }
                Field { label: "Postfach automatisch aktualisieren", class: "max-w-sm",
                    hint: "Solange nrzMail offen ist, wird so oft beim Mail-Server nach neuen Nachrichten gefragt.",
                    select { class: SELECT,
                        onchange: { let p = p.clone(); move |e: FormEvent| { let v = e.value().parse().unwrap_or(30); save(Preferences { auto_refresh_seconds: v, ..p.clone() }) } },
                        for (secs , label) in REFRESH_CHOICES {
                            option { value: "{secs}", selected: p.auto_refresh_seconds == secs, "{label}" }
                        }
                        if !REFRESH_CHOICES.iter().any(|(s, _)| *s == p.auto_refresh_seconds) {
                            option { value: "{p.auto_refresh_seconds}", selected: true, "Alle {p.auto_refresh_seconds} Sekunden" }
                        }
                    }
                }
                Field { label: "Unterhaltungen anzeigen", class: "max-w-sm",
                    hint: "Chat: Sprechblasen, deine Nachrichten rechts, Zitate ausgeblendet. Klassisch: jede Mail vollständig als Karte.",
                    select { class: SELECT,
                        onchange: { let p = p.clone(); move |e: FormEvent| { let v = if e.value() == "classic" { ConversationView::Classic } else { ConversationView::Chat }; save(Preferences { conversation_view: v, ..p.clone() }) } },
                        option { value: "chat", selected: p.conversation_view == ConversationView::Chat, "Als Chat" }
                        option { value: "classic", selected: p.conversation_view == ConversationView::Classic, "Klassisch als E-Mails" }
                    }
                }
                if p.conversation_view == ConversationView::Chat {
                    PrefToggle { label: "Zitate im Chat ausblenden", hint: "Nur den neuen Teil jeder Nachricht zeigen („Am … schrieb …“ und „>“-Zeilen weg).", checked: p.chat_hide_quotes,
                        onchange: { let p = p.clone(); move |v| save(Preferences { chat_hide_quotes: v, ..p.clone() }) } }
                    PrefToggle { label: "Antwortfeld unter dem Chat", hint: "Direkt antworten wie in einem Messenger – geht als normale E-Mail mit Signatur raus.", checked: p.chat_quick_reply,
                        onchange: { let p = p.clone(); move |v| save(Preferences { chat_quick_reply: v, ..p.clone() }) } }
                }
                PrefToggle { label: "KI-Texte erst freigeben", hint: "Was die KI schreibt, erscheint als Vorschlag – erst nach „Übernehmen“ landet es in deiner Nachricht.", checked: p.ai_require_approval,
                    onchange: { let p = p.clone(); move |v| save(Preferences { ai_require_approval: v, ..p.clone() }) } }
                Field { label: "Nach dem Abmelden von Newslettern", class: "max-w-sm",
                    hint: "Was mit den bisherigen Nachrichten des Absenders geschieht.",
                    select { class: SELECT,
                        onchange: { let p = p.clone(); move |e: FormEvent| { let v = match e.value().as_str() { "archive" => UnsubscribeCleanup::Archive, "trash" => UnsubscribeCleanup::Trash, _ => UnsubscribeCleanup::None }; save(Preferences { unsubscribe_cleanup: v, ..p.clone() }) } },
                        option { value: "none", selected: p.unsubscribe_cleanup == UnsubscribeCleanup::None, "Liegen lassen" }
                        option { value: "archive", selected: p.unsubscribe_cleanup == UnsubscribeCleanup::Archive, "Alle archivieren" }
                        option { value: "trash", selected: p.unsubscribe_cleanup == UnsubscribeCleanup::Trash, "Alle in den Papierkorb" }
                    }
                }
                PrefToggle { label: "Beim Öffnen als gelesen markieren", checked: p.mark_read_on_open,
                    onchange: { let p = p.clone(); move |v| save(Preferences { mark_read_on_open: v, ..p.clone() }) } }
                PrefToggle { label: "Externe Bilder automatisch laden", hint: "Aus: Absender erfahren nicht, wann du eine Nachricht öffnest.", checked: p.load_remote_images,
                    onchange: { let p = p.clone(); move |v| save(Preferences { load_remote_images: v, ..p.clone() }) } }
                Field { label: "Senden rückgängig machen", class: "max-w-sm",
                    hint: "Nachrichten warten so lange intern, bevor sie wirklich rausgehen – bis dahin lassen sie sich zurückziehen.",
                    select { class: SELECT,
                        onchange: { let p = p.clone(); move |e: FormEvent| { let v = e.value().parse().unwrap_or(nrzmail_api::UNDO_SEND_DEFAULT); save(Preferences { undo_send_seconds: v, ..p.clone() }) } },
                        for (secs , label) in UNDO_CHOICES {
                            option { value: "{secs}", selected: p.undo_send_seconds == secs, "{label}" }
                        }
                        // Eigener Wert (z. B. über die API gesetzt)
                        if !UNDO_CHOICES.iter().any(|(s, _)| *s == p.undo_send_seconds) {
                            option { value: "{p.undo_send_seconds}", selected: true, "{p.undo_send_seconds} Sekunden" }
                        }
                    }
                }
                Field { label: "Postfach für neue Nachrichten", class: "max-w-sm",
                    select { class: SELECT,
                        onchange: { let p = p.clone(); move |e: FormEvent| { let v = e.value(); save(Preferences { default_account: (!v.is_empty()).then_some(v), ..p.clone() }) } },
                        option { value: "", selected: p.default_account.is_none(), "Automatisch (erstes Postfach)" }
                        for a in accounts.iter().filter(|a| a.kind == nrzmail_api::AccountKind::Imap) {
                            option { value: "{a.id}", selected: p.default_account.as_deref() == Some(a.id.as_str()), "{a.label()} – {a.email}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn PrefToggle(#[props(into)] label: String, #[props(into, optional)] hint: Option<String>, checked: bool, onchange: EventHandler<bool>) -> Element {
    rsx! {
        div { class: "flex items-start justify-between gap-6",
            div {
                p { class: "text-sm font-medium", "{label}" }
                if let Some(h) = hint { p { class: "mt-0.5 text-xs text-muted-foreground", "{h}" } }
            }
            Toggle { checked, onchange: move |v| onchange.call(v) }
        }
    }
}

#[component]
fn Password() -> Element {
    let mut current = use_signal(String::new);
    let mut new = use_signal(String::new);
    let mut repeat = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let save = move |e: FormEvent| {
        e.prevent_default();
        if new() != repeat() {
            return toast_error("Die neuen Passwörter stimmen nicht überein.");
        }
        if new().chars().count() < 8 {
            return toast_error("Das neue Passwort braucht mindestens 8 Zeichen.");
        }
        spawn(async move {
            busy.set(true);
            match api::put::<serde::de::IgnoredAny, _>("/me/password", &ChangePassword { current: current(), new: new() }).await {
                Ok(_) => {
                    toast("Passwort geändert. Andere Sitzungen wurden abgemeldet.");
                    current.set(String::new());
                    new.set(String::new());
                    repeat.set(String::new());
                }
                Err(e) => toast_error(e),
            }
            busy.set(false);
        });
    };
    rsx! {
        Section { title: "Passwort", description: "Nach der Änderung bleiben nur diese Sitzung und dieses Gerät angemeldet.",
            form { class: "grid max-w-xl gap-4", onsubmit: save,
                Field { label: "Aktuelles Passwort",
                    input { class: INPUT, r#type: "password", autocomplete: "current-password", required: true, value: "{current}", oninput: move |e| current.set(e.value()) }
                }
                Field { label: "Neues Passwort", hint: "Mindestens 8 Zeichen",
                    input { class: INPUT, r#type: "password", autocomplete: "new-password", required: true, minlength: 8, value: "{new}", oninput: move |e| new.set(e.value()) }
                }
                Field { label: "Neues Passwort wiederholen",
                    input { class: INPUT, r#type: "password", autocomplete: "new-password", required: true, value: "{repeat}", oninput: move |e| repeat.set(e.value()) }
                }
                div { button { class: btn(BTN_PRIMARY), r#type: "submit", disabled: busy(), "Passwort ändern" } }
            }
        }
    }
}
