//! Der Assistent (Vorgabe „Dieter“): Chat unten rechts über die eigenen Mails
//! („Wann kam die letzte Rechnung von Swisscom?“, „Was wollte Anna letzte Woche?“).
//! Die geöffnete Nachricht geht als Kontext mit; Quellen sind Links, die die Nachricht öffnen.

use crate::Route;
use crate::api;
use crate::components::compose;
use crate::state::AI;
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{ChatMessage, ChatReply, ChatRequest, ChatRole, MessageRef};

#[derive(Debug, Clone, PartialEq)]
struct Msg {
    role: ChatRole,
    content: String,
    sources: Vec<MessageRef>,
    error: bool,
}

/// Gespräch – bleibt beim Seitenwechsel erhalten.
static CHAT: GlobalSignal<Vec<Msg>> = Signal::global(Vec::new);
static OPEN: GlobalSignal<bool> = Signal::global(|| false);
/// Frage, die beim Öffnen gleich gestellt wird (z. B. aus dem KI-Blatt im Lesebereich).
static PENDING: GlobalSignal<Option<String>> = Signal::global(|| None);

/// Assistenten öffnen.
pub fn open() {
    *OPEN.write() = true;
    if !*crate::state::PHONE.peek() {
        util::focus("assistant-input");
    }
}

/// Assistenten öffnen und gleich fragen.
pub fn ask(text: impl Into<String>) {
    *PENDING.write() = Some(text.into());
    *OPEN.write() = true;
}

/// Sehr kleines Markdown: **fett**, Listen mit -/*/1., Absätze.
pub fn markdown(text: &str) -> Element {
    let inline = |line: &str| -> Element {
        let parts: Vec<String> = line.split("**").map(str::to_string).collect();
        rsx! {
            for (i , p) in parts.into_iter().enumerate() {
                if i % 2 == 1 { strong { "{p}" } } else { "{p}" }
            }
        }
    };
    let mut blocks: Vec<Element> = Vec::new();
    let mut list: Vec<String> = Vec::new();
    let flush = |list: &mut Vec<String>, blocks: &mut Vec<Element>| {
        if !list.is_empty() {
            let items = std::mem::take(list);
            blocks.push(rsx! { ul { class: "my-1 list-disc space-y-0.5 pl-5", for it in items { li { {inline(&it)} } } } });
        }
    };
    for raw in text.lines() {
        let line = raw.trim();
        let item = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
            .or_else(|| {
                let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
                (digits > 0 && line[digits..].starts_with(". ")).then(|| &line[digits + 2..])
            });
        match item {
            Some(it) => list.push(it.to_string()),
            None => {
                flush(&mut list, &mut blocks);
                if !line.is_empty() {
                    let l = line.trim_start_matches('#').trim().to_string();
                    blocks.push(rsx! { p { class: "my-1", {inline(&l)} } });
                }
            }
        }
    }
    flush(&mut list, &mut blocks);
    rsx! { for b in blocks { {b} } }
}

#[component]
pub fn Assistant() -> Element {
    let route = use_route::<Route>();
    let mut input = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let ai = AI();
    let name = if ai.assistant_name.trim().is_empty() { "Dieter".to_string() } else { ai.assistant_name.trim().to_string() };
    let initial = util::initial(&name);
    let messages = CHAT();
    // Geöffnete Nachricht als Kontext
    let context = match &route {
        Route::Mail { query } if !query.open.is_empty() => Some(query.open.clone()),
        _ => None,
    };
    let has_context = context.is_some();

    let send = use_callback(move |text: String| {
        let text = text.trim().to_string();
        if text.is_empty() || busy() {
            return;
        }
        CHAT.write().push(Msg { role: ChatRole::User, content: text, sources: Vec::new(), error: false });
        input.set(String::new());
        busy.set(true);
        let message_id = context.clone();
        spawn(async move {
            let history: Vec<ChatMessage> = CHAT
                .peek()
                .iter()
                .filter(|m| !m.error)
                .map(|m| ChatMessage { role: m.role, content: m.content.clone() })
                .collect();
            let req = ChatRequest { messages: history, message_id, tz: crate::util::tz_offset_minutes() };
            let msg = match api::post::<ChatReply, _>("/ai/chat", &req).await {
                Ok(a) => Msg { role: ChatRole::Assistant, content: a.reply, sources: a.sources, error: false },
                Err(e) => Msg { role: ChatRole::Assistant, content: e.message, sources: Vec::new(), error: true },
            };
            CHAT.write().push(msg);
            busy.set(false);
            document::eval("requestAnimationFrame(() => { const c = document.getElementById('assistant-log'); if (c) c.scrollTop = c.scrollHeight; })");
        });
    });
    // Von aussen gestellte Frage abschicken
    use_effect(move || {
        if OPEN() && PENDING.read().is_some() && !busy() {
            if let Some(q) = PENDING.write().take() {
                send.call(q);
            }
        }
    });
    let phone = crate::state::PHONE();
    let on_mail = matches!(route, Route::Mail { .. });
    let suggestions = if has_context {
        vec!["Fasse diese Nachricht kurz zusammen.", "Was wird hier von mir erwartet?", "Gibt es frühere Mails zu diesem Thema?"]
    } else {
        vec!["Welche Rechnungen kamen diesen Monat?", "Was habe ich zuletzt mit Anna besprochen?", "Welche Newsletter lese ich nie?"]
    };

    if !OPEN() {
        // Knopf ausblenden, solange das Verfassen-Fenster offen ist
        if compose::is_open() {
            return rsx! {};
        }
        // Handy, Mail-Ansicht: Dieter sitzt in der Werkzeugleiste unten
        if phone && on_mail {
            return rsx! {};
        }
        return rsx! {
            button {
                class: "fixed bottom-5 right-5 z-40 flex items-center gap-2 rounded-full border bg-primary px-4 py-3 text-sm font-medium text-primary-foreground shadow-lg transition hover:scale-[1.03] animate-in max-md:bottom-[calc(1rem+env(safe-area-inset-bottom))] max-md:right-4 max-md:px-3",
                title: "{name} fragen",
                "aria-label": "{name} fragen",
                onclick: move |_| open(),
                icons::MessageCircle { class: "size-5" }
                span { class: "hidden lg:inline", "{name} fragen" }
            }
        };
    }

    rsx! {
        div { class: "fixed z-[56] flex flex-col overflow-hidden bg-popover text-popover-foreground shadow-2xl max-md:inset-x-0 max-md:top-[var(--vvt,0px)] max-md:h-[var(--vvh,100dvh)] max-md:pt-safe max-md:animate-sheet-in md:bottom-5 md:right-5 md:z-40 md:h-[min(36rem,85dvh)] md:w-[25rem] md:rounded-2xl md:border md:animate-toast-in",
            role: "dialog",
            "aria-label": "Assistent {name}",
            onkeydown: move |e| if e.key() == Key::Escape { *OPEN.write() = false },
            // Kopf
            div { class: "flex items-center gap-3 border-b px-4 py-3 max-md:px-3",
                if phone {
                    button { class: "grid size-10 place-items-center rounded-md text-muted-foreground hover:bg-accent", title: "Schliessen", "aria-label": "Schliessen",
                        onclick: move |_| *OPEN.write() = false,
                        icons::ChevronDown { class: "size-5" }
                    }
                }
                span { class: "grid size-9 place-items-center rounded-full bg-primary text-sm font-bold text-primary-foreground", "{initial}" }
                div { class: "min-w-0 flex-1",
                    p { class: "font-semibold leading-tight", "{name}" }
                    p { class: "truncate text-xs text-muted-foreground",
                        if has_context { "Kennt die geöffnete Nachricht" } else { "Dein Assistent für die Mails" }
                    }
                }
                if !messages.is_empty() {
                    button { class: "grid size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground", title: "Neues Gespräch",
                        onclick: move |_| CHAT.write().clear(),
                        icons::SquarePen { class: "size-4" }
                    }
                }
                if !phone {
                    button { class: "grid size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground", title: "Schliessen",
                        onclick: move |_| *OPEN.write() = false,
                        icons::X { class: "size-4" }
                    }
                }
            }
            // Verlauf
            div { id: "assistant-log", class: "min-h-0 flex-1 space-y-3 overflow-y-auto px-4 py-4 text-sm", "aria-live": "polite",
                if messages.is_empty() {
                    div { class: "rounded-xl bg-accent/50 p-3",
                        p { "Hallo, ich bin {name}. Frag mich nach deinen Mails – ich suche, lese und zeige dir die Nachrichten, auf die ich mich stütze." }
                    }
                    if ai.available {
                        div { class: "grid gap-1.5",
                            for s in suggestions {
                                button { class: "rounded-lg border px-3 py-2 text-left text-xs text-muted-foreground hover:bg-accent hover:text-foreground max-md:rounded-xl max-md:px-4 max-md:py-3 max-md:text-sm",
                                    onclick: move |_| send.call(s.to_string()), "{s}" }
                            }
                        }
                    } else {
                        p { class: "rounded-lg border border-dashed p-3 text-xs text-muted-foreground",
                            {ai.message.clone().unwrap_or_else(|| "Dafür braucht es eine KI – ein Administrator richtet sie unter Einstellungen → KI & Modelle ein.".into())}
                        }
                    }
                }
                for (i , m) in messages.iter().enumerate() {
                    if m.role == ChatRole::User {
                        div { key: "{i}", class: "ml-8 whitespace-pre-wrap rounded-2xl rounded-br-md bg-primary px-3 py-2 text-primary-foreground", "{m.content}" }
                    } else {
                        div { key: "{i}", class: "mr-4 grid gap-2",
                            div { class: if m.error { "rounded-2xl rounded-bl-md border border-destructive/50 bg-destructive/10 px-3 py-2" } else { "rounded-2xl rounded-bl-md bg-accent px-3 py-2" },
                                {markdown(&m.content)}
                            }
                            if !m.sources.is_empty() {
                                div { class: "grid gap-1",
                                    p { class: "text-[11px] uppercase tracking-wider text-muted-foreground",
                                        if m.sources.len() == 1 { "Quelle" } else { "{m.sources.len()} Quellen" }
                                    }
                                    for s in m.sources.iter().cloned() {
                                        Link { key: "{s.id}", to: Route::message(&s.id),
                                            // Handy: Vollbild schliessen, damit man die Nachricht sieht
                                            onclick: move |_| if *crate::state::PHONE.peek() { *OPEN.write() = false },
                                            class: "flex items-center gap-2 rounded-lg border bg-card px-2.5 py-1.5 text-xs hover:bg-accent",
                                            icons::Mail { class: "size-3.5 shrink-0 text-muted-foreground" }
                                            span { class: "min-w-0 flex-1",
                                                span { class: "block truncate font-medium", if s.subject.is_empty() { "(kein Betreff)" } else { "{s.subject}" } }
                                                span { class: "block truncate text-muted-foreground", "{s.from.label()}" }
                                            }
                                            span { class: "shrink-0 text-muted-foreground", {util::date(s.date)} }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if busy() {
                    div { class: "flex items-center gap-2 text-xs text-muted-foreground",
                        icons::LoaderCircle { class: "size-4 animate-spin" }
                        "{name} sucht in deinen Mails …"
                    }
                }
            }
            // Eingabe
            form { class: "flex items-end gap-2 border-t p-3 max-md:pb-safe-2",
                onsubmit: move |e| { e.prevent_default(); send.call(input()); },
                textarea {
                    id: "assistant-input",
                    class: "max-h-32 min-h-10 flex-1 resize-none rounded-lg border border-input bg-transparent px-3 py-2 text-sm outline-none max-md:rounded-2xl max-md:text-base placeholder:text-muted-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 dark:bg-input/30",
                    rows: 1,
                    "aria-label": "Frage an {name}",
                    disabled: !ai.available,
                    placeholder: "Frag {name} …",
                    value: "{input}",
                    oninput: move |e| input.set(e.value()),
                    onkeydown: move |e| if e.key() == Key::Enter && !e.modifiers().shift() { e.prevent_default(); send.call(input()); },
                }
                button { class: "grid size-10 shrink-0 place-items-center rounded-lg bg-primary text-primary-foreground disabled:opacity-50",
                    r#type: "submit", disabled: busy() || input().trim().is_empty() || !ai.available, title: "Senden", "aria-label": "Senden",
                    icons::SendHorizontal { class: "size-4" }
                }
            }
        }
    }
}
