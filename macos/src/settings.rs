//! Einstellungen der App (`~/Library/Application Support/nrzMail/app.json`): mit welcher
//! nrzMail-Instanz sie verbunden ist und ob sie gerade lokal oder mit dem Server arbeitet.
//!
//! * **Server** (empfohlen): Das Fenster zeigt die Oberfläche der Instanz (z. B. auf dem Umbrel).
//!   Postfächer, Mails, KI und Einstellungen liegen dort – nichts muss in der App neu verbunden
//!   werden. Die Anmeldung merkt sich WebKit (Cookie, 30 Tage, verlängert sich bei Benutzung).
//! * **Lokal**: Der eingebaute Server der App mit eigener Datenbank – geht ohne Netz und ohne
//!   Server-Instanz.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Noch nicht gewählt: Willkommensseite
    #[default]
    Unset,
    Server,
    Local,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    /// Adresse der Instanz, z. B. `http://umbrel.local:8330` (ohne `/` am Ende)
    pub server: Option<String>,
}

fn path() -> PathBuf {
    crate::server::data_dir().join("app.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }
}

/// Eingabe zur Adresse einer Instanz: `http://` ergänzen, Pfad und `/` am Ende weg.
pub fn normalize(input: &str) -> Result<String, String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return Err("Bitte die Adresse des nrzMail-Servers eingeben.".into());
    }
    let with_scheme = if s.contains("://") { s.to_string() } else { format!("http://{s}") };
    let (scheme, rest) = with_scheme.split_once("://").unwrap_or(("http", &with_scheme));
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err("Die Adresse muss mit http:// oder https:// beginnen.".into());
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains(char::is_whitespace) || host.contains('@') {
        return Err("Diese Adresse ist ungültig.".into());
    }
    Ok(format!("{scheme}://{host}"))
}

/// Ursprung (`scheme://host[:port]`) einer Adresse, klein geschrieben – zum Vergleichen.
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    let scheme = scheme.to_ascii_lowercase();
    let mut host = host.to_ascii_lowercase();
    // Standard-Ports wie der Browser weglassen
    if (scheme == "http" && host.ends_with(":80")) || (scheme == "https" && host.ends_with(":443")) {
        host.truncate(host.rfind(':').unwrap_or(host.len()));
    }
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

#[derive(Deserialize)]
struct Health {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
}

/// Prüft, ob unter `base` ein nrzMail-Server antwortet (`/api/v1/health`); Ergebnis: Version.
pub async fn probe(base: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .connect_timeout(Duration::from_secs(4))
        .user_agent(concat!("nrzMail-macOS/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let res = client.get(format!("{base}/api/v1/health")).send().await.map_err(|e| {
        if e.is_timeout() || e.is_connect() {
            "Der Server antwortet nicht. Läuft nrzMail dort, und stimmt die Adresse (mit Port)?".to_string()
        } else {
            format!("Keine Verbindung: {e}")
        }
    })?;
    if !res.status().is_success() {
        return Err(format!("Der Server antwortet mit {} – ist das wirklich nrzMail?", res.status()));
    }
    let h: Health = res.json().await.map_err(|_| "Unter dieser Adresse läuft kein nrzMail.".to_string())?;
    if h.name != "nrzMail" {
        return Err("Unter dieser Adresse läuft kein nrzMail.".into());
    }
    Ok(h.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("umbrel.local:8330").unwrap(), "http://umbrel.local:8330");
        assert_eq!(normalize(" https://mail.example.ch/ ").unwrap(), "https://mail.example.ch");
        assert_eq!(normalize("http://10.0.0.2:8330/mail?x").unwrap(), "http://10.0.0.2:8330");
        assert!(normalize("").is_err());
        assert!(normalize("ftp://x").is_err());
    }

    #[test]
    fn origins() {
        assert_eq!(origin("HTTP://Umbrel.local:8330/mail").unwrap(), "http://umbrel.local:8330");
        assert_eq!(origin("https://a.ch:443/").unwrap(), "https://a.ch");
        assert_eq!(origin("about:blank"), None);
    }
}
