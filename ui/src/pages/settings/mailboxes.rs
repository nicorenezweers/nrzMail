//! Postfächer: Liste mit Status (letzter Abgleich, Fehler), jetzt abgleichen, hinzufügen und
//! bearbeiten mit Anbieter-Voreinstellungen, Verbindung testen, Aufbewahrung, Ordner-Rollen
//! und Abgleich je Ordner, löschen. Art „Nur Archiv“ für Postfächer, die nur Importe enthalten.

use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::pages::layout::folder_label;
use crate::state::{self, ACCOUNTS, FOLDERS};
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{
    AccountKind, AccountPreset, ConnectionTest, Folder, FolderRole, FolderUpdate, MailAccount, MailAccountInput, Security,
    account_presets, preset_for_email,
};

/// Ruhige Farben für den Punkt in der Seitenleiste.
const COLORS: [&str; 10] = ["#a3a3a3", "#e5e5e5", "#737373", "#93c5fd", "#86efac", "#fde68a", "#fca5a5", "#d8b4fe", "#f9a8d4", "#99f6e4"];

#[component]
pub fn Mailboxes() -> Element {
    let accounts = ACCOUNTS();
    let loaded = *state::ACCOUNTS_LOADED.read();
    let mut dialog = use_signal(|| false);
    let mut editing = use_signal(|| None::<MailAccount>);
    let mut start_tab = use_signal(|| "verbindung");
    // Während ein Abgleich läuft, öfter nachsehen
    use_future(move || async move {
        loop {
            let syncing = ACCOUNTS.peek().iter().any(|a| a.syncing);
            gloo_timers::future::TimeoutFuture::new(if syncing { 3_000 } else { 15_000 }).await;
            state::load_accounts().await;
        }
    });
    rsx! {
        Section { title: "Postfächer", description: "IMAP zum Empfangen, SMTP zum Senden. Alles, was nrzMail abholt, bleibt im lokalen Archiv – auch wenn es auf dem Server gelöscht wird.",
            actions: rsx! {
                button { class: btn(BTN_PRIMARY), onclick: move |_| { editing.set(None); start_tab.set("verbindung"); dialog.set(true); },
                    icons::Plus { class: "size-4" } "Postfach hinzufügen"
                }
            },
            if !loaded {
                Loading {}
            } else if accounts.is_empty() {
                EmptyState { title: "Noch kein Postfach", description: "Füge dein erstes Postfach hinzu – für bekannte Anbieter sind Server und Ports schon eingetragen.",
                    icon: rsx! { icons::Inbox {} },
                    button { class: btn(BTN_PRIMARY), onclick: move |_| { editing.set(None); dialog.set(true); }, icons::Plus { class: "size-4" } "Postfach hinzufügen" }
                }
            } else {
                div { class: "grid gap-3",
                    for a in accounts {
                        AccountCard { key: "{a.id}", account: a.clone(),
                            on_edit: move |(a, tab): (MailAccount, &'static str)| { editing.set(Some(a)); start_tab.set(tab); dialog.set(true); },
                        }
                    }
                }
            }
        }
        Modal { open: dialog, title: if editing().is_some() { "Postfach bearbeiten" } else { "Postfach hinzufügen" }, class: "max-w-2xl",
            AccountForm { key: "{editing().map(|a| a.id).unwrap_or_default()}", account: editing(), tab: start_tab(), open: dialog }
        }
    }
}

#[component]
fn AccountCard(account: MailAccount, on_edit: EventHandler<(MailAccount, &'static str)>) -> Element {
    let a = account;
    let error = a.last_error.clone().filter(|e| !e.trim().is_empty());
    let id = a.id.clone();
    let id_del = a.id.clone();
    let label = a.label().to_string();
    let (e1, e2) = (a.clone(), a.clone());
    rsx! {
        div { class: "rounded-xl border bg-background/40 p-4",
            div { class: "flex flex-wrap items-start gap-3",
                Dot { color: a.color.clone(), class: "mt-1.5 size-2.5" }
                div { class: "min-w-0 flex-1 basis-64",
                    div { class: "flex flex-wrap items-center gap-2",
                        p { class: "font-semibold", "{label}" }
                        Pill { if a.kind == AccountKind::Import { "Nur Archiv" } else { "IMAP" } }
                        if !a.sync_enabled && a.kind == AccountKind::Imap { Pill { "Abgleich aus" } }
                    }
                    p { class: "truncate text-sm text-muted-foreground", "{a.email}" }
                    div { class: "mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground",
                        span { {util::plural(a.message_count, "Nachricht", "Nachrichten")} }
                        span { "{util::num(a.unread_count)} ungelesen" }
                        span { {util::size(a.size_bytes)} }
                        if a.syncing {
                            span { class: "flex items-center gap-1 text-foreground", icons::RefreshCw { class: "size-3 animate-spin" } "Abgleich läuft …" }
                        } else if let Some(t) = a.last_sync_at {
                            span { title: "{util::datetime(t)}", "Abgeglichen {util::ago(t)}" }
                        } else if a.kind == AccountKind::Imap {
                            span { "Noch nie abgeglichen" }
                        }
                    }
                    if let Some(e) = error {
                        p { class: "mt-2 flex items-start gap-1.5 rounded-md border border-destructive/40 bg-destructive/10 px-2.5 py-1.5 text-xs",
                            icons::CircleAlert { class: "mt-px size-3.5 shrink-0 text-destructive" }
                            span { class: "break-words", "{e}" }
                        }
                    }
                }
                div { class: "flex items-center gap-1.5",
                    if a.kind == AccountKind::Imap {
                        button { class: "{btn(BTN_OUTLINE)} {BTN_SM}", disabled: a.syncing,
                            onclick: move |_| {
                                let id = id.clone();
                                spawn(async move {
                                    match api::action(&format!("/accounts/{}/sync", api::enc(&id))).await {
                                        Ok(()) => toast("Abgleich gestartet."),
                                        Err(e) => toast_error(e),
                                    }
                                    state::load_accounts().await;
                                });
                            },
                            icons::RefreshCw { class: "size-3.5" } "Jetzt abgleichen"
                        }
                    }
                    button { class: "{btn(BTN_OUTLINE)} {BTN_SM}", onclick: move |_| on_edit.call((e1.clone(), "verbindung")), icons::Pencil { class: "size-3.5" } "Bearbeiten" }
                    Dropdown { align: Align::End, class: "w-56",
                        trigger_class: "{btn(BTN_GHOST)} size-8 p-0",
                        trigger: rsx! { span { "aria-label": "Mehr", icons::EllipsisVertical { class: "size-4" } } },
                        if a.kind == AccountKind::Imap {
                            MenuItem { onclick: move |_| on_edit.call((e2.clone(), "ordner")), icons::FolderTree {} "Ordner & Abgleich" }
                            MenuSeparator {}
                        }
                        MenuItem { destructive: true,
                            onclick: move |_| {
                                let (id, label) = (id_del.clone(), label.clone());
                                spawn(async move {
                                    let ask = Ask::new(format!("Postfach „{label}“ löschen?"), "Das lokale Archiv dieses Postfachs wird mitgelöscht. Auf dem Mailserver bleibt alles, wie es ist.")
                                        .confirm("Postfach löschen").destructive();
                                    if confirm(ask).await {
                                        match api::delete(&format!("/accounts/{}", api::enc(&id))).await {
                                            Ok(()) => { toast("Postfach gelöscht."); state::load_accounts().await; state::refresh_list(); }
                                            Err(e) => toast_error(e),
                                        }
                                    }
                                });
                            },
                            icons::Trash2 {} "Löschen"
                        }
                    }
                }
            }
        }
    }
}

fn input_of(a: &MailAccount) -> MailAccountInput {
    MailAccountInput {
        kind: a.kind,
        name: a.name.clone(),
        email: a.email.clone(),
        display_name: a.display_name.clone(),
        color: a.color.clone(),
        signature: a.signature.clone(),
        imap_host: a.imap_host.clone(),
        imap_port: a.imap_port,
        imap_security: a.imap_security,
        imap_username: a.imap_username.clone(),
        imap_password: None,
        smtp_host: a.smtp_host.clone(),
        smtp_port: a.smtp_port,
        smtp_security: a.smtp_security,
        smtp_username: a.smtp_username.clone(),
        smtp_password: None,
        sync_enabled: a.sync_enabled,
        sync_interval_minutes: a.sync_interval_minutes,
        delete_from_server_after_days: a.delete_from_server_after_days,
        archive_retention_days: a.archive_retention_days,
    }
}

fn apply_preset(f: &mut MailAccountInput, p: &AccountPreset) {
    f.imap_host = p.imap_host.to_string();
    f.imap_port = p.imap_port;
    f.imap_security = p.imap_security;
    f.smtp_host = p.smtp_host.to_string();
    f.smtp_port = p.smtp_port;
    f.smtp_security = p.smtp_security;
}

fn security_label(s: Security) -> &'static str {
    match s {
        Security::Tls => "TLS (SSL)",
        Security::Starttls => "STARTTLS",
        Security::None => "Unverschlüsselt",
    }
}

/// Übliche Ports je Verschlüsselung: (IMAP, SMTP).
fn default_ports(s: Security) -> (u16, u16) {
    match s {
        Security::Tls => (993, 465),
        Security::Starttls => (143, 587),
        Security::None => (143, 25),
    }
}

#[component]
fn AccountForm(account: Option<MailAccount>, tab: &'static str, open: Signal<bool>) -> Element {
    let mut open = open;
    let editing_id = account.as_ref().map(|a| a.id.clone());
    let is_new = editing_id.is_none();
    let mut form = use_signal(|| account.as_ref().map(input_of).unwrap_or_default());
    let imap_pw = use_signal(String::new);
    let smtp_pw = use_signal(String::new);
    let mut same_pw = use_signal(|| is_new);
    let mut preset = use_signal(|| account.as_ref().and_then(|a| preset_for_email(&a.email)).map(|p| p.id.to_string()).unwrap_or_default());
    let mut test = use_signal(|| None::<ConnectionTest>);
    let mut testing = use_signal(|| false);
    let mut saving = use_signal(|| false);
    let mut current_tab = use_signal(|| tab);
    let presets = account_presets();
    let note = presets.iter().find(|p| p.id == preset()).map(|p| p.note).filter(|n| !n.is_empty());
    let f = form();
    let is_import = f.kind == AccountKind::Import;
    let hints = account.as_ref().map(|a| (a.imap_password_hint.clone(), a.smtp_password_hint.clone())).unwrap_or_default();

    // Eingaben samt Passwörtern für Test und Speichern
    let collect = move || -> MailAccountInput {
        let mut x = form();
        x.email = x.email.trim().to_string();
        if x.imap_username.trim().is_empty() {
            x.imap_username = x.email.clone();
        }
        if x.smtp_username.trim().is_empty() {
            x.smtp_username = x.imap_username.clone();
        }
        let ip = imap_pw();
        x.imap_password = (!ip.is_empty()).then_some(ip.clone());
        let sp = if same_pw() { ip } else { smtp_pw() };
        x.smtp_password = (!sp.is_empty()).then_some(sp);
        x
    };
    let id_test = editing_id.clone();
    let run_test = move |_| {
        let body = collect();
        let path = match &id_test {
            Some(id) => format!("/accounts/test?account={}", api::enc(id)),
            None => "/accounts/test".to_string(),
        };
        spawn(async move {
            testing.set(true);
            test.set(None);
            match api::post::<ConnectionTest, _>(&path, &body).await {
                Ok(t) => test.set(Some(t)),
                Err(e) => toast_error(e),
            }
            testing.set(false);
        });
    };
    let id_save = editing_id.clone();
    let save = move |e: FormEvent| {
        e.prevent_default();
        let body = collect();
        if body.email.is_empty() || !body.email.contains('@') {
            return toast_error("Bitte eine gültige E-Mail-Adresse angeben.");
        }
        if body.kind == AccountKind::Imap && (body.imap_host.trim().is_empty() || body.smtp_host.trim().is_empty()) {
            return toast_error("Bitte IMAP- und SMTP-Server angeben.");
        }
        let id = id_save.clone();
        spawn(async move {
            saving.set(true);
            let res = match &id {
                Some(id) => api::put::<MailAccount, _>(&format!("/accounts/{}", api::enc(id)), &body).await,
                None => api::post::<MailAccount, _>("/accounts", &body).await,
            };
            saving.set(false);
            match res {
                Ok(_) => {
                    toast(if id.is_some() { "Postfach gespeichert." } else { "Postfach hinzugefügt – der erste Abgleich läuft." });
                    open.set(false);
                    state::load_accounts().await;
                    state::refresh_list();
                }
                Err(e) => toast_error(e),
            }
        });
    };

    let tabs: Vec<(&'static str, &'static str)> = if is_new || is_import {
        vec![("verbindung", "Verbindung"), ("aufbewahrung", "Aufbewahrung & Signatur")]
    } else {
        vec![("verbindung", "Verbindung"), ("aufbewahrung", "Aufbewahrung & Signatur"), ("ordner", "Ordner")]
    };

    rsx! {
        div { class: "mb-5 flex gap-1 border-b", role: "tablist",
            for (key , label) in tabs {
                button { r#type: "button", role: "tab", "aria-selected": "{current_tab() == key}",
                    class: if current_tab() == key { "-mb-px border-b-2 border-foreground px-3 py-2 text-sm font-medium" } else { "-mb-px border-b-2 border-transparent px-3 py-2 text-sm text-muted-foreground hover:text-foreground" },
                    onclick: move |_| current_tab.set(key),
                    "{label}"
                }
            }
        }
        if current_tab() == "ordner" {
            if let Some(id) = editing_id.clone() {
                FolderSettings { account_id: id }
            }
            div { class: "mt-6 flex justify-end",
                button { class: btn(BTN_OUTLINE), onclick: move |_| open.set(false), "Schliessen" }
            }
        } else {
            form { class: "grid gap-5", onsubmit: save,
                if current_tab() == "verbindung" {
                    // Art
                    div { class: "grid gap-2 sm:grid-cols-2",
                        for (kind , title , text) in [
                            (AccountKind::Imap, "IMAP & SMTP", "Empfangen und Senden über einen Mailserver."),
                            (AccountKind::Import, "Nur Archiv", "Kein Server – Nachrichten kommen per Import (mbox, eml)."),
                        ] {
                            button { r#type: "button",
                                class: if f.kind == kind { "rounded-lg border border-foreground/60 bg-accent/50 p-3 text-left" } else { "rounded-lg border p-3 text-left hover:bg-accent/30" },
                                "aria-pressed": "{f.kind == kind}",
                                disabled: !is_new && f.kind != kind,
                                onclick: move |_| form.write().kind = kind,
                                p { class: "text-sm font-medium", "{title}" }
                                p { class: "mt-0.5 text-xs text-muted-foreground", "{text}" }
                            }
                        }
                    }
                    div { class: "grid gap-4 sm:grid-cols-2",
                        Field { label: "E-Mail-Adresse",
                            input { class: INPUT, r#type: "email", required: true, autocomplete: "email", value: "{f.email}",
                                oninput: move |e| form.write().email = e.value(),
                                onblur: move |_| {
                                    // Anbieter an der Domain erkennen und Server eintragen
                                    let email = form.peek().email.clone();
                                    if let Some(p) = preset_for_email(&email) {
                                        if form.peek().imap_host.is_empty() || preset.peek().is_empty() {
                                            apply_preset(&mut form.write(), &p);
                                            preset.set(p.id.to_string());
                                        }
                                    } else if let Some((_, domain)) = email.rsplit_once('@') {
                                        let mut w = form.write();
                                        if w.imap_host.is_empty() && domain.contains('.') {
                                            w.imap_host = format!("imap.{domain}");
                                            w.smtp_host = format!("smtp.{domain}");
                                        }
                                    }
                                    let mut w = form.write();
                                    if w.name.is_empty() {
                                        w.name = email.rsplit_once('@').map(|(_, d)| d.split('.').next().unwrap_or(d).to_string()).map(|s| {
                                            let mut c = s.chars();
                                            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                                        }).unwrap_or_default();
                                    }
                                },
                            }
                        }
                        Field { label: "Name in der Seitenleiste", hint: "z. B. Privat oder Arbeit",
                            input { class: INPUT, value: "{f.name}", oninput: move |e| form.write().name = e.value() }
                        }
                        Field { label: "Absendername", hint: "So sehen Empfänger dich",
                            input { class: INPUT, autocomplete: "name", value: "{f.display_name}", oninput: move |e| form.write().display_name = e.value() }
                        }
                        div { class: "grid gap-1.5",
                            span { class: "text-sm font-medium leading-none", "Farbe" }
                            div { class: "flex flex-wrap items-center gap-1.5",
                                for c in COLORS {
                                    button { r#type: "button", title: "{c}", "aria-label": "Farbe {c}",
                                        class: if f.color.eq_ignore_ascii_case(c) || (f.color.is_empty() && c == COLORS[0]) { "size-6 rounded-full ring-2 ring-foreground ring-offset-2 ring-offset-background" } else { "size-6 rounded-full" },
                                        style: "background: {c}",
                                        onclick: move |_| form.write().color = c.to_string(),
                                    }
                                }
                                input { r#type: "color", class: "size-7 cursor-pointer rounded border-0 bg-transparent p-0", "aria-label": "Eigene Farbe",
                                    value: if f.color.starts_with('#') && f.color.len() == 7 { f.color.clone() } else { "#a3a3a3".to_string() },
                                    oninput: move |e| form.write().color = e.value() }
                            }
                        }
                    }
                    if !is_import {
                        Field { label: "Anbieter", hint: "Trägt Server und Ports ein.",
                            select { class: SELECT, value: "{preset}",
                                onchange: move |e| {
                                    let id = e.value();
                                    if let Some(p) = account_presets().into_iter().find(|p| p.id == id) {
                                        apply_preset(&mut form.write(), &p);
                                    }
                                    preset.set(id);
                                },
                                option { value: "", selected: preset().is_empty(), "Eigener Server" }
                                for p in presets.iter() {
                                    option { value: "{p.id}", selected: preset() == p.id, "{p.name}" }
                                }
                            }
                        }
                        if let Some(n) = note {
                            Callout { text: n }
                        }
                        ServerFields { title: "Empfangen (IMAP)", form, imap: true, password: imap_pw, hint: hints.0.clone() }
                        div { class: "grid gap-3",
                            ServerFields { title: "Senden (SMTP)", form, imap: false, password: smtp_pw, hint: hints.1.clone(), hide_password: same_pw() }
                            Checkbox { checked: same_pw(), onchange: move |v| same_pw.set(v), label: "Gleiches Passwort wie für IMAP" }
                        }
                        div { class: "grid gap-4 sm:grid-cols-2",
                            div { class: "flex items-center", Toggle { checked: f.sync_enabled, onchange: move |v| form.write().sync_enabled = v, label: "Automatisch abgleichen" } }
                            Field { label: "Alle … Minuten",
                                input { class: INPUT, r#type: "number", min: 1, max: 1440, value: "{f.sync_interval_minutes}", disabled: !f.sync_enabled,
                                    oninput: move |e| form.write().sync_interval_minutes = e.value().parse().unwrap_or(5).clamp(1, 1440) }
                            }
                        }
                        // Test
                        div { class: "grid gap-3 rounded-lg border p-3",
                            div { class: "flex flex-wrap items-center gap-3",
                                button { r#type: "button", class: "{btn(BTN_OUTLINE)} {BTN_SM}", disabled: testing(), onclick: run_test,
                                    if testing() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::FlaskConical { class: "size-4" } }
                                    "Verbindung testen"
                                }
                                span { class: "text-xs text-muted-foreground", "Meldet sich bei IMAP und SMTP an, ohne etwas zu speichern." }
                            }
                            if let Some(t) = test() {
                                div { class: "grid gap-1.5 text-sm",
                                    TestLine { ok: t.imap_ok, label: "IMAP", message: t.imap_message.clone() }
                                    TestLine { ok: t.smtp_ok, label: "SMTP", message: t.smtp_message.clone() }
                                    if !t.folders.is_empty() {
                                        p { class: "text-xs text-muted-foreground", {format!("{} Ordner gefunden: {}", t.folders.len(), t.folders.join(", "))} }
                                    }
                                }
                            }
                        }
                    }
                }
                if current_tab() == "aufbewahrung" {
                    if !is_import {
                        Field { label: "Auf dem Server löschen nach", hint: "Nachrichten verschwinden nach so vielen Tagen vom Mailserver – im lokalen Archiv bleiben sie.",
                            DaysSelect { value: f.delete_from_server_after_days, never: "Nie (auf dem Server lassen)",
                                options: vec![30, 90, 180, 365, 730], onchange: move |v| form.write().delete_from_server_after_days = v }
                        }
                    }
                    Field { label: "Im lokalen Archiv behalten", hint: "Ältere Nachrichten werden auch aus dem Archiv gelöscht.",
                        DaysSelect { value: f.archive_retention_days, never: "Für immer",
                            options: vec![365, 730, 1825, 3650], onchange: move |v| form.write().archive_retention_days = v }
                    }
                    if !is_import {
                        Field { label: "Signatur", hint: "Steht automatisch unter neuen Nachrichten und Antworten.",
                            textarea { class: TEXTAREA, rows: 5, value: "{f.signature}", placeholder: "Freundliche Grüsse\nAnna Muster",
                                oninput: move |e| form.write().signature = e.value() }
                        }
                    }
                }
                div { class: "flex justify-end gap-2 border-t pt-4",
                    button { r#type: "button", class: btn(BTN_OUTLINE), onclick: move |_| open.set(false), "Abbrechen" }
                    button { r#type: "submit", class: btn(BTN_PRIMARY), disabled: saving(),
                        if saving() { icons::LoaderCircle { class: "size-4 animate-spin" } }
                        if is_new { "Hinzufügen" } else { "Speichern" }
                    }
                }
            }
        }
    }
}

#[component]
fn TestLine(ok: bool, label: &'static str, #[props(into)] message: String) -> Element {
    rsx! {
        p { class: "flex items-start gap-2",
            if ok { icons::CircleCheck { class: "mt-0.5 size-4 shrink-0 text-success" } } else { icons::CircleX { class: "mt-0.5 size-4 shrink-0 text-destructive" } }
            span { class: "font-medium", "{label}" }
            span { class: "break-words text-muted-foreground", if message.is_empty() { if ok { "verbunden" } else { "fehlgeschlagen" } } else { "{message}" } }
        }
    }
}

#[component]
fn ServerFields(
    title: &'static str,
    form: Signal<MailAccountInput>,
    imap: bool,
    password: Signal<String>,
    #[props(into)] hint: String,
    #[props(default)] hide_password: bool,
) -> Element {
    let mut form = form;
    let mut password = password;
    let f = form();
    let (host, port, security, user) = if imap {
        (f.imap_host.clone(), f.imap_port, f.imap_security, f.imap_username.clone())
    } else {
        (f.smtp_host.clone(), f.smtp_port, f.smtp_security, f.smtp_username.clone())
    };
    let user_placeholder = if imap || f.imap_username.is_empty() { f.email.clone() } else { f.imap_username.clone() };
    let pw_placeholder = if hint.is_empty() { "Passwort".to_string() } else { format!("Unverändert ({hint})") };
    rsx! {
        fieldset { class: "grid gap-3 rounded-lg border p-3",
            legend { class: "px-1 text-sm font-medium", "{title}" }
            div { class: "grid gap-3 sm:grid-cols-[minmax(0,1fr)_6rem_9rem]",
                Field { label: "Server",
                    input { class: INPUT, value: "{host}", placeholder: if imap { "imap.example.com" } else { "smtp.example.com" }, autocomplete: "off",
                        oninput: move |e| { let mut w = form.write(); if imap { w.imap_host = e.value() } else { w.smtp_host = e.value() } } }
                }
                Field { label: "Port",
                    input { class: INPUT, r#type: "number", min: 1, max: 65535, value: "{port}",
                        oninput: move |e| { let v = e.value().parse().unwrap_or(0); let mut w = form.write(); if imap { w.imap_port = v } else { w.smtp_port = v } } }
                }
                Field { label: "Verschlüsselung",
                    select { class: SELECT,
                        onchange: move |e| {
                            let Some(s) = Security::parse(&e.value()) else { return };
                            let mut w = form.write();
                            let (old, new) = if imap { (w.imap_security, s) } else { (w.smtp_security, s) };
                            let (oi, os) = default_ports(old);
                            let (ni, ns) = default_ports(new);
                            if imap {
                                w.imap_security = s;
                                if w.imap_port == oi || w.imap_port == 0 { w.imap_port = ni; }
                            } else {
                                w.smtp_security = s;
                                if w.smtp_port == os || w.smtp_port == 0 { w.smtp_port = ns; }
                            }
                        },
                        for s in [Security::Tls, Security::Starttls, Security::None] {
                            option { value: "{s.as_str()}", selected: s == security, "{security_label(s)}" }
                        }
                    }
                }
            }
            div { class: "grid gap-3 sm:grid-cols-2",
                Field { label: "Benutzername",
                    input { class: INPUT, value: "{user}", placeholder: "{user_placeholder}", autocomplete: "off",
                        oninput: move |e| { let mut w = form.write(); if imap { w.imap_username = e.value() } else { w.smtp_username = e.value() } } }
                }
                if !hide_password {
                    Field { label: "Passwort",
                        input { class: INPUT, r#type: "password", autocomplete: "new-password", value: "{password}", placeholder: "{pw_placeholder}",
                            oninput: move |e| password.set(e.value()) }
                    }
                }
            }
            if security == Security::None {
                p { class: "text-xs text-destructive", "Unverschlüsselt – Passwort und Nachrichten gehen im Klartext über das Netz. Nur im eigenen Netz verwenden." }
            }
        }
    }
}

/// Auswahl „nie“ / feste Werte / eigene Anzahl Tage.
#[component]
fn DaysSelect(value: Option<u32>, never: &'static str, options: Vec<u32>, onchange: EventHandler<Option<u32>>) -> Element {
    let custom = value.is_some_and(|v| !options.contains(&v));
    let label = |d: u32| match d {
        365 => "1 Jahr".to_string(),
        730 => "2 Jahren".to_string(),
        1825 => "5 Jahren".to_string(),
        3650 => "10 Jahren".to_string(),
        d => format!("{d} Tagen"),
    };
    rsx! {
        div { class: "flex flex-wrap items-center gap-2",
            select { class: "{SELECT_SM} h-9 min-w-48 text-sm",
                onchange: move |e| match e.value().as_str() {
                    "" => onchange.call(None),
                    "custom" => onchange.call(Some(value.unwrap_or(60))),
                    v => onchange.call(v.parse().ok()),
                },
                option { value: "", selected: value.is_none(), "{never}" }
                for d in options.iter().copied() {
                    option { value: "{d}", selected: value == Some(d), {label(d)} }
                }
                option { value: "custom", selected: custom, "Eigene Anzahl Tage …" }
            }
            if custom {
                input { class: "{INPUT} w-28", r#type: "number", min: 1, value: "{value.unwrap_or(60)}", "aria-label": "Tage",
                    oninput: move |e| onchange.call(e.value().parse::<u32>().ok().filter(|d| *d > 0)) }
                span { class: "text-sm text-muted-foreground", "Tage" }
            }
        }
    }
}

/// Ordner eines Postfachs: Rolle und Abgleich je Ordner.
#[component]
fn FolderSettings(account_id: String) -> Element {
    let folders: Vec<Folder> = FOLDERS.read().iter().filter(|f| f.account_id == account_id).cloned().collect();
    let update = move |(id, body): (String, FolderUpdate)| {
        spawn(async move {
            match api::put::<Folder, _>(&format!("/folders/{}", api::enc(&id)), &body).await {
                Ok(f) => {
                    if let Some(x) = FOLDERS.write().iter_mut().find(|x| x.id == f.id) {
                        *x = f;
                    }
                    toast("Ordner gespeichert.");
                }
                Err(e) => toast_error(e),
            }
        });
    };
    if folders.is_empty() {
        return rsx! { Callout { text: "Noch keine Ordner – sie erscheinen nach dem ersten Abgleich." } };
    }
    rsx! {
        p { class: "mb-3 text-sm text-muted-foreground", "Die Rolle bestimmt, wohin Archivieren, Löschen und Spam verschieben und welche Ordner in den vereinheitlichten Ansichten erscheinen." }
        div { class: "divide-y rounded-lg border",
            for f in folders {
                div { key: "{f.id}", class: "flex flex-wrap items-center gap-3 px-3 py-2",
                    div { class: "min-w-0 flex-1", style: "padding-left: {f.depth * 12}px",
                        p { class: "truncate text-sm font-medium", {folder_label(&f)} }
                        p { class: "truncate text-xs text-muted-foreground", "{f.path} · {util::num(f.total)} Nachrichten" }
                    }
                    select { class: "{SELECT_SM} w-40", "aria-label": "Rolle von {f.name}",
                        onchange: { let id = f.id.clone(); move |e: FormEvent| if let Some(r) = FolderRole::parse(&e.value()) { update((id.clone(), FolderUpdate { role: Some(r), sync_enabled: None })) } },
                        for r in FolderRole::ALL {
                            option { value: "{r.as_str()}", selected: r == f.role, if r == FolderRole::Other { "Normaler Ordner" } else { "{r.label()}" } }
                        }
                    }
                    Toggle { checked: f.sync_enabled, label: "Abgleichen",
                        onchange: { let id = f.id.clone(); move |v: bool| update((id.clone(), FolderUpdate { role: None, sync_enabled: Some(v) })) } }
                }
            }
        }
    }
}
