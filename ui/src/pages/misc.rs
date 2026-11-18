//! Kleine Seiten: Weiterleitungen und „nicht gefunden“.

use crate::Route;
use crate::components::common::*;
use dioxus::prelude::*;
use nrzmail_api::MailView;

/// `/` → Posteingang.
#[component]
pub fn Home() -> Element {
    let nav = use_navigator();
    use_effect(move || {
        let current = crate::state::current_account();
        nav.replace(if current.is_empty() { Route::view(MailView::Inbox) } else { Route::account_inbox(&current) });
    });
    rsx! {}
}

/// `/settings` → erster Bereich.
#[component]
pub fn SettingsHome() -> Element {
    let nav = use_navigator();
    use_effect(move || {
        nav.replace(Route::Settings { section: "konto".into() });
    });
    rsx! {}
}

#[component]
pub fn NotFound(segments: Vec<String>) -> Element {
    let path = format!("/{}", segments.join("/"));
    rsx! {
        document::Title { "Nicht gefunden – nrzMail" }
        div { class: "grid min-h-screen place-items-center p-6",
            EmptyState {
                title: "Seite nicht gefunden",
                description: "Unter {path} gibt es nichts.",
                icon: rsx! { icons::MailQuestionMark {} },
                Link { class: btn(BTN_OUTLINE), to: Route::view(MailView::Inbox), "Zum Posteingang" }
            }
        }
    }
}
