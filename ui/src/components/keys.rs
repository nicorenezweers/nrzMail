//! Tastenkürzel: ein Listener auf dem Dokument (über `document::eval`) meldet Tasten an Rust.
//! Nicht, während ein Eingabefeld den Fokus hat, ein Dialog offen ist oder im Verfassen-Fenster
//! getippt wird. Allgemeine Kürzel (⌘K, c, /, ?) erledigt die Shell, alles andere geht an die
//! gerade offene Seite (siehe [`PAGE_KEYS`]).

use crate::Route;
use crate::components::common::*;
use crate::components::overlay::Modal;
use crate::components::{compose, palette};
use crate::ui::kbd::Kbd;
use dioxus::prelude::*;
use nrzmail_api::MailView;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
struct KeyPress {
    key: String,
}

/// Tasten-Empfänger der aktuellen Seite (die Mail-Ansicht setzt ihn beim Öffnen).
pub static PAGE_KEYS: GlobalSignal<Option<Callback<String>>> = Signal::global(|| None);

static HELP: GlobalSignal<bool> = Signal::global(|| false);

pub fn open_help() {
    *HELP.write() = true;
}

/// Taste an die Seite weitergeben (auch aus der Befehlspalette).
pub fn dispatch(key: &str) {
    if let Some(cb) = *PAGE_KEYS.peek() {
        cb.call(key.to_string());
    }
}

const LISTEN: &str = r#"
if (window.__nrzmailKeys) document.removeEventListener('keydown', window.__nrzmailKeys, true);
const handler = (e) => {
  const t = e.target;
  const mod = e.metaKey || e.ctrlKey;
  if (mod && !e.altKey && !e.shiftKey && (e.key === 'k' || e.key === 'K')) {
    e.preventDefault();
    dioxus.send({ key: 'palette' });
    return;
  }
  if (e.isComposing || e.defaultPrevented) return;
  const typing = t && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName));
  if (typing) return;
  if (t && t.closest && t.closest('[data-compose]')) return;
  if (document.querySelector('[aria-modal=true], [role=alertdialog]')) return;
  const k = e.key;
  if (['j', 'k', 'e', 'r', 'a', 'f', 'c', 's', 'u', 'x'].includes(k) && !mod && !e.altKey) {
    e.preventDefault();
    dioxus.send({ key: k });
  } else if (['#', '/', '?'].includes(k) && !e.metaKey) {
    e.preventDefault();
    dioxus.send({ key: k });
  } else if (k === 'Escape') {
    dioxus.send({ key: k });
  } else if (k === 'Enter' && !mod && !(t && t.closest && t.closest('button, a, [role=button], summary'))) {
    e.preventDefault();
    dioxus.send({ key: k });
  }
};
window.__nrzmailKeys = handler;
document.addEventListener('keydown', handler, true);
await new Promise(() => {});
"#;

/// In der Shell einmal aufrufen.
pub fn use_shortcuts() {
    let nav = use_navigator();
    let route = use_route::<crate::Route>();
    let mut on_mail = use_signal(|| false);
    // Ist die Mail-Ansicht offen? (für „/“)
    use_effect(use_reactive!(|route| on_mail.set(matches!(route, Route::Mail { .. }))));
    use_future(move || async move {
        let mut ev = document::eval(LISTEN);
        while let Ok(k) = ev.recv::<KeyPress>().await {
            match k.key.as_str() {
                "palette" => palette::toggle(),
                "c" => compose::open_new(),
                "?" => open_help(),
                "/" => {
                    if !*on_mail.peek() {
                        nav.push(Route::view(MailView::Inbox));
                    }
                    crate::util::focus("mail-search");
                }
                other => dispatch(other),
            }
        }
    });
}

/// Übersicht der Tastenkürzel.
#[component]
pub fn ShortcutHelp() -> Element {
    let mut open = HELP.signal();
    let groups: [(&str, &[(&str, &str)]); 3] = [
        ("Liste", &[("j / k", "Nächste / vorige Nachricht"), ("Enter", "Öffnen"), ("x", "Auswählen"), ("/", "Suchen"), ("Esc", "Schliessen / Auswahl aufheben")]),
        ("Nachricht", &[("e", "Archivieren"), ("#", "Löschen"), ("r", "Antworten"), ("a", "Allen antworten"), ("f", "Weiterleiten"), ("s", "Stern an/aus"), ("u", "Als ungelesen markieren")]),
        ("Überall", &[("c", "Neue Nachricht"), ("⌘K / Ctrl+K", "Befehlspalette"), ("?", "Diese Übersicht")]),
    ];
    rsx! {
        Modal { open, title: "Tastenkürzel", description: "Wirken nicht, solange ein Eingabefeld den Fokus hat.",
            div { class: "grid gap-5 sm:grid-cols-2",
                for (title , items) in groups {
                    div { class: "grid content-start gap-2",
                        p { class: "text-xs font-medium uppercase tracking-wider text-muted-foreground", "{title}" }
                        for (k , what) in items.iter() {
                            div { class: "flex items-center justify-between gap-3 text-sm",
                                span { "{what}" }
                                Kbd { "{k}" }
                            }
                        }
                    }
                }
            }
            div { class: "mt-6 flex justify-end",
                button { class: btn(BTN_OUTLINE), onclick: move |_| open.set(false), "Schliessen" }
            }
        }
    }
}
