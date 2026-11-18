//! Datenmodelle des Kerns (Benutzer). Mail und KI haben ihre Zeilen-Strukturen in ihren
//! eigenen Modulen. Abfragen laufen zur Laufzeit (`query_as`), da die Makros zur
//! Übersetzungszeit eine unverschlüsselte Datenbank bräuchten.

use crate::api;
use sqlx::FromRow;

/// Neue ID: UUIDv7 (zeitlich sortierbar) als Text.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Jetzt als Unix-Sekunden (UTC) – so stehen Zeitpunkte in der Datenbank.
pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Zeile aus `users`.
#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub language: String,
    pub preferences: String,
    pub last_login_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl User {
    /// Für die API – ohne Passwort-Hash.
    pub fn to_api(&self) -> api::User {
        api::User {
            id: self.id.clone(),
            username: self.username.clone(),
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            is_admin: self.is_admin,
            disabled: self.disabled,
            language: self.language.clone(),
            created_at: self.created_at,
            last_login_at: self.last_login_at,
        }
    }

    /// Gespeicherte Einstellungen der Oberfläche (fehlende Felder mit Vorgabewerten).
    pub fn preferences(&self) -> api::Preferences {
        serde_json::from_str(&self.preferences).unwrap_or_default()
    }
}
