//! Dialog, Bestätigung, Menü und Meldungen – Aussehen von rust-ui (Dialog, Alert Dialog,
//! Dropdown Menu, Toast), der Zustand liegt hier in Dioxus-Signalen statt in eingeschleustem
//! JavaScript. Ersetzt überall die Browser-Popups (`confirm`, `alert`, `prompt`).

use dioxus::prelude::*;
use futures_channel::oneshot;
use std::cell::RefCell;
use std::rc::Rc;
use tw_merge::tw_merge;

// ------------------------------------------------------------------ Dialog

/// Modales Fenster. Schliesst mit Esc, Klick daneben (abschaltbar) oder dem X.
#[component]
pub fn Modal(
    open: Signal<bool>,
    #[props(into)] title: String,
    #[props(into, optional)] description: Option<String>,
    #[props(into, optional)] class: Option<String>,
    #[props(default = true)] dismissible: bool,
    children: Element,
) -> Element {
    if !open() {
        return rsx! {};
    }
    let content = tw_merge!(
        "relative bg-background border rounded-2xl shadow-lg p-6 w-full max-w-lg max-h-[85vh] overflow-y-auto animate-in max-sm:max-w-none max-sm:max-h-[92dvh] max-sm:rounded-b-none max-sm:border-x-0 max-sm:border-b-0 max-sm:px-5 max-sm:pt-7 max-sm:pb-safe-4 max-sm:animate-sheet-in",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            class: "fixed inset-0 z-[60] flex items-center justify-center bg-black/60 p-4 backdrop-blur-[2px] max-sm:items-end max-sm:p-0 max-sm:animate-fade-in",
            tabindex: "-1",
            // Erstes Eingabefeld des Dialogs fokussieren, sonst die Fläche selbst (für Escape)
            onmounted: move |_| {
                document::eval(
                    r#"requestAnimationFrame(() => {
                        const d = [...document.querySelectorAll('[role=dialog]')].pop();
                        if (!d) return;
                        // Handy: Tastatur nicht ungefragt aufklappen
                        if (matchMedia('(max-width: 639px)').matches && !d.querySelector('[autofocus]')) { d.parentElement.focus(); return; }
                        const f = d.querySelector('[autofocus], input:not([type=hidden]):not([readonly]):not([disabled]), textarea:not([readonly]), select:not([disabled])');
                        (f || d.parentElement).focus();
                    })"#,
                );
            },
            onkeydown: move |e| if e.key() == Key::Escape && dismissible { open.set(false) },
            onclick: move |_| if dismissible { open.set(false) },
            div {
                class: "{content}",
                role: "dialog",
                "aria-modal": "true",
                onclick: move |e| e.stop_propagation(),
                // Griff (Handy: Blatt von unten)
                div { class: "absolute left-1/2 top-2 h-1 w-10 -translate-x-1/2 rounded-full bg-muted-foreground/30 sm:hidden" }
                button {
                    class: "absolute top-4 right-4 rounded-sm p-1 text-muted-foreground hover:text-foreground focus:outline-none focus:ring-2 focus:ring-ring",
                    title: "Schliessen",
                    onclick: move |_| open.set(false),
                    icons::X { class: "size-4" }
                }
                div { class: "flex flex-col gap-2 pr-6",
                    h3 { class: "text-lg leading-none font-semibold", "{title}" }
                    if let Some(d) = description {
                        p { class: "text-muted-foreground text-sm", "{d}" }
                    }
                }
                div { class: "mt-5", {children} }
            }
        }
    }
}

// ------------------------------------------------------------------ Bestätigung

#[derive(Clone)]
pub struct Ask {
    pub title: String,
    pub description: String,
    pub confirm: String,
    pub cancel: String,
    pub destructive: bool,
}

impl Ask {
    pub fn new(title: impl Into<String>, description: impl Into<String>) -> Self {
        Ask { title: title.into(), description: description.into(), confirm: "Bestätigen".into(), cancel: "Abbrechen".into(), destructive: false }
    }
    pub fn confirm(mut self, label: impl Into<String>) -> Self {
        self.confirm = label.into();
        self
    }
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
}

type Pending = Rc<RefCell<Option<oneshot::Sender<bool>>>>;

static ASK: GlobalSignal<Option<(Ask, Pending)>> = Signal::global(|| None);

/// Eigene Rückfrage statt `window.confirm` – `true`, wenn bestätigt.
pub async fn confirm(ask: Ask) -> bool {
    let (tx, rx) = oneshot::channel();
    if let Some((_, old)) = ASK.write().replace((ask, Rc::new(RefCell::new(Some(tx)))))
        && let Some(s) = old.borrow_mut().take() {
            let _ = s.send(false);
        }
    rx.await.unwrap_or(false)
}

fn answer(yes: bool) {
    if let Some((_, pending)) = ASK.write().take()
        && let Some(s) = pending.borrow_mut().take() {
            let _ = s.send(yes);
        }
}

/// Einmal in der App einbinden.
#[component]
pub fn ConfirmHost() -> Element {
    let Some((ask, _)) = ASK.read().clone() else { return rsx! {} };
    let action = if ask.destructive {
        "bg-destructive text-white hover:bg-destructive/90"
    } else {
        "bg-primary text-primary-foreground hover:bg-primary/90"
    };
    rsx! {
        div {
            class: "fixed inset-0 z-[70] flex items-center justify-center bg-black/60 p-4 backdrop-blur-[2px] max-sm:items-end max-sm:p-2 max-sm:pb-safe-2 max-sm:animate-fade-in",
            tabindex: "-1",
            onmounted: move |e| async move { let _ = e.set_focus(true).await; },
            onkeydown: move |e| match e.key() {
                Key::Escape => answer(false),
                Key::Enter => answer(true),
                _ => {}
            },
            div { class: "w-full max-w-md rounded-2xl border bg-background p-6 shadow-lg animate-in max-sm:max-w-none max-sm:animate-sheet-in", role: "alertdialog",
                div { class: "flex gap-4",
                    if ask.destructive {
                        div { class: "grid size-10 shrink-0 place-items-center rounded-full bg-destructive/15 text-destructive",
                            icons::TriangleAlert { class: "size-5" }
                        }
                    }
                    div { class: "flex flex-col gap-2",
                        h3 { class: "text-lg leading-none font-semibold", "{ask.title}" }
                        p { class: "text-muted-foreground text-sm", "{ask.description}" }
                    }
                }
                footer { class: "mt-6 flex flex-col-reverse gap-2 sm:flex-row sm:justify-end",
                    button {
                        class: "inline-flex h-9 items-center justify-center rounded-md border bg-background px-4 text-sm font-medium shadow-xs hover:bg-accent dark:bg-input/30 dark:border-input max-sm:h-12 max-sm:rounded-xl max-sm:text-base",
                        onclick: move |_| answer(false),
                        "{ask.cancel}"
                    }
                    button {
                        class: "inline-flex h-9 items-center justify-center rounded-md px-4 text-sm font-medium shadow-xs {action} max-sm:h-12 max-sm:rounded-xl max-sm:text-base",
                        onclick: move |_| answer(true),
                        "{ask.confirm}"
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ Meldungen

#[derive(Clone, PartialEq)]
struct Toast {
    id: u64,
    text: String,
    error: bool,
    /// Knopf in der Meldung (z. B. „Rückgängig“)
    action: Option<ToastAction>,
    /// Wird gerade ausgeblendet (Animation läuft, danach entfernt)
    leaving: bool,
}

#[derive(Clone)]
struct ToastAction {
    label: String,
    run: std::rc::Rc<dyn Fn()>,
}

impl PartialEq for ToastAction {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label && std::rc::Rc::ptr_eq(&self.run, &other.run)
    }
}

static TOASTS: GlobalSignal<Vec<Toast>> = Signal::global(Vec::new);
static NEXT: GlobalSignal<u64> = Signal::global(|| 0);

/// Dauer der Ausblende-Animation (muss zu `toast-out` in tailwind.css passen).
const LEAVE_MS: u32 = 220;

fn push(text: String, error: bool) {
    push_with(text, error, None, if error { 7000 } else { 3500 });
}

fn push_with(text: String, error: bool, action: Option<ToastAction>, ms: u32) {
    let id = {
        let mut n = NEXT.write();
        *n += 1;
        *n
    };
    TOASTS.write().push(Toast { id, text, error, action, leaving: false });
    spawn(async move {
        gloo_timers::future::TimeoutFuture::new(ms).await;
        dismiss(id);
    });
}

/// Meldung sanft ausblenden und danach entfernen.
fn dismiss(id: u64) {
    let mut found = false;
    for t in TOASTS.write().iter_mut().filter(|t| t.id == id && !t.leaving) {
        t.leaving = true;
        found = true;
    }
    if found {
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(LEAVE_MS).await;
            TOASTS.write().retain(|t| t.id != id);
        });
    }
}

/// Kurze Meldung unten rechts.
pub fn toast(text: impl Into<String>) {
    push(text.into(), false);
}

/// Fehlermeldung (bleibt länger stehen).
pub fn toast_error(text: impl std::fmt::Display) {
    push(text.to_string(), true);
}

/// Meldung mit Knopf (z. B. „Rückgängig“), sichtbar für `ms` Millisekunden.
pub fn toast_action(text: impl Into<String>, label: impl Into<String>, ms: u32, run: impl Fn() + 'static) {
    push_with(text.into(), false, Some(ToastAction { label: label.into(), run: std::rc::Rc::new(run) }), ms);
}

#[component]
pub fn Toaster() -> Element {
    rsx! {
        div { class: "pointer-events-none fixed bottom-20 right-4 z-[80] flex w-full max-w-sm flex-col gap-2 max-md:inset-x-3 max-md:bottom-[calc(4.25rem+env(safe-area-inset-bottom))] max-md:w-auto max-md:max-w-none",
            for t in TOASTS.read().iter().cloned() {
                div {
                    key: "{t.id}",
                    role: if t.error { "alert" } else { "status" },
                    class: format!(
                        "pointer-events-auto flex items-start gap-3 rounded-xl border bg-popover px-4 py-3 text-sm text-popover-foreground shadow-lg {} {}",
                        if t.error { "border-destructive/40" } else { "" },
                        if t.leaving { "animate-toast-out" } else { "animate-toast-in" },
                    ),
                    if t.error {
                        icons::CircleAlert { class: "mt-0.5 size-4 shrink-0 text-destructive" }
                    } else {
                        icons::Check { class: "mt-0.5 size-4 shrink-0 text-muted-foreground" }
                    }
                    span { class: "flex-1", "{t.text}" }
                    if let Some(a) = t.action.clone() {
                        button {
                            class: "font-semibold text-primary hover:underline",
                            onclick: move |_| { dismiss(t.id); (a.run)(); },
                            "{a.label}"
                        }
                    }
                    button {
                        class: "text-muted-foreground hover:text-foreground",
                        onclick: move |_| dismiss(t.id),
                        icons::X { class: "size-3.5" }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ Menü

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Align {
    #[default]
    Start,
    End,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Side {
    #[default]
    Bottom,
    Top,
}

/// Aufklappmenü (rust-ui Dropdown Menu). `trigger` ist der sichtbare Knopf.
#[component]
pub fn Dropdown(
    trigger: Element,
    #[props(default)] align: Align,
    #[props(default)] side: Side,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] trigger_class: Option<String>,
    children: Element,
) -> Element {
    let mut open = use_signal(|| false);
    provide_context(MenuOpen(open));
    let pos = match (align, side) {
        (Align::Start, Side::Bottom) => "left-0 top-full mt-1.5",
        (Align::End, Side::Bottom) => "right-0 top-full mt-1.5",
        (Align::Start, Side::Top) => "left-0 bottom-full mb-1.5",
        (Align::End, Side::Top) => "right-0 bottom-full mb-1.5",
    };
    let menu = tw_merge!(
        "absolute z-50 min-w-[12rem] overflow-hidden rounded-lg border bg-popover p-1 text-popover-foreground shadow-lg animate-in",
        pos,
        class.as_deref().unwrap_or("")
    );
    // Handy: Blatt von unten über die ganze Breite, gut mit dem Daumen erreichbar
    let sheet = "max-md:fixed max-md:inset-x-2 max-md:top-auto max-md:bottom-2 max-md:left-2 max-md:right-2 max-md:z-[61] max-md:m-0 max-md:w-auto max-md:max-h-[75dvh] max-md:overflow-y-auto max-md:rounded-2xl max-md:p-1.5 max-md:pb-safe-2 max-md:animate-sheet-in";
    rsx! {
        div { class: "relative",
            button {
                class: trigger_class.unwrap_or_default(),
                "aria-expanded": "{open}",
                onclick: move |_| open.toggle(),
                {trigger}
            }
            if open() {
                div { class: "fixed inset-0 z-40 max-md:z-[60] max-md:bg-black/50 max-md:animate-fade-in", onclick: move |_| open.set(false) }
                div { class: "{menu} {sheet}", role: "menu", {children} }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct MenuOpen(Signal<bool>);

/// Eintrag im Menü; schliesst es nach dem Klick.
#[component]
pub fn MenuItem(
    onclick: EventHandler<MouseEvent>,
    #[props(default)] destructive: bool,
    #[props(default)] active: bool,
    children: Element,
) -> Element {
    let ctx = try_consume_context::<MenuOpen>();
    let color = if destructive { "text-destructive hover:bg-destructive/10" } else { "hover:bg-accent hover:text-accent-foreground" };
    let act = if active { "bg-accent/60" } else { "" };
    rsx! {
        button {
            class: "relative flex w-full cursor-default select-none items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none [&_svg]:size-4 [&_svg]:shrink-0 max-md:gap-3 max-md:rounded-lg max-md:px-3 max-md:py-3 max-md:text-[15px] max-md:[&_svg]:size-5 {color} {act}",
            role: "menuitem",
            onclick: move |e| {
                if let Some(MenuOpen(mut o)) = ctx {
                    o.set(false);
                }
                onclick.call(e);
            },
            {children}
        }
    }
}

#[component]
pub fn MenuLabel(children: Element) -> Element {
    rsx! { div { class: "px-2 py-1.5 text-xs font-medium text-muted-foreground max-md:px-3 max-md:pt-2.5", {children} } }
}

#[component]
pub fn MenuSeparator() -> Element {
    rsx! { div { class: "-mx-1 my-1 h-px bg-border" } }
}
