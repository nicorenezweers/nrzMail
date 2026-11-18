//! Nachrichtenliste: Absender (fett = ungelesen), Betreff, Vorschau, Stern, Büroklammer,
//! Anzahl in der Unterhaltung, relatives Datum, Auswahl. Wird von der Mail-Ansicht und der
//! Archiv-Suche benutzt.

use crate::components::common::*;
use crate::state;
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{Category, MessageSummary};
use std::collections::HashSet;

#[derive(Props, Clone, PartialEq)]
pub struct ListProps {
    pub items: Vec<MessageSummary>,
    pub selected: Signal<HashSet<String>>,
    /// Gerade geöffnete Nachricht
    #[props(into, default)]
    pub open_id: String,
    /// Tastatur-Markierung (j/k)
    #[props(default)]
    pub cursor: Option<String>,
    #[props(default)]
    pub compact: bool,
    #[props(default = true)]
    pub snippets: bool,
    /// Gesendet-Ansicht: Empfänger statt Absender zeigen
    #[props(default)]
    pub sent_view: bool,
    /// Kategorie als Abzeichen zeigen (ausserhalb der Reiter)
    #[props(default)]
    pub show_category: bool,
    pub onopen: EventHandler<MessageSummary>,
    pub onstar: EventHandler<MessageSummary>,
    /// Wischen auf dem Touchscreen (nach links bzw. rechts); ohne: kein Wischen
    #[props(default)]
    pub onswipe: Option<EventHandler<(MessageSummary, Swipe)>>,
    /// Was Wischen nach links tut (Beschriftung und Farbe)
    #[props(default)]
    pub swipe_left: SwipeLeft,
    /// Auswahlmodus (Handy): Kästchen immer zeigen, Tippen wählt aus
    #[props(default)]
    pub select_mode: bool,
}

/// Richtung einer Wischgeste.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Swipe {
    Left,
    Right,
}

/// Aktion hinter Wischen nach links.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum SwipeLeft {
    #[default]
    Archive,
    Restore,
}

#[component]
pub fn MessageList(props: ListProps) -> Element {
    let many_accounts = state::ACCOUNTS.read().len() > 1;
    let any_selected = !props.selected.read().is_empty() || props.select_mode;
    // Handy: immer die zweizeilige Ansicht mit Bild – die kompakte braucht Breite
    let phone = state::PHONE();
    rsx! {
        div { role: "list", class: "divide-y divide-border/60",
            for m in props.items.iter().cloned() {
                Row {
                    key: "{m.id}",
                    open: m.id == props.open_id,
                    cursor: props.cursor.as_deref() == Some(m.id.as_str()),
                    checked: props.selected.read().contains(&m.id),
                    any_selected,
                    compact: props.compact && !phone,
                    phone,
                    select_mode: props.select_mode,
                    onswipe: props.onswipe,
                    swipe_left: props.swipe_left,
                    snippets: props.snippets,
                    sent_view: props.sent_view || m.is_sent,
                    show_account: many_accounts,
                    show_category: props.show_category,
                    selected: props.selected,
                    onopen: props.onopen,
                    onstar: props.onstar,
                    message: m,
                }
            }
        }
    }
}

#[component]
fn Row(
    message: MessageSummary,
    open: bool,
    cursor: bool,
    checked: bool,
    any_selected: bool,
    compact: bool,
    snippets: bool,
    sent_view: bool,
    show_account: bool,
    show_category: bool,
    selected: Signal<HashSet<String>>,
    onopen: EventHandler<MessageSummary>,
    onstar: EventHandler<MessageSummary>,
    phone: bool,
    select_mode: bool,
    onswipe: Option<EventHandler<(MessageSummary, Swipe)>>,
    swipe_left: SwipeLeft,
) -> Element {
    let m = message.clone();
    // Wischen: Versatz in px, Startpunkt, Richtung (0 offen, 1 waagrecht, 2 senkrecht)
    let mut dx = use_signal(|| 0.0f64);
    let mut start = use_signal(|| None::<(f64, f64)>);
    let mut axis = use_signal(|| 0u8);
    let mut dragging = use_signal(|| false);
    let mut swiped = use_signal(|| false);
    const TRIGGER: f64 = 96.0;
    let unread = !m.seen && !m.is_sent;
    let who = if sent_view {
        let names: Vec<&str> = m.to.iter().map(|a| a.label()).collect();
        if names.is_empty() { "(keine Empfänger)".to_string() } else { format!("An: {}", names.join(", ")) }
    } else {
        m.from.label().to_string()
    };
    let subject = if m.subject.trim().is_empty() { "(kein Betreff)".to_string() } else { m.subject.clone() };
    let bg = if open {
        "bg-accent"
    } else if checked {
        "bg-accent/50"
    } else if unread {
        "bg-card/40 group-hover:bg-accent/40"
    } else {
        "group-hover:bg-accent/40"
    };
    let ring = if cursor { "ring-1 ring-inset ring-ring" } else { "" };
    let weight = if unread { "font-semibold text-foreground" } else { "text-foreground/85" };
    let color = state::account_color(&m.account_id);
    let date_title = util::datetime(m.date);
    let id = format!("msg-{}", m.id);
    let check_visible = if any_selected || checked { "opacity-100" } else { "opacity-0 group-hover:opacity-100 focus-visible:opacity-100" };
    let m_open = m.clone();
    let m_key = m.clone();
    let m_star = m.clone();
    let m_swipe = m.clone();
    let mid = m.id.clone();
    let mid_tap = m.id.clone();
    let can_swipe = onswipe.is_some() && !select_mode;
    let offset = dx();
    let left_label = match swipe_left {
        SwipeLeft::Archive => "Archivieren",
        SwipeLeft::Restore => "Posteingang",
    };
    let armed = offset.abs() >= TRIGGER;
    let check_base = match (phone, any_selected) {
        (true, true) => "relative mt-2.5 grid size-6 shrink-0 place-items-center rounded-full border border-input".to_string(),
        (true, false) => "hidden".to_string(),
        _ => format!("relative mt-0.5 grid size-5 shrink-0 place-items-center rounded-[4px] border border-input transition-opacity {check_visible}"),
    };
    let category = (show_category && m.category != Category::Primary && !m.is_sent).then_some(m.category);

    rsx! {
        div {
            id: "{id}",
            class: "relative overflow-hidden",
            class: if can_swipe { "touch-pan-y" } else { "" },
            ontouchstart: move |e| {
                swiped.set(false);
                if !can_swipe {
                    return;
                }
                if let Some(t) = e.touches().into_iter().next() {
                    let p = t.client_coordinates();
                    start.set(Some((p.x, p.y)));
                    axis.set(0);
                    dragging.set(true);
                }
            },
            ontouchmove: move |e| {
                let Some((sx, sy)) = start() else { return };
                let touches = e.touches();
                let Some(t) = touches.first() else { return };
                let p = t.client_coordinates();
                let (ddx, ddy) = (p.x - sx, p.y - sy);
                if axis() == 0 {
                    if ddx.abs() > 12.0 && ddx.abs() > ddy.abs() * 1.3 {
                        axis.set(1);
                    } else if ddy.abs() > 10.0 {
                        axis.set(2);
                    }
                }
                if axis() == 1 {
                    // Über die Auslöseschwelle hinaus gebremst ziehen
                    let v = if ddx.abs() > TRIGGER { ddx.signum() * (TRIGGER + (ddx.abs() - TRIGGER) * 0.4) } else { ddx };
                    let before = dx().abs() >= TRIGGER;
                    dx.set(v);
                    if before != (v.abs() >= TRIGGER) {
                        crate::util::vibrate(8);
                    }
                }
            },
            ontouchend: move |_| {
                let v = dx();
                let horizontal = axis() == 1;
                start.set(None);
                dragging.set(false);
                axis.set(0);
                if !horizontal {
                    dx.set(0.0);
                    return;
                }
                swiped.set(true);
                let Some(h) = onswipe else { dx.set(0.0); return };
                let m = m_swipe.clone();
                if v <= -TRIGGER {
                    // Hinausschieben, dann ausführen (die Zeile verschwindet aus der Liste)
                    dx.set(-900.0);
                    spawn(async move {
                        gloo_timers::future::TimeoutFuture::new(170).await;
                        h.call((m, Swipe::Left));
                        gloo_timers::future::TimeoutFuture::new(400).await;
                        dx.set(0.0);
                    });
                } else if v >= TRIGGER {
                    dx.set(0.0);
                    h.call((m, Swipe::Right));
                } else {
                    dx.set(0.0);
                }
            },
            ontouchcancel: move |_| {
                start.set(None);
                dragging.set(false);
                axis.set(0);
                dx.set(0.0);
            },
            // Hintergrund beim Wischen
            if offset < 0.0 {
                div { class: if armed { "absolute inset-0 flex items-center justify-end gap-2 bg-success px-6 text-sm font-semibold text-success-foreground" } else { "absolute inset-0 flex items-center justify-end gap-2 bg-muted px-6 text-sm font-medium text-muted-foreground" },
                    if swipe_left == SwipeLeft::Restore { icons::ArchiveRestore { class: "size-5" } } else { icons::Archive { class: "size-5" } }
                    "{left_label}"
                }
            } else if offset > 0.0 {
                div { class: if armed { "absolute inset-0 flex items-center gap-2 bg-primary px-6 text-sm font-semibold text-primary-foreground" } else { "absolute inset-0 flex items-center gap-2 bg-muted px-6 text-sm font-medium text-muted-foreground" },
                    if unread { icons::MailOpen { class: "size-5" } } else { icons::Mail { class: "size-5" } }
                    if unread { "Gelesen" } else { "Ungelesen" }
                }
            }
        div {
            role: "listitem",
            class: "group relative isolate flex cursor-pointer items-start bg-background outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring {ring}",
            class: if dragging() { "" } else { "transition-transform duration-200 ease-out" },
            class: if compact { "gap-2 px-3 py-1.5" } else if phone { "gap-3 py-3 pl-4 pr-3" } else { "gap-2 px-3 py-2.5" },
            // Nie leer: Dioxus entfernt ein leer gewordenes style-Attribut nicht
            style: if offset != 0.0 { "transform: translateX({offset}px)" } else { "transform: none" },
            tabindex: "0",
            "aria-current": if open { "true" } else { "false" },
            "aria-label": "{who}: {subject}",
            onclick: move |_| {
                if swiped() {
                    swiped.set(false);
                    return;
                }
                if select_mode {
                    let mut s = selected.write();
                    if !s.remove(&mid_tap) {
                        s.insert(mid_tap.clone());
                    }
                    return;
                }
                onopen.call(m_open.clone())
            },
            onkeydown: move |e| {
                if e.key() == Key::Enter || e.key() == Key::Character(" ".into()) {
                    e.prevent_default();
                    onopen.call(m_key.clone());
                }
            },
            span { class: "pointer-events-none absolute inset-0 -z-10 transition-colors {bg}" }
            // Ungelesen-Punkt
            span { class: if unread { if phone { "absolute left-1.5 top-[1.6rem] size-2 rounded-full bg-primary" } else { "absolute left-1 top-1/2 size-1.5 -translate-y-1/2 rounded-full bg-primary" } } else { "hidden" } }
            // Auswahl
            button {
                class: "{check_base}",
                class: if checked { "border-primary bg-primary text-primary-foreground" } else { "" },
                role: "checkbox",
                "aria-checked": "{checked}",
                "aria-label": "Auswählen",
                onclick: move |e| {
                    e.stop_propagation();
                    let mut s = selected.write();
                    if !s.remove(&mid) {
                        s.insert(mid.clone());
                    }
                },
                if checked { icons::Check { class: "size-3.5" } }
            }
            // Bild des Absenders (Handy)
            if phone && !any_selected {
                Avatar { name: who.trim_start_matches("An: ").to_string(), class: "relative mt-0.5 size-10 text-[15px]" }
            }
            // Stern
            if !phone {
            button {
                class: if m.flagged { "mt-0.5 grid size-5 shrink-0 place-items-center text-foreground" } else { "mt-0.5 grid size-5 shrink-0 place-items-center text-muted-foreground/50 hover:text-foreground" },
                "aria-label": if m.flagged { "Stern entfernen" } else { "Mit Stern markieren" },
                "aria-pressed": "{m.flagged}",
                title: if m.flagged { "Stern entfernen (s)" } else { "Mit Stern markieren (s)" },
                onclick: move |e| {
                    e.stop_propagation();
                    onstar.call(m_star.clone());
                },
                if m.flagged {
                    icons::Star { class: "size-4 fill-current" }
                } else {
                    icons::Star { class: "size-4" }
                }
            }
            }
            if compact {
                div { class: "flex min-w-0 flex-1 items-center gap-2 text-sm",
                    if show_account { Dot { color: color.clone() } }
                    span { class: "w-40 shrink-0 truncate {weight}", "{who}" }
                    if m.thread_count > 1 {
                        span { class: "shrink-0 text-xs tabular-nums text-muted-foreground", "{m.thread_count}" }
                    }
                    span { class: "min-w-0 flex-1 truncate",
                        span { class: "{weight}", "{subject}" }
                        if snippets && !m.snippet.is_empty() {
                            span { class: "text-muted-foreground", " – {m.snippet}" }
                        }
                    }
                    if m.has_attachments { icons::Paperclip { class: "size-3.5 shrink-0 text-muted-foreground" } }
                    span { class: "shrink-0 text-xs tabular-nums text-muted-foreground", title: "{date_title}", {util::relative(m.date)} }
                }
            } else {
                div { class: "relative grid min-w-0 flex-1 gap-0.5",
                    div { class: if phone { "flex min-w-0 items-center gap-1.5 text-[15px]" } else { "flex min-w-0 items-center gap-2 text-sm" },
                        if show_account { Dot { color: color.clone() } }
                        span { class: "min-w-0 truncate {weight}", "{who}" }
                        if m.thread_count > 1 {
                            span { class: "shrink-0 rounded border px-1 text-[11px] leading-4 tabular-nums text-muted-foreground", title: "Nachrichten in der Unterhaltung", "{m.thread_count}" }
                        }
                        if m.answered {
                            span { title: "Beantwortet", icons::Reply { class: "size-3.5 shrink-0 text-muted-foreground" } }
                        }
                        span { class: "flex-1" }
                        if m.has_attachments { span { title: "Mit Anhang", icons::Paperclip { class: "size-3.5 shrink-0 text-muted-foreground" } } }
                        if !m.on_server { span { title: "Nur noch im lokalen Archiv", icons::Database { class: "size-3.5 shrink-0 text-muted-foreground" } } }
                        if phone && m.flagged { icons::Star { class: "size-3.5 shrink-0 fill-current text-warning" } }
                        span { class: if unread { "shrink-0 text-xs font-medium tabular-nums text-foreground" } else { "shrink-0 text-xs tabular-nums text-muted-foreground" },
                            title: "{date_title}", {util::relative(m.date)} }
                        if phone { icons::ChevronRight { class: "-mr-1 size-4 shrink-0 text-muted-foreground/60" } }
                    }
                    div { class: "flex min-w-0 items-center gap-2",
                        p { class: if phone { "min-w-0 flex-1 truncate text-[14px] {weight}" } else { "min-w-0 flex-1 truncate text-sm {weight}" }, "{subject}" }
                        if let Some(c) = category {
                            Pill { "{c.label()}" }
                        }
                    }
                    if snippets && !m.snippet.is_empty() {
                        p { class: if phone { "line-clamp-2 text-[14px] leading-snug text-muted-foreground" } else { "line-clamp-2 text-[13px] leading-snug text-muted-foreground" }, "{m.snippet}" }
                    }
                }
            }
        }
        }
    }
}
