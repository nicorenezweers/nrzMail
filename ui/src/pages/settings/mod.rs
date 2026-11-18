//! Einstellungen unter `/settings/<bereich>`: Konto, Postfächer – und für Administratoren
//! KI & Modelle, Benutzer, Allgemein und das Protokoll.

mod account;
mod admin;
mod ai;
mod mailboxes;

use crate::Route;
use crate::components::common::*;
use crate::pages::layout::MobileMenuButton;
use crate::state::ME;
use dioxus::prelude::*;

/// Bereiche in der Reihenfolge der Navigation: (Schlüssel, Titel).
pub const SECTIONS: [(&str, &str); 6] = [
    ("konto", "Konto & Darstellung"),
    ("postfaecher", "Postfächer"),
    ("ki", "KI & Modelle"),
    ("benutzer", "Benutzer"),
    ("allgemein", "Allgemein"),
    ("protokoll", "Protokoll"),
];

/// Nur für Administratoren?
pub fn admin_only(key: &str) -> bool {
    matches!(key, "ki" | "benutzer" | "allgemein" | "protokoll")
}

fn icon(key: &str) -> Element {
    match key {
        "konto" => rsx! { icons::User {} },
        "postfaecher" => rsx! { icons::Server {} },
        "ki" => rsx! { icons::Sparkles {} },
        "benutzer" => rsx! { icons::Users {} },
        "allgemein" => rsx! { icons::ShieldCheck {} },
        _ => rsx! { icons::ScrollText {} },
    }
}

#[component]
pub fn Settings(section: String) -> Element {
    let admin = ME.read().as_ref().is_some_and(|u| u.is_admin);
    let visible: Vec<(&str, &str)> = SECTIONS.iter().copied().filter(|(k, _)| admin || !admin_only(k)).collect();
    let title = visible.iter().find(|(k, _)| *k == section).map(|(_, l)| *l).unwrap_or("Einstellungen");
    rsx! {
        document::Title { "{title} – Einstellungen – nrzMail" }
        div { class: "flex h-14 shrink-0 items-center gap-2 border-b px-3 md:hidden",
            MobileMenuButton {}
            span { class: "font-semibold", "Einstellungen" }
        }
        div { class: "min-h-0 flex-1 overflow-y-auto",
            div { class: "mx-auto max-w-6xl p-4 pb-24 md:p-6 md:pb-24",
                div { class: "mb-6",
                    h1 { class: "text-2xl font-semibold tracking-tight", "Einstellungen" }
                    p { class: "mt-1 text-sm text-muted-foreground", "{title}" }
                }
                div { class: "grid gap-6 lg:grid-cols-[14rem_minmax(0,1fr)]",
                    nav { class: "no-scrollbar flex gap-1 overflow-x-auto max-md:-mx-4 max-md:px-4 lg:flex-col lg:overflow-visible", "aria-label": "Bereiche",
                        p { class: "hidden px-2.5 pb-1 text-xs font-medium uppercase tracking-wider text-muted-foreground lg:block", "Persönlich" }
                        for (key , label) in visible.iter().copied().filter(|(k, _)| !admin_only(k)) {
                            SectionLink { key: "{key}", key_name: key, label, active: key == section }
                        }
                        if admin {
                            p { class: "hidden px-2.5 pb-1 pt-4 text-xs font-medium uppercase tracking-wider text-muted-foreground lg:block", "Instanz" }
                            for (key , label) in visible.iter().copied().filter(|(k, _)| admin_only(k)) {
                                SectionLink { key: "{key}", key_name: key, label, active: key == section }
                            }
                        }
                    }
                    div { class: "min-w-0 space-y-6",
                        match section.as_str() {
                            "konto" => rsx! { account::AccountSettings {} },
                            "postfaecher" => rsx! { mailboxes::Mailboxes {} },
                            "ki" if admin => rsx! { ai::AiSettingsPage {} },
                            "benutzer" if admin => rsx! { admin::Users {} },
                            "allgemein" if admin => rsx! { admin::Instance {} },
                            "protokoll" if admin => rsx! { admin::Activity {} },
                            _ => rsx! { Callout { text: "Diesen Bereich gibt es nicht oder er ist nur für Administratoren." } },
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SectionLink(key_name: &'static str, label: &'static str, active: bool) -> Element {
    rsx! {
        Link { to: Route::Settings { section: key_name.to_string() },
            class: if active { "flex shrink-0 items-center gap-2.5 rounded-md bg-accent px-2.5 py-1.5 text-sm font-medium [&_svg]:size-4" } else { "flex shrink-0 items-center gap-2.5 rounded-md px-2.5 py-1.5 text-sm text-muted-foreground hover:bg-accent/50 hover:text-foreground [&_svg]:size-4" },
            "aria-current": if active { "page" } else { "false" },
            {icon(key_name)}
            "{label}"
        }
    }
}
