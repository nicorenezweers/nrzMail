//! Rahmen: Anmeldung prüfen (AppLayout) und die Seitenleiste mit den vereinheitlichten
//! Ansichten, den Postfächern samt Ordnerbaum, Archiv/Statistik/Einstellungen und dem
//! Benutzermenü. Dazu global: Verfassen, Assistent, Befehlspalette, Tastenkürzel.

use crate::Route;
use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::components::{assistant, compose, keys, palette};
use crate::state::{self, ACCOUNTS, FOLDERS, ME, MOBILE_NAV, PREFS, SIDEBAR_COLLAPSED};
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{AuthStatus, Folder, FolderRole, MailAccount, MailView};
use std::collections::HashSet;

/// Lädt den angemeldeten Benutzer; ohne Anmeldung zur Anmeldeseite (bzw. Einrichtung).
#[component]
pub fn AppLayout() -> Element {
    let nav = use_navigator();
    let me = use_resource(move || async move { state::load_me().await });
    // Nicht angemeldet: Einrichtung oder Anmeldung (mit Rücksprung hierher)
    use_effect(move || {
        if let Some(None) = &*me.read() {
            spawn(async move {
                match api::get::<AuthStatus>("/auth/status").await {
                    Ok(s) if s.setup_required => {
                        nav.replace(Route::Setup {});
                    }
                    _ => {
                        let next = web_sys::window()
                            .map(|w| {
                                let l = w.location();
                                format!("{}{}", l.pathname().unwrap_or_default(), l.search().unwrap_or_default())
                            })
                            .unwrap_or_default();
                        nav.replace(Route::Login { next });
                    }
                }
            });
        }
    });
    match &*me.read() {
        Some(Some(_)) => rsx! { Shell {} },
        _ => rsx! { FullscreenLoading {} },
    }
}

#[component]
fn FullscreenLoading() -> Element {
    rsx! {
        div { class: "grid h-dvh place-items-center",
            div { class: "flex flex-col items-center gap-4",
                img { src: "/static/icon.png", class: "size-12 rounded-xl opacity-80", alt: "" }
                Loading {}
            }
        }
    }
}

/// Seitenleiste, Inhalt und alles, was über allen Seiten liegt.
#[component]
fn Shell() -> Element {
    let route = use_route::<Route>();
    let collapsed = SIDEBAR_COLLAPSED();

    // Zähler (Postfächer, Ordner) regelmässig auffrischen; läuft ein Abgleich, öfter. Dazu
    // nach der Einstellung „Postfach automatisch aktualisieren“ beim Server neue Mails anfragen
    // (nur solange die Seite sichtbar ist).
    use_future(move || async move {
        let mut last_sync = js_sys::Date::now();
        let mut last_load = js_sys::Date::now();
        loop {
            let syncing = ACCOUNTS.peek().iter().any(|a| a.syncing);
            gloo_timers::future::TimeoutFuture::new(if syncing { 3_000 } else { 5_000 }).await;
            let now = js_sys::Date::now();
            let every = f64::from(PREFS.peek().auto_refresh_seconds) * 1000.0;
            let mut force = syncing;
            if every > 0.0 && now - last_sync >= every && util::page_visible() {
                last_sync = now;
                if api::action("/sync").await.is_ok() {
                    gloo_timers::future::TimeoutFuture::new(1_500).await;
                    force = true;
                }
            }
            if !force && js_sys::Date::now() - last_load < 30_000.0 {
                continue;
            }
            last_load = js_sys::Date::now();
            if state::load_accounts().await {
                state::refresh_list();
            }
        }
    });
    keys::use_shortcuts();

    // Seitenleiste auf dem Handy nach jedem Seitenwechsel schliessen
    use_effect(use_reactive!(|route| {
        let _ = route;
        if *MOBILE_NAV.peek() {
            *MOBILE_NAV.write() = false;
        }
    }));

    let width = if collapsed { "md:w-[3.75rem]" } else { "md:w-64" };
    rsx! {
        div { class: "flex h-dvh overflow-hidden pt-safe px-safe",
            aside { class: "hidden shrink-0 border-r border-sidenav-border bg-sidenav transition-[width] duration-200 md:flex md:flex-col {width}",
                "aria-label": "Navigation",
                Sidebar { collapsed }
            }
            if MOBILE_NAV() {
                div { class: "fixed inset-0 z-50 bg-black/60 animate-fade-in md:hidden", onclick: move |_| *MOBILE_NAV.write() = false }
                aside { class: "fixed inset-y-0 left-0 z-50 flex w-[min(20rem,86vw)] flex-col border-r bg-sidenav pt-safe pb-safe shadow-xl animate-drawer-in md:hidden",
                    "aria-label": "Navigation",
                    Sidebar { collapsed: false }
                }
            }
            main { class: "flex min-w-0 flex-1 flex-col", Outlet::<Route> {} }
        }
        compose::ComposeHost {}
        assistant::Assistant {}
        palette::Palette {}
        keys::ShortcutHelp {}
    }
}

/// Knopf, der auf dem Handy die Seitenleiste öffnet.
#[component]
pub fn MobileMenuButton() -> Element {
    rsx! {
        button { class: "{btn(BTN_GHOST)} {BTN_ICON} md:hidden max-md:-ml-1 max-md:size-10", title: "Navigation öffnen", "aria-label": "Navigation öffnen",
            onclick: move |_| *MOBILE_NAV.write() = true,
            icons::Menu { class: "size-5" }
        }
    }
}

fn view_icon(v: MailView) -> Element {
    match v {
        MailView::Inbox => rsx! { icons::Inbox {} },
        MailView::Unread => rsx! { icons::Mail {} },
        MailView::Starred => rsx! { icons::Star {} },
        MailView::Sent => rsx! { icons::Send {} },
        MailView::Drafts => rsx! { icons::FilePen {} },
        MailView::Archive => rsx! { icons::Archive {} },
        MailView::Junk => rsx! { icons::ShieldAlert {} },
        MailView::Trash => rsx! { icons::Trash2 {} },
        MailView::All => rsx! { icons::Mails {} },
    }
}

pub fn role_icon(r: FolderRole, open: bool) -> Element {
    match r {
        FolderRole::Inbox => rsx! { icons::Inbox {} },
        FolderRole::Sent => rsx! { icons::Send {} },
        FolderRole::Drafts => rsx! { icons::FilePen {} },
        FolderRole::Trash => rsx! { icons::Trash2 {} },
        FolderRole::Junk => rsx! { icons::ShieldAlert {} },
        FolderRole::Archive => rsx! { icons::Archive {} },
        FolderRole::Other if open => rsx! { icons::FolderOpen {} },
        FolderRole::Other => rsx! { icons::Folder {} },
    }
}

/// Name eines Ordners für die Anzeige: Rollen deutsch, sonst der Name vom Server.
pub fn folder_label(f: &Folder) -> String {
    match f.role {
        FolderRole::Other => f.name.clone(),
        r if f.depth == 0 => r.label().to_string(),
        _ => f.name.clone(),
    }
}

const ITEM: &str = "flex w-full items-center gap-2.5 rounded-md px-2.5 py-1.5 text-sm transition-colors max-md:py-2.5 max-md:text-[15px] outline-none focus-visible:ring-2 focus-visible:ring-ring/60 [&_svg]:size-4 [&_svg]:shrink-0";
const ITEM_ACTIVE: &str = "bg-sidenav-accent font-medium text-sidenav-accent-foreground";
const ITEM_IDLE: &str = "text-muted-foreground hover:bg-sidenav-accent/60 hover:text-sidenav-accent-foreground";

#[component]
fn NavLink(
    to: Route,
    #[props(into)] label: String,
    icon: Element,
    active: bool,
    collapsed: bool,
    #[props(default)] badge: i64,
    #[props(default)] muted_badge: bool,
) -> Element {
    let state = if active { ITEM_ACTIVE } else { ITEM_IDLE };
    let center = if collapsed { "justify-center px-0" } else { "" };
    let badge_class = if muted_badge {
        "text-[11px] tabular-nums text-muted-foreground"
    } else {
        "rounded-full bg-primary px-1.5 text-[11px] font-semibold leading-5 tabular-nums text-primary-foreground"
    };
    let title = if badge > 0 { format!("{label} ({badge})") } else { label.clone() };
    rsx! {
        Link { to, class: "{ITEM} {state} {center} relative", title: "{title}",
            "aria-current": if active { "page" } else { "false" },
            {icon}
            if !collapsed {
                span { class: "flex-1 truncate", "{label}" }
                if badge > 0 {
                    span { class: "{badge_class}", {util::num(badge)} }
                }
            } else if badge > 0 && !muted_badge {
                span { class: "absolute right-1.5 top-1 size-1.5 rounded-full bg-primary" }
            }
        }
    }
}

#[component]
fn Sidebar(collapsed: bool) -> Element {
    let route = use_route::<Route>();
    let accounts = ACCOUNTS();
    let folders = FOLDERS();
    let (cur_view, cur_account, cur_folder) = match &route {
        Route::Mail { query } => (MailView::parse(&query.view).unwrap_or_default(), query.account.clone(), query.folder.clone()),
        _ => (MailView::Inbox, "-".into(), "-".into()),
    };
    let on_mail = matches!(route, Route::Mail { .. });
    let current = state::current_account();
    let unread_total: i64 = accounts.iter().filter(|a| current.is_empty() || a.id == current).map(|a| a.unread_count).sum();
    let cur = current.clone();
    let badge = move |v: MailView| -> (i64, bool) {
        match v {
            MailView::Inbox => (state::unread_in_role_of(FolderRole::Inbox, &cur), false),
            MailView::Unread => (unread_total, true),
            MailView::Drafts => (state::total_in_role_of(FolderRole::Drafts, &cur), true),
            MailView::Junk => (state::unread_in_role_of(FolderRole::Junk, &cur), true),
            _ => (0, true),
        }
    };
    // Postfächer in der Seitenleiste: mit Auswahl nur das gewählte
    let shown_accounts: Vec<MailAccount> = accounts.iter().filter(|a| current.is_empty() || a.id == current).cloned().collect();
    let mut collapsed_folders = use_signal(|| {
        util::storage_get("nrzmail.collapsed")
            .and_then(|s| serde_json::from_str::<HashSet<String>>(&s).ok())
            .unwrap_or_default()
    });
    let toggle = move |id: String| {
        let mut set = collapsed_folders.write();
        if !set.remove(&id) {
            set.insert(id);
        }
        util::storage_set("nrzmail.collapsed", &serde_json::to_string(&*set).unwrap_or_default());
    };
    let is_page = |r: &Route| std::mem::discriminant(r) == std::mem::discriminant(&route);

    rsx! {
        // Postfach-Auswahl und Einklappen
        div { class: if collapsed { "flex h-14 shrink-0 items-center justify-center" } else { "flex h-14 shrink-0 items-center gap-1.5 px-3" },
            if !collapsed {
                AccountSwitcher { current: current.clone() }
            }
            button { class: "hidden size-8 place-items-center rounded-md text-muted-foreground hover:bg-sidenav-accent hover:text-foreground md:grid",
                title: if collapsed { "Seitenleiste ausklappen" } else { "Seitenleiste einklappen" },
                "aria-label": if collapsed { "Seitenleiste ausklappen" } else { "Seitenleiste einklappen" },
                onclick: move |_| {
                    let next = !*SIDEBAR_COLLAPSED.peek();
                    *SIDEBAR_COLLAPSED.write() = next;
                    util::storage_set("nrzmail.sidebar", if next { "collapsed" } else { "open" });
                },
                if collapsed { icons::PanelLeftOpen { class: "size-4" } } else { icons::PanelLeftClose { class: "size-4" } }
            }
        }
        div { class: if collapsed { "px-2 pb-2" } else { "px-3 pb-3" },
            button {
                class: if collapsed { format!("{} {BTN_ICON} w-full", btn(BTN_PRIMARY)) } else { format!("{} w-full justify-start", btn(BTN_PRIMARY)) },
                title: "Neue Nachricht (c)",
                onclick: move |_| compose::open_new(),
                icons::SquarePen { class: "size-4" }
                if !collapsed {
                    span { class: "flex-1 text-left", "Neue Nachricht" }
                    crate::ui::kbd::Kbd { class: "bg-primary-foreground/10 text-primary-foreground/70 max-md:hidden", "C" }
                }
            }
        }

        div { class: if collapsed { "min-h-0 flex-1 overflow-y-auto px-2 pb-3" } else { "min-h-0 flex-1 overflow-y-auto px-3 pb-3" },
            nav { class: "flex flex-col gap-0.5", "aria-label": "Ansichten",
                for v in MailView::ALL {
                    {
                        let (n, muted) = badge(v);
                        rsx! {
                            NavLink { key: "{v.as_str()}", to: Route::mail(v.as_str(), &current, "", "", "", ""), label: v.label(), icon: view_icon(v), collapsed,
                                active: on_mail && cur_view == v && cur_account == current && cur_folder.is_empty(),
                                badge: n, muted_badge: muted,
                            }
                        }
                    }
                }
            }

            if !accounts.is_empty() {
                if collapsed {
                    div { class: "mt-4 flex flex-col items-center gap-1 border-t border-sidenav-border pt-3",
                        for a in accounts.iter().cloned() {
                            Link { key: "{a.id}",
                                to: Route::account_inbox(&a.id),
                                onclick: { let id = a.id.clone(); move |_| state::set_current_account(&id) },
                                class: "grid size-9 place-items-center rounded-md hover:bg-sidenav-accent",
                                title: "{a.label()}",
                                Dot { color: a.color.clone(), class: "size-2.5" }
                            }
                        }
                    }
                } else {
                    p { class: "mt-5 px-2.5 pb-1 text-xs font-medium uppercase tracking-wider text-muted-foreground",
                        if current.is_empty() { "Postfächer" } else { "Ordner" }
                    }
                    for a in shown_accounts.iter().cloned() {
                        AccountTree { key: "{a.id}",
                            folders: folders.iter().filter(|f| f.account_id == a.id).cloned().collect::<Vec<_>>(),
                            active_inbox: on_mail && cur_account == a.id && cur_folder.is_empty(),
                            active_folder: if on_mail { cur_folder.clone() } else { String::new() },
                            collapsed: collapsed_folders(),
                            on_toggle: toggle,
                            account: a,
                        }
                    }
                }
            } else if !collapsed && *state::ACCOUNTS_LOADED.read() {
                Link { to: Route::Settings { section: "postfaecher".into() },
                    class: "mt-5 flex items-center gap-2 rounded-md border border-dashed px-2.5 py-2 text-sm text-muted-foreground hover:bg-sidenav-accent/60 hover:text-foreground",
                    icons::Plus { class: "size-4" }
                    "Postfach hinzufügen"
                }
            }
        }

        div { class: if collapsed { "flex flex-col gap-0.5 border-t border-sidenav-border px-2 py-2" } else { "flex flex-col gap-0.5 border-t border-sidenav-border px-3 py-2" },
            NavLink { to: Route::Archive {}, label: "Archiv", icon: rsx! { icons::Database {} }, collapsed, active: is_page(&Route::Archive {}) }
            NavLink { to: Route::Stats {}, label: "Statistik", icon: rsx! { icons::ChartColumn {} }, collapsed, active: is_page(&Route::Stats {}) }
            NavLink { to: Route::Settings { section: "konto".into() }, label: "Einstellungen", icon: rsx! { icons::Settings {} }, collapsed,
                active: is_page(&Route::Settings { section: String::new() }) }
        }
        div { class: if collapsed { "border-t border-sidenav-border p-2" } else { "border-t border-sidenav-border p-3" },
            UserMenu { collapsed }
        }
    }
}

/// Oben links: Postfach wählen. „Alle Postfächer“ zeigt alles zusammen, ein Postfach trennt
/// Ansichten, Zähler und Ordner von den anderen.
#[component]
fn AccountSwitcher(current: String) -> Element {
    let nav = use_navigator();
    let accounts = ACCOUNTS();
    let selected = accounts.iter().find(|a| a.id == current).cloned();
    let unread_all: i64 = accounts.iter().map(|a| a.unread_count).sum();
    let pick = move |id: String| {
        state::set_current_account(&id);
        nav.push(if id.is_empty() { Route::view(MailView::Inbox) } else { Route::account_inbox(&id) });
    };
    rsx! {
        Dropdown { class: "w-72", 
            trigger_class: "flex min-w-0 flex-1 items-center gap-2.5 rounded-md px-1.5 py-1 text-left outline-none hover:bg-sidenav-accent focus-visible:ring-2 focus-visible:ring-ring/60",
            trigger: rsx! {
                match selected.as_ref() {
                    Some(a) => rsx! {
                        span { class: "grid size-7 shrink-0 place-items-center rounded-lg bg-sidenav-accent", Dot { color: a.color.clone(), class: "size-2.5" } }
                        span { class: "min-w-0 flex-1",
                            span { class: "block truncate text-sm font-semibold leading-tight", "{a.label()}" }
                            span { class: "block truncate text-[11px] text-muted-foreground", "{a.email}" }
                        }
                    },
                    None => rsx! {
                        img { src: "/static/icon.png", class: "size-7 shrink-0 rounded-lg", alt: "" }
                        span { class: "min-w-0 flex-1",
                            span { class: "block truncate text-sm font-semibold leading-tight", "nrzMail" }
                            span { class: "block truncate text-[11px] text-muted-foreground", "Alle Postfächer" }
                        }
                    },
                }
                icons::ChevronsUpDown { class: "size-4 shrink-0 text-muted-foreground" }
            },
            MenuLabel { "Postfach" }
            MenuItem { active: current.is_empty(), onclick: { let pick = pick.clone(); move |_| pick(String::new()) },
                icons::Mails {}
                span { class: "flex-1", "Alle Postfächer" }
                if unread_all > 0 { span { class: "text-xs tabular-nums text-muted-foreground", {util::num(unread_all)} } }
            }
            for a in accounts.iter().cloned() {
                MenuItem { key: "{a.id}", active: a.id == current, onclick: { let pick = pick.clone(); let id = a.id.clone(); move |_| pick(id.clone()) },
                    Dot { color: a.color.clone() }
                    span { class: "min-w-0 flex-1",
                        span { class: "block truncate", "{a.label()}" }
                        span { class: "block truncate text-xs text-muted-foreground", "{a.email}" }
                    }
                    if a.unread_count > 0 { span { class: "text-xs tabular-nums text-muted-foreground", {util::num(a.unread_count)} } }
                }
            }
            MenuSeparator {}
            MenuItem { onclick: move |_| { nav.push(Route::Settings { section: "postfaecher".into() }); }, icons::Plus {} "Postfach hinzufügen" }
        }
    }
}

/// Ordner eines Postfachs als flache Liste in Baum-Reihenfolge: (Ordner, Tiefe, hat Kinder).
/// Unterordner eingeklappter Ordner fehlen.
fn tree(folders: &[Folder], collapsed: &HashSet<String>) -> Vec<(Folder, u32, bool)> {
    let paths: HashSet<&str> = folders.iter().map(|f| f.path.as_str()).collect();
    let is_root = |f: &Folder| f.parent_path.as_deref().is_none_or(|p| !paths.contains(p));
    fn rank(f: &Folder) -> u8 {
        match f.role {
        FolderRole::Inbox => 0,
        FolderRole::Drafts => 1,
        FolderRole::Sent => 2,
        FolderRole::Archive => 3,
        FolderRole::Junk => 4,
        FolderRole::Trash => 5,
        FolderRole::Other => 6,
        }
    }
    fn sorted(mut v: Vec<&Folder>) -> Vec<&Folder> {
        v.sort_by_key(|a| (rank(a), a.name.to_lowercase()));
        v
    }
    fn walk<'a>(
        node: &'a Folder,
        depth: u32,
        all: &'a [Folder],
        collapsed: &HashSet<String>,
        out: &mut Vec<(Folder, u32, bool)>,
    ) {
        let children = sorted(all.iter().filter(|c| c.parent_path.as_deref() == Some(node.path.as_str())).collect());
        out.push((node.clone(), depth, !children.is_empty()));
        if !collapsed.contains(&node.id) {
            for c in children {
                walk(c, depth + 1, all, collapsed, out);
            }
        }
    }
    let mut out = Vec::new();
    for root in sorted(folders.iter().filter(|f| is_root(f)).collect()) {
        walk(root, 0, folders, collapsed, &mut out);
    }
    out
}

#[component]
fn AccountTree(
    account: MailAccount,
    folders: Vec<Folder>,
    active_inbox: bool,
    active_folder: String,
    collapsed: HashSet<String>,
    on_toggle: EventHandler<String>,
) -> Element {
    let key = format!("account:{}", account.id);
    let open = !collapsed.contains(&key);
    let rows = if open { tree(&folders, &collapsed) } else { Vec::new() };
    let state = if active_inbox { ITEM_ACTIVE } else { ITEM_IDLE };
    let error = account.last_error.clone().filter(|e| !e.trim().is_empty());
    let key2 = key.clone();
    rsx! {
        div { class: "mt-0.5",
            div { class: "group flex items-center",
                button { class: "grid size-6 shrink-0 place-items-center rounded text-muted-foreground hover:text-foreground",
                    "aria-label": if open { "Ordner einklappen" } else { "Ordner ausklappen" },
                    "aria-expanded": "{open}",
                    onclick: move |_| on_toggle.call(key2.clone()),
                    if open { icons::ChevronDown { class: "size-3.5" } } else { icons::ChevronRight { class: "size-3.5" } }
                }
                Link {
                    to: Route::account_inbox(&account.id),
                    class: "{ITEM} {state} min-w-0 flex-1 pl-1.5",
                    title: "{account.email}",
                    Dot { color: account.color.clone() }
                    span { class: "min-w-0 flex-1 truncate", "{account.label()}" }
                    if account.syncing {
                        icons::RefreshCw { class: "size-3.5 animate-spin text-muted-foreground" }
                    } else if let Some(e) = error {
                        span { title: "{e}", icons::CircleAlert { class: "size-3.5 text-destructive" } }
                    }
                    if account.unread_count > 0 {
                        span { class: "text-[11px] tabular-nums text-muted-foreground", {util::num(account.unread_count)} }
                    }
                }
            }
            if open {
                div { class: "mt-0.5 flex flex-col gap-px",
                    for (f , depth , has_children) in rows {
                        FolderRow { key: "{f.id}", active: f.id == active_folder, depth, has_children,
                            is_collapsed: collapsed.contains(&f.id), on_toggle, folder: f }
                    }
                }
            }
        }
    }
}

#[component]
fn FolderRow(folder: Folder, depth: u32, has_children: bool, is_collapsed: bool, active: bool, on_toggle: EventHandler<String>) -> Element {
    let state = if active { ITEM_ACTIVE } else { ITEM_IDLE };
    let pad = 20 + depth * 14;
    let id = folder.id.clone();
    rsx! {
        div { class: "relative",
            if has_children {
                button { class: "absolute top-1/2 z-10 grid size-5 -translate-y-1/2 place-items-center rounded text-muted-foreground hover:text-foreground",
                    style: "left: {pad - 18}px",
                    "aria-label": if is_collapsed { "Unterordner zeigen" } else { "Unterordner verbergen" },
                    "aria-expanded": "{!is_collapsed}",
                    onclick: move |_| on_toggle.call(id.clone()),
                    if is_collapsed { icons::ChevronRight { class: "size-3" } } else { icons::ChevronDown { class: "size-3" } }
                }
            }
            Link { to: Route::folder(&folder.account_id, &folder.id),
                class: "{ITEM} {state} py-1",
                style: "padding-left: {pad}px",
                title: "{folder.path}",
                {role_icon(folder.role, active)}
                span { class: "min-w-0 flex-1 truncate", {folder_label(&folder)} }
                if folder.unread > 0 {
                    span { class: "text-[11px] font-medium tabular-nums text-foreground/80", {util::num(folder.unread)} }
                }
            }
        }
    }
}

#[component]
fn UserMenu(collapsed: bool) -> Element {
    let nav = use_navigator();
    let me = ME().unwrap_or_else(|| nrzmail_api::User {
        id: String::new(),
        username: String::new(),
        display_name: String::new(),
        email: String::new(),
        is_admin: false,
        disabled: false,
        language: "de".into(),
        created_at: 0,
        last_login_at: None,
    });
    let label = me.label().to_string();
    rsx! {
        Dropdown { side: Side::Top, class: "w-60",
            trigger_class: if collapsed { "grid w-full place-items-center rounded-lg py-1 hover:bg-sidenav-accent" } else { "flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left hover:bg-sidenav-accent" },
            trigger: rsx! {
                Avatar { name: label.clone(), class: "size-8" }
                if !collapsed {
                    span { class: "min-w-0 flex-1",
                        span { class: "block truncate text-sm font-medium", "{label}" }
                        span { class: "block truncate text-xs text-muted-foreground", if me.is_admin { "Administrator" } else { "{me.username}" } }
                    }
                    icons::ChevronsUpDown { class: "size-4 text-muted-foreground" }
                }
            },
            MenuLabel { if me.email.is_empty() { "{me.username}" } else { "{me.email}" } }
            MenuItem { onclick: move |_| { nav.push(Route::Settings { section: "konto".into() }); }, icons::User {} "Konto & Darstellung" }
            MenuItem { onclick: move |_| { nav.push(Route::Settings { section: "postfaecher".into() }); }, icons::Server {} "Postfächer" }
            MenuSeparator {}
            MenuItem { onclick: move |_| palette::open(),
                icons::Command {}
                span { class: "flex-1", "Befehlspalette" }
                crate::ui::kbd::Kbd { "⌘K" }
            }
            MenuItem { onclick: move |_| keys::open_help(),
                icons::Keyboard {}
                span { class: "flex-1", "Tastenkürzel" }
                crate::ui::kbd::Kbd { "?" }
            }
            MenuSeparator {}
            MenuItem { onclick: move |_| logout(), icons::LogOut {} "Abmelden" }
        }
    }
}

/// Abmelden und zur Anmeldeseite.
pub fn logout() {
    spawn(async move {
        let _ = api::action("/auth/logout").await;
        *ME.write() = None;
        if let Some(w) = web_sys::window() {
            let _ = w.location().assign("/login");
        }
    });
}
