//! Einstellungen. Quelle ist Rockets Figment: `Rocket.toml` und Umgebungsvariablen mit
//! dem Präfix `NRZMAIL_` (z. B. `NRZMAIL_DB_KEY`, `NRZMAIL_DATA_DIR`).

use rocket::figment::providers::{Env, Format, Toml};
use rocket::figment::Figment;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Datenordner: Datenbank und Schlüsseldatei.
    pub data_dir: PathBuf,
    /// Schlüssel der Datenbank. Fehlt er, wird `data_dir/nrzmail.key` erzeugt bzw. gelesen.
    pub db_key: Option<String>,
    /// Neue Benutzer dürfen sich selbst registrieren (das erste Konto immer).
    pub registration_open: bool,
    /// Cookies nur über HTTPS senden – einschalten, sobald nrzMail hinter HTTPS läuft.
    pub cookie_secure: bool,
    /// Hintergrundarbeiten (Abgleich, geplantes Senden, Aufräumen, KI) starten. Tests schalten sie aus.
    pub background: bool,
    /// Gleichzeitige Abgleiche von Postfächern.
    pub sync_workers: usize,
    /// Kürzester erlaubter Abstand zwischen zwei Abgleichen eines Postfachs (Minuten).
    pub min_sync_interval_minutes: u32,
    /// Beim ersten Abgleich höchstens so viele Nachrichten je Ordner holen (0 = alle).
    pub initial_sync_limit: u32,
    /// Erlaubt Verbindungen zu Mail-Servern ohne gültiges Zertifikat (nur für eigene Server im LAN).
    pub allow_invalid_certs: bool,
    /// Tage, die Nachrichten im Papierkorb bleiben, bevor nrzMail sie dort endgültig löscht (0 = nie).
    pub trash_retention_days: u32,
    /// KI-Anbieter aus der Umgebung (z. B. "openai", "anthropic", "ollama"; leer = OpenRouter).
    /// In den Einstellungen der Oberfläche lässt sich jeder Anbieter wählen.
    pub ai_provider: String,
    /// Schlüssel für `ai_provider`
    pub ai_key: Option<String>,
    /// Eigene Adresse der API (leer = Vorgabe des Anbieters)
    pub ai_url: String,
    /// Modell (leer = Vorgabe des Anbieters)
    pub ai_model: String,
    /// Anmeldebremse: so viele Fehlversuche je IP und Benutzername sind erlaubt (0 = aus) …
    pub login_max_failures: u32,
    /// … innerhalb dieser Minuten; danach wird bis zum Ende des Zeitfensters abgewiesen.
    pub login_window_minutes: u32,
    /// Nur in der macOS-App (lokaler Server): Token, mit dem sich die App ohne Passwort anmeldet
    /// (`/__desktop/login`). Ohne Token gibt es diese Anmeldung nicht.
    pub desktop_token: Option<String>,
    /// Benutzername und Anzeigename für das Konto, das die App beim ersten Start anlegt.
    pub desktop_user: String,
    pub desktop_full_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data"),
            db_key: None,
            registration_open: false,
            cookie_secure: false,
            background: true,
            sync_workers: 3,
            min_sync_interval_minutes: 1,
            initial_sync_limit: 0,
            allow_invalid_certs: false,
            trash_retention_days: 0,
            ai_provider: String::new(),
            ai_key: None,
            ai_url: String::new(),
            ai_model: String::new(),
            login_max_failures: 10,
            login_window_minutes: 15,
            desktop_token: None,
            desktop_user: String::new(),
            desktop_full_name: String::new(),
        }
    }
}

impl Config {
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("nrzmail.db")
    }

    pub fn key_path(&self) -> PathBuf {
        self.data_dir.join("nrzmail.key")
    }
}

/// Rocket-Konfiguration plus `NRZMAIL_*`-Umgebung (statt `ROCKET_*`).
pub fn figment() -> Figment {
    Figment::from(rocket::Config::default())
        .merge(Toml::file(Env::var_or("NRZMAIL_CONFIG", "Rocket.toml")).nested())
        // NRZMAIL_LOG ist der tracing-Filter (main.rs), keine Rocket-Einstellung
        .merge(
            Env::prefixed("NRZMAIL_")
                .ignore(&["PROFILE", "LOG"])
                .global(),
        )
        .select(rocket::figment::Profile::from_env_or(
            "NRZMAIL_PROFILE",
            rocket::Config::DEFAULT_PROFILE,
        ))
}
