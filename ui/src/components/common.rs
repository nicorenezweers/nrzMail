//! Gemeinsame Bausteine im Stil von rust-ui: Feld mit Beschriftung, Knöpfe, Seitenkopf,
//! leerer Zustand, Ladeanzeige, Schalter, Postfach-Farbpunkt.

use dioxus::prelude::*;
use tw_merge::tw_merge;

pub const INPUT: &str = "flex h-9 w-full min-w-0 rounded-md border border-input bg-transparent px-3 py-1 text-base shadow-xs outline-none transition-[color,box-shadow] placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 md:text-sm dark:bg-input/30";
pub const TEXTAREA: &str = "flex min-h-16 w-full rounded-md border border-input bg-transparent px-3 py-2 text-base shadow-xs outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 md:text-sm dark:bg-input/30";
pub const SELECT: &str = "flex h-9 w-full items-center rounded-md border border-input bg-transparent px-3 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30 [&>option]:bg-popover";
/// Kleines Auswahlfeld (Filter in Kopfzeilen), Breite nach Inhalt.
pub const SELECT_SM: &str = "flex h-8 w-auto items-center rounded-md border border-input bg-transparent px-2.5 text-xs shadow-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30 [&>option]:bg-popover";
pub const BTN: &str = "inline-flex shrink-0 cursor-pointer items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-all outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4";
pub const BTN_PRIMARY: &str = "bg-primary text-primary-foreground shadow-xs hover:bg-primary/90 h-9 px-4 py-2";
pub const BTN_OUTLINE: &str = "border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground dark:bg-input/30 dark:border-input dark:hover:bg-input/50 h-9 px-4 py-2";
pub const BTN_GHOST: &str = "hover:bg-accent hover:text-accent-foreground dark:hover:bg-accent/50 h-9 px-3";
pub const BTN_SM: &str = "h-8 px-3 text-[13px]";
pub const BTN_ICON: &str = "size-9 p-0";

/// Klassen für einen Knopf, z. B. `btn(BTN_PRIMARY)`.
pub fn btn(variant: &str) -> String {
    format!("{BTN} {variant}")
}

#[component]
pub fn Field(#[props(into)] label: String, #[props(into, optional)] hint: Option<String>, #[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let c = tw_merge!("grid gap-1.5", class.as_deref().unwrap_or(""));
    rsx! {
        label { class: "{c}",
            span { class: "text-sm font-medium leading-none", "{label}" }
            {children}
            if let Some(h) = hint {
                span { class: "text-xs text-muted-foreground", "{h}" }
            }
        }
    }
}

#[component]
pub fn PageHeader(#[props(into)] title: String, #[props(into, optional)] description: Option<String>, #[props(optional)] actions: Option<Element>, #[props(optional)] eyebrow: Option<String>) -> Element {
    rsx! {
        div { class: "mb-6 flex flex-wrap items-end justify-between gap-4",
            div { class: "min-w-0",
                if let Some(e) = eyebrow {
                    p { class: "text-xs font-medium uppercase tracking-wider text-muted-foreground", "{e}" }
                }
                h1 { class: "truncate text-2xl font-semibold tracking-tight", "{title}" }
                if let Some(d) = description {
                    p { class: "mt-1 text-sm text-muted-foreground", "{d}" }
                }
            }
            if let Some(a) = actions {
                div { class: "flex flex-wrap items-center gap-2", {a} }
            }
        }
    }
}

#[component]
pub fn EmptyState(#[props(into)] title: String, #[props(into, optional)] description: Option<String>, #[props(optional)] icon: Option<Element>, #[props(optional)] children: Option<Element>) -> Element {
    rsx! {
        div { class: "flex min-h-[16rem] flex-col items-center justify-center gap-3 rounded-xl border border-dashed p-10 text-center",
            if let Some(i) = icon {
                div { class: "grid size-12 place-items-center rounded-xl bg-muted text-muted-foreground [&_svg]:size-6", {i} }
            }
            div {
                p { class: "font-medium", "{title}" }
                if let Some(d) = description {
                    p { class: "mt-1 max-w-sm text-sm text-muted-foreground", "{d}" }
                }
            }
            if let Some(c) = children {
                {c}
            }
        }
    }
}

#[component]
pub fn Loading(#[props(into, optional)] text: Option<String>) -> Element {
    let text = text.unwrap_or_else(|| "Lade …".into());
    rsx! {
        div { class: "flex items-center gap-2 py-10 text-sm text-muted-foreground justify-center",
            icons::LoaderCircle { class: "size-4 animate-spin" }
            "{text}"
        }
    }
}

#[component]
pub fn Callout(#[props(into)] text: String, #[props(default)] error: bool) -> Element {
    let c = if error { "border-destructive/40 bg-destructive/10" } else { "bg-muted/50" };
    rsx! {
        div { class: "flex items-start gap-2 rounded-lg border px-3 py-2.5 text-sm {c}",
            if error {
                icons::CircleAlert { class: "mt-0.5 size-4 shrink-0 text-destructive" }
            } else {
                icons::Info { class: "mt-0.5 size-4 shrink-0 text-muted-foreground" }
            }
            span { "{text}" }
        }
    }
}

/// Abschnitt einer Einstellungsseite (rust-ui Card).
#[component]
pub fn Section(#[props(into)] title: String, #[props(into, optional)] description: Option<String>, #[props(optional)] actions: Option<Element>, children: Element) -> Element {
    rsx! {
        section { class: "rounded-xl border bg-card text-card-foreground shadow-sm",
            div { class: "flex flex-wrap items-start justify-between gap-3 border-b px-6 py-4",
                div {
                    h2 { class: "font-semibold leading-none", "{title}" }
                    if let Some(d) = description {
                        p { class: "mt-1.5 text-sm text-muted-foreground", "{d}" }
                    }
                }
                if let Some(a) = actions {
                    div { class: "flex items-center gap-2", {a} }
                }
            }
            div { class: "px-6 py-5", {children} }
        }
    }
}

/// rust-ui Switch als kontrollierte Komponente.
#[component]
pub fn Toggle(checked: bool, onchange: EventHandler<bool>, #[props(into, optional)] label: Option<String>) -> Element {
    let state = if checked { "bg-primary" } else { "bg-input dark:bg-input/80" };
    let thumb = if checked { "translate-x-[calc(100%-2px)] dark:bg-primary-foreground" } else { "translate-x-0 dark:bg-foreground" };
    rsx! {
        label { class: "inline-flex cursor-pointer items-center gap-2.5 text-sm",
            button {
                r#type: "button",
                role: "switch",
                "aria-checked": "{checked}",
                class: "peer inline-flex h-[1.15rem] w-8 shrink-0 items-center rounded-full border border-transparent shadow-xs transition-all outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 {state}",
                onclick: move |_| onchange.call(!checked),
                span { class: "pointer-events-none block size-4 rounded-full bg-background ring-0 transition-transform {thumb}" }
            }
            if let Some(l) = label {
                span { "{l}" }
            }
        }
    }
}

#[component]
pub fn Checkbox(checked: bool, onchange: EventHandler<bool>, #[props(into)] label: String, #[props(into, optional)] hint: Option<String>) -> Element {
    let state = if checked { "bg-primary text-primary-foreground border-primary" } else { "dark:bg-input/30" };
    rsx! {
        label { class: "flex cursor-pointer items-start gap-2.5 text-sm",
            button {
                r#type: "button",
                role: "checkbox",
                "aria-checked": "{checked}",
                class: "mt-0.5 grid size-4 shrink-0 place-items-center rounded-[4px] border border-input shadow-xs outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 {state}",
                onclick: move |_| onchange.call(!checked),
                if checked {
                    icons::Check { class: "size-3.5" }
                }
            }
            span { class: "grid gap-0.5",
                span { "{label}" }
                if let Some(h) = hint {
                    span { class: "text-xs text-muted-foreground", "{h}" }
                }
            }
        }
    }
}

/// Farbpunkt eines Postfachs.
#[component]
pub fn Dot(#[props(into)] color: String, #[props(into, optional)] class: Option<String>) -> Element {
    let c = tw_merge!("inline-block size-2 shrink-0 rounded-full", class.as_deref().unwrap_or(""));
    let color = if color.trim().is_empty() { "#a3a3a3".to_string() } else { color };
    rsx! { span { class: "{c}", style: "background: {color}" } }
}

/// Rundes Abzeichen mit Anfangsbuchstaben (Absender, Benutzer).
#[component]
pub fn Avatar(#[props(into)] name: String, #[props(into, optional)] class: Option<String>) -> Element {
    let c = tw_merge!(
        "grid size-9 shrink-0 select-none place-items-center rounded-full bg-secondary text-sm font-semibold text-secondary-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! { span { class: "{c}", "aria-hidden": "true", {crate::util::initial(&name)} } }
}

/// Kleines Abzeichen (rust-ui Badge, grau).
#[component]
pub fn Pill(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let c = tw_merge!(
        "inline-flex items-center gap-1 rounded-full border bg-secondary/60 px-2 py-0.5 text-[11px] font-medium text-secondary-foreground [&_svg]:size-3",
        class.as_deref().unwrap_or("")
    );
    rsx! { span { class: "{c}", {children} } }
}

/// Fortschrittsbalken 0.0–1.0.
#[component]
pub fn Progress(value: f64, #[props(into, optional)] label: Option<String>) -> Element {
    let pct = (value.clamp(0.0, 1.0) * 100.0).round();
    rsx! {
        div { class: "grid gap-1.5",
            div { class: "h-2 w-full overflow-hidden rounded-full bg-secondary",
                role: "progressbar",
                "aria-valuemin": "0",
                "aria-valuemax": "100",
                "aria-valuenow": "{pct}",
                div { class: "h-full rounded-full bg-primary transition-[width] duration-200", style: "width: {pct}%" }
            }
            if let Some(l) = label {
                span { class: "text-xs text-muted-foreground", "{l} – {pct}%" }
            }
        }
    }
}

/// Ladezustand einer Liste: graue Zeilen.
#[component]
pub fn SkeletonRows(#[props(default = 8)] rows: usize) -> Element {
    rsx! {
        div { class: "grid gap-px", "aria-busy": "true",
            for i in 0..rows {
                div { key: "{i}", class: "flex items-start gap-3 px-4 py-3",
                    crate::ui::skeleton::Skeleton { class: "size-8 rounded-full" }
                    div { class: "grid flex-1 gap-2",
                        crate::ui::skeleton::Skeleton { class: "h-3 w-1/3" }
                        crate::ui::skeleton::Skeleton { class: "h-3 w-4/5" }
                        crate::ui::skeleton::Skeleton { class: "h-3 w-2/3" }
                    }
                }
            }
        }
    }
}
