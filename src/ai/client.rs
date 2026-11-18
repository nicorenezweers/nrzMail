//! Gespräch mit dem Anbieter: OpenAI-Chat-Protokoll (`/chat/completions`) und Anthropic
//! Messages (`/messages`), mit Verlauf, Zeitlimits, Ersatzmodellen bei OpenRouter und
//! verständlichen Fehlern. Inhalte von Anfragen und Antworten werden nie protokolliert.

use super::error::{AiError, AiResult};
use super::provider::{Auth, Protocol, DEFAULT_MODEL};
use super::settings::AiConfig;
use crate::api;
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent(concat!("nrzMail/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP-Client")
});

/// Wer spricht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub role: Role,
    pub content: String,
}

impl Turn {
    pub fn user(content: impl Into<String>) -> Self {
        Turn {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Turn {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// Eine Anfrage: Systemprompt, Verlauf (die letzte Nachricht ist vom Benutzer), Grenzen.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub system: String,
    pub turns: Vec<Turn>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout: Duration,
}

impl Prompt {
    /// Systemprompt plus eine Nachricht.
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Prompt {
            system: system.into(),
            turns: vec![Turn::user(user)],
            max_tokens: 2000,
            temperature: 0.3,
            timeout: Duration::from_secs(120),
        }
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = t;
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }
}

/// Antwort und das Modell, das sie tatsächlich geschrieben hat.
#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub model: String,
}

/// Anfrage stellen – nur wenn die KI eingeschaltet und eingerichtet ist.
pub async fn complete(ai: &AiConfig, prompt: &Prompt) -> AiResult<Completion> {
    if let Some(why) = ai.unavailable() {
        return Err(AiError::Unavailable(why));
    }
    complete_unchecked(ai, prompt).await
}

/// Anfrage stellen, auch wenn die KI ausgeschaltet ist (für „Verbindung testen“).
/// Gewünschtes Modell zuerst, dann die Ersatzmodelle; das freie Routing zweimal.
pub async fn complete_unchecked(ai: &AiConfig, prompt: &Prompt) -> AiResult<Completion> {
    if let Some(why) = ai.missing() {
        return Err(AiError::Unavailable(why));
    }
    let mut models = vec![ai.model.clone()];
    if ai.model == DEFAULT_MODEL && ai.provider.is_openrouter() {
        models.push(DEFAULT_MODEL.into());
    }
    models.extend(ai.fallbacks.iter().filter(|m| **m != ai.model).cloned());
    let turns = normalize(&prompt.turns);
    let mut last = None;
    for model in &models {
        match call(ai, model, prompt, &turns).await {
            Ok(c) => return Ok(c),
            Err(e) if e.try_next_model() => {
                tracing::debug!(provider = ai.provider.id, model = %model, "KI-Modell versagt: {e}");
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| AiError::Failed("Kein Modell verfügbar.".into())))
}

/// Verlauf für die Anbieter aufbereiten: beginnt mit dem Benutzer, Rollen wechseln sich ab
/// (Anthropic verlangt das), leere Nachrichten fallen weg.
fn normalize(turns: &[Turn]) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for t in turns.iter().filter(|t| !t.content.trim().is_empty()) {
        if out.is_empty() && t.role == Role::Assistant {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.role == t.role => {
                last.content.push_str("\n\n");
                last.content.push_str(&t.content);
            }
            _ => out.push(t.clone()),
        }
    }
    if out.is_empty() {
        out.push(Turn::user("…"));
    }
    out
}

/// Adresse für einen Pfad; eine Abfrage in der Grundadresse (Azure: `?api-version=…`) bleibt erhalten.
fn endpoint(base: &str, path: &str) -> String {
    let (base, base_query) = base.split_once('?').unwrap_or((base, ""));
    let (path, path_query) = path.split_once('?').unwrap_or((path, ""));
    let query = [base_query, path_query]
        .into_iter()
        .filter(|q| !q.is_empty())
        .collect::<Vec<_>>()
        .join("&");
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    if query.is_empty() {
        url
    } else {
        format!("{url}?{query}")
    }
}

fn request(ai: &AiConfig, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    let mut req = HTTP.request(method, endpoint(&ai.url, path));
    if !ai.key.is_empty() {
        req = match ai.provider.auth {
            Auth::Bearer => req.bearer_auth(&ai.key),
            Auth::ApiKey => req.header("api-key", &ai.key),
            Auth::XApiKey => req.header("x-api-key", &ai.key),
        };
    }
    if ai.provider.protocol == Protocol::Anthropic {
        req = req.header("anthropic-version", "2023-06-01");
    }
    if ai.provider.is_openrouter() {
        req = req.header("X-Title", "nrzMail");
    }
    req
}

pub(crate) fn openai_body(
    model: &str,
    prompt: &Prompt,
    turns: &[Turn],
    openrouter: bool,
    compat: bool,
) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": prompt.system })];
    messages.extend(
        turns
            .iter()
            .map(|t| json!({ "role": t.role.as_str(), "content": t.content })),
    );
    let mut body = json!({ "model": model, "messages": messages, "stream": false });
    if compat {
        // Neuere OpenAI-Modelle: anderer Name für die Grenze, keine eigene Temperatur
        body["max_completion_tokens"] = json!(prompt.max_tokens);
    } else {
        body["max_tokens"] = json!(prompt.max_tokens);
        body["temperature"] = json!(prompt.temperature);
    }
    if openrouter {
        body["reasoning"] = json!({ "effort": "low" });
    }
    body
}

pub(crate) fn anthropic_body(model: &str, prompt: &Prompt, turns: &[Turn]) -> Value {
    json!({
        "model": model,
        "max_tokens": prompt.max_tokens,
        "temperature": prompt.temperature,
        "system": prompt.system,
        "messages": turns
            .iter()
            .map(|t| json!({ "role": t.role.as_str(), "content": t.content }))
            .collect::<Vec<_>>(),
    })
}

/// Fehlertext aus der Antwort des Anbieters (verschiedene Formate).
fn error_message(v: &Value) -> String {
    v["error"]["message"]
        .as_str()
        .or_else(|| v["error"].as_str())
        .or_else(|| v["message"].as_str())
        .or_else(|| v["detail"].as_str())
        .or_else(|| v["error"]["metadata"]["raw"].as_str())
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect()
}

/// HTTP-Fehler des Anbieters als verständliche Meldung.
fn classify(ai: &AiConfig, model: &str, status: u16, v: &Value) -> AiError {
    let name = ai.provider.name;
    let msg = error_message(v);
    let lower = msg.to_lowercase();
    let about_model = lower.contains("model")
        && [
            "not found",
            "does not exist",
            "invalid",
            "unknown",
            "not a valid",
            "no endpoints",
            "not available",
            "unsupported",
            "not supported",
            "nicht gefunden",
        ]
        .iter()
        .any(|w| lower.contains(w));
    let about_params = status == 400
        && (lower.contains("temperature")
            || lower.contains("max_tokens")
            || lower.contains("max_completion_tokens"));
    match status {
        401 | 403 if ai.key.is_empty() => AiError::KeyRejected(format!(
            "{name} verlangt einen Schlüssel – unter Einstellungen → KI eintragen."
        )),
        401 | 403 => AiError::KeyRejected(format!(
            "{name} hat den Schlüssel abgelehnt (falsch, abgelaufen oder ohne Berechtigung)."
        )),
        402 => AiError::Failed(format!("Kein Guthaben mehr bei {name} – bitte beim Anbieter aufladen.")),
        429 => AiError::RateLimited(format!(
            "{name}: zu viele Anfragen oder Kontingent erschöpft – bitte etwas später nochmals versuchen."
        )),
        400 | 404 | 422 if about_model => AiError::UnknownModel(format!(
            "{name} kennt das Modell „{model}“ nicht – bitte in den Einstellungen ein anderes wählen."
        )),
        404 => AiError::Failed(format!(
            "{name}: Adresse nicht gefunden (HTTP 404) – stimmt die Adresse der API ({})?",
            ai.url
        )),
        400 if about_params => AiError::Failed(format!("{name}: {msg}")),
        s if s >= 500 => AiError::Failed(format!(
            "{name} hat einen Fehler gemeldet (HTTP {s}) – bitte später nochmals versuchen."
        )),
        s if msg.is_empty() => AiError::Failed(format!("{name}: HTTP {s}")),
        s => AiError::Failed(format!("{name}: {msg} (HTTP {s})")),
    }
}

fn network_error(ai: &AiConfig, e: reqwest::Error) -> AiError {
    let name = ai.provider.name;
    if e.is_timeout() {
        AiError::Unreachable(format!(
            "{name} antwortet nicht (Zeitüberschreitung) – bitte später nochmals versuchen."
        ))
    } else if e.is_connect() {
        AiError::Unreachable(format!(
            "{name} ist nicht erreichbar ({}) – läuft der Dienst, stimmt die Adresse?",
            ai.url
        ))
    } else {
        AiError::Unreachable(format!("{name} ist nicht erreichbar: {}", e.without_url()))
    }
}

/// Text der Antwort (beide Protokolle; Inhalt als Text oder als Liste von Teilen).
fn content_of(v: &Value, anthropic: bool) -> String {
    let parts = |c: &Value| -> String {
        match c {
            Value::String(s) => s.clone(),
            Value::Array(a) => a
                .iter()
                .filter(|p| p["type"].as_str().is_none_or(|t| t == "text"))
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        }
    };
    let text = if anthropic {
        parts(&v["content"])
    } else {
        parts(&v["choices"][0]["message"]["content"])
    };
    strip_thinking(&text)
}

/// Gedankengänge mancher Modelle (`<think>…</think>`) gehören nicht in die Antwort.
pub(crate) fn strip_thinking(text: &str) -> String {
    static THINK: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?s)<think(?:ing)?>.*?</think(?:ing)?>").expect("Regex")
    });
    THINK.replace_all(text, "").trim().to_string()
}

async fn call(ai: &AiConfig, model: &str, prompt: &Prompt, turns: &[Turn]) -> AiResult<Completion> {
    if ai.provider.protocol == Protocol::Cli {
        return super::cli::complete(ai, model, prompt, turns).await;
    }
    let anthropic = ai.provider.protocol == Protocol::Anthropic;
    let mut compat = false;
    loop {
        let (path, body) = if anthropic {
            ("messages", anthropic_body(model, prompt, turns))
        } else {
            (
                "chat/completions",
                openai_body(model, prompt, turns, ai.provider.is_openrouter(), compat),
            )
        };
        let resp = request(ai, reqwest::Method::POST, path)
            .timeout(prompt.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| network_error(ai, e))?;
        let status = resp.status().as_u16();
        let raw = resp.text().await.map_err(|e| network_error(ai, e))?;
        let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        // OpenRouter meldet manche Fehler mit Status 200 und `error.code`
        let status = match v["error"]["code"].as_u64() {
            Some(code) if (400..600).contains(&code) => code as u16,
            _ if !v["error"].is_null() && (200..300).contains(&status) => 502,
            _ => status,
        };
        if !(200..300).contains(&status) {
            // Parameter, die neuere OpenAI-Modelle nicht mögen: einmal ohne sie versuchen
            let lower = error_message(&v).to_lowercase();
            if !anthropic
                && !compat
                && status == 400
                && (lower.contains("temperature") || lower.contains("max_tokens"))
            {
                compat = true;
                continue;
            }
            return Err(classify(ai, model, status, &v));
        }
        if v.is_null() {
            return Err(AiError::Failed(format!(
                "{}: unverständliche Antwort (kein JSON) – stimmt die Adresse der API?",
                ai.provider.name
            )));
        }
        let text = content_of(&v, anthropic);
        if text.is_empty() {
            return Err(AiError::Failed(format!(
                "{}: leere Antwort von {model}.",
                ai.provider.name
            )));
        }
        let used = v["model"]
            .as_str()
            .filter(|m| !m.is_empty())
            .unwrap_or(model);
        return Ok(Completion {
            text,
            model: used.to_string(),
        });
    }
}

/// Modelle des Anbieters (`GET {url}/models`), bei OpenRouter mit „kostenlos“.
pub async fn models(ai: &AiConfig) -> AiResult<Vec<api::AiModel>> {
    if ai.provider.protocol == Protocol::Cli {
        return super::cli::models(ai).await;
    }
    if ai.url.is_empty() {
        return Err(AiError::Unavailable(format!(
            "Für {} fehlt die Adresse der API.",
            ai.provider.name
        )));
    }
    let path = if ai.provider.protocol == Protocol::Anthropic {
        "models?limit=1000"
    } else {
        "models"
    };
    let resp = request(ai, reqwest::Method::GET, path)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| network_error(ai, e))?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(classify(ai, &ai.model, status, &v));
    }
    let list = v["data"]
        .as_array()
        .or_else(|| v["models"].as_array())
        .or_else(|| v.as_array());
    let mut out: Vec<api::AiModel> = list
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str().or_else(|| m["name"].as_str())?;
            let id = id.trim_start_matches("models/").to_string();
            let name = m["display_name"]
                .as_str()
                .or_else(|| m["displayName"].as_str())
                .or_else(|| m["name"].as_str())
                .unwrap_or(&id)
                .trim_start_matches("models/")
                .to_string();
            let free = ai.provider.is_openrouter()
                && (id.ends_with(":free")
                    || id == DEFAULT_MODEL
                    || (m["pricing"]["prompt"].as_str() == Some("0")
                        && m["pricing"]["completion"].as_str() == Some("0")));
            Some(api::AiModel { id, name, free })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

/// Kurze Probe-Anfrage („Verbindung testen“), auch bei ausgeschalteter KI.
pub async fn test(ai: &AiConfig) -> api::AiTestResult {
    let prompt = Prompt::new(
        "Antworte nur mit dem Wort OK.",
        "Verbindungstest – bitte nur „OK“ antworten.",
    )
    .max_tokens(300)
    .temperature(0.0)
    .timeout(Duration::from_secs(45));
    let started = Instant::now();
    match complete_unchecked(ai, &prompt).await {
        Ok(c) => api::AiTestResult {
            ok: true,
            message: format!(
                "Verbindung zu {} klappt – Antwort von {} in {:.1} s.",
                ai.provider.name,
                c.model,
                started.elapsed().as_secs_f32()
            ),
            model: c.model,
        },
        Err(e) => api::AiTestResult {
            ok: false,
            message: e.to_string(),
            model: ai.model.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_for_both_protocols() {
        let p = Prompt::new("sys", "frage").max_tokens(100).temperature(0.0);
        let turns = normalize(&[
            Turn::assistant("Hallo, ich bin Dieter."),
            Turn::user("a"),
            Turn::user("b"),
            Turn::assistant("c"),
            Turn::user("d"),
        ]);
        assert_eq!(
            turns.len(),
            3,
            "beginnt mit user, gleiche Rollen zusammengelegt"
        );
        assert_eq!(turns[0].content, "a\n\nb");
        let a = anthropic_body("claude", &p, &turns);
        assert_eq!(a["system"], "sys");
        assert_eq!(a["messages"][0]["role"], "user");
        assert_eq!(a["messages"][1]["role"], "assistant");
        let o = openai_body("gpt", &p, &turns, false, false);
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(o["messages"].as_array().unwrap().len(), 4);
        assert_eq!(o["max_tokens"], 100);
        assert!(o.get("reasoning").is_none());
        let c = openai_body("gpt", &p, &turns, true, true);
        assert!(c.get("temperature").is_none() && c["max_completion_tokens"] == 100);
        assert!(c.get("reasoning").is_some());
    }

    #[test]
    fn endpoints_keep_queries() {
        assert_eq!(
            endpoint("http://x/v1", "chat/completions"),
            "http://x/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://r.openai.azure.com/openai/deployments/m?api-version=2024-10-21", "chat/completions"),
            "https://r.openai.azure.com/openai/deployments/m/chat/completions?api-version=2024-10-21"
        );
        assert_eq!(
            endpoint("http://x/v1?a=1", "models?limit=5"),
            "http://x/v1/models?a=1&limit=5"
        );
    }

    #[test]
    fn content_formats() {
        let o = json!({ "choices": [{ "message": { "content": "<think>hmm</think>\nHallo" } }] });
        assert_eq!(content_of(&o, false), "Hallo");
        let parts = json!({ "choices": [{ "message": { "content": [{ "type": "text", "text": "A" }, { "type": "text", "text": "B" }] } }] });
        assert_eq!(content_of(&parts, false), "AB");
        let a = json!({ "content": [{ "type": "thinking", "thinking": "x" }, { "type": "text", "text": "Hi" }] });
        assert_eq!(content_of(&a, true), "Hi");
    }
}
