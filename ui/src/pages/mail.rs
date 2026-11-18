//! Hauptansicht in drei Spalten (Seitenleiste, Liste, Lesebereich): vereinheitlichte Ansichten
//! oder ein Postfach/Ordner, Suche mit Syntax-Hilfe, Kategorie-Reiter im Posteingang,
//! Mehrfachauswahl mit Aktionsleiste, Tastenkürzel. Der ganze Zustand steht in der URL.
//! Auf dem Handy eine Spalte mit Zurück-Navigation.

use crate::{MailQuery, Route};
use crate::actions;
use crate::api;
use crate::components::common::*;
use crate::components::compose;
use crate::components::keys::PAGE_KEYS;
use crate::components::message_list::{MessageList, Swipe, SwipeLeft};
use crate::components::assistant;
use crate::components::overlay::*;
use crate::components::reader::{self, Loaded, Reader};
use crate::pages::layout::{MobileMenuButton, folder_label};
use crate::state::{self, ACCOUNTS, FOLDERS, LIST_REFRESH, PREFS};
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{
    AccountKind, Category, FolderRole, MailView, MessageAction, MessageList as List, MessageQuery, MessageSummary,
    OutboxItem, OutboxStatus, format_addresses,
};
use std::collections::HashSet;
use tw_merge::tw_merge;

const PAGE: i64 = 50;
/// Ziehen zum Aktualisieren: so weit (px) ziehen, dann loslassen
const PULL_TRIGGER: f64 = 64.0;

/// Ort in der App ohne die geöffnete Nachricht – daraus entstehen die Links.
#[derive(Clone, PartialEq)]
struct Loc {
    view: String,
    account: String,
    folder: String,
    category: String,
    q: String,
}

impl Loc {
    fn route(&self, open: &str) -> Route {
        Route::mail(&self.view, &self.account, &self.folder, &self.category, &self.q, open)
    }

    fn with_q(&self, q: &str) -> Route {
        Loc { q: q.to_string(), ..self.clone() }.route("")
    }

    fn with_category(&self, c: &str) -> Route {
        Loc { category: c.to_string(), ..self.clone() }.route("")
    }
}

fn some(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[component]
pub fn Mail(query: MailQuery) -> Element {
    let MailQuery { view, account, folder, category, q, open } = query;
    let nav = use_navigator();
    let prefs = PREFS();
    let loc = Loc { view: view.clone(), account: account.clone(), folder: folder.clone(), category: category.clone(), q: q.clone() };
    let mail_view = MailView::parse(&view).unwrap_or_default();
    let in_folder = !folder.is_empty();
    let searching = !q.trim().is_empty();
    let tabs_on = prefs.category_tabs && mail_view == MailView::Inbox && !in_folder && !searching;
    let cat: Option<Category> = if tabs_on {
        match category.as_str() {
            "all" => None,
            "" if prefs.inbox_start_all => None,
            c => Some(Category::parse(c).unwrap_or(Category::Primary)),
        }
    } else {
        None
    };
    let query_view = if in_folder {
        None
    } else if searching && mail_view == MailView::Inbox {
        Some(MailView::All)
    } else {
        Some(mail_view)
    };
    let query = MessageQuery {
        view: query_view,
        account: some(&account),
        folder: some(&folder),
        category: cat,
        q: some(&q),
        threads: Some(prefs.threads),
        offset: Some(0),
        limit: Some(PAGE),
        ..Default::default()
    };
    let key = query.to_query();

    let mut items = use_signal(Vec::<MessageSummary>::new);
    let mut total = use_signal(|| 0i64);
    let mut loading = use_signal(|| true);
    let mut loading_more = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut selected = use_signal(HashSet::<String>::new);
    let mut cursor = use_signal(|| None::<String>);
    let mut generation = use_signal(|| 0u64);
    let mut last_key = use_signal(String::new);
    let mut thread = use_signal(|| None as Loaded);
    let mut outbox = use_signal(Vec::<OutboxItem>::new);
    let refresh = LIST_REFRESH();
    let phone = state::PHONE();
    // Handy: Auswahlmodus („Auswählen“ oben rechts), Liste gescrollt (kleiner Titel oben),
    // Ziehen zum Aktualisieren
    let mut select_mode = use_signal(|| false);
    let mut scrolled = use_signal(|| false);
    let mut pull = use_signal(|| 0.0f64);
    let mut pull_start = use_signal(|| None::<f64>);

    // Liste laden: neuer Filter → leeren und neu; gleicher Filter (Auffrischen) → im Hintergrund
    let q_load = query.clone();
    let drafts_view = mail_view == MailView::Drafts && !in_folder && !searching;
    use_effect(use_reactive!(|key, refresh, drafts_view| {
        let _ = refresh;
        let soft = *last_key.peek() == key;
        last_key.set(key.clone());
        *generation.write() += 1;
        let g = *generation.peek();
        let mut query = q_load.clone();
        if soft {
            query.limit = Some((items.peek().len() as i64).max(PAGE));
        } else {
            items.set(Vec::new());
            selected.set(HashSet::new());
            select_mode.set(false);
            cursor.set(None);
            total.set(0);
            error.set(None);
            loading.set(true);
        }
        spawn(async move {
            let res = api::get::<List>(&format!("/messages?{}", query.to_query())).await;
            if *generation.peek() != g {
                return;
            }
            loading.set(false);
            match res {
                Ok(list) => {
                    if *items.peek() != list.items {
                        items.set(list.items);
                    }
                    total.set(list.total);
                    error.set(None);
                }
                Err(e) if !soft => error.set(Some(e.message)),
                Err(_) => {}
            }
        });
        if drafts_view {
            spawn(async move {
                if let Ok(list) = api::get::<Vec<OutboxItem>>("/outbox").await {
                    outbox.set(list.into_iter().filter(|o| o.status != OutboxStatus::Sent).collect());
                }
            });
        }
    }));

    let q_more = query.clone();
    let load_more = move |_| {
        if loading_more() {
            return;
        }
        let mut query = q_more.clone();
        query.offset = Some(items.peek().len() as i64);
        let g = *generation.peek();
        spawn(async move {
            loading_more.set(true);
            let res = api::get::<List>(&format!("/messages?{}", query.to_query())).await;
            loading_more.set(false);
            if *generation.peek() != g {
                return;
            }
            match res {
                Ok(list) => {
                    let mut v = items.write();
                    for m in list.items {
                        if !v.iter().any(|x| x.id == m.id) {
                            v.push(m);
                        }
                    }
                    total.set(list.total);
                }
                Err(e) => toast_error(e),
            }
        });
    };

    // Aktion ausführen und Liste/Lesebereich nachführen
    let loc_act = loc.clone();
    let open_act = open.clone();
    let act = use_callback(move |(ids, action): (Vec<String>, MessageAction)| {
        let loc = loc_act.clone();
        let open_now = open_act.clone();
        spawn(async move {
            if ids.is_empty() {
                return;
            }
            if action == MessageAction::Delete {
                let n = ids.len();
                let ask = Ask::new(
                    if n == 1 { "Nachricht endgültig löschen?".to_string() } else { format!("{n} Nachrichten endgültig löschen?") },
                    "Sie verschwinden vom Server und aus dem lokalen Archiv. Das lässt sich nicht rückgängig machen.",
                )
                .confirm("Endgültig löschen")
                .destructive();
                if !confirm(ask).await {
                    return;
                }
            }
            // Für die Folge-Navigation: Position vor der Änderung
            let list_before = items.peek().clone();
            // Im Posteingang: Archivieren/Löschen mit „Rückgängig“ (wichtig fürs Wischen)
            let undo = matches!(action, MessageAction::Archive | MessageAction::Trash)
                && query_view == Some(MailView::Inbox)
                && loc.folder.is_empty();
            if !actions::apply_with(ids.clone(), action.clone(), !undo).await {
                return;
            }
            if undo {
                let back = ids.clone();
                toast_action(actions::done_text(&action, ids.len()).unwrap_or_default(), "Rückgängig", 6000, move || {
                    let back = back.clone();
                    spawn(async move {
                        if actions::apply_with(back, MessageAction::Restore, false).await {
                            toast("Zurückgelegt.");
                            state::refresh_list();
                        }
                    });
                });
            }
            let removes = actions::removes(&action, query_view, !loc.folder.is_empty());
            {
                let mut v = items.write();
                for m in v.iter_mut().filter(|m| ids.contains(&m.id)) {
                    match &action {
                        MessageAction::MarkRead => m.seen = true,
                        MessageAction::MarkUnread => m.seen = false,
                        MessageAction::Flag => m.flagged = true,
                        MessageAction::Unflag => m.flagged = false,
                        MessageAction::SetCategory { category } => m.category = *category,
                        _ => {}
                    }
                }
                if removes {
                    v.retain(|m| !ids.contains(&m.id));
                }
            }
            if removes {
                let removed = list_before.len() - items.peek().len();
                let before = *total.peek();
                total.set((before - removed as i64).max(0));
                selected.write().retain(|id| !ids.contains(id));
            }
            // Lesebereich nachführen
            if let Some(Ok(msgs)) = thread.write().as_mut() {
                for m in msgs.iter_mut().filter(|m| ids.contains(&m.summary.id)) {
                    match &action {
                        MessageAction::MarkRead => m.summary.seen = true,
                        MessageAction::MarkUnread => m.summary.seen = false,
                        MessageAction::Flag => m.summary.flagged = true,
                        MessageAction::Unflag => m.summary.flagged = false,
                        MessageAction::SetCategory { category } => m.summary.category = *category,
                        _ => {}
                    }
                }
            }
            let open_affected = !open_now.is_empty() && (ids.contains(&open_now) || thread_contains(&thread.peek(), &ids));
            if open_affected && (removes || action == MessageAction::MarkUnread) {
                // Zur nächsten Nachricht (darunter, sonst darüber) oder zurück zur Liste
                let pos = list_before.iter().position(|m| m.id == open_now);
                let remaining = items.peek().clone();
                let next = if action == MessageAction::MarkUnread {
                    None
                } else {
                    pos.and_then(|p| {
                        list_before[p + 1..]
                            .iter()
                            .chain(list_before[..p].iter().rev())
                            .find(|m| remaining.iter().any(|r| r.id == m.id))
                            .map(|m| m.id.clone())
                    })
                };
                cursor.set(next.clone());
                nav.replace(loc.route(next.as_deref().unwrap_or("")));
            }
        });
    });

    // Aktion auf Listeneinträge (bei Unterhaltungen auf die ganze Unterhaltung)
    let act_items = use_callback(move |(list, action): (Vec<MessageSummary>, MessageAction)| {
        spawn(async move {
            let ids = actions::expand(list, &action).await;
            act.call((ids, action));
        });
    });

    // Tastenkürzel der Seite
    let loc_keys = loc.clone();
    let open_keys = open.clone();
    let on_key = use_callback(move |k: String| {
        let list = items.peek().clone();
        let current = cursor.peek().clone().or_else(|| (!open_keys.is_empty()).then(|| open_keys.clone()));
        let pos = current.as_ref().and_then(|c| list.iter().position(|m| &m.id == c));
        let loaded = thread.peek().clone().and_then(Result::ok);
        let reading = !open_keys.is_empty();
        // Ziel für Aktionen: offene Unterhaltung, sonst Auswahl, sonst markierte Zeile
        let targets: Vec<MessageSummary> = {
            let sel = selected.peek();
            if !sel.is_empty() && !reading {
                list.iter().filter(|m| sel.contains(&m.id)).cloned().collect()
            } else {
                pos.map(|p| vec![list[p].clone()]).unwrap_or_default()
            }
        };
        let run = |action: MessageAction| {
            if reading {
                if let Some(msgs) = loaded.as_ref() {
                    act.call((reader::thread_ids(msgs, &open_keys, &action), action));
                }
            } else if !targets.is_empty() {
                act_items.call((targets.clone(), action));
            }
        };
        match k.as_str() {
            "j" | "k" => {
                if list.is_empty() {
                    return;
                }
                let next = match (pos, k.as_str()) {
                    (None, _) => 0,
                    (Some(p), "j") => (p + 1).min(list.len() - 1),
                    (Some(p), _) => p.saturating_sub(1),
                };
                let id = list[next].id.clone();
                cursor.set(Some(id.clone()));
                util::scroll_into_view(&format!("msg-{id}"));
                if reading {
                    nav.replace(loc_keys.route(&id));
                }
            }
            "Enter" => {
                if let Some(p) = pos {
                    nav.push(loc_keys.route(&list[p].id));
                }
            }
            "x" => {
                if let Some(p) = pos {
                    let id = list[p].id.clone();
                    let mut s = selected.write();
                    if !s.remove(&id) {
                        s.insert(id);
                    }
                }
            }
            "Escape" => {
                if reading {
                    nav.push(loc_keys.route(""));
                } else if !selected.peek().is_empty() {
                    selected.set(HashSet::new());
                }
            }
            "e" => run(if matches!(query_view, Some(MailView::Archive | MailView::Trash | MailView::Junk)) { MessageAction::Restore } else { MessageAction::Archive }),
            "#" => run(if query_view == Some(MailView::Trash) { MessageAction::Delete } else { MessageAction::Trash }),
            "u" => run(MessageAction::MarkUnread),
            "s" => {
                let flagged = if reading {
                    loaded.as_ref().is_some_and(|m| m.iter().any(|x| x.summary.flagged))
                } else {
                    targets.iter().all(|m| m.flagged) && !targets.is_empty()
                };
                run(if flagged { MessageAction::Unflag } else { MessageAction::Flag });
            }
            "r" | "a" | "f" => {
                if let Some(target) = loaded.as_deref().and_then(reader::reply_target) {
                    match k.as_str() {
                        "r" => compose::reply(target, false),
                        "a" => compose::reply(target, true),
                        _ => compose::forward(target),
                    }
                } else if let Some(p) = pos {
                    nav.push(loc_keys.route(&list[p].id));
                }
            }
            _ => {}
        }
    });
    use_hook(move || *PAGE_KEYS.write() = Some(on_key));
    use_drop(move || *PAGE_KEYS.write() = None);

    // Kopf
    let accounts = ACCOUNTS();
    let account_obj = accounts.iter().find(|a| a.id == account).cloned();
    let folder_obj = FOLDERS.read().iter().find(|f| f.id == folder).cloned();
    let title = if searching {
        format!("Suche: {}", q.trim())
    } else if let Some(f) = folder_obj.as_ref() {
        folder_label(f)
    } else {
        mail_view.label().to_string()
    };
    let subtitle = match (account_obj.as_ref(), folder_obj.as_ref()) {
        (Some(a), _) => Some(a.label().to_string()),
        (None, Some(f)) => accounts.iter().find(|a| a.id == f.account_id).map(|a| a.label().to_string()),
        _ => None,
    };
    let unread_badge = match (in_folder, mail_view) {
        (true, _) => folder_obj.as_ref().map(|f| f.unread).unwrap_or(0),
        (false, MailView::Inbox) => match account_obj.as_ref() {
            Some(a) => state::folder_with_role(&a.id, FolderRole::Inbox).map(|f| f.unread).unwrap_or(a.unread_count),
            None => state::unread_in_role(FolderRole::Inbox),
        },
        _ => 0,
    };
    let page_title = if unread_badge > 0 { format!("{title} ({unread_badge}) – nrzMail") } else { format!("{title} – nrzMail") };

    let list = items();
    let sel_count = selected.read().len();
    let all_selected = !list.is_empty() && list.iter().all(|m| selected.read().contains(&m.id));
    let reading = !open.is_empty();
    let sent_view = query_view == Some(MailView::Sent) || folder_obj.as_ref().is_some_and(|f| f.role == FolderRole::Sent);
    let loc_open = loc.clone();
    let loc_close = loc.clone();
    let no_accounts = *state::ACCOUNTS_LOADED.read() && accounts.is_empty();
    let selected_items: Vec<MessageSummary> = list.iter().filter(|m| selected.read().contains(&m.id)).cloned().collect();
    let selected_items_phone = selected_items.clone();
    let sync_account = account_obj.as_ref().map(|a| a.id.clone()).or_else(|| folder_obj.as_ref().map(|f| f.account_id.clone()));
    let restore_swipe = matches!(query_view, Some(MailView::Archive | MailView::Trash | MailView::Junk))
        || folder_obj.as_ref().is_some_and(|f| matches!(f.role, FolderRole::Archive | FolderRole::Trash | FolderRole::Junk));
    // Vorherige/nächste Nachricht für die Pfeile im Lesebereich (Handy)
    let open_pos = list.iter().position(|m| m.id == open);
    let prev_id = open_pos.and_then(|p| p.checked_sub(1)).map(|p| list[p].id.clone());
    let next_id = open_pos.and_then(|p| list.get(p + 1)).map(|m| m.id.clone());
    let loc_step = loc.clone();

    rsx! {
        document::Title { "{page_title}" }
        div { class: "flex min-h-0 flex-1",
            // Liste
            section {
                class: "flex min-h-0 min-w-0 flex-col border-r lg:w-[22rem] lg:shrink-0 xl:w-[26rem] 2xl:w-[30rem]",
                class: if reading { "hidden lg:flex" } else { "flex w-full" },
                "aria-label": "Nachrichten",
                if phone {
                    // Handy: schmale Leiste oben; der grosse Titel scrollt mit der Liste
                    div { class: "flex h-12 shrink-0 items-center gap-1 border-b px-2 transition-colors",
                        class: if scrolled() || select_mode() { "bg-background" } else { "border-transparent" },
                        if select_mode() {
                            button { class: "h-10 min-w-16 rounded-md px-2 text-left text-[15px] font-medium",
                                onclick: move |_| {
                                    if all_selected { selected.set(HashSet::new()); } else { selected.set(items.peek().iter().map(|m| m.id.clone()).collect()); }
                                },
                                if all_selected { "Keine" } else { "Alle" }
                            }
                        } else {
                            MobileMenuButton {}
                        }
                        div { class: "min-w-0 flex-1 text-center",
                            p { class: "truncate text-[15px] font-semibold transition-opacity duration-200",
                                class: if scrolled() || select_mode() { "opacity-100" } else { "opacity-0" },
                                if select_mode() {
                                    if sel_count == 0 { "Auswählen" } else { "{sel_count} ausgewählt" }
                                } else {
                                    "{title}"
                                }
                            }
                        }
                        if select_mode() {
                            button { class: "h-10 min-w-16 rounded-md px-2 text-right text-[15px] font-semibold",
                                onclick: move |_| { select_mode.set(false); selected.set(HashSet::new()); },
                                "Fertig"
                            }
                        } else {
                            button { class: "h-8 rounded-full bg-secondary px-3.5 text-sm font-medium text-secondary-foreground active:bg-accent disabled:opacity-40",
                                disabled: list.is_empty(),
                                onclick: move |_| select_mode.set(true),
                                "Auswählen"
                            }
                        }
                    }
                } else {
                    div { class: "flex h-14 shrink-0 items-center gap-2 border-b px-3",
                        MobileMenuButton {}
                        div { class: "min-w-0 flex-1",
                            h1 { class: "truncate text-[15px] font-semibold leading-tight", "{title}" }
                            p { class: "truncate text-xs text-muted-foreground",
                                if let Some(s) = subtitle.clone() { "{s} · " }
                                if loading() { "Lade …" } else { {util::plural(total(), "Nachricht", "Nachrichten")} }
                            }
                        }
                        SyncButton { account: sync_account.clone() }
                    }
                    SearchBar { loc: loc.clone() }
                    if tabs_on {
                        CategoryTabs { loc: loc.clone(), current: cat, start_all: prefs.inbox_start_all, refresh }
                    }
                }
                // Aktionsleiste bei Auswahl
                if sel_count > 0 && !phone {
                    BulkBar {
                        count: sel_count,
                        all_selected,
                        items: selected_items,
                        view: query_view,
                        on_all: move |on: bool| {
                            if on { selected.set(items.peek().iter().map(|m| m.id.clone()).collect()); } else { selected.set(HashSet::new()); }
                        },
                        on_act: move |(list, action): (Vec<MessageSummary>, MessageAction)| act_items.call((list, action)),
                    }
                }
                div { class: "min-h-0 flex-1 overflow-y-auto pb-20 max-md:pb-4 max-md:overscroll-y-none", id: "message-list",
                    onscroll: move |e| {
                        let y = e.data().scroll_top();
                        let s = y > 52.0;
                        if *scrolled.peek() != s {
                            scrolled.set(s);
                        }
                    },
                    // Ziehen zum Aktualisieren (Handy, nur ganz oben)
                    ontouchstart: move |e| {
                        if !phone || select_mode() {
                            return;
                        }
                        let top = util::scroll_top("message-list");
                        if top <= 0.0 {
                            if let Some(t) = e.touches().into_iter().next() {
                                pull_start.set(Some(t.client_coordinates().y));
                            }
                        }
                    },
                    ontouchmove: move |e| {
                        let Some(y0) = pull_start() else { return };
                        let touches = e.touches();
                        let Some(t) = touches.first() else { return };
                        let dy = t.client_coordinates().y - y0;
                        let before = *pull.peek() >= PULL_TRIGGER;
                        let v = if dy > 0.0 { (dy * 0.5).min(96.0) } else { 0.0 };
                        pull.set(v);
                        if before != (v >= PULL_TRIGGER) {
                            util::vibrate(8);
                        }
                    },
                    ontouchend: {
                        let account = sync_account.clone();
                        move |_| {
                            if pull_start.peek().is_some() && *pull.peek() >= PULL_TRIGGER {
                                sync_now(account.clone());
                            }
                            pull_start.set(None);
                            pull.set(0.0);
                        }
                    },
                    if phone {
                        div { class: "flex items-end justify-center overflow-hidden text-muted-foreground",
                            class: if pull_start().is_none() { "transition-[height] duration-200" } else { "" },
                            style: "height: {pull()}px",
                            span { class: "mb-2 grid size-8 place-items-center rounded-full bg-secondary",
                                class: if pull() >= PULL_TRIGGER { "text-foreground" } else { "" },
                                span { class: "grid place-items-center", style: "transform: rotate({pull() * 4.0}deg)",
                                    icons::RefreshCw { class: "size-4" }
                                }
                            }
                        }
                        div { class: "px-4 pb-2",
                            h1 { class: "truncate text-[32px] font-bold leading-tight tracking-tight", "{title}" }
                            p { class: "truncate text-[13px] text-muted-foreground",
                                if let Some(s) = subtitle.clone() { "{s} · " }
                                if loading() { "Lade …" } else if unread_badge > 0 { "{util::num(unread_badge)} ungelesen" } else { {util::plural(total(), "Nachricht", "Nachrichten")} }
                            }
                        }
                        SearchBar { loc: loc.clone() }
                        if tabs_on {
                            CategoryTabs { loc: loc.clone(), current: cat, start_all: prefs.inbox_start_all, refresh }
                        }
                    }
                    if drafts_view && !outbox().is_empty() {
                        OutboxSection { items: outbox(), on_deleted: move |id: String| outbox.write().retain(|o| o.id != id) }
                    }
                    if no_accounts {
                        div { class: "p-4",
                            EmptyState { title: "Noch kein Postfach", description: "Richte ein IMAP-Postfach ein oder lege ein Archiv-Postfach für Importe an.",
                                icon: rsx! { icons::Inbox {} },
                                Link { class: btn(BTN_PRIMARY), to: Route::Settings { section: "postfaecher".into() }, icons::Plus { class: "size-4" } "Postfach hinzufügen" }
                            }
                        }
                    } else if let Some(e) = error() {
                        div { class: "p-4", Callout { text: e, error: true } }
                    } else if loading() && list.is_empty() {
                        SkeletonRows {}
                    } else if list.is_empty() {
                        div { class: "p-4",
                            EmptyState {
                                title: empty_title(query_view, searching, cat),
                                description: empty_text(query_view, searching),
                                icon: empty_icon(searching),
                            }
                        }
                    } else {
                        MessageList {
                            items: list.clone(),
                            selected,
                            open_id: open.clone(),
                            cursor: cursor(),
                            compact: prefs.compact,
                            snippets: prefs.show_snippets,
                            sent_view,
                            show_category: !tabs_on,
                            select_mode: select_mode(),
                            swipe_left: if restore_swipe { SwipeLeft::Restore } else { SwipeLeft::Archive },
                            onswipe: move |(m, dir): (MessageSummary, Swipe)| {
                                let action = match dir {
                                    Swipe::Left if restore_swipe => MessageAction::Restore,
                                    Swipe::Left => MessageAction::Archive,
                                    Swipe::Right if m.seen => MessageAction::MarkUnread,
                                    Swipe::Right => MessageAction::MarkRead,
                                };
                                act_items.call((vec![m], action));
                            },
                            onopen: move |m: MessageSummary| {
                                cursor.set(Some(m.id.clone()));
                                nav.push(loc_open.route(&m.id));
                            },
                            onstar: move |m: MessageSummary| {
                                act.call((vec![m.id.clone()], if m.flagged { MessageAction::Unflag } else { MessageAction::Flag }));
                            },
                        }
                        if (list.len() as i64) < total() {
                            div { class: "flex justify-center p-4",
                                button { class: "{btn(BTN_OUTLINE)} {BTN_SM}", disabled: loading_more(), onclick: load_more,
                                    if loading_more() { icons::LoaderCircle { class: "size-4 animate-spin" } }
                                    "Mehr laden ({util::num(list.len() as i64)} von {util::num(total())})"
                                }
                            }
                        }
                    }
                }
                if phone {
                    if select_mode() {
                        BulkBar {
                            count: sel_count,
                            all_selected,
                            items: selected_items_phone,
                            view: query_view,
                            phone: true,
                            on_all: move |on: bool| {
                                if on { selected.set(items.peek().iter().map(|m| m.id.clone()).collect()); } else { selected.set(HashSet::new()); }
                            },
                            on_act: move |(list, action): (Vec<MessageSummary>, MessageAction)| {
                                act_items.call((list, action));
                                select_mode.set(false);
                            },
                        }
                    } else {
                        PhoneToolbar { account: sync_account.clone(), unread: unread_badge, total: total(), loading: loading() }
                    }
                }
            }
            // Lesebereich
            section { class: "min-h-0 min-w-0 flex-1",
                class: if reading { "flex flex-col" } else { "hidden lg:flex lg:flex-col" },
                "aria-label": "Lesebereich",
                if reading {
                    Reader {
                        key: "{open}",
                        id: open.clone(),
                        thread,
                        view: query_view,
                        on_act: move |(ids, action): (Vec<String>, MessageAction)| act.call((ids, action)),
                        on_close: move |_| { nav.push(loc_close.route("")); },
                        back_label: title.clone(),
                        on_prev: prev_id.clone().map(|id| { let loc = loc_step.clone(); EventHandler::new(move |_: ()| { cursor.set(Some(id.clone())); nav.replace(loc.route(&id)); }) }),
                        on_next: next_id.clone().map(|id| { let loc = loc_step.clone(); EventHandler::new(move |_: ()| { cursor.set(Some(id.clone())); nav.replace(loc.route(&id)); }) }),
                    }
                } else {
                    div { class: "grid flex-1 place-items-center p-6 text-center",
                        div { class: "grid justify-items-center gap-3 text-muted-foreground",
                            icons::MailOpen { class: "size-10 opacity-40" }
                            p { class: "text-sm", "Keine Nachricht ausgewählt" }
                            p { class: "text-xs",
                                crate::ui::kbd::Kbd { "j" } " / " crate::ui::kbd::Kbd { "k" } " zum Blättern, "
                                crate::ui::kbd::Kbd { "⌘K" } " für alle Befehle"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn empty_icon(searching: bool) -> Element {
    if searching { rsx! { icons::Search {} } } else { rsx! { icons::Inbox {} } }
}

fn thread_contains(t: &Loaded, ids: &[String]) -> bool {
    matches!(t, Some(Ok(msgs)) if msgs.iter().any(|m| ids.contains(&m.summary.id)))
}

fn empty_title(view: Option<MailView>, searching: bool, cat: Option<Category>) -> String {
    if searching {
        return "Keine Treffer".into();
    }
    match (view, cat) {
        (Some(MailView::Inbox), Some(Category::Primary) | None) => "Posteingang leer".into(),
        (Some(MailView::Inbox), Some(c)) => format!("Nichts unter „{}“", c.label()),
        (Some(MailView::Unread), _) => "Alles gelesen".into(),
        (Some(MailView::Starred), _) => "Keine markierten Nachrichten".into(),
        (Some(MailView::Junk), _) => "Kein Spam".into(),
        (Some(MailView::Trash), _) => "Papierkorb leer".into(),
        (Some(MailView::Drafts), _) => "Keine Entwürfe".into(),
        _ => "Keine Nachrichten".into(),
    }
}

fn empty_text(view: Option<MailView>, searching: bool) -> String {
    if searching {
        return "Andere Begriffe versuchen – die Suchhilfe (?) neben dem Suchfeld zeigt, was geht.".into();
    }
    match view {
        Some(MailView::Inbox) => "Alles erledigt. Neue Nachrichten erscheinen hier, sobald der Abgleich sie holt.".into(),
        Some(MailView::Starred) => "Mit dem Stern (s) markierte Nachrichten landen hier.".into(),
        _ => "Hier ist gerade nichts.".into(),
    }
}

/// Postfach (oder alle) jetzt abgleichen.
#[component]
fn SyncButton(account: Option<String>) -> Element {
    let syncing = match &account {
        Some(id) => ACCOUNTS.read().iter().any(|a| &a.id == id && a.syncing),
        None => ACCOUNTS.read().iter().any(|a| a.syncing),
    };
    rsx! {
        button { class: "{btn(BTN_GHOST)} {BTN_ICON} text-muted-foreground", title: "Jetzt abgleichen", "aria-label": "Jetzt abgleichen", disabled: syncing,
            onclick: move |_| sync_now(account.clone()),
            icons::RefreshCw { class: if syncing { "size-4 animate-spin" } else { "size-4" } }
        }
    }
}

/// Postfach (oder alle mit Abgleich) jetzt beim Server abgleichen.
fn sync_now(account: Option<String>) {
    spawn(async move {
        let ids: Vec<String> = match account {
            Some(id) => vec![id],
            None => ACCOUNTS.peek().iter().filter(|a| a.kind == AccountKind::Imap && a.sync_enabled).map(|a| a.id.clone()).collect(),
        };
        for id in ids {
            if let Err(e) = api::action(&format!("/accounts/{}/sync", api::enc(&id))).await {
                toast_error(e);
            }
        }
        state::load_accounts().await;
    });
}

/// „Gerade eben“, „vor 5 Min.“, sonst Uhrzeit bzw. Datum.
fn ago(ts: i64) -> String {
    let mins = ((js_sys::Date::now() / 1000.0 - ts as f64) / 60.0).floor() as i64;
    match mins {
        i64::MIN..=0 => "gerade eben".into(),
        1..=59 => format!("vor {mins} Min."),
        _ => util::relative(ts),
    }
}

/// Handy: Werkzeugleiste unten mit Dieter, Stand des Abgleichs und Verfassen – alles in
/// Daumenreichweite.
#[component]
fn PhoneToolbar(account: Option<String>, unread: i64, total: i64, loading: bool) -> Element {
    let ai = state::AI();
    let accounts = ACCOUNTS();
    let relevant: Vec<_> = accounts.iter().filter(|a| account.as_ref().is_none_or(|id| &a.id == id)).collect();
    let syncing = relevant.iter().any(|a| a.syncing);
    let last = relevant.iter().filter_map(|a| a.last_sync_at).max();
    let error = relevant.iter().find_map(|a| a.last_error.clone().filter(|e| !e.trim().is_empty()));
    let name = if ai.assistant_name.trim().is_empty() { "Dieter".to_string() } else { ai.assistant_name.trim().to_string() };
    let tool = "grid size-11 shrink-0 place-items-center rounded-xl text-foreground active:bg-accent [&_svg]:size-[22px]";
    rsx! {
        div { class: "box-content flex h-[3.25rem] shrink-0 items-center gap-1 border-t bg-background px-2 pb-safe",
            if ai.available {
                button { class: tool, title: "{name} fragen", "aria-label": "{name} fragen", onclick: move |_| assistant::open(),
                    icons::Sparkles {}
                }
            } else {
                div { class: "size-11 shrink-0" }
            }
            button { class: "min-w-0 flex-1 text-center leading-tight", "aria-label": "Jetzt abgleichen", disabled: syncing,
                onclick: move |_| sync_now(account.clone()),
                if syncing {
                    p { class: "flex items-center justify-center gap-1.5 text-xs font-medium",
                        icons::LoaderCircle { class: "size-3.5 animate-spin" }
                        "Wird abgeglichen …"
                    }
                } else {
                    p { class: "truncate text-xs font-medium",
                        if loading { "Lade …" } else if unread > 0 { "{util::num(unread)} ungelesen" } else { {util::plural(total, "Nachricht", "Nachrichten")} }
                    }
                    if let Some(e) = error {
                        p { class: "truncate text-[11px] text-destructive", title: "{e}", "Abgleich gestört" }
                    } else if let Some(t) = last {
                        p { class: "truncate text-[11px] text-muted-foreground", "Aktualisiert {ago(t)}" }
                    }
                }
            }
            button { class: tool, title: "Neue Nachricht", "aria-label": "Neue Nachricht", onclick: move |_| compose::open_new(),
                icons::SquarePen {}
            }
        }
    }
}

const SEARCH_HELP: &[(&str, &str)] = &[
    ("from:anna", "Absender (Name oder Adresse)"),
    ("to:hans@example.com", "Empfänger"),
    ("subject:rechnung", "im Betreff"),
    ("has:attachment", "mit Anhang"),
    ("is:unread", "ungelesen"),
    ("is:starred", "mit Stern"),
    ("is:sent", "von dir gesendet"),
    ("after:2024-01-01", "ab Datum"),
    ("before:2024-12-31", "vor Datum"),
    ("category:newsletter", "Kategorie (newsletter, social, notification)"),
    ("folder:Rechnungen", "in einem Ordner"),
    ("\"genau so\"", "genaue Wortfolge"),
    ("-werbung", "Wort ausschliessen"),
];

#[component]
fn SearchBar(loc: Loc) -> Element {
    let nav = use_navigator();
    let current = loc.q.clone();
    let mut text = use_signal(|| current.clone());
    let mut help = use_signal(|| false);
    // Suche in der Adresse geändert (z. B. Zurück) → Feld nachziehen
    use_effect(use_reactive!(|current| text.set(current)));
    let loc_submit = loc.clone();
    let loc_clear = loc.clone();
    let loc_help = loc.clone();
    let phone = state::PHONE();
    rsx! {
        form { class: if phone { "flex items-center gap-1 px-4 pb-2.5" } else { "flex items-center gap-1.5 border-b px-3 py-2" }, role: "search",
            onsubmit: move |e| {
                e.prevent_default();
                nav.push(loc_submit.with_q(text().trim()));
                if *state::PHONE.peek() {
                    document::eval("document.activeElement?.blur()");
                }
            },
            div { class: "relative min-w-0 flex-1",
                icons::Search { class: "pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" }
                input {
                    id: "mail-search",
                    class: if phone { tw_merge!(INPUT, "h-10 rounded-xl border-transparent bg-secondary pl-8 pr-8 dark:bg-secondary") } else { tw_merge!(INPUT, "h-8 pl-8 pr-8") },
                    r#type: "search",
                    enterkeyhint: "search",
                    "aria-label": "Nachrichten suchen",
                    placeholder: if phone { "Suchen" } else { "Suchen … (/)" },
                    value: "{text}",
                    oninput: move |e| text.set(e.value()),
                    onkeydown: move |e| if e.key() == Key::Escape {
                        e.prevent_default();
                        document::eval("document.activeElement?.blur()");
                    },
                }
                if !text().is_empty() {
                    button { r#type: "button", class: "absolute right-1.5 top-1/2 grid size-5 -translate-y-1/2 place-items-center rounded text-muted-foreground hover:text-foreground",
                        "aria-label": "Suche leeren",
                        onclick: move |_| {
                            text.set(String::new());
                            nav.push(loc_clear.with_q(""));
                        },
                        icons::X { class: "size-3.5" }
                    }
                }
            }
            button { r#type: "button", class: if phone { "grid size-10 shrink-0 place-items-center rounded-xl text-muted-foreground active:bg-accent" } else { "{btn(BTN_GHOST)} size-8 p-0 text-muted-foreground" }, title: "Suchhilfe", "aria-label": "Suchhilfe",
                onclick: move |_| help.set(true),
                icons::Info { class: "size-4" }
            }
        }
        Modal { open: help, title: "Suchhilfe", description: "Begriffe lassen sich frei kombinieren. Klick übernimmt ins Suchfeld.",
            div { class: "grid gap-1.5 text-sm",
                for (code , what) in SEARCH_HELP.iter() {
                    button { class: "flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-md px-2 py-1.5 text-left hover:bg-accent",
                        onclick: {
                            let loc = loc_help.clone();
                            move |_| {
                                let mut t = text();
                                if !t.is_empty() && !t.ends_with(' ') {
                                    t.push(' ');
                                }
                                t.push_str(code);
                                text.set(t.clone());
                                help.set(false);
                                // Platzhalter wie „anna“ will man meist ändern – nur übernehmen, nicht suchen
                                let _ = &loc;
                                util::focus("mail-search");
                            }
                        },
                        code { class: "rounded bg-muted px-1.5 py-0.5 font-mono text-xs", "{code}" }
                        span { class: "text-muted-foreground", "{what}" }
                    }
                }
            }
        }
    }
}

/// Reiter im Posteingang mit der Zahl ungelesener Nachrichten je Kategorie – so geht nichts
/// Neues in einem anderen Reiter unter. `start_all`: „Alle“ ist die Vorgabe (ohne Wert in der Adresse).
#[component]
fn CategoryTabs(loc: Loc, current: Option<Category>, start_all: bool, refresh: u64) -> Element {
    const CATS: [Category; 4] = [Category::Primary, Category::Newsletter, Category::Social, Category::Notification];
    let mut unread = use_signal(Vec::<(Category, i64)>::new);
    let account = loc.account.clone();
    use_effect(use_reactive!(|account, refresh| {
        let _ = refresh;
        let account = account.clone();
        spawn(async move {
            let queries = CATS.iter().map(|c| {
                let q = MessageQuery {
                    view: Some(MailView::Inbox),
                    account: some(&account),
                    category: Some(*c),
                    unread: Some(true),
                    limit: Some(1),
                    ..Default::default()
                };
                async move { (*c, api::get::<List>(&format!("/messages?{}", q.to_query())).await.map(|l| l.total).unwrap_or(0)) }
            });
            let counts = futures_util::future::join_all(queries).await;
            if *unread.peek() != counts {
                unread.set(counts);
            }
        });
    }));
    let counts = unread();
    let tabs: Vec<(String, &str, Option<Category>)> = CATS
        .into_iter()
        .map(|c| (c.as_str().to_string(), c.label(), Some(c)))
        .chain(std::iter::once(("all".to_string(), "Alle", None)))
        .collect();
    let tabs: Vec<_> = if start_all {
        // „Alle“ zuerst, dann die Kategorien
        let mut t = tabs;
        t.sort_by_key(|(k, ..)| if k == "all" { 0 } else { 1 + CATS.iter().position(|c| c.as_str() == k).unwrap_or(0) });
        t
    } else {
        tabs
    };
    if state::PHONE() {
        // Handy: runde Pillen, nur die gewählte mit Namen (spart Breite, bleibt eindeutig)
        return rsx! {
            nav { class: "no-scrollbar flex shrink-0 gap-2 overflow-x-auto px-4 pb-3", "aria-label": "Kategorien",
                for (key , label , c) in tabs {
                    {
                        let n = c.and_then(|c| counts.iter().find(|(x, _)| *x == c).map(|(_, n)| *n)).unwrap_or(0);
                        let active = c == current;
                        rsx! {
                            Link {
                                key: "{key}",
                                to: loc.with_category(if (start_all && key == "all") || (!start_all && key == "primary") { "" } else { &key }),
                                class: if active { "flex h-9 shrink-0 items-center gap-2 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground transition-all" } else { "relative grid h-9 w-12 shrink-0 place-items-center rounded-full bg-secondary text-secondary-foreground transition-all active:bg-accent" },
                                "aria-current": if active { "page" } else { "false" },
                                "aria-label": "{label}",
                                title: "{label}",
                                {category_icon_with(c, "size-[18px]")}
                                if active {
                                    "{label}"
                                    if n > 0 {
                                        span { class: "rounded-full bg-primary-foreground/15 px-1.5 text-[11px] leading-5 tabular-nums", {util::num(n)} }
                                    }
                                } else if n > 0 {
                                    span { class: "absolute -right-0.5 -top-1 min-w-[18px] rounded-full border-2 border-background bg-primary px-1 text-center text-[10px] font-bold leading-[14px] tabular-nums text-primary-foreground", {util::num(n)} }
                                }
                            }
                        }
                    }
                }
            }
        };
    }
    rsx! {
        nav { class: "no-scrollbar flex shrink-0 gap-1 overflow-x-auto border-b px-2 py-1.5", "aria-label": "Kategorien",
            for (key , label , c) in tabs {
                Link {
                    key: "{key}",
                    to: loc.with_category(if (start_all && key == "all") || (!start_all && key == "primary") { "" } else { &key }),
                    class: if c == current { "shrink-0 rounded-md bg-accent px-2.5 py-1 text-xs font-medium" } else { "shrink-0 rounded-md px-2.5 py-1 text-xs text-muted-foreground hover:bg-accent/50 hover:text-foreground" },
                    "aria-current": if c == current { "page" } else { "false" },
                    {category_icon(c)}
                    "{label}"
                    if let Some(n) = c.and_then(|c| counts.iter().find(|(x, _)| *x == c).map(|(_, n)| *n)).filter(|n| *n > 0) {
                        span { class: "ml-1.5 rounded-full bg-primary px-1.5 text-[10px] font-semibold leading-4 tabular-nums text-primary-foreground", {util::num(n)} }
                    }
                }
            }
        }
    }
}

fn category_icon(c: Option<Category>) -> Element {
    category_icon_with(c, "mr-1.5 inline size-3.5 align-[-2px]")
}

fn category_icon_with(c: Option<Category>, class: &'static str) -> Element {
    match c {
        Some(Category::Primary) => rsx! { icons::Inbox { class } },
        Some(Category::Newsletter) => rsx! { icons::Newspaper { class } },
        Some(Category::Social) => rsx! { icons::Users { class } },
        Some(Category::Notification) => rsx! { icons::Bell { class } },
        _ => rsx! { icons::Mails { class } },
    }
}

/// Aktionsleiste für ausgewählte Nachrichten.
#[component]
fn BulkBar(
    count: usize,
    all_selected: bool,
    items: Vec<MessageSummary>,
    view: Option<MailView>,
    on_all: EventHandler<bool>,
    on_act: EventHandler<(Vec<MessageSummary>, MessageAction)>,
    /// Handy: als Werkzeugleiste unten, gross und ohne Zähler (der steht oben)
    #[props(default)]
    phone: bool,
) -> Element {
    let tool = if phone {
        "grid size-11 place-items-center rounded-xl text-foreground active:bg-accent disabled:opacity-35 [&_svg]:size-[22px]"
    } else {
        "grid size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground [&_svg]:size-4"
    };
    let none = count == 0;
    let one_account = items.first().map(|m| m.account_id.clone()).filter(|a| items.iter().all(|m| &m.account_id == a));
    let targets: Vec<_> = match &one_account {
        Some(a) => FOLDERS.read().iter().filter(|f| &f.account_id == a).cloned().collect(),
        None => Vec::new(),
    };
    let all_flagged = items.iter().all(|m| m.flagged);
    let any_unread = items.iter().any(|m| !m.seen);
    let in_trash = view == Some(MailView::Trash);
    let restore = matches!(view, Some(MailView::Trash | MailView::Junk | MailView::Archive));
    let it = items.clone();
    let send = move |action: MessageAction| on_act.call((it.clone(), action));
    let (s1, s2, s3, s4, s5, s6, s7) = (send.clone(), send.clone(), send.clone(), send.clone(), send.clone(), send.clone(), send.clone());
    rsx! {
        div { class: if phone { "box-content flex h-[3.25rem] shrink-0 items-center justify-around border-t bg-background px-1 pb-safe" } else { "flex shrink-0 items-center gap-0.5 border-b bg-accent/30 px-2 py-1" },
            if !phone {
                Checkbox { checked: all_selected, onchange: move |v| on_all.call(v), label: "" }
                span { class: "mr-1 whitespace-nowrap text-xs font-medium tabular-nums", "{count} ausgewählt" }
                div { class: "flex-1" }
            }
            button { class: tool, disabled: none, title: if any_unread { "Als gelesen markieren" } else { "Als ungelesen markieren" },
                onclick: move |_| s1(if any_unread { MessageAction::MarkRead } else { MessageAction::MarkUnread }),
                if any_unread { icons::MailOpen {} } else { icons::Mail {} }
            }
            button { class: tool, disabled: none, title: if all_flagged { "Stern entfernen" } else { "Mit Stern markieren" },
                onclick: move |_| s2(if all_flagged { MessageAction::Unflag } else { MessageAction::Flag }),
                icons::Star {}
            }
            if restore {
                button { class: tool, disabled: none, title: "In den Posteingang", onclick: move |_| s3(MessageAction::Restore), icons::ArchiveRestore {} }
            } else {
                button { class: tool, disabled: none, title: "Archivieren", onclick: move |_| s3(MessageAction::Archive), icons::Archive {} }
            }
            if in_trash {
                button { class: "{tool} hover:text-destructive", disabled: none, title: "Endgültig löschen", onclick: move |_| s4(MessageAction::Delete), icons::Trash2 {} }
            } else {
                button { class: tool, disabled: none, title: "Löschen", onclick: move |_| s4(MessageAction::Trash), icons::Trash2 {} }
            }
            button { class: tool, disabled: none, title: if view == Some(MailView::Junk) { "Kein Spam" } else { "Spam" },
                onclick: move |_| s5(if view == Some(MailView::Junk) { MessageAction::NotSpam } else { MessageAction::Spam }),
                icons::ShieldAlert {}
            }
            Dropdown { align: Align::End, class: "max-h-80 w-60 overflow-y-auto",
                trigger_class: tool,
                trigger: rsx! { span { title: "Verschieben", icons::FolderInput {} } },
                MenuLabel { "Verschieben nach" }
                if targets.is_empty() {
                    div { class: "px-2 py-1.5 text-xs text-muted-foreground", "Nur möglich, wenn alle aus demselben Postfach sind." }
                }
                for f in targets {
                    MenuItem { onclick: { let s = s6.clone(); let id = f.id.clone(); move |_| s(MessageAction::Move { folder_id: id.clone() }) },
                        {crate::pages::layout::role_icon(f.role, false)}
                        span { class: "truncate", style: "padding-left: {f.depth * 10}px", {folder_label(&f)} }
                    }
                }
            }
            Dropdown { align: Align::End, class: "w-56",
                trigger_class: tool,
                trigger: rsx! { span { title: "Einteilen", icons::Tag {} } },
                MenuLabel { "Einteilen als" }
                for c in Category::ALL {
                    MenuItem { onclick: { let s = s7.clone(); move |_| s(MessageAction::SetCategory { category: c }) }, "{c.label()}" }
                }
            }
            if !phone {
                button { class: tool, title: "Auswahl aufheben (Esc)", onclick: move |_| on_all.call(false), icons::X {} }
            }
        }
    }
}

/// Entwürfe, geplante und fehlgeschlagene Nachrichten aus nrzMail (`GET /outbox`).
#[component]
fn OutboxSection(items: Vec<OutboxItem>, on_deleted: EventHandler<String>) -> Element {
    rsx! {
        div { class: "border-b",
            p { class: "px-4 pb-1 pt-3 text-xs font-medium uppercase tracking-wider text-muted-foreground", "In nrzMail" }
            for item in items {
                OutboxRow { key: "{item.id}", item, on_deleted }
            }
        }
    }
}

#[component]
fn OutboxRow(item: OutboxItem, on_deleted: EventHandler<String>) -> Element {
    let c = &item.compose;
    let to = if c.to.is_empty() { "(keine Empfänger)".to_string() } else { format_addresses(&c.to) };
    let subject = if c.subject.trim().is_empty() { "(kein Betreff)".to_string() } else { c.subject.clone() };
    let (label, tone) = match item.status {
        OutboxStatus::Scheduled => (
            format!("Geplant · {}", item.send_at.map(util::datetime).unwrap_or_default()),
            "text-foreground",
        ),
        OutboxStatus::Failed => ("Fehlgeschlagen".to_string(), "text-destructive"),
        OutboxStatus::Sending => ("Wird gesendet".to_string(), "text-muted-foreground"),
        _ => ("Entwurf".to_string(), "text-muted-foreground"),
    };
    let edit = item.clone();
    let id = item.id.clone();
    rsx! {
        div { class: "group flex items-start gap-3 px-4 py-2.5 hover:bg-accent/40",
            button { class: "grid min-w-0 flex-1 gap-0.5 text-left", onclick: move |_| compose::edit_outbox(&edit),
                span { class: "flex items-center gap-2 text-xs {tone}",
                    if item.status == OutboxStatus::Scheduled { icons::Clock { class: "size-3.5" } } else if item.status == OutboxStatus::Failed { icons::CircleAlert { class: "size-3.5" } } else { icons::FilePen { class: "size-3.5" } }
                    "{label}"
                }
                span { class: "truncate text-sm font-medium", "{subject}" }
                span { class: "truncate text-xs text-muted-foreground", "An: {to}" }
                if let Some(e) = item.error.clone().filter(|e| !e.trim().is_empty()) {
                    span { class: "text-xs text-destructive", "{e}" }
                }
            }
            button { class: "grid size-8 shrink-0 place-items-center rounded-md text-muted-foreground opacity-0 hover:bg-accent hover:text-destructive group-hover:opacity-100 focus-visible:opacity-100",
                title: "Löschen", "aria-label": "Entwurf löschen",
                onclick: move |_| {
                    let id = id.clone();
                    spawn(async move {
                        let ask = Ask::new("Entwurf löschen?", "Die Nachricht wird nicht gesendet.").confirm("Löschen").destructive();
                        if confirm(ask).await {
                            match api::delete(&format!("/outbox/{}", api::enc(&id))).await {
                                Ok(()) => { toast("Entwurf gelöscht."); on_deleted.call(id); }
                                Err(e) => toast_error(e),
                            }
                        }
                    });
                },
                icons::Trash2 { class: "size-4" }
            }
        }
    }
}
