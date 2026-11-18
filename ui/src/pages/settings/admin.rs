//! Verwaltung (nur Administratoren): Benutzer anlegen, Admin-Recht, sperren, Passwort setzen,
//! löschen; Registrierung offen/zu; Zugriffsprotokoll.

use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::state::ME;
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{ActivityEntry, InstanceSettings, NewUser, UpdateUser, User};

#[component]
pub fn Users() -> Element {
    let mut users = use_signal(|| None::<Vec<User>>);
    let mut reload = use_signal(|| 0u32);
    let mut create_open = use_signal(|| false);
    let mut password_for = use_signal(|| None::<User>);
    let mut password_open = use_signal(|| false);
    use_effect(move || {
        let _ = reload();
        spawn(async move {
            match api::get::<Vec<User>>("/admin/users").await {
                Ok(u) => users.set(Some(u)),
                Err(e) => toast_error(e),
            }
        });
    });
    let me_id = ME.read().as_ref().map(|u| u.id.clone()).unwrap_or_default();
    let update = move |(id, body, done): (String, UpdateUser, &'static str)| {
        spawn(async move {
            match api::put::<User, _>(&format!("/admin/users/{}", api::enc(&id)), &body).await {
                Ok(u) => {
                    if let Some(list) = users.write().as_mut()
                        && let Some(x) = list.iter_mut().find(|x| x.id == u.id) {
                            *x = u;
                        }
                    toast(done);
                }
                Err(e) => toast_error(e),
            }
        });
    };
    rsx! {
        Section { title: "Benutzer", description: "Jeder Benutzer hat eigene Postfächer und ein eigenes Archiv – niemand sieht die Mails der anderen.",
            actions: rsx! {
                button { class: btn(BTN_PRIMARY), onclick: move |_| create_open.set(true), icons::UserPlus { class: "size-4" } "Benutzer anlegen" }
            },
            match users() {
                None => rsx! { Loading {} },
                Some(list) => rsx! {
                    div { class: "divide-y rounded-lg border",
                        for u in list {
                            UserRow { key: "{u.id}", is_me: u.id == me_id, user: u,
                                on_update: move |x: (String, UpdateUser, &'static str)| update(x),
                                on_password: move |u: User| { password_for.set(Some(u)); password_open.set(true); },
                                on_deleted: move |_| reload += 1,
                            }
                        }
                    }
                },
            }
        }
        Modal { open: create_open, title: "Benutzer anlegen",
            NewUserForm { open: create_open, on_created: move |_| reload += 1 }
        }
        Modal { open: password_open, title: "Passwort setzen", description: "Der Benutzer wird überall abgemeldet und meldet sich mit dem neuen Passwort an.",
            if let Some(u) = password_for() {
                SetPasswordForm { key: "{u.id}", user: u, open: password_open, on_update: move |x: (String, UpdateUser, &'static str)| update(x) }
            }
        }
    }
}

#[component]
fn UserRow(user: User, is_me: bool, on_update: EventHandler<(String, UpdateUser, &'static str)>, on_password: EventHandler<User>, on_deleted: EventHandler<()>) -> Element {
    let u = user;
    let (id1, id2, id3) = (u.id.clone(), u.id.clone(), u.id.clone());
    let pw_user = u.clone();
    let label = u.label().to_string();
    rsx! {
        div { class: "flex flex-wrap items-center gap-3 px-4 py-3",
            Avatar { name: label.clone() }
            div { class: "min-w-0 flex-1",
                p { class: "flex flex-wrap items-center gap-2 font-medium",
                    "{label}"
                    if is_me { Pill { "Du" } }
                    if u.is_admin { Pill { "Administrator" } }
                    if u.disabled { Pill { class: "border-destructive/40 text-destructive", "Gesperrt" } }
                }
                p { class: "truncate text-xs text-muted-foreground",
                    "{u.username}"
                    if !u.email.is_empty() { " · {u.email}" }
                    " · seit {util::date(u.created_at)}"
                    if let Some(t) = u.last_login_at { " · zuletzt {util::datetime(t)}" }
                }
            }
            Dropdown { align: Align::End, class: "w-56",
                trigger_class: "{btn(BTN_OUTLINE)} {BTN_SM}",
                trigger: rsx! { "Verwalten" icons::ChevronDown { class: "size-3.5" } },
                if !is_me {
                    MenuItem { onclick: move |_| on_update.call((id1.clone(), UpdateUser { is_admin: Some(!u.is_admin), ..Default::default() }, if u.is_admin { "Admin-Recht entzogen." } else { "Ist jetzt Administrator." })),
                        icons::ShieldCheck {} if u.is_admin { "Admin-Recht entziehen" } else { "Zum Administrator machen" }
                    }
                    MenuItem { onclick: move |_| on_update.call((id2.clone(), UpdateUser { disabled: Some(!u.disabled), ..Default::default() }, if u.disabled { "Benutzer entsperrt." } else { "Benutzer gesperrt." })),
                        icons::Ban {} if u.disabled { "Entsperren" } else { "Sperren" }
                    }
                }
                MenuItem { onclick: move |_| on_password.call(pw_user.clone()), icons::KeyRound {} "Passwort setzen …" }
                if !is_me {
                    MenuSeparator {}
                    MenuItem { destructive: true,
                        onclick: move |_| {
                            let (id, label) = (id3.clone(), label.clone());
                            spawn(async move {
                                let ask = Ask::new(format!("„{label}“ löschen?"), "Alle Postfächer und das ganze Archiv dieses Benutzers werden gelöscht.").confirm("Benutzer löschen").destructive();
                                if confirm(ask).await {
                                    match api::delete(&format!("/admin/users/{}", api::enc(&id))).await {
                                        Ok(()) => { toast("Benutzer gelöscht."); on_deleted.call(()); }
                                        Err(e) => toast_error(e),
                                    }
                                }
                            });
                        },
                        icons::UserX {} "Löschen"
                    }
                }
            }
        }
    }
}

#[component]
fn NewUserForm(open: Signal<bool>, on_created: EventHandler<()>) -> Element {
    let mut open = open;
    let mut form = use_signal(NewUser::default);
    let mut busy = use_signal(|| false);
    let f = form();
    let submit = move |e: FormEvent| {
        e.prevent_default();
        let body = form();
        if body.password.chars().count() < 8 {
            return toast_error("Das Passwort braucht mindestens 8 Zeichen.");
        }
        spawn(async move {
            busy.set(true);
            match api::post::<User, _>("/admin/users", &body).await {
                Ok(u) => {
                    toast(format!("{} angelegt.", u.label()));
                    form.set(NewUser::default());
                    open.set(false);
                    on_created.call(());
                }
                Err(e) => toast_error(e),
            }
            busy.set(false);
        });
    };
    rsx! {
        form { class: "grid gap-4", onsubmit: submit,
            Field { label: "Benutzername", hint: "2–64 Zeichen: Buchstaben, Zahlen, Punkt, Minus, Unterstrich, @",
                input { class: INPUT, required: true, autocomplete: "off", value: "{f.username}", oninput: move |e| form.write().username = e.value() }
            }
            div { class: "grid gap-4 sm:grid-cols-2",
                Field { label: "Name", input { class: INPUT, value: "{f.display_name}", oninput: move |e| form.write().display_name = e.value() } }
                Field { label: "E-Mail", input { class: INPUT, r#type: "email", value: "{f.email}", oninput: move |e| form.write().email = e.value() } }
            }
            Field { label: "Passwort", hint: "Mindestens 8 Zeichen – der Benutzer kann es danach selbst ändern.",
                input { class: INPUT, r#type: "password", required: true, autocomplete: "new-password", value: "{f.password}", oninput: move |e| form.write().password = e.value() }
            }
            Checkbox { checked: f.is_admin, onchange: move |v| form.write().is_admin = v, label: "Administrator", hint: "Darf Benutzer verwalten, die KI einrichten und das Protokoll sehen." }
            div { class: "flex justify-end gap-2",
                button { r#type: "button", class: btn(BTN_OUTLINE), onclick: move |_| open.set(false), "Abbrechen" }
                button { r#type: "submit", class: btn(BTN_PRIMARY), disabled: busy(), "Anlegen" }
            }
        }
    }
}

#[component]
fn SetPasswordForm(user: User, open: Signal<bool>, on_update: EventHandler<(String, UpdateUser, &'static str)>) -> Element {
    let mut open = open;
    let mut pw = use_signal(String::new);
    let id = user.id.clone();
    rsx! {
        form { class: "grid gap-4",
            onsubmit: move |e| {
                e.prevent_default();
                if pw().chars().count() < 8 {
                    return toast_error("Das Passwort braucht mindestens 8 Zeichen.");
                }
                on_update.call((id.clone(), UpdateUser { password: Some(pw()), ..Default::default() }, "Passwort gesetzt."));
                open.set(false);
            },
            p { class: "text-sm", "Neues Passwort für ", strong { "{user.label()}" } }
            input { class: INPUT, r#type: "password", autocomplete: "new-password", required: true, minlength: 8, "aria-label": "Neues Passwort",
                value: "{pw}", oninput: move |e| pw.set(e.value()) }
            div { class: "flex justify-end gap-2",
                button { r#type: "button", class: btn(BTN_OUTLINE), onclick: move |_| open.set(false), "Abbrechen" }
                button { r#type: "submit", class: btn(BTN_PRIMARY), "Setzen" }
            }
        }
    }
}

#[component]
pub fn Instance() -> Element {
    let mut settings = use_signal(|| None::<InstanceSettings>);
    use_future(move || async move {
        match api::get::<InstanceSettings>("/admin/settings").await {
            Ok(s) => settings.set(Some(s)),
            Err(e) => toast_error(e),
        }
    });
    let Some(s) = settings() else { return rsx! { Loading {} } };
    rsx! {
        Section { title: "Registrierung", description: "Ist sie offen, kann sich jeder mit Zugang zu dieser Adresse ein Konto anlegen. Sonst legen Administratoren die Konten unter „Benutzer“ an.",
            Toggle { checked: s.registration_open, label: "Registrierung offen",
                onchange: move |v: bool| {
                    spawn(async move {
                        match api::put::<InstanceSettings, _>("/admin/settings", &InstanceSettings { registration_open: v }).await {
                            Ok(s) => { settings.set(Some(s)); toast(if v { "Registrierung geöffnet." } else { "Registrierung geschlossen." }); }
                            Err(e) => toast_error(e),
                        }
                    });
                },
            }
        }
    }
}

fn action_label(a: &str) -> &str {
    match a {
        "login" => "Anmeldung",
        "login_failed" => "Anmeldung fehlgeschlagen",
        "logout" => "Abmeldung",
        "export" => "Export",
        "import" => "Import",
        "restore" => "Wiederherstellen",
        "delete" | "messages_deleted" => "Gelöscht",
        "account_created" => "Postfach angelegt",
        "account_deleted" => "Postfach gelöscht",
        "account_updated" => "Postfach geändert",
        "user_created" => "Benutzer angelegt",
        "user_deleted" => "Benutzer gelöscht",
        "user_updated" => "Benutzer geändert",
        "password_changed" => "Passwort geändert",
        "send" | "sent" => "Gesendet",
        "settings" => "Einstellungen",
        other => other,
    }
}

#[component]
pub fn Activity() -> Element {
    let mut entries = use_signal(|| None::<Vec<ActivityEntry>>);
    let mut limit = use_signal(|| 200u32);
    let mut filter = use_signal(String::new);
    use_effect(move || {
        let l = limit();
        spawn(async move {
            match api::get::<Vec<ActivityEntry>>(&format!("/admin/activity?limit={l}")).await {
                Ok(e) => entries.set(Some(e)),
                Err(e) => toast_error(e),
            }
        });
    });
    let f = filter().to_lowercase();
    rsx! {
        Section { title: "Protokoll", description: "Anmeldungen, Exporte, Importe, Löschungen und Änderungen an Konten – die neuesten zuerst.",
            actions: rsx! {
                input { class: "{INPUT} h-8 w-48", placeholder: "Filtern …", "aria-label": "Protokoll filtern", value: "{filter}", oninput: move |e| filter.set(e.value()) }
            },
            match entries() {
                None => rsx! { Loading {} },
                Some(list) if list.is_empty() => rsx! { EmptyState { title: "Noch keine Einträge", icon: rsx! { icons::ScrollText {} } } },
                Some(list) => {
                    let n = list.len() as u32;
                    let rows: Vec<ActivityEntry> = list
                        .into_iter()
                        .filter(|e| f.is_empty() || format!("{} {} {}", e.action, e.detail, e.username.clone().unwrap_or_default()).to_lowercase().contains(&f))
                        .collect();
                    rsx! {
                        div { class: "overflow-x-auto rounded-lg border",
                            table { class: "w-full text-left text-sm",
                                thead { class: "border-b bg-muted/30 text-xs text-muted-foreground",
                                    tr {
                                        th { class: "px-3 py-2 font-medium", "Zeit" }
                                        th { class: "px-3 py-2 font-medium", "Benutzer" }
                                        th { class: "px-3 py-2 font-medium", "Aktion" }
                                        th { class: "px-3 py-2 font-medium", "Details" }
                                    }
                                }
                                tbody { class: "divide-y",
                                    for e in rows {
                                        tr { key: "{e.id}", class: if e.action.ends_with("failed") { "bg-destructive/5" } else { "" },
                                            td { class: "whitespace-nowrap px-3 py-2 text-xs tabular-nums text-muted-foreground", {util::datetime(e.created_at)} }
                                            td { class: "whitespace-nowrap px-3 py-2", {e.username.clone().unwrap_or_else(|| "–".into())} }
                                            td { class: "whitespace-nowrap px-3 py-2", {action_label(&e.action).to_string()} }
                                            td { class: "px-3 py-2 text-muted-foreground break-all", "{e.detail}" }
                                        }
                                    }
                                }
                            }
                        }
                        if n >= limit() {
                            div { class: "mt-3 flex justify-center",
                                button { class: "{btn(BTN_OUTLINE)} {BTN_SM}", onclick: move |_| limit += 300, "Ältere laden" }
                            }
                        }
                    }
                }
            }
        }
    }
}
