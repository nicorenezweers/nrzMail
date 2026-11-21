//! Befehlspalette (⌘K / Ctrl+K): Ansichten, Postfächer und Ordner, Aktionen auf die offene
//! Nachricht, Seiten und Einstellungen – mit Suche, Pfeiltasten und Enter.

use crate::Route;
use crate::api;
use crate::components::overlay::{toast, toast_error};
use crate::components::{compose, keys};
use crate::pages::layout::{folder_label, logout};
use crate::state::{ACCOUNTS, FOLDERS, ME};
use dioxus::prelude::*;
use nrzmail_api::{FolderRole, MailView};

static OPEN: GlobalSignal<bool> = Signal::global(|| false);

pub fn open() {
    *OPEN.write() = true;
}

pub fn toggle() {
    let next = !*OPEN.peek();
    *OPEN.write() = next;
}

#[derive(Clone, PartialEq)]
enum Run {
    Go(Route),
    Compose,
    Key(&'static str),
    SyncAll,
    Help,
    Logout,
}

#[derive(Clone, PartialEq)]
struct Command {
    group: &'static str,
    label: String,
    hint: String,
    run: Run,
}

fn cmd(group: &'static str, label: impl Into<String>, hint: impl Into<String>, run: Run) -> Command {
    Command { group, label: label.into(), hint: hint.into(), run }
}

fn commands(on_message: bool) -> Vec<Command> {
    let mut out = vec![
        cmd("Aktionen", "Neue Nachricht", "C", Run::Compose),
        cmd("Aktionen", "Alle Postfächer jetzt abgleichen", "", Run::SyncAll),
    ];
    if on_message {
        for (label, key, hint) in [
            ("Antworten", "r", "R"),
            ("Allen antworten", "a", "A"),
            ("Weiterleiten", "f", "F"),
            ("Archivieren", "e", "E"),
            ("Löschen", "#", "#"),
            ("Stern an/aus", "s", "S"),
            ("Als ungelesen markieren", "u", "U"),
        ] {
            out.push(cmd("Nachricht", label, hint, Run::Key(key)));
        }
    }
    for v in MailView::SIDEBAR {
        out.push(cmd("Ansichten", v.label(), "", Run::Go(Route::view(v))));
    }
    let accounts = ACCOUNTS.peek().clone();
    let folders = FOLDERS.peek().clone();
    for a in &accounts {
        out.push(cmd(
            "Postfächer",
            format!("{} – Posteingang", a.label()),
            a.email.clone(),
            Run::Go(Route::account_inbox(&a.id)),
        ));
    }
    for a in &accounts {
        for f in folders.iter().filter(|f| f.account_id == a.id && f.role != FolderRole::Inbox) {
            out.push(cmd("Ordner", format!("{} / {}", a.label(), folder_label(f)), f.path.clone(), Run::Go(Route::folder(&a.id, &f.id))));
        }
    }
    out.push(cmd("Seiten", "Archiv", "Übersicht, Suche, Import/Export", Run::Go(Route::Archive {})));
    out.push(cmd("Seiten", "Statistik", "", Run::Go(Route::Stats {})));
    let admin = ME.peek().as_ref().is_some_and(|u| u.is_admin);
    for (key, label) in crate::pages::settings::SECTIONS {
        if crate::pages::settings::admin_only(key) && !admin {
            continue;
        }
        out.push(cmd("Einstellungen", format!("Einstellungen: {label}"), "", Run::Go(Route::Settings { section: key.to_string() })));
    }
    out.push(cmd("Hilfe", "Tastenkürzel", "?", Run::Help));
    out.push(cmd("Konto", "Abmelden", "", Run::Logout));
    out
}

fn matches(c: &Command, filter: &str) -> bool {
    let hay = format!("{} {} {}", c.label, c.group, c.hint).to_lowercase();
    filter.split_whitespace().all(|w| hay.contains(w))
}

#[component]
pub fn Palette() -> Element {
    if !OPEN() {
        return rsx! {};
    }
    rsx! { PaletteDialog {} }
}

#[component]
fn PaletteDialog() -> Element {
    let nav = use_navigator();
    let route = use_route::<Route>();
    let on_message = matches!(&route, Route::Mail { query } if !query.open.is_empty());
    let mut filter = use_signal(String::new);
    let mut index = use_signal(|| 0usize);
    let all = use_hook(|| commands(on_message));
    let f = filter().to_lowercase();
    let shown: Vec<Command> = all.iter().filter(|c| matches(c, &f)).take(60).cloned().collect();
    let count = shown.len();
    let sel = index().min(count.saturating_sub(1));

    let run = move |c: Command| {
        *OPEN.write() = false;
        match c.run {
            Run::Go(r) => {
                nav.push(r);
            }
            Run::Compose => compose::open_new(),
            Run::Key(k) => keys::dispatch(k),
            Run::Help => keys::open_help(),
            Run::Logout => logout(),
            Run::SyncAll => {
                spawn(async move {
                    let ids: Vec<String> = ACCOUNTS.peek().iter().filter(|a| a.kind == nrzmail_api::AccountKind::Imap).map(|a| a.id.clone()).collect();
                    let mut failed = 0;
                    for id in &ids {
                        if api::action(&format!("/accounts/{}/sync", api::enc(id))).await.is_err() {
                            failed += 1;
                        }
                    }
                    if failed > 0 {
                        toast_error(format!("{failed} Abgleich(e) liessen sich nicht starten."));
                    } else {
                        toast("Abgleich gestartet.");
                    }
                    crate::state::load_accounts().await;
                });
            }
        }
    };
    let shown_for_key = shown.clone();
    // Gruppen-Überschrift vor dem ersten Eintrag jeder Gruppe
    let rows: Vec<(bool, usize, Command)> = shown
        .into_iter()
        .enumerate()
        .scan("", |last, (i, c)| {
            let head = c.group != *last;
            *last = c.group;
            Some((head, i, c))
        })
        .collect();

    rsx! {
        div { class: "fixed inset-0 z-[65] flex items-start justify-center bg-black/60 p-4 pt-[12vh] backdrop-blur-[2px]",
            onclick: move |_| *OPEN.write() = false,
            div { class: "w-full max-w-xl overflow-hidden rounded-2xl border bg-popover text-popover-foreground shadow-2xl animate-in",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": "Befehlspalette",
                onclick: move |e| e.stop_propagation(),
                div { class: "flex items-center gap-3 border-b px-4",
                    icons::Search { class: "size-4 shrink-0 text-muted-foreground" }
                    input {
                        class: "h-12 w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground",
                        placeholder: "Befehl oder Ansicht suchen …",
                        autofocus: true,
                        "aria-label": "Befehl suchen",
                        value: "{filter}",
                        onmounted: move |e| async move { let _ = e.set_focus(true).await; },
                        oninput: move |e| { filter.set(e.value()); index.set(0); },
                        onkeydown: move |e| match e.key() {
                            Key::ArrowDown if count > 0 => {
                                e.prevent_default();
                                index.set((sel + 1) % count);
                                crate::util::scroll_into_view(&format!("palette-{}", (sel + 1) % count));
                            }
                            Key::ArrowUp if count > 0 => {
                                e.prevent_default();
                                index.set((sel + count - 1) % count);
                                crate::util::scroll_into_view(&format!("palette-{}", (sel + count - 1) % count));
                            }
                            Key::Enter => {
                                e.prevent_default();
                                if let Some(c) = shown_for_key.get(sel).cloned() { run(c); }
                            }
                            Key::Escape => *OPEN.write() = false,
                            _ => {}
                        },
                    }
                    crate::ui::kbd::Kbd { "Esc" }
                }
                div { class: "max-h-[min(60vh,28rem)] overflow-y-auto p-1.5", role: "listbox",
                    if rows.is_empty() {
                        p { class: "px-3 py-6 text-center text-sm text-muted-foreground", "Nichts gefunden." }
                    }
                    for (head , i , c) in rows {
                        div { key: "{i}",
                        if head {
                            p { class: "px-2.5 pb-1 pt-2.5 text-[11px] font-medium uppercase tracking-wider text-muted-foreground", "{c.group}" }
                        }
                        button {
                            id: "palette-{i}",
                            role: "option",
                            "aria-selected": "{i == sel}",
                            class: if i == sel { "flex w-full items-center gap-3 rounded-lg bg-accent px-2.5 py-2 text-left text-sm" } else { "flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left text-sm hover:bg-accent/60" },
                            onmouseenter: move |_| index.set(i),
                            onclick: {
                                let c = c.clone();
                                move |_| run(c.clone())
                            },
                            span { class: "min-w-0 flex-1 truncate", "{c.label}" }
                            if !c.hint.is_empty() {
                                span { class: "max-w-[45%] truncate text-xs text-muted-foreground", "{c.hint}" }
                            }
                        }
                        }
                    }
                }
            }
        }
    }
}
