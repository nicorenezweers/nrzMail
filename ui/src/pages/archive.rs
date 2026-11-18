//! Archiv: Übersicht je Postfach (Anzahl, Grösse, Zeitraum, nur lokal), Zeitachse Jahre →
//! Monate, Ordner-Browser, erweiterte Suche mit Ergebnisliste und Lesebereich, Export der
//! Auswahl oder aller Treffer (mbox / eml-zip), Import mit Fortschritt und Wiederherstellen
//! ausgewählter Nachrichten auf den Server.

use crate::actions;
use crate::api;
use crate::components::common::*;
use crate::components::message_list::MessageList;
use crate::components::overlay::*;
use crate::components::reader::{Loaded, Reader};
use crate::pages::layout::{MobileMenuButton, folder_label};
use crate::state::{self, ACCOUNTS, FOLDERS};
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{
    AccountKind, ArchiveOverview, BulkResult, ImportResult, MailView, MessageAction, MessageList as List, MessageQuery,
    MessageSummary, PeriodCount, RestoreRequest,
};
use std::collections::HashSet;

const PAGE: i64 = 50;

/// Felder der erweiterten Suche.
#[derive(Clone, PartialEq, Default)]
struct Search {
    words: String,
    from: String,
    to: String,
    subject: String,
    after: String,
    before: String,
    attachments: bool,
    account: String,
    folder: String,
}

fn quoted(v: &str) -> String {
    let v = v.trim();
    if v.contains(char::is_whitespace) { format!("\"{}\"", v.replace('"', "")) } else { v.to_string() }
}

impl Search {
    fn is_empty(&self) -> bool {
        *self == Search::default()
    }

    fn query(&self) -> MessageQuery {
        let mut q = Vec::new();
        if !self.words.trim().is_empty() {
            q.push(self.words.trim().to_string());
        }
        for (key, v) in [("from", &self.from), ("to", &self.to), ("subject", &self.subject)] {
            if !v.trim().is_empty() {
                q.push(format!("{key}:{}", quoted(v)));
            }
        }
        // `before` meint „bis und mit diesem Tag“
        let before = util::parse_local_date(&self.before).map(|t| t + 86_400);
        MessageQuery {
            view: Some(MailView::All),
            account: Some(self.account.clone()).filter(|a| !a.is_empty()),
            folder: Some(self.folder.clone()).filter(|f| !f.is_empty()),
            q: Some(q.join(" ")).filter(|q| !q.is_empty()),
            attachments: self.attachments.then_some(true),
            after: util::parse_local_date(&self.after),
            before,
            threads: Some(false),
            offset: Some(0),
            limit: Some(PAGE),
            ..Default::default()
        }
    }
}

/// Erster Tag eines Monats als `<input type=date>`-Wert.
fn month_start(y: i32, m: u32) -> String {
    format!("{y:04}-{m:02}-01")
}

fn month_end(y: i32, m: u32) -> String {
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    };
    format!("{y:04}-{m:02}-{days:02}")
}

#[component]
pub fn Archive() -> Element {
    let mut overview = use_signal(|| None::<Result<ArchiveOverview, String>>);
    let mut reload = use_signal(|| 0u32);
    let mut form = use_signal(Search::default);
    let mut active = use_signal(|| None::<Search>);
    let mut results = use_signal(Vec::<MessageSummary>::new);
    let mut total = use_signal(|| 0i64);
    let mut searching = use_signal(|| false);
    let mut selected = use_signal(HashSet::<String>::new);
    let mut open = use_signal(String::new);
    let thread = use_signal(|| None as Loaded);
    let mut import_open = use_signal(|| false);
    let mut restore_open = use_signal(|| false);
    let mut generation = use_signal(|| 0u64);

    use_effect(move || {
        let _ = reload();
        spawn(async move {
            let res = api::get::<ArchiveOverview>("/archive").await.map_err(|e| e.message);
            overview.set(Some(res));
        });
    });

    let mut run_search = move |s: Search, append: bool| {
        let mut q = s.query();
        if append {
            q.offset = Some(results.peek().len() as i64);
        } else {
            selected.set(HashSet::new());
            open.set(String::new());
            active.set(Some(s.clone()));
        }
        *generation.write() += 1;
        let g = *generation.peek();
        spawn(async move {
            searching.set(true);
            let res = api::get::<List>(&format!("/messages?{}", q.to_query())).await;
            searching.set(false);
            if *generation.peek() != g {
                return;
            }
            match res {
                Ok(list) => {
                    if append {
                        results.write().extend(list.items);
                    } else {
                        results.set(list.items);
                    }
                    total.set(list.total);
                }
                Err(e) => toast_error(e),
            }
        });
        if !append {
            document::eval("requestAnimationFrame(() => document.getElementById('archive-results')?.scrollIntoView({ behavior: 'smooth', block: 'start' }))");
        }
    };

    let act = use_callback(move |(ids, action): (Vec<String>, MessageAction)| {
        spawn(async move {
            if action == MessageAction::Delete {
                let ask = Ask::new("Endgültig löschen?", "Die Nachrichten verschwinden vom Server und aus dem lokalen Archiv.").confirm("Endgültig löschen").destructive();
                if !confirm(ask).await {
                    return;
                }
            }
            if !actions::apply(ids.clone(), action.clone()).await {
                return;
            }
            let mut v = results.write();
            for m in v.iter_mut().filter(|m| ids.contains(&m.id)) {
                match action {
                    MessageAction::MarkRead => m.seen = true,
                    MessageAction::MarkUnread => m.seen = false,
                    MessageAction::Flag => m.flagged = true,
                    MessageAction::Unflag => m.flagged = false,
                    _ => {}
                }
            }
            if action == MessageAction::Delete {
                v.retain(|m| !ids.contains(&m.id));
                if ids.contains(&open.peek()) {
                    open.set(String::new());
                }
            }
        });
    });

    let accounts = ACCOUNTS();
    let s = form();
    let folders_for: Vec<_> = FOLDERS.read().iter().filter(|f| f.account_id == s.account).cloned().collect();
    let list = results();
    let sel: Vec<MessageSummary> = list.iter().filter(|m| selected.read().contains(&m.id)).cloned().collect();
    let export_query = active().map(|a| {
        let mut q = a.query();
        q.offset = None;
        q.limit = None;
        q.to_query()
    });
    let ids_param = sel.iter().map(|m| api::enc(&m.id)).collect::<Vec<_>>().join(",");

    rsx! {
        document::Title { "Archiv – nrzMail" }
        div { class: "flex h-14 shrink-0 items-center gap-2 border-b px-3 md:hidden",
            MobileMenuButton {}
            span { class: "font-semibold", "Archiv" }
        }
        div { class: "min-h-0 flex-1 overflow-y-auto",
            div { class: "mx-auto max-w-6xl p-4 pb-24 md:p-6 md:pb-24",
                PageHeader { title: "Archiv", description: "Alles, was nrzMail je abgeholt oder importiert hat – auch was auf dem Server längst gelöscht ist.",
                    actions: rsx! {
                        button { class: btn(BTN_OUTLINE), onclick: move |_| import_open.set(true), icons::Upload { class: "size-4" } "Importieren" }
                    },
                }
                match overview() {
                    None => rsx! { OverviewSkeleton {} },
                    Some(Err(e)) => rsx! { Callout { text: e, error: true } },
                    Some(Ok(o)) => rsx! {
                        OverviewCards { overview: o.clone(),
                            on_folder: move |(account, folder): (String, String)| {
                                let s = Search { account, folder, ..Default::default() };
                                form.set(s.clone());
                                run_search(s, false);
                            },
                        }
                        Timeline {
                            accounts: o.accounts.iter().map(|a| (a.account.id.clone(), a.account.label().to_string())).collect::<Vec<_>>(),
                            on_pick: move |(account, after, before): (String, String, String)| {
                                let s = Search { account, after, before, ..Default::default() };
                                form.set(s.clone());
                                run_search(s, false);
                            },
                        }
                    },
                }

                // Erweiterte Suche
                Section { title: "Suchen", description: "Felder lassen sich kombinieren; leer bedeutet „egal“.",
                    form { class: "grid gap-4",
                        onsubmit: move |e| { e.prevent_default(); run_search(form(), false); },
                        div { class: "grid gap-4 md:grid-cols-2",
                            Field { label: "Wörter", class: "md:col-span-2",
                                input { class: INPUT, r#type: "search", placeholder: "z. B. rechnung 2024 \"genau so\"", value: "{s.words}", oninput: move |e| form.write().words = e.value() }
                            }
                            Field { label: "Von", input { class: INPUT, placeholder: "Name oder Adresse", value: "{s.from}", oninput: move |e| form.write().from = e.value() } }
                            Field { label: "An", input { class: INPUT, placeholder: "Name oder Adresse", value: "{s.to}", oninput: move |e| form.write().to = e.value() } }
                            Field { label: "Betreff", input { class: INPUT, value: "{s.subject}", oninput: move |e| form.write().subject = e.value() } }
                            div { class: "grid grid-cols-2 gap-3",
                                Field { label: "Ab", input { class: INPUT, r#type: "date", value: "{s.after}", oninput: move |e| form.write().after = e.value() } }
                                Field { label: "Bis", input { class: INPUT, r#type: "date", value: "{s.before}", oninput: move |e| form.write().before = e.value() } }
                            }
                            Field { label: "Postfach",
                                select { class: SELECT, value: "{s.account}",
                                    onchange: move |e| { let mut w = form.write(); w.account = e.value(); w.folder.clear(); },
                                    option { value: "", "Alle Postfächer" }
                                    for a in accounts.iter() {
                                        option { value: "{a.id}", selected: s.account == a.id, "{a.label()}" }
                                    }
                                }
                            }
                            Field { label: "Ordner",
                                select { class: SELECT, value: "{s.folder}", disabled: s.account.is_empty(),
                                    onchange: move |e| form.write().folder = e.value(),
                                    option { value: "", if s.account.is_empty() { "Zuerst ein Postfach wählen" } else { "Alle Ordner" } }
                                    for f in folders_for.iter() {
                                        option { value: "{f.id}", selected: s.folder == f.id, {format!("{}{}", "\u{2003}".repeat(f.depth as usize), folder_label(f))} }
                                    }
                                }
                            }
                        }
                        div { class: "flex flex-wrap items-center gap-3",
                            Checkbox { checked: s.attachments, onchange: move |v| form.write().attachments = v, label: "Nur mit Anhang" }
                            div { class: "flex-1" }
                            if !s.is_empty() {
                                button { r#type: "button", class: btn(BTN_GHOST), onclick: move |_| form.set(Search::default()), "Zurücksetzen" }
                            }
                            button { r#type: "submit", class: btn(BTN_PRIMARY), disabled: searching(),
                                if searching() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::Search { class: "size-4" } }
                                "Suchen"
                            }
                        }
                    }
                }

                // Treffer
                if active().is_some() {
                    section { id: "archive-results", class: "mt-6 rounded-xl border bg-card",
                        div { class: "flex flex-wrap items-center gap-2 border-b px-4 py-3",
                            div { class: "min-w-0 flex-1",
                                h2 { class: "font-semibold leading-none", "Treffer" }
                                p { class: "mt-1 text-xs text-muted-foreground",
                                    if searching() && list.is_empty() { "Suche …" } else { {util::plural(total(), "Nachricht", "Nachrichten")} }
                                    if !sel.is_empty() { " · {sel.len()} ausgewählt" }
                                }
                            }
                            if !list.is_empty() {
                                button { class: "{btn(BTN_GHOST)} {BTN_SM}",
                                    onclick: move |_| {
                                        let all: HashSet<String> = results.peek().iter().map(|m| m.id.clone()).collect();
                                        if *selected.peek() == all { selected.set(HashSet::new()) } else { selected.set(all) }
                                    },
                                    "Alle auswählen"
                                }
                            }
                            if let Some(q) = export_query.clone() {
                                Dropdown { align: Align::End, class: "w-64",
                                    trigger_class: "{btn(BTN_OUTLINE)} {BTN_SM}",
                                    trigger: rsx! { icons::Download { class: "size-4" } "Exportieren" icons::ChevronDown { class: "size-3.5" } },
                                    if !sel.is_empty() {
                                        MenuLabel { "{sel.len()} ausgewählte" }
                                        ExportLink { href: api::url(&format!("/export?format=mbox&ids={ids_param}")), label: "als mbox" }
                                        ExportLink { href: api::url(&format!("/export?format=eml&ids={ids_param}")), label: "als eml (zip)" }
                                        MenuSeparator {}
                                    }
                                    MenuLabel { "Alle {util::num(total())} Treffer" }
                                    ExportLink { href: api::url(&format!("/export?format=mbox&{q}")), label: "als mbox" }
                                    ExportLink { href: api::url(&format!("/export?format=eml&{q}")), label: "als eml (zip)" }
                                }
                            }
                            if !sel.is_empty() {
                                button { class: "{btn(BTN_OUTLINE)} {BTN_SM}", onclick: move |_| restore_open.set(true),
                                    icons::ArchiveRestore { class: "size-4" } "Auf den Server"
                                }
                            }
                        }
                        div { class: "grid lg:grid-cols-[minmax(0,26rem)_minmax(0,1fr)]",
                            div { class: "max-h-[70vh] min-h-0 overflow-y-auto lg:border-r",
                                if searching() && list.is_empty() {
                                    SkeletonRows { rows: 5 }
                                } else if list.is_empty() {
                                    div { class: "p-4", EmptyState { title: "Keine Treffer", description: "Weniger Felder ausfüllen oder den Zeitraum vergrössern.", icon: rsx! { icons::Search {} } } }
                                } else {
                                    MessageList {
                                        items: list.clone(),
                                        selected,
                                        open_id: open(),
                                        show_category: true,
                                        onopen: move |m: MessageSummary| open.set(m.id),
                                        onstar: move |m: MessageSummary| act.call((vec![m.id.clone()], if m.flagged { MessageAction::Unflag } else { MessageAction::Flag })),
                                    }
                                    if (list.len() as i64) < total() {
                                        div { class: "flex justify-center p-4",
                                            button { class: "{btn(BTN_OUTLINE)} {BTN_SM}", disabled: searching(),
                                                onclick: move |_| if let Some(s) = active() { run_search(s, true) },
                                                "Mehr laden"
                                            }
                                        }
                                    }
                                }
                            }
                            // Lesebereich: Desktop daneben, Handy als Vollbild
                            if !open().is_empty() {
                                div { class: "fixed inset-0 z-50 flex flex-col bg-background lg:static lg:z-auto lg:max-h-[70vh] lg:min-h-[24rem]",
                                    Reader {
                                        key: "{open}",
                                        id: open(),
                                        thread,
                                        view: Some(MailView::All),
                                        on_act: move |x: (Vec<String>, MessageAction)| act.call(x),
                                        on_close: move |_| open.set(String::new()),
                                    }
                                }
                            } else {
                                div { class: "hidden place-items-center p-6 text-sm text-muted-foreground lg:grid", "Nachricht auswählen, um sie zu lesen." }
                            }
                        }
                    }
                }
            }
        }
        Modal { open: import_open, title: "Importieren", description: "mbox-Dateien (z. B. aus Thunderbird oder Google Takeout), einzelne .eml oder ein .zip mit .eml-Dateien. Doppelte Nachrichten werden erkannt und übersprungen.",
            ImportForm { on_done: move |_| { reload += 1; spawn(async { state::load_accounts().await; }); } }
        }
        Modal { open: restore_open, title: "Auf den Server zurückladen", description: "Die ausgewählten Nachrichten werden per IMAP wieder in einen Ordner des Postfachs gelegt.",
            RestoreForm { items: sel.clone(), open: restore_open, on_done: move |_| selected.set(HashSet::new()) }
        }
    }
}

#[component]
fn ExportLink(href: String, label: &'static str) -> Element {
    rsx! {
        a { href: "{href}", download: "",
            class: "relative flex w-full cursor-default select-none items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none hover:bg-accent hover:text-accent-foreground",
            icons::FileDown { class: "size-4" }
            "{label}"
        }
    }
}

#[component]
fn OverviewSkeleton() -> Element {
    use crate::ui::skeleton::Skeleton;
    rsx! {
        div { class: "mb-6 grid gap-3 md:grid-cols-3",
            for i in 0..3 { Skeleton { key: "{i}", class: "h-36" } }
        }
    }
}

#[component]
fn OverviewCards(overview: ArchiveOverview, on_folder: EventHandler<(String, String)>) -> Element {
    let o = overview;
    let mut browse = use_signal(|| None::<String>);
    if o.accounts.is_empty() {
        return rsx! {
            div { class: "mb-6",
                EmptyState { title: "Das Archiv ist leer", description: "Füge ein Postfach hinzu oder importiere eine mbox-Datei.", icon: rsx! { icons::Database {} } }
            }
        };
    }
    rsx! {
        div { class: "mb-6 grid gap-3",
            div { class: "grid grid-cols-3 gap-3",
                div { class: "rounded-xl border bg-card p-4",
                    p { class: "text-xs text-muted-foreground", "Nachrichten" }
                    p { class: "mt-1 text-2xl font-semibold tabular-nums", {util::num(o.total_messages)} }
                }
                div { class: "rounded-xl border bg-card p-4",
                    p { class: "text-xs text-muted-foreground", "Inhalt" }
                    p { class: "mt-1 text-2xl font-semibold tabular-nums", {util::size(o.total_size_bytes)} }
                }
                div { class: "rounded-xl border bg-card p-4",
                    p { class: "text-xs text-muted-foreground", "Datenbank" }
                    p { class: "mt-1 text-2xl font-semibold tabular-nums", {util::size(o.database_bytes)} }
                    p { class: "text-xs text-muted-foreground", "verschlüsselt" }
                }
            }
            div { class: "grid gap-3 md:grid-cols-2",
                for a in o.accounts.iter().cloned() {
                    div { key: "{a.account.id}", class: "rounded-xl border bg-card p-4",
                        div { class: "flex items-center gap-2",
                            Dot { color: a.account.color.clone(), class: "size-2.5" }
                            p { class: "min-w-0 flex-1 truncate font-semibold", "{a.account.label()}" }
                            if a.account.kind == AccountKind::Import { Pill { "Nur Archiv" } }
                        }
                        p { class: "truncate text-xs text-muted-foreground", "{a.account.email}" }
                        dl { class: "mt-3 grid grid-cols-2 gap-x-4 gap-y-2 text-sm",
                            div { dt { class: "text-xs text-muted-foreground", "Nachrichten" } dd { class: "tabular-nums", {util::num(a.message_count)} } }
                            div { dt { class: "text-xs text-muted-foreground", "Grösse" } dd { class: "tabular-nums", {util::size(a.size_bytes)} } }
                            div { dt { class: "text-xs text-muted-foreground", "Zeitraum" }
                                dd { class: "text-xs",
                                    match (a.oldest, a.newest) {
                                        (Some(x), Some(y)) => rsx! { {format!("{} – {}", util::date(x), util::date(y))} },
                                        _ => rsx! { "–" },
                                    }
                                }
                            }
                            div { dt { class: "text-xs text-muted-foreground", "Nur noch lokal" } dd { class: "tabular-nums", {util::num(a.local_only_count)} } }
                            div { dt { class: "text-xs text-muted-foreground", "Anhänge" } dd { class: "tabular-nums", {util::num(a.attachment_count)} } }
                            div { dt { class: "text-xs text-muted-foreground", "Ordner" } dd { class: "tabular-nums", {util::num(a.folders.len() as i64)} } }
                        }
                        if !a.years.is_empty() {
                            YearSpark { years: a.years.clone() }
                        }
                        div { class: "mt-3",
                            button { class: "{btn(BTN_GHOST)} {BTN_SM} -ml-2",
                                "aria-expanded": "{browse() == Some(a.account.id.clone())}",
                                onclick: { let id = a.account.id.clone(); move |_| browse.set(if browse() == Some(id.clone()) { None } else { Some(id.clone()) }) },
                                icons::FolderTree { class: "size-4" } "Ordner durchsehen"
                            }
                            if browse() == Some(a.account.id.clone()) {
                                div { class: "mt-1 max-h-64 overflow-y-auto rounded-lg border",
                                    for f in a.folders.iter().cloned() {
                                        button { key: "{f.id}", class: "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-accent/50",
                                            style: "padding-left: {12 + f.depth * 14}px",
                                            onclick: { let (acc, fid) = (a.account.id.clone(), f.id.clone()); move |_| on_folder.call((acc.clone(), fid.clone())) },
                                            {crate::pages::layout::role_icon(f.role, false)}
                                            span { class: "min-w-0 flex-1 truncate", {folder_label(&f)} }
                                            span { class: "text-xs tabular-nums text-muted-foreground", {util::num(f.total)} }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Kleine Jahresbalken in der Karte eines Postfachs.
#[component]
fn YearSpark(years: Vec<PeriodCount>) -> Element {
    let max = years.iter().map(|y| y.count).max().unwrap_or(0).max(1);
    rsx! {
        div { class: "mt-3 flex h-10 items-end gap-1", "aria-hidden": "true",
            for y in years {
                div { key: "{y.label}", class: "min-w-1 max-w-5 flex-1 rounded-sm bg-foreground/30",
                    style: "height: {(y.count as f64 / max as f64 * 100.0).max(4.0)}%",
                    title: "{y.label}: {util::num(y.count)}",
                }
            }
        }
    }
}

/// Zeitachse: Jahre als Balken, ein Klick zeigt die Monate, ein Klick auf einen Monat sucht.
#[component]
fn Timeline(accounts: Vec<(String, String)>, on_pick: EventHandler<(String, String, String)>) -> Element {
    let mut account = use_signal(String::new);
    let mut year = use_signal(|| None::<i32>);
    let periods = use_resource(move || async move {
        let mut path = format!("/archive/timeline?account={}", api::enc(&account()));
        if let Some(y) = year() {
            path += &format!("&year={y}");
        }
        api::get::<Vec<PeriodCount>>(&path).await
    });
    let list = match &*periods.read() {
        Some(Ok(p)) => Some(p.clone()),
        _ => None,
    };
    let max = list.as_ref().map(|l| l.iter().map(|p| p.count).max().unwrap_or(0)).unwrap_or(0).max(1);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez"];
    rsx! {
        div { class: "mb-6",
            Section { title: "Zeitachse", description: if year().is_some() { "Monate – ein Klick sucht alle Nachrichten des Monats." } else { "Jahre – ein Klick zeigt die Monate." },
                actions: rsx! {
                    if let Some(y) = year() {
                        button { class: "{btn(BTN_GHOST)} {BTN_SM}", onclick: move |_| year.set(None), icons::ArrowLeft { class: "size-4" } "Alle Jahre" }
                        button { class: "{btn(BTN_OUTLINE)} {BTN_SM}",
                            onclick: move |_| on_pick.call((account(), format!("{y:04}-01-01"), format!("{y:04}-12-31"))),
                            icons::Search { class: "size-4" } "{y} durchsuchen"
                        }
                    }
                    if accounts.len() > 1 {
                        select { class: SELECT_SM, "aria-label": "Postfach", value: "{account}",
                            onchange: move |e| { account.set(e.value()); },
                            option { value: "", "Alle Postfächer" }
                            for (id , label) in accounts.iter() {
                                option { value: "{id}", selected: account() == *id, "{label}" }
                            }
                        }
                    }
                },
                match list {
                    None => rsx! { div { class: "h-44 animate-pulse rounded-lg bg-muted/40" } },
                    Some(l) if l.is_empty() => rsx! { p { class: "py-8 text-center text-sm text-muted-foreground", "Keine Nachrichten." } },
                    Some(l) => rsx! {
                        div { class: "flex h-44 items-end gap-1 overflow-x-auto pb-1", role: "list",
                            for p in l {
                                {
                                    let pct = (p.count as f64 / max as f64 * 100.0).max(2.0);
                                    let (y, m) = {
                                        let mut it = p.label.split('-');
                                        (it.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0), it.next().and_then(|v| v.parse::<u32>().ok()))
                                    };
                                    let short = match m {
                                        Some(m) => MONTHS[(m.clamp(1, 12) - 1) as usize].to_string(),
                                        None => p.label.clone(),
                                    };
                                    rsx! {
                                        button { key: "{p.label}", role: "listitem",
                                            class: "group flex h-full min-w-8 flex-1 flex-col items-center justify-end gap-1 rounded-md px-0.5 outline-none focus-visible:ring-2 focus-visible:ring-ring/60",
                                            title: "{p.label}: {util::num(p.count)} Nachrichten, {util::size(p.size_bytes)}",
                                            onclick: move |_| match m {
                                                Some(m) => on_pick.call((account(), month_start(y, m), month_end(y, m))),
                                                None => year.set(Some(y)),
                                            },
                                            span { class: "text-[10px] tabular-nums text-muted-foreground opacity-0 group-hover:opacity-100", {util::num(p.count)} }
                                            div { class: "w-full rounded-sm bg-foreground/40 transition-colors group-hover:bg-foreground/70", style: "height: {pct}%" }
                                            span { class: "text-[11px] text-muted-foreground", "{short}" }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn ImportForm(on_done: EventHandler<()>) -> Element {
    let accounts = ACCOUNTS();
    let first = accounts.iter().find(|a| a.kind == AccountKind::Import).or(accounts.first()).map(|a| a.id.clone()).unwrap_or_default();
    let mut account = use_signal(|| first);
    let mut folder = use_signal(String::new);
    let mut file = use_signal(|| None::<web_sys::File>);
    let mut progress = use_signal(|| None::<f64>);
    let mut result = use_signal(|| None::<ImportResult>);
    let folders: Vec<_> = FOLDERS.read().iter().filter(|f| f.account_id == account()).cloned().collect();
    let busy = progress().is_some();
    let start = move |_| {
        let Some(f) = file() else { return toast_error("Bitte eine Datei wählen.") };
        if account().is_empty() {
            return toast_error("Bitte ein Postfach wählen.");
        }
        let mut path = format!("/import?account={}", api::enc(&account()));
        if !folder().is_empty() {
            path += &format!("&folder={}", api::enc(&folder()));
        }
        spawn(async move {
            result.set(None);
            progress.set(Some(0.0));
            let res = api::upload_raw::<ImportResult>(&path, &f, |p| progress.set(Some(p))).await;
            progress.set(None);
            match res {
                Ok(r) => {
                    toast(format!("{} Nachrichten importiert.", util::num(r.imported as i64)));
                    result.set(Some(r));
                    file.set(None);
                    on_done.call(());
                }
                Err(e) => toast_error(e),
            }
        });
    };
    if accounts.is_empty() {
        return rsx! { Callout { text: "Zuerst ein Postfach anlegen – für reine Importe die Art „Nur Archiv“ wählen." } };
    }
    rsx! {
        div { class: "grid gap-4",
            Field { label: "In Postfach",
                select { class: SELECT, value: "{account}", disabled: busy,
                    onchange: move |e| { account.set(e.value()); folder.set(String::new()); },
                    for a in accounts.iter() {
                        option { value: "{a.id}", selected: account() == a.id,
                            {format!("{} – {}{}", a.label(), a.email, if a.kind == AccountKind::Import { " (nur Archiv)" } else { "" })}
                        }
                    }
                }
            }
            Field { label: "Ordner", hint: "Leer: nrzMail legt die Nachrichten nach den Angaben in der Datei ab.",
                select { class: SELECT, value: "{folder}", disabled: busy, onchange: move |e| folder.set(e.value()),
                    option { value: "", "Automatisch" }
                    for f in folders.iter() {
                        option { value: "{f.id}", selected: folder() == f.id, {format!("{}{}", "\u{2003}".repeat(f.depth as usize), folder_label(f))} }
                    }
                }
            }
            label { class: "flex cursor-pointer flex-col items-center gap-2 rounded-xl border border-dashed p-6 text-center hover:bg-accent/30",
                ondragover: move |e| e.prevent_default(),
                ondrop: move |e| {
                    e.prevent_default();
                    if let Some(f) = util::dropped_files(&e).into_iter().next() { file.set(Some(f)); result.set(None); }
                },
                icons::FileUp { class: "size-8 text-muted-foreground" }
                match file() {
                    Some(f) => rsx! {
                        span { class: "text-sm font-medium", "{f.name()}" }
                        span { class: "text-xs text-muted-foreground", {util::size(f.size() as i64)} }
                    },
                    None => rsx! {
                        span { class: "text-sm font-medium", "Datei wählen oder hierher ziehen" }
                        span { class: "text-xs text-muted-foreground", ".mbox, .eml oder .zip" }
                    },
                }
                input { r#type: "file", class: "sr-only", accept: ".mbox,.mbx,.eml,.zip,application/mbox,message/rfc822,application/zip", disabled: busy,
                    onchange: move |e| { if let Some(f) = util::input_files(&e).into_iter().next() { file.set(Some(f)); result.set(None); } } }
            }
            if let Some(p) = progress() {
                Progress { value: p, label: if p >= 1.0 { "Wird verarbeitet …" } else { "Hochladen" } }
            }
            if let Some(r) = result() {
                div { class: "rounded-lg border bg-muted/30 p-3 text-sm",
                    p { class: "font-medium", "Import abgeschlossen" }
                    p { class: "mt-1 text-muted-foreground",
                        {format!("{} importiert · {} doppelt übersprungen · {} fehlerhaft", util::num(r.imported as i64), util::num(r.duplicates as i64), util::num(r.failed as i64))}
                    }
                    if !r.message.is_empty() { p { class: "mt-1 text-xs text-muted-foreground", "{r.message}" } }
                }
            }
            div { class: "flex justify-end",
                button { class: btn(BTN_PRIMARY), disabled: busy || file().is_none(), onclick: start,
                    if busy { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::Upload { class: "size-4" } }
                    "Importieren"
                }
            }
        }
    }
}

#[component]
fn RestoreForm(items: Vec<MessageSummary>, open: Signal<bool>, on_done: EventHandler<()>) -> Element {
    let mut open = open;
    let account = items.first().map(|m| m.account_id.clone()).filter(|a| items.iter().all(|m| &m.account_id == a));
    let folders: Vec<_> = match &account {
        Some(a) => FOLDERS.read().iter().filter(|f| &f.account_id == a).cloned().collect(),
        None => Vec::new(),
    };
    let inbox = folders.iter().find(|f| f.role == nrzmail_api::FolderRole::Inbox).or(folders.first()).map(|f| f.id.clone()).unwrap_or_default();
    let mut target = use_signal(|| inbox);
    let mut busy = use_signal(|| false);
    let ids: Vec<String> = items.iter().map(|m| m.id.clone()).collect();
    let imap = account.as_ref().and_then(|a| state::account(a)).is_some_and(|a| a.kind == AccountKind::Imap);
    if account.is_none() {
        return rsx! { Callout { text: "Die Auswahl stammt aus mehreren Postfächern – bitte je Postfach einzeln zurückladen." } };
    }
    if !imap {
        return rsx! { Callout { text: "Dieses Postfach hat keinen Server (nur Archiv)." } };
    }
    rsx! {
        div { class: "grid gap-4",
            p { class: "text-sm", {util::plural(ids.len() as i64, "Nachricht", "Nachrichten")} " ausgewählt." }
            Field { label: "Zielordner auf dem Server",
                select { class: SELECT, value: "{target}", onchange: move |e| target.set(e.value()),
                    for f in folders.iter() {
                        option { value: "{f.id}", selected: target() == f.id, {format!("{}{}", "\u{2003}".repeat(f.depth as usize), folder_label(f))} }
                    }
                }
            }
            div { class: "flex justify-end gap-2",
                button { class: btn(BTN_OUTLINE), onclick: move |_| open.set(false), "Abbrechen" }
                button { class: btn(BTN_PRIMARY), disabled: busy() || target().is_empty(),
                    onclick: move |_| {
                        let body = RestoreRequest { ids: ids.clone(), folder_id: target() };
                        spawn(async move {
                            busy.set(true);
                            match api::post::<BulkResult, _>("/archive/restore", &body).await {
                                Ok(r) => {
                                    if r.failed > 0 {
                                        toast_error(r.message.unwrap_or_else(|| format!("{} Nachrichten liessen sich nicht zurückladen.", r.failed)));
                                    } else {
                                        toast(format!("{} auf den Server zurückgeladen.", util::plural(r.updated as i64, "Nachricht", "Nachrichten")));
                                    }
                                    open.set(false);
                                    on_done.call(());
                                }
                                Err(e) => toast_error(e),
                            }
                            busy.set(false);
                        });
                    },
                    icons::ArchiveRestore { class: "size-4" }
                    "Zurückladen"
                }
            }
        }
    }
}
