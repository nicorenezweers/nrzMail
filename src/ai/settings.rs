//! Einstellungen der KI: was ein Administrator in der Oberfläche einträgt (in `app_settings`
//! unter `ai`, also in der verschlüsselten Datenbank), zusammen mit der Umgebung
//! (`ai_provider`, `ai_key`, `ai_url`, `ai_model` aus [`Config`]) und dem eingebauten
//! OpenRouter-Schlüssel. Schlüssel verlassen den Server nie – nach aussen geht nur ein Hinweis.
//!
//! Schlüssel, Adressen und Modelle werden je Anbieter gemerkt: wer den Anbieter wechselt und
//! zurückkommt, findet seine Einstellungen wieder.

use super::provider::{self, Provider, BUILTIN_KEY, DEFAULT_MODEL, FALLBACK_MODELS};
use crate::api;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::services::settings;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::BTreeMap;

pub const SETTINGS_KEY: &str = "ai";
pub const DEFAULT_ASSISTANT: &str = "Dieter";

/// Gespeicherte Einstellungen (JSON in `app_settings`). Leere Felder = Vorgabe.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Stored {
    /// `None` = Vorgabe: eingeschaltet, sobald ein Schlüssel da ist bzw. bei lokalen Anbietern
    pub enabled: Option<bool>,
    /// Gewählter Anbieter (leer = aus der Umgebung, sonst OpenRouter)
    pub provider: String,
    /// Schlüssel je Anbieter
    pub keys: BTreeMap<String, String>,
    /// Eigene Adresse je Anbieter
    pub urls: BTreeMap<String, String>,
    /// Modell je Anbieter
    pub models: BTreeMap<String, String>,
    pub auto_categorize: bool,
    pub assistant_name: String,
}

/// Woher der wirksame Schlüssel kommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Settings,
    Environment,
    Builtin,
    None,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Settings => "settings",
            KeySource::Environment => "environment",
            KeySource::Builtin => "builtin",
            KeySource::None => "none",
        }
    }
}

/// Wirksamer Zugang zur KI.
#[derive(Clone)]
pub struct AiConfig {
    pub provider: &'static Provider,
    pub key: String,
    pub key_source: KeySource,
    /// Adresse ohne `/` am Ende
    pub url: String,
    pub model: String,
    /// Weitere Modelle, wenn das gewünschte versagt (nur bei kostenlosen OpenRouter-Modellen)
    pub fallbacks: Vec<String>,
    pub enabled: bool,
    pub auto_categorize: bool,
    pub assistant_name: String,
}

impl std::fmt::Debug for AiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiConfig")
            .field("provider", &self.provider.id)
            .field("key", &hint(&self.key))
            .field("url", &self.url)
            .field("model", &self.model)
            .field("enabled", &self.enabled)
            .finish()
    }
}

fn non_empty(s: &str) -> Option<&str> {
    Some(s.trim()).filter(|s| !s.is_empty())
}

/// Anbieter aus der Umgebung (leer oder unbekannt = OpenRouter).
fn env_provider(config: &Config) -> &'static Provider {
    provider::provider(&config.ai_provider)
}

/// Adresse ohne eigene Einstellung: aus der Umgebung (gleicher Anbieter) oder die Vorgabe.
fn fallback_url(config: &Config, p: &Provider) -> String {
    match non_empty(&config.ai_url) {
        Some(u) if env_provider(config).id == p.id => u.trim_end_matches('/').to_string(),
        _ => p.url.to_string(),
    }
}

/// Modell ohne eigene Einstellung: aus der Umgebung (gleicher Anbieter) oder die Vorgabe.
fn fallback_model(config: &Config, p: &Provider) -> String {
    match non_empty(&config.ai_model) {
        Some(m) if env_provider(config).id == p.id => m.to_string(),
        _ => p.model.to_string(),
    }
}

impl AiConfig {
    /// Umgebung plus gespeicherte Einstellungen (die haben Vorrang).
    pub fn resolve(config: &Config, s: &Stored) -> Self {
        let env = env_provider(config);
        let p = non_empty(&s.provider)
            .map(provider::provider)
            .unwrap_or(env);
        let (key, key_source) = if let Some(k) = s.keys.get(p.id).and_then(|k| non_empty(k)) {
            (k.to_string(), KeySource::Settings)
        } else if let Some(k) = config
            .ai_key
            .as_deref()
            .and_then(non_empty)
            .filter(|_| env.id == p.id)
        {
            (k.to_string(), KeySource::Environment)
        } else if let Some(k) = BUILTIN_KEY.filter(|_| p.is_openrouter()) {
            (k.to_string(), KeySource::Builtin)
        } else {
            (String::new(), KeySource::None)
        };
        let url = s
            .urls
            .get(p.id)
            .and_then(|u| non_empty(u))
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| fallback_url(config, p));
        let model = s
            .models
            .get(p.id)
            .and_then(|m| non_empty(m))
            .map(String::from)
            .unwrap_or_else(|| fallback_model(config, p));
        let fallbacks = if p.is_openrouter() && (model == DEFAULT_MODEL || model.ends_with(":free"))
        {
            FALLBACK_MODELS
                .iter()
                .filter(|m| **m != model)
                .map(|m| m.to_string())
                .collect()
        } else {
            Vec::new()
        };
        let enabled = s.enabled.unwrap_or(!key.is_empty() || p.key_optional);
        AiConfig {
            provider: p,
            key,
            key_source,
            url,
            model,
            fallbacks,
            enabled,
            auto_categorize: s.auto_categorize,
            assistant_name: non_empty(&s.assistant_name)
                .unwrap_or(DEFAULT_ASSISTANT)
                .to_string(),
        }
    }

    /// Wirksame Einstellungen aus Datenbank und Umgebung.
    pub async fn load(pool: &SqlitePool, config: &Config) -> AppResult<Self> {
        Ok(Self::resolve(config, &load_stored(pool).await?))
    }

    /// Was fehlt, um mit dem Anbieter zu sprechen (unabhängig vom Schalter „eingeschaltet“).
    pub fn missing(&self) -> Option<String> {
        let name = self.provider.name;
        if self.key.is_empty() && !self.provider.key_optional {
            Some(format!(
                "Kein Schlüssel für {name} eingetragen – ein Administrator kann ihn unter Einstellungen → KI hinterlegen."
            ))
        } else if self.url.is_empty() && self.provider.is_cli() {
            Some(format!("Für {name} fehlt der Befehl (z. B. „claude“)."))
        } else if self.url.is_empty() {
            Some(format!("Für {name} fehlt die Adresse der API."))
        } else if self.model.is_empty() && !self.provider.is_cli() {
            Some(format!("Für {name} ist kein Modell gewählt."))
        } else {
            None
        }
    }

    /// Warum die KI nicht nutzbar ist (`None` = bereit).
    pub fn unavailable(&self) -> Option<String> {
        if !self.enabled {
            Some("Die KI ist ausgeschaltet.".into())
        } else {
            self.missing()
        }
    }

    pub fn available(&self) -> bool {
        self.unavailable().is_none()
    }

    /// Zeichen-Budget für Mail-Text: lokale Modelle haben oft nur ein kleines Kontextfenster
    /// (Ollama schneidet sonst stillschweigend den Anfang ab – samt Systemprompt).
    pub fn budget(&self, chars: usize) -> usize {
        if self.provider.local {
            chars / 3
        } else {
            chars
        }
    }

    /// Hinweis auf den Schlüssel für die Anzeige.
    pub fn key_hint(&self) -> String {
        match self.key_source {
            KeySource::Builtin => "eingebauter Schlüssel".into(),
            KeySource::None => String::new(),
            _ => hint(&self.key),
        }
    }

    /// Für `GET /ai/settings` – ohne Schlüssel.
    pub fn to_settings(&self) -> api::AiSettings {
        api::AiSettings {
            enabled: self.enabled,
            provider: self.provider.id.into(),
            model: self.model.clone(),
            base_url: if self.url != self.provider.url {
                self.url.clone()
            } else {
                String::new()
            },
            key_hint: self.key_hint(),
            key_source: self.key_source.as_str().into(),
            auto_categorize: self.auto_categorize,
            assistant_name: self.assistant_name.clone(),
        }
    }

    /// Für `GET /ai/status` (alle Benutzer).
    pub fn to_status(&self) -> api::AiStatus {
        let why = self.unavailable();
        api::AiStatus {
            available: why.is_none(),
            provider: self.provider.id.into(),
            model: self.model.clone(),
            assistant_name: self.assistant_name.clone(),
            message: why,
        }
    }
}

pub async fn load_stored(pool: &SqlitePool) -> AppResult<Stored> {
    Ok(settings::get::<Stored>(pool, SETTINGS_KEY)
        .await?
        .unwrap_or_default())
}

pub async fn save_stored(pool: &SqlitePool, s: &Stored) -> AppResult<()> {
    settings::set(pool, SETTINGS_KEY, s).await
}

/// Kurzform eines Schlüssels: Anfang und Ende, bei kurzen Schlüsseln nur das Ende.
pub fn hint(key: &str) -> String {
    let k: Vec<char> = key.trim().chars().collect();
    let tail = |n: usize| k[k.len() - n..].iter().collect::<String>();
    match k.len() {
        0 => String::new(),
        n if n >= 20 => format!("{}…{}", k[..10].iter().collect::<String>(), tail(4)),
        n if n >= 8 => format!("…{}", tail(2)),
        _ => "…".into(),
    }
}

/// Änderung aus der Oberfläche übernehmen (prüft alle Felder).
///
/// Wechselt der Anbieter und kommen Modell bzw. Adresse als Vorgabe des bisherigen Anbieters
/// zurück (das Formular wurde nicht angepasst), gelten sie als nicht angegeben – das
/// Vorgabemodell von OpenAI passt nicht zu Anthropic. Eigene Werte werden übernommen.
pub fn apply_update(s: &mut Stored, u: &api::AiSettingsUpdate, config: &Config) -> AppResult<()> {
    let before = AiConfig::resolve(config, s);
    let p = match non_empty(&u.provider) {
        None => before.provider,
        Some(id) => {
            provider::find(id).ok_or_else(|| AppError::bad("Diesen KI-Anbieter gibt es nicht."))?
        }
    };
    let switched = p.id != before.provider.id;
    s.provider = p.id.to_string();

    match u.api_key.as_deref().map(str::trim) {
        None => {}
        Some("") => {
            s.keys.remove(p.id);
        }
        Some(k) => {
            if k.chars().count() < 8
                || k.len() > 4096
                || k.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(AppError::bad("Das sieht nicht wie ein API-Schlüssel aus."));
            }
            if p.is_openrouter() && !k.starts_with("sk-") {
                return Err(AppError::bad(
                    "Das sieht nicht wie ein OpenRouter-Schlüssel aus (sk-or-…).",
                ));
            }
            s.keys.insert(p.id.to_string(), k.to_string());
        }
    }

    let url = u.base_url.trim().trim_end_matches('/');
    if !(switched && !url.is_empty() && url == fallback_url(config, before.provider)) {
        if url.is_empty() || url == fallback_url(config, p) {
            s.urls.remove(p.id);
        } else if p.is_cli() {
            // Befehl (Pfad zum Programm, ggf. mit festen Argumenten) – nur Admins
            if url.len() > 500 || url.chars().any(char::is_control) {
                return Err(AppError::bad("Der Befehl ist zu lang oder enthält Steuerzeichen."));
            }
            s.urls.insert(p.id.to_string(), url.to_string());
        } else if !(url.starts_with("http://") || url.starts_with("https://"))
            || url.len() > 500
            || url.contains(char::is_whitespace)
        {
            return Err(AppError::bad(
                "Die Adresse der API muss mit http:// oder https:// beginnen.",
            ));
        } else {
            s.urls.insert(p.id.to_string(), url.to_string());
        }
    }

    let model: String = u.model.trim().chars().take(200).collect();
    if model.contains(char::is_whitespace) {
        return Err(AppError::bad("Der Name des Modells enthält Leerzeichen."));
    }
    if !(switched && !model.is_empty() && model == fallback_model(config, before.provider)) {
        if model.is_empty() || model == fallback_model(config, p) {
            s.models.remove(p.id);
        } else {
            s.models.insert(p.id.to_string(), model);
        }
    }

    let name = u.assistant_name.trim();
    if name.chars().count() > 40 || name.chars().any(char::is_control) {
        return Err(AppError::bad(
            "Der Name des Assistenten darf höchstens 40 Zeichen lang sein.",
        ));
    }
    s.assistant_name = if name == DEFAULT_ASSISTANT {
        String::new()
    } else {
        name.to_string()
    };
    s.enabled = Some(u.enabled);
    s.auto_categorize = u.auto_categorize;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(provider: &str) -> api::AiSettingsUpdate {
        api::AiSettingsUpdate {
            enabled: true,
            provider: provider.into(),
            ..Default::default()
        }
    }

    #[test]
    fn hints_never_show_the_whole_key() {
        assert_eq!(hint("sk-or-v1-0123456789abcdef"), "sk-or-v1-0…cdef");
        assert_eq!(hint("abcdefghij"), "…ij");
        assert_eq!(hint("abc"), "…");
        assert_eq!(hint(""), "");
    }

    #[test]
    fn defaults_and_environment() {
        let config = Config {
            ai_provider: "openai".into(),
            ai_key: Some("sk-env-0123456789abcdef".into()),
            ai_model: "gpt-4.1-mini".into(),
            ..Config::default()
        };
        let ai = AiConfig::resolve(&config, &Stored::default());
        assert_eq!(ai.provider.id, "openai");
        assert_eq!(ai.key_source, KeySource::Environment);
        assert_eq!(ai.model, "gpt-4.1-mini");
        assert!(ai.enabled && ai.available());
        assert_eq!(ai.assistant_name, "Dieter");
        assert!(!ai.auto_categorize);

        // Anderer Anbieter in den Einstellungen: kein Umgebungs-Schlüssel, dessen Vorgaben
        let s = Stored {
            provider: "mistral".into(),
            ..Default::default()
        };
        let ai = AiConfig::resolve(&config, &s);
        assert_eq!(
            (ai.key_source, ai.model.as_str()),
            (KeySource::None, "mistral-small-latest")
        );
        assert!(!ai.enabled, "ohne Schlüssel standardmässig aus");
        assert!(ai.unavailable().unwrap().contains("ausgeschaltet"));
        assert!(ai.missing().unwrap().contains("Mistral"));

        // Lokal: ohne Schlüssel eingeschaltet
        let s = Stored {
            provider: "ollama".into(),
            ..Default::default()
        };
        let ai = AiConfig::resolve(&Config::default(), &s);
        assert!(ai.enabled && ai.available());
        assert_eq!(ai.url, "http://localhost:11434/v1");
    }

    #[test]
    fn update_keeps_keys_per_provider() {
        let config = Config::default();
        let mut s = Stored::default();
        let mut u = update("anthropic");
        u.api_key = Some("sk-ant-0123456789abcdef".into());
        apply_update(&mut s, &u, &config).unwrap();
        let ai = AiConfig::resolve(&config, &s);
        assert_eq!(
            (ai.key_source, ai.model.as_str()),
            (KeySource::Settings, "claude-haiku-5-5")
        );
        assert!(s.models.is_empty(), "Vorgabe wird nicht gespeichert");

        // Wechsel zu Ollama: Modell und Adresse vom alten Anbieter zählen nicht
        let mut u = update("ollama");
        u.model = "claude-haiku-5-5".into();
        u.base_url = "https://api.anthropic.com/v1".into();
        apply_update(&mut s, &u, &config).unwrap();
        let ai = AiConfig::resolve(&config, &s);
        assert_eq!(
            (ai.model.as_str(), ai.url.as_str()),
            ("llama3.2", "http://localhost:11434/v1")
        );

        let mut u = update("ollama");
        u.model = "qwen3".into();
        u.base_url = "http://192.168.1.5:11434/v1/".into();
        apply_update(&mut s, &u, &config).unwrap();
        let ai = AiConfig::resolve(&config, &s);
        assert_eq!(
            (ai.model.as_str(), ai.url.as_str()),
            ("qwen3", "http://192.168.1.5:11434/v1")
        );
        assert_eq!(ai.to_settings().base_url, "http://192.168.1.5:11434/v1");

        // Zurück zu Anthropic: Schlüssel noch da; Some("") entfernt ihn
        apply_update(&mut s, &update("anthropic"), &config).unwrap();
        assert_eq!(
            AiConfig::resolve(&config, &s).key_source,
            KeySource::Settings
        );
        let mut u = update("anthropic");
        u.api_key = Some(String::new());
        apply_update(&mut s, &u, &config).unwrap();
        let ai = AiConfig::resolve(&config, &s);
        assert_eq!(ai.key_source, KeySource::None);
        assert!(ai.unavailable().is_some());

        for bad in [
            api::AiSettingsUpdate {
                provider: "gibts-nicht".into(),
                ..Default::default()
            },
            api::AiSettingsUpdate {
                provider: "custom".into(),
                base_url: "ftp://x".into(),
                ..Default::default()
            },
            api::AiSettingsUpdate {
                provider: "openrouter".into(),
                api_key: Some("falsch-12345".into()),
                ..Default::default()
            },
            api::AiSettingsUpdate {
                provider: "openai".into(),
                api_key: Some("kurz".into()),
                ..Default::default()
            },
            api::AiSettingsUpdate {
                assistant_name: "x".repeat(41),
                ..Default::default()
            },
        ] {
            assert!(
                apply_update(&mut s.clone(), &bad, &config).is_err(),
                "{bad:?}"
            );
        }
    }
}
