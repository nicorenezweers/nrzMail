//! KI & Modelle (nur Administratoren): Anbieter, Schlüssel, Adresse, Modell mit Liste vom
//! Anbieter, Test, automatische Einteilung neuer Nachrichten und der Name des Assistenten.

use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::state;
use dioxus::prelude::*;
use nrzmail_api::{AiModel, AiProvider, AiSettings, AiSettingsUpdate, AiTestResult};

fn source_label(s: &str) -> &'static str {
    match s {
        "settings" => "Hier hinterlegt",
        "environment" => "Aus der Umgebung (Variable beim Start)",
        "builtin" => "Eingebauter Schlüssel",
        _ => "Kein Schlüssel",
    }
}

#[component]
pub fn AiSettingsPage() -> Element {
    let mut current = use_signal(|| None::<AiSettings>);
    let mut providers = use_signal(Vec::<AiProvider>::new);
    let mut enabled = use_signal(|| false);
    let mut provider = use_signal(String::new);
    let mut model = use_signal(String::new);
    let mut base_url = use_signal(String::new);
    let mut key = use_signal(String::new);
    let mut auto = use_signal(|| false);
    let mut name = use_signal(String::new);
    let mut models = use_signal(Vec::<AiModel>::new);
    let mut model_filter = use_signal(String::new);
    let mut loading_models = use_signal(|| false);
    let mut testing = use_signal(|| false);
    let mut saving = use_signal(|| false);
    let mut result = use_signal(|| None::<AiTestResult>);

    let mut apply = move |s: AiSettings| {
        enabled.set(s.enabled);
        provider.set(s.provider.clone());
        model.set(s.model.clone());
        base_url.set(s.base_url.clone());
        auto.set(s.auto_categorize);
        name.set(s.assistant_name.clone());
        key.set(String::new());
        current.set(Some(s));
    };
    use_future(move || async move {
        if let Ok(p) = api::get::<Vec<AiProvider>>("/ai/providers").await {
            providers.set(p);
        }
        match api::get::<AiSettings>("/ai/settings").await {
            Ok(s) => apply(s),
            Err(e) => toast_error(e),
        }
    });
    let body = move |clear_key: bool| AiSettingsUpdate {
        enabled: enabled(),
        provider: provider(),
        model: model().trim().to_string(),
        base_url: base_url().trim().to_string(),
        api_key: if clear_key { Some(String::new()) } else { Some(key()).filter(|k| !k.trim().is_empty()) },
        auto_categorize: auto(),
        assistant_name: name().trim().to_string(),
    };
    let save = move |clear_key: bool| {
        let b = body(clear_key);
        spawn(async move {
            saving.set(true);
            match api::put::<AiSettings, _>("/ai/settings", &b).await {
                Ok(s) => {
                    apply(s);
                    toast("KI-Einstellungen gespeichert.");
                    state::load_ai().await;
                }
                Err(e) => toast_error(e),
            }
            saving.set(false);
        });
    };
    let load_models = move |_| {
        // Zuerst speichern, damit Schlüssel und Adresse gelten
        let b = body(false);
        spawn(async move {
            loading_models.set(true);
            match api::put::<AiSettings, _>("/ai/settings", &b).await {
                Ok(s) => apply(s),
                Err(e) => {
                    toast_error(e);
                    loading_models.set(false);
                    return;
                }
            }
            match api::get::<Vec<AiModel>>("/ai/models").await {
                Ok(m) => {
                    if m.is_empty() {
                        toast("Der Anbieter meldet keine Modelle.");
                    }
                    models.set(m);
                }
                Err(e) => toast_error(e),
            }
            loading_models.set(false);
        });
    };
    let run_test = move |_| {
        let b = body(false);
        spawn(async move {
            testing.set(true);
            result.set(None);
            match api::post::<AiTestResult, _>("/ai/test", &b).await {
                Ok(r) => result.set(Some(r)),
                Err(e) => result.set(Some(AiTestResult { ok: false, message: e.message, model: String::new() })),
            }
            testing.set(false);
        });
    };

    let Some(cur) = current() else { return rsx! { Loading {} } };
    let list = providers();
    let p = list.iter().find(|x| x.id == provider()).cloned();
    let needs_key = p.as_ref().is_some_and(|p| p.needs_key);
    let is_cli = p.as_ref().is_some_and(|p| p.cli);
    let default_url = p.as_ref().map(|p| p.default_url.clone()).unwrap_or_default();
    let default_model = p.as_ref().map(|p| p.default_model.clone()).unwrap_or_default();
    let filter = model_filter().to_lowercase();
    let shown: Vec<AiModel> = models().into_iter().filter(|m| filter.is_empty() || m.id.to_lowercase().contains(&filter) || m.name.to_lowercase().contains(&filter)).take(200).collect();
    let status = state::AI();
    let provider_name = list.iter().find(|x| x.id == cur.provider).map(|x| x.name.clone()).unwrap_or_else(|| cur.provider.clone());

    rsx! {
        Section { title: "Status",
            div { class: "grid gap-3 sm:grid-cols-3",
                div { class: "rounded-lg border p-4",
                    p { class: "text-xs text-muted-foreground", "KI" }
                    p { class: "mt-1 flex items-center gap-2 font-medium",
                        span { class: if status.available { "size-2 rounded-full bg-success" } else { "size-2 rounded-full bg-destructive" } }
                        if !cur.enabled { "Ausgeschaltet" } else if status.available { "Bereit" } else { "Nicht bereit" }
                    }
                    if let Some(m) = status.message.clone() { p { class: "mt-1 text-xs text-muted-foreground", "{m}" } }
                }
                div { class: "rounded-lg border p-4",
                    p { class: "text-xs text-muted-foreground", "Anbieter und Schlüssel" }
                    p { class: "mt-1 font-medium", "{provider_name}" }
                    p { class: "text-xs text-muted-foreground", {source_label(&cur.key_source)} }
                    if !cur.key_hint.is_empty() { p { class: "font-mono text-xs text-muted-foreground", "{cur.key_hint}" } }
                }
                div { class: "rounded-lg border p-4",
                    p { class: "text-xs text-muted-foreground", "Modell" }
                    p { class: "mt-1 break-all font-mono text-sm", if status.model.is_empty() { "–" } else { "{status.model}" } }
                }
            }
            div { class: "mt-4 flex flex-wrap items-center gap-3",
                button { class: btn(BTN_OUTLINE), disabled: testing(), onclick: run_test,
                    if testing() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::FlaskConical { class: "size-4" } }
                    "Verbindung testen"
                }
                if let Some(r) = result() {
                    span { class: if r.ok { "text-sm text-success" } else { "text-sm text-destructive" },
                        if r.ok && !r.model.is_empty() { "{r.model}: " }
                        "{r.message}"
                    }
                }
            }
        }
        Section { title: "Anbieter und Modell",
            description: "Die KI fasst Nachrichten zusammen, entwirft Antworten, schreibt und überarbeitet Texte, teilt den Posteingang ein und beantwortet Fragen im Assistenten. Jeder Anbieter mit OpenAI-kompatibler API funktioniert, dazu lokale Server wie Ollama oder LM Studio.",
            form { class: "grid max-w-2xl gap-5", onsubmit: move |e| { e.prevent_default(); save(false); },
                Toggle { checked: enabled(), onchange: move |v| enabled.set(v), label: "KI verwenden" }
                Field { label: "Anbieter",
                    select { class: SELECT,
                        onchange: move |e| {
                            let id = e.value();
                            if id != current.peek().as_ref().map(|c| c.provider.clone()).unwrap_or_default() {
                                model.set(String::new());
                                base_url.set(String::new());
                            }
                            models.set(Vec::new());
                            key.set(String::new());
                            provider.set(id);
                        },
                        for (group , local , cli) in [("Cloud", false, false), ("Programm auf dem Server", false, true), ("Lokal (eigener Rechner)", true, false)] {
                            optgroup { label: group,
                                for x in list.iter().filter(|x| x.local == local && x.cli == cli) {
                                    option { value: "{x.id}", selected: x.id == provider(), "{x.name}" }
                                }
                            }
                        }
                    }
                }
                if is_cli {
                    Callout { text: "Das Programm muss auf dem Server installiert und angemeldet sein (Claude Code: `claude setup-token`, OpenCode: `opencode auth login`). Im Docker-Image ist es nicht enthalten. Optional einen Schlüssel eintragen – bei Claude Code ein API-Schlüssel oder ein Token aus `claude setup-token`.".to_string() }
                }
                if needs_key || is_cli || !cur.key_hint.is_empty() {
                    Field { label: if is_cli { "Schlüssel (optional)" } else { "API-Schlüssel" }, hint: if cur.key_hint.is_empty() { "Wird verschlüsselt gespeichert und nie wieder angezeigt.".to_string() } else { format!("Gespeichert: {} – leer lassen, um ihn zu behalten.", cur.key_hint) },
                        div { class: "flex gap-2",
                            input { class: INPUT, r#type: "password", autocomplete: "off", placeholder: if cur.key_hint.is_empty() { "sk-…" } else { "Unverändert" },
                                value: "{key}", oninput: move |e| key.set(e.value()) }
                            if !cur.key_hint.is_empty() && cur.key_source == "settings" {
                                button { r#type: "button", class: "{btn(BTN_OUTLINE)} shrink-0", onclick: move |_| save(true), "Entfernen" }
                            }
                        }
                    }
                }
                Field { label: if is_cli { "Befehl" } else { "Adresse der API" },
                    hint: if is_cli { format!("Programm oder voller Pfad, z. B. /usr/local/bin/{default_url} – leer = {default_url}") } else if default_url.is_empty() { "Leer = Vorgabe des Anbieters".to_string() } else { format!("Leer = {default_url}") },
                    input { class: INPUT, r#type: if is_cli { "text" } else { "url" }, placeholder: "{default_url}", value: "{base_url}", oninput: move |e| base_url.set(e.value()) }
                }
                Field { label: "Modell", hint: if default_model.is_empty() { "Leer = Vorgabe des Anbieters".to_string() } else { format!("Leer = {default_model}") },
                    div { class: "flex gap-2",
                        input { class: "{INPUT} font-mono", placeholder: "{default_model}", value: "{model}", oninput: move |e| model.set(e.value()) }
                        button { r#type: "button", class: "{btn(BTN_OUTLINE)} shrink-0", disabled: loading_models(), onclick: load_models,
                            if loading_models() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::RefreshCw { class: "size-4" } }
                            "Modelle laden"
                        }
                    }
                }
                if !models().is_empty() {
                    div { class: "grid gap-2",
                        input { class: INPUT, placeholder: "Modelle filtern …", "aria-label": "Modelle filtern", value: "{model_filter}", oninput: move |e| model_filter.set(e.value()) }
                        div { class: "max-h-64 overflow-y-auto rounded-lg border",
                            for m in shown {
                                button { r#type: "button", key: "{m.id}",
                                    class: if m.id == model() { "flex w-full items-center gap-2 bg-accent px-3 py-1.5 text-left text-sm" } else { "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-accent/50" },
                                    onclick: { let id = m.id.clone(); move |_| model.set(id.clone()) },
                                    span { class: "min-w-0 flex-1 truncate font-mono text-xs", "{m.id}" }
                                    if !m.name.is_empty() && m.name != m.id { span { class: "max-w-[45%] truncate text-xs text-muted-foreground", "{m.name}" } }
                                }
                            }
                        }
                    }
                }
                Toggle { checked: auto(), onchange: move |v| auto.set(v), label: "Neue Nachrichten automatisch einteilen (Allgemein, Newsletter, …)" }
                Field { label: "Name des Assistenten", hint: "Erscheint im Chat unten rechts.",
                    input { class: "{INPUT} max-w-xs", placeholder: "Dieter", value: "{name}", oninput: move |e| name.set(e.value()) }
                }
                div { button { class: btn(BTN_PRIMARY), r#type: "submit", disabled: saving(), "Speichern" } }
            }
        }
    }
}
