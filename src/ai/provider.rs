//! Katalog der KI-Anbieter: alles, was die OpenAI-Chat-API spricht (OpenRouter, OpenAI,
//! Google Gemini, Mistral, Groq, xAI, DeepSeek, Together, Fireworks, Nvidia, Hugging Face,
//! Azure OpenAI, Ollama, LM Studio, vLLM, LocalAI und jede eigene Adresse), dazu Anthropic
//! über die Messages-API.

use crate::api;

/// Eingebauter Schlüssel für OpenRouter (beim Bauen als Secret gesetzt, nie im Quelltext),
/// damit die KI ohne Einrichtung läuft. Ein leerer Wert zählt als „keiner“: BuildKit setzt
/// die Variable bei fehlendem Secret auf "".
pub const BUILTIN_KEY: Option<&str> = match option_env!("NRZ_OPENROUTER_KEY") {
    Some(k) if !k.is_empty() => Some(k),
    _ => None,
};

/// Kostenloses Routing von OpenRouter – Vorgabe ohne eigene Einstellungen.
pub const DEFAULT_MODEL: &str = "openrouter/free";

/// Kostenlose Ersatzmodelle bei OpenRouter, wenn das freie Routing versagt (der Reihe nach).
pub const FALLBACK_MODELS: &[&str] = &[
    "nvidia/nemotron-3.5-super-vl:free",
    "google/gemma-4-31b-it:free",
    "google/gemma-4-26b-a4b-it:free",
];

/// Wie mit dem Anbieter gesprochen wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `POST {url}/chat/completions` (OpenAI-kompatibel)
    OpenAi,
    /// `POST {url}/messages` (Anthropic)
    Anthropic,
    /// Programm auf dem Server (Claude Code, OpenCode); `url` ist der Befehl
    Cli,
}

/// Wie der Schlüssel mitgeschickt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// `Authorization: Bearer …`
    Bearer,
    /// `api-key: …` (Azure OpenAI)
    ApiKey,
    /// `x-api-key: …` (Anthropic)
    XApiKey,
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: Protocol,
    pub auth: Auth,
    /// Voreingestellte Adresse (leer = muss eingetragen werden)
    pub url: &'static str,
    /// Ohne Schlüssel nutzbar (lokale Server, eigene Adresse)
    pub key_optional: bool,
    /// Läuft auf dem eigenen Rechner bzw. im eigenen Netz
    pub local: bool,
    /// Voreingestelltes Modell (leer = in den Einstellungen wählen)
    pub model: &'static str,
    /// Wo es Schlüssel gibt
    pub keys_url: &'static str,
}

const fn cloud(
    id: &'static str,
    name: &'static str,
    url: &'static str,
    model: &'static str,
    keys_url: &'static str,
) -> Provider {
    Provider {
        id,
        name,
        protocol: Protocol::OpenAi,
        auth: Auth::Bearer,
        url,
        key_optional: false,
        local: false,
        model,
        keys_url,
    }
}

const fn local(
    id: &'static str,
    name: &'static str,
    url: &'static str,
    model: &'static str,
) -> Provider {
    Provider {
        key_optional: true,
        local: true,
        ..cloud(id, name, url, model, "")
    }
}

const fn cli(id: &'static str, name: &'static str, command: &'static str, model: &'static str, docs: &'static str) -> Provider {
    Provider {
        protocol: Protocol::Cli,
        key_optional: true,
        ..cloud(id, name, command, model, docs)
    }
}

pub const PROVIDERS: &[Provider] = &[
    cloud(
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        DEFAULT_MODEL,
        "https://openrouter.ai/keys",
    ),
    cloud(
        "opencode",
        "OpenCode Zen",
        "https://opencode.ai/zen/v1",
        "",
        "https://opencode.ai/auth",
    ),
    cli(
        "claude-code",
        "Claude Code (CLI auf dem Server)",
        "claude",
        "sonnet",
        "https://docs.claude.com/en/docs/claude-code/setup",
    ),
    cli(
        "opencode-cli",
        "OpenCode (CLI auf dem Server)",
        "opencode",
        "",
        "https://opencode.ai/docs/cli/",
    ),
    cloud(
        "xai",
        "Grok (xAI)",
        "https://api.x.ai/v1",
        "grok-4",
        "https://console.x.ai",
    ),
    cloud(
        "openai",
        "OpenAI",
        "https://api.openai.com/v1",
        "gpt-4o-mini",
        "https://platform.openai.com/api-keys",
    ),
    Provider {
        protocol: Protocol::Anthropic,
        auth: Auth::XApiKey,
        ..cloud(
            "anthropic",
            "Anthropic (Claude)",
            "https://api.anthropic.com/v1",
            "claude-haiku-5-5",
            "https://console.anthropic.com/settings/keys",
        )
    },
    cloud(
        "gemini",
        "Google Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "gemini-2.5-flash",
        "https://aistudio.google.com/apikey",
    ),
    cloud(
        "mistral",
        "Mistral AI",
        "https://api.mistral.ai/v1",
        "mistral-small-latest",
        "https://console.mistral.ai/api-keys",
    ),
    cloud(
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        "llama-3.3-70b-versatile",
        "https://console.groq.com/keys",
    ),
    cloud(
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com/v1",
        "deepseek-chat",
        "https://platform.deepseek.com/api_keys",
    ),
    cloud(
        "together",
        "Together AI",
        "https://api.together.xyz/v1",
        "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        "https://api.together.ai/settings/api-keys",
    ),
    cloud(
        "fireworks",
        "Fireworks AI",
        "https://api.fireworks.ai/inference/v1",
        "",
        "https://fireworks.ai/account/api-keys",
    ),
    cloud(
        "nvidia",
        "NVIDIA NIM",
        "https://integrate.api.nvidia.com/v1",
        "",
        "https://build.nvidia.com",
    ),
    cloud(
        "huggingface",
        "Hugging Face",
        "https://router.huggingface.co/v1",
        "",
        "https://huggingface.co/settings/tokens",
    ),
    Provider {
        auth: Auth::ApiKey,
        ..cloud("azure", "Azure OpenAI", "", "", "https://portal.azure.com")
    },
    local(
        "ollama",
        "Ollama (lokal)",
        "http://localhost:11434/v1",
        "llama3.2",
    ),
    local(
        "lmstudio",
        "LM Studio (lokal)",
        "http://localhost:1234/v1",
        "",
    ),
    local("vllm", "vLLM (lokal)", "http://localhost:8000/v1", ""),
    local("localai", "LocalAI (lokal)", "http://localhost:8080/v1", ""),
    Provider {
        key_optional: true,
        ..cloud("custom", "Eigene OpenAI-kompatible Adresse", "", "", "")
    },
];

/// Anbieter zur ID; unbekannt oder leer = OpenRouter.
pub fn provider(id: &str) -> &'static Provider {
    find(id).unwrap_or(&PROVIDERS[0])
}

/// Anbieter zur ID, falls es ihn gibt.
pub fn find(id: &str) -> Option<&'static Provider> {
    let id = id.trim();
    PROVIDERS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

impl Provider {
    pub fn to_api(&self) -> api::AiProvider {
        api::AiProvider {
            id: self.id.into(),
            name: self.name.into(),
            default_url: self.url.into(),
            default_model: self.model.into(),
            needs_key: !self.key_optional,
            local: self.local,
            keys_url: self.keys_url.into(),
            cli: self.protocol == Protocol::Cli,
        }
    }

    pub fn is_openrouter(&self) -> bool {
        self.id == "openrouter"
    }

    pub fn is_cli(&self) -> bool {
        self.protocol == Protocol::Cli
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        let mut ids: Vec<&str> = PROVIDERS.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), PROVIDERS.len(), "IDs eindeutig");
        assert_eq!(provider("gibt-es-nicht").id, "openrouter");
        assert_eq!(provider("").id, "openrouter");
        assert_eq!(provider("Anthropic").protocol, Protocol::Anthropic);
        for p in PROVIDERS {
            assert!(p.is_cli() || p.url.is_empty() || p.url.starts_with("http"), "{}", p.id);
            assert!(!p.url.ends_with('/'), "{}", p.id);
            assert!(!p.local || p.key_optional, "{}", p.id);
        }
        for id in [
            "openrouter",
            "opencode",
            "claude-code",
            "opencode-cli",
            "openai",
            "anthropic",
            "gemini",
            "mistral",
            "groq",
            "xai",
            "deepseek",
            "together",
            "fireworks",
            "nvidia",
            "huggingface",
            "azure",
            "ollama",
            "lmstudio",
            "vllm",
            "localai",
            "custom",
        ] {
            assert!(find(id).is_some(), "{id}");
        }
    }
}
