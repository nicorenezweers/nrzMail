//! nrzMail – Typen der JSON-API unter `/api/v1`, gemeinsam für den Server (Rocket) und die
//! Oberfläche (Dioxus/WebAssembly). Beide Seiten benutzen genau diese Strukturen, damit
//! Anfragen und Antworten nicht auseinanderlaufen.
//!
//! Regeln: Felder in camelCase, Zeitpunkte als Unix-Sekunden (UTC), IDs als Text (UUIDv7).
//! Fehler kommen immer als [`ApiError`] (`{"error":{"code":…,"message":…}}`) mit passendem Status.
//! Anmeldung über das Sitzungs-Cookie; ändernde Anfragen mit fremdem `Origin` werden abgewiesen.
//!
//! # Endpunkte
//!
//! | Methode & Pfad | Anfrage | Antwort |
//! |---|---|---|
//! | **Allgemein** | | |
//! | `GET /health` | – | [`Health`] |
//! | **Anmeldung & Konto** | | |
//! | `GET /auth/status` | – | [`AuthStatus`] (auch ohne Anmeldung) |
//! | `POST /auth/setup` | [`SetupRequest`] | [`User`] – nur solange es keine Benutzer gibt; erstes Konto = Admin, meldet an |
//! | `POST /auth/register` | [`SetupRequest`] | [`User`] – nur wenn die Registrierung offen ist; meldet an |
//! | `POST /auth/login` | [`LoginRequest`] | [`User`] |
//! | `POST /auth/logout` | – | 204 |
//! | `GET /me` | – | [`User`] |
//! | `PUT /me` | [`UpdateProfile`] | [`User`] |
//! | `PUT /me/password` | [`ChangePassword`] | 204 (beendet alle anderen Sitzungen) |
//! | `GET /me/preferences` | – | [`Preferences`] |
//! | `PUT /me/preferences` | [`Preferences`] | [`Preferences`] |
//! | **Verwaltung (nur Admins)** | | |
//! | `GET /admin/users` | – | `Vec<`[`User`]`>` |
//! | `POST /admin/users` | [`NewUser`] | [`User`] (201) |
//! | `PUT /admin/users/<id>` | [`UpdateUser`] | [`User`] |
//! | `DELETE /admin/users/<id>` | – | 204 (nicht sich selbst, nicht den letzten Admin) |
//! | `GET /admin/settings` | – | [`InstanceSettings`] |
//! | `PUT /admin/settings` | [`InstanceSettings`] | [`InstanceSettings`] |
//! | `GET /admin/activity?limit=` | – | `Vec<`[`ActivityEntry`]`>` |
//! | **Postfächer** (jeder Benutzer sieht nur seine eigenen) | | |
//! | `GET /accounts` | – | `Vec<`[`MailAccount`]`>` |
//! | `POST /accounts` | [`MailAccountInput`] | [`MailAccount`] (201), startet den ersten Abgleich |
//! | `GET /accounts/<id>` | – | [`MailAccount`] |
//! | `PUT /accounts/<id>` | [`MailAccountInput`] | [`MailAccount`] (leere Passwörter = unverändert) |
//! | `DELETE /accounts/<id>` | – | 204 (löscht auch das lokale Archiv des Postfachs) |
//! | `POST /accounts/test` | [`MailAccountInput`] | [`ConnectionTest`] (mit `id` in `?account=`: gespeicherte Passwörter nutzen) |
//! | `POST /accounts/<id>/sync` | – | 202, Abgleich läuft im Hintergrund |
//! | `GET /folders?account=` | – | `Vec<`[`Folder`]`>` |
//! | `PUT /folders/<id>` | [`FolderUpdate`] | [`Folder`] |
//! | **Nachrichten** | | |
//! | `GET /messages?<MessageQuery>` | – | [`MessageList`] |
//! | `GET /messages/<id>` | – | [`MessageDetail`] (markiert nicht als gelesen) |
//! | `GET /messages/<id>/raw` | – | `message/rfc822` (Download `.eml`) |
//! | `GET /messages/<id>/pdf` | – | `application/pdf` (Nachricht als PDF) |
//! | `GET /threads/<thread_id>/pdf` | – | `application/pdf` (ganze Unterhaltung als PDF) |
//! | `POST /messages/<id>/unsubscribe` | – | [`UnsubscribeResult`] (meldet beim Absender ab: One-Click, Mail an die Abmeldeadresse oder Abmeldeseite samt Formular) |
//! | `POST /sync` | – | 202, gleicht alle eigenen Postfächer ab (höchstens alle 30 Sekunden je Postfach) |
//! | `GET /threads/<thread_id>` | – | [`Thread`] |
//! | `POST /messages/actions` | [`BulkAction`] | [`BulkResult`] (wirkt lokal und per IMAP auf dem Server) |
//! | `GET /attachments/<id>?inline=` | – | Datei (Download, mit `inline=true` zum Anzeigen) |
//! | `GET /contacts?q=&limit=` | – | `Vec<`[`Contact`]`>` (aus Absendern/Empfängern, für die Autovervollständigung) |
//! | **Senden** | | |
//! | `POST /send` | [`Compose`] | [`SendResult`] (mit `sendAt` in der Zukunft: geplant; sonst nach der Sendeverzögerung aus den Einstellungen, `undoUntil`) |
//! | `GET /outbox?status=` | – | `Vec<`[`OutboxItem`]`>` (Entwürfe, geplante, fehlgeschlagene) |
//! | `POST /drafts` | [`Compose`] | [`OutboxItem`] (201) |
//! | `PUT /drafts/<id>` | [`Compose`] | [`OutboxItem`] |
//! | `DELETE /outbox/<id>` | – | 204 |
//! | `POST /outbox/<id>/undo` | – | [`OutboxItem`] (Senden zurückziehen: wird wieder Entwurf; 409, wenn schon unterwegs) |
//! | **Archiv** | | |
//! | `GET /archive` | – | [`ArchiveOverview`] |
//! | `GET /archive/timeline?account=&year=` | – | `Vec<`[`PeriodCount`]`>` (Jahre, mit `year` die Monate) |
//! | `GET /export?format=mbox\|eml&<MessageQuery>&ids=` | – | Datei (`.mbox` oder `.zip` mit `.eml`) |
//! | `POST /import?account=&folder=` | Rohdaten (mbox, eml oder zip mit eml) | [`ImportResult`] |
//! | `POST /archive/restore` | [`RestoreRequest`] | [`BulkResult`] (lädt Nachrichten wieder auf den Server) |
//! | **Statistik** | | |
//! | `GET /stats?<StatsQuery>` | – | [`Stats`] |
//! | **KI** | | |
//! | `GET /ai/status` | – | [`AiStatus`] |
//! | `GET /ai/providers` | – | `Vec<`[`AiProvider`]`>` |
//! | `GET /ai/settings` | – | [`AiSettings`] (nur Admins) |
//! | `PUT /ai/settings` | [`AiSettingsUpdate`] | [`AiSettings`] (nur Admins) |
//! | `GET /ai/models?provider=` | – | `Vec<`[`AiModel`]`>` (nur Admins; ohne `provider` der gewählte Anbieter) |
//! | `POST /ai/test` | [`AiSettingsUpdate`] (optional) | [`AiTestResult`] (nur Admins; prüft ohne zu speichern, ohne Body die gespeicherten Einstellungen) |
//! | `POST /ai/summarize` | [`SummarizeRequest`] | [`AiText`] |
//! | `POST /ai/reply` | [`DraftReplyRequest`] | [`AiDraft`] |
//! | `POST /ai/write` | [`WriteRequest`] | [`AiDraft`] |
//! | `POST /ai/chat` | [`ChatRequest`] | [`ChatReply`] |
//! | `POST /ai/categorize` | [`CategorizeRequest`] | [`BulkResult`] |

use serde::{Deserialize, Serialize};

// =================================================================== Allgemein

/// Fehlerantwort der API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Maschinenlesbar: `bad_request`, `unauthorized`, `forbidden`, `not_found`, `conflict`, …
    pub code: String,
    /// Für Menschen (deutsch).
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub name: String,
    pub version: String,
}

/// Ergebnis einer Aktion auf mehrere Einträge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BulkResult {
    pub updated: u32,
    pub failed: u32,
    /// Hinweis bei Teil-Fehlern (z. B. „Server nicht erreichbar, nur lokal geändert“).
    pub message: Option<String>,
}

// =================================================================== Anmeldung & Benutzer

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub is_admin: bool,
    pub disabled: bool,
    /// Sprache der Oberfläche, z. B. "de" oder "en".
    pub language: String,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
}

impl User {
    /// Anzeigename, sonst Benutzername.
    pub fn label(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    /// Noch kein Benutzer angelegt → Oberfläche zeigt die Einrichtung.
    pub setup_required: bool,
    pub registration_open: bool,
    pub user: Option<User>,
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
    pub display_name: String,
    pub email: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateProfile {
    pub display_name: String,
    pub email: String,
    pub language: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChangePassword {
    pub current: String,
    pub new: String,
}

/// Persönliche Einstellungen der Oberfläche (je Benutzer gespeichert).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    /// Dichte Liste (eine Zeile je Nachricht).
    pub compact: bool,
    /// Vorschautext in der Liste zeigen.
    pub show_snippets: bool,
    /// Beim Öffnen als gelesen markieren.
    pub mark_read_on_open: bool,
    /// Reiter Allgemein / Newsletter / Soziale Netzwerke / Benachrichtigungen im Posteingang.
    pub category_tabs: bool,
    /// Nachrichten zu Unterhaltungen bündeln.
    pub threads: bool,
    /// Externe Bilder in HTML-Mails automatisch laden.
    pub load_remote_images: bool,
    /// Postfach, mit dem „Neue Nachricht“ startet.
    pub default_account: Option<String>,
    /// Senden verzögern, damit man es zurückziehen kann (Sekunden, 0 = sofort, höchstens
    /// [`UNDO_SEND_MAX`]). Vorgabe eine Minute.
    pub undo_send_seconds: u32,
    /// Postfach automatisch abgleichen, solange nrzMail offen ist (Sekunden, 0 = aus).
    pub auto_refresh_seconds: u32,
    /// Posteingang mit dem Reiter „Alle“ öffnen (sonst „Allgemein“), damit nichts Neues in
    /// einem anderen Reiter untergeht.
    pub inbox_start_all: bool,
    /// Wie Unterhaltungen im Lesebereich erscheinen.
    pub conversation_view: ConversationView,
    /// In der Chat-Ansicht zitierten Text früherer Nachrichten ausblenden.
    pub chat_hide_quotes: bool,
    /// In der Chat-Ansicht ein Antwortfeld unter der Unterhaltung zeigen.
    pub chat_quick_reply: bool,
    /// Texte der KI erst als Vorschlag zeigen, der übernommen oder verworfen wird.
    pub ai_require_approval: bool,
    /// Was nach dem Abmelden von einem Newsletter mit den bisherigen Nachrichten des
    /// Absenders geschieht.
    pub unsubscribe_cleanup: UnsubscribeCleanup,
}

/// Darstellung von Unterhaltungen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationView {
    /// Sprechblasen wie in einem Chat (eigene rechts), Zitate ausgeblendet
    #[default]
    Chat,
    /// Eine Karte je Nachricht, vollständig
    Classic,
}

/// Aufräumen nach dem Abmelden.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnsubscribeCleanup {
    /// Nachrichten bleiben, wo sie sind
    #[default]
    None,
    /// Alle Nachrichten des Absenders archivieren
    Archive,
    /// Alle Nachrichten des Absenders in den Papierkorb
    Trash,
}

impl UnsubscribeCleanup {
    pub fn as_str(self) -> &'static str {
        match self {
            UnsubscribeCleanup::None => "none",
            UnsubscribeCleanup::Archive => "archive",
            UnsubscribeCleanup::Trash => "trash",
        }
    }
}

/// Längste Sendeverzögerung (30 Minuten).
pub const UNDO_SEND_MAX: u32 = 1800;
/// Kürzester Abstand des automatischen Abgleichs (Sekunden).
pub const AUTO_REFRESH_MIN: u32 = 30;
/// Vorgabe der Sendeverzögerung (1 Minute).
pub const UNDO_SEND_DEFAULT: u32 = 60;

impl Default for Preferences {
    fn default() -> Self {
        Self {
            compact: false,
            show_snippets: true,
            mark_read_on_open: true,
            category_tabs: true,
            threads: true,
            load_remote_images: false,
            default_account: None,
            undo_send_seconds: UNDO_SEND_DEFAULT,
            auto_refresh_seconds: 30,
            inbox_start_all: true,
            conversation_view: ConversationView::Chat,
            chat_hide_quotes: true,
            chat_quick_reply: true,
            ai_require_approval: true,
            unsubscribe_cleanup: UnsubscribeCleanup::None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewUser {
    pub username: String,
    pub password: String,
    pub display_name: String,
    pub email: String,
    pub is_admin: bool,
}

/// Felder, die `None` sind, bleiben unverändert.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateUser {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub is_admin: Option<bool>,
    pub disabled: Option<bool>,
    /// Neues Passwort setzen (beendet die Sitzungen des Benutzers).
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstanceSettings {
    /// Dürfen sich neue Benutzer selbst registrieren?
    pub registration_open: bool,
}

/// Eintrag im Zugriffsprotokoll (Anmeldungen, Exporte, Löschungen, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntry {
    pub id: i64,
    pub user_id: Option<String>,
    pub username: Option<String>,
    /// Kurz und maschinenlesbar, z. B. `login`, `login_failed`, `export`, `account_created`.
    pub action: String,
    pub detail: String,
    pub created_at: i64,
}

// =================================================================== Postfächer

/// Verschlüsselung der Verbindung.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    /// Direkt TLS (IMAP 993, SMTP 465)
    #[default]
    Tls,
    /// STARTTLS (IMAP 143, SMTP 587)
    Starttls,
    /// Unverschlüsselt (nur im eigenen Netz)
    None,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::Starttls => "starttls",
            Security::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tls" => Some(Security::Tls),
            "starttls" => Some(Security::Starttls),
            "none" => Some(Security::None),
            _ => None,
        }
    }
}

/// Art des Postfachs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    /// IMAP zum Empfangen, SMTP zum Senden
    #[default]
    Imap,
    /// Nur Archiv: Nachrichten kommen per Import (mbox/eml), kein Server
    Import,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AccountKind::Imap => "imap",
            AccountKind::Import => "import",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "imap" => Some(AccountKind::Imap),
            "import" => Some(AccountKind::Import),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccount {
    pub id: String,
    pub kind: AccountKind,
    /// Name in der Seitenleiste, z. B. „Privat“.
    pub name: String,
    pub email: String,
    /// Absendername.
    pub display_name: String,
    /// Farbe als CSS-Wert (z. B. "#a3a3a3"), leer = Grau.
    pub color: String,
    pub signature: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: Security,
    pub imap_username: String,
    /// Nur ein Hinweis wie `ab…yz` – Passwörter verlassen den Server nie.
    pub imap_password_hint: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: Security,
    pub smtp_username: String,
    pub smtp_password_hint: String,
    pub sync_enabled: bool,
    pub sync_interval_minutes: u32,
    /// Nachrichten nach so vielen Tagen auf dem Server löschen (bleiben im Archiv). `None` = nie.
    pub delete_from_server_after_days: Option<u32>,
    /// Nachrichten nach so vielen Tagen auch aus dem lokalen Archiv löschen. `None` = für immer behalten.
    pub archive_retention_days: Option<u32>,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    /// Läuft gerade ein Abgleich?
    pub syncing: bool,
    pub message_count: i64,
    pub unread_count: i64,
    pub size_bytes: i64,
    pub created_at: i64,
}

impl MailAccount {
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

/// Neues oder geändertes Postfach. Leere bzw. fehlende Passwörter bleiben beim Ändern unverändert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MailAccountInput {
    pub kind: AccountKind,
    pub name: String,
    pub email: String,
    pub display_name: String,
    pub color: String,
    pub signature: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: Security,
    pub imap_username: String,
    pub imap_password: Option<String>,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: Security,
    pub smtp_username: String,
    pub smtp_password: Option<String>,
    pub sync_enabled: bool,
    pub sync_interval_minutes: u32,
    pub delete_from_server_after_days: Option<u32>,
    pub archive_retention_days: Option<u32>,
}

impl Default for MailAccountInput {
    fn default() -> Self {
        Self {
            kind: AccountKind::Imap,
            name: String::new(),
            email: String::new(),
            display_name: String::new(),
            color: String::new(),
            signature: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            imap_security: Security::Tls,
            imap_username: String::new(),
            imap_password: None,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_security: Security::Tls,
            smtp_username: String::new(),
            smtp_password: None,
            sync_enabled: true,
            sync_interval_minutes: 5,
            delete_from_server_after_days: None,
            archive_retention_days: None,
        }
    }
}

/// Ergebnis von „Verbindung testen“.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnectionTest {
    pub imap_ok: bool,
    pub imap_message: String,
    pub smtp_ok: bool,
    pub smtp_message: String,
    /// Ordner auf dem Server (bei erfolgreicher IMAP-Anmeldung).
    pub folders: Vec<String>,
}

/// Bekannte Anbieter für das Formular „Postfach hinzufügen“.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPreset {
    pub id: &'static str,
    pub name: &'static str,
    /// E-Mail-Domains, an denen der Anbieter erkannt wird.
    pub domains: &'static [&'static str],
    pub imap_host: &'static str,
    pub imap_port: u16,
    pub imap_security: Security,
    pub smtp_host: &'static str,
    pub smtp_port: u16,
    pub smtp_security: Security,
    /// Hinweis, z. B. „App-Passwort nötig“.
    pub note: &'static str,
}

/// Voreinstellungen gängiger Anbieter.
pub fn account_presets() -> Vec<AccountPreset> {
    use Security::{Starttls, Tls};
    let p =
        |id, name, domains, imap_host, smtp_host, smtp_port, smtp_security, note| AccountPreset {
            id,
            name,
            domains,
            imap_host,
            imap_port: 993,
            imap_security: Tls,
            smtp_host,
            smtp_port,
            smtp_security,
            note,
        };
    vec![
        p("gmail", "Gmail", &["gmail.com", "googlemail.com"], "imap.gmail.com", "smtp.gmail.com", 465, Tls,
          "Mit Zwei-Faktor-Anmeldung ein App-Passwort verwenden (Google-Konto → Sicherheit → App-Passwörter)."),
        p("icloud", "iCloud Mail", &["icloud.com", "me.com", "mac.com"], "imap.mail.me.com", "smtp.mail.me.com", 587, Starttls,
          "App-spezifisches Passwort unter appleid.apple.com erstellen; Benutzername ist die volle Adresse."),
        p("outlook", "Outlook.com", &["outlook.com", "hotmail.com", "live.com", "msn.com"], "outlook.office365.com", "smtp-mail.outlook.com", 587, Starttls,
          "IMAP muss in den Outlook-Einstellungen eingeschaltet sein."),
        p("gmx", "GMX", &["gmx.de", "gmx.ch", "gmx.net", "gmx.at"], "imap.gmx.net", "mail.gmx.net", 465, Tls,
          "IMAP in den GMX-Einstellungen unter POP3/IMAP freischalten."),
        p("webde", "WEB.DE", &["web.de"], "imap.web.de", "smtp.web.de", 587, Starttls,
          "IMAP in den WEB.DE-Einstellungen freischalten."),
        p("infomaniak", "Infomaniak", &["ik.me", "ikmail.com"], "mail.infomaniak.com", "mail.infomaniak.com", 465, Tls, ""),
        p("proton", "Proton Mail (Bridge)", &["proton.me", "protonmail.com", "pm.me"], "127.0.0.1", "127.0.0.1", 1025, Starttls,
          "Nur über Proton Mail Bridge; Host und Ports aus der Bridge übernehmen (IMAP meist 1143 mit STARTTLS)."),
        p("fastmail", "Fastmail", &["fastmail.com", "fastmail.fm"], "imap.fastmail.com", "smtp.fastmail.com", 465, Tls,
          "App-Passwort unter Einstellungen → Datenschutz & Sicherheit erstellen."),
        p("yahoo", "Yahoo Mail", &["yahoo.com", "yahoo.de", "ymail.com"], "imap.mail.yahoo.com", "smtp.mail.yahoo.com", 465, Tls,
          "App-Passwort in den Kontosicherheits-Einstellungen erstellen."),
        p("bluewin", "Bluewin (Swisscom)", &["bluewin.ch"], "imaps.bluewin.ch", "smtpauths.bluewin.ch", 465, Tls, ""),
        p("hostpoint", "Hostpoint", &[], "imap.mail.hostpoint.ch", "asmtp.mail.hostpoint.ch", 465, Tls, ""),
    ]
}

/// Passende Voreinstellung zur Domain einer Adresse.
pub fn preset_for_email(email: &str) -> Option<AccountPreset> {
    let domain = email.rsplit_once('@')?.1.trim().to_ascii_lowercase();
    account_presets()
        .into_iter()
        .find(|p| p.domains.iter().any(|d| *d == domain))
}

// =================================================================== Ordner

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    #[default]
    Other,
}

impl FolderRole {
    pub const ALL: [FolderRole; 7] = [
        FolderRole::Inbox,
        FolderRole::Sent,
        FolderRole::Drafts,
        FolderRole::Trash,
        FolderRole::Junk,
        FolderRole::Archive,
        FolderRole::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Sent => "sent",
            FolderRole::Drafts => "drafts",
            FolderRole::Trash => "trash",
            FolderRole::Junk => "junk",
            FolderRole::Archive => "archive",
            FolderRole::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// Deutscher Name für die Oberfläche.
    pub fn label(self) -> &'static str {
        match self {
            FolderRole::Inbox => "Posteingang",
            FolderRole::Sent => "Gesendet",
            FolderRole::Drafts => "Entwürfe",
            FolderRole::Trash => "Papierkorb",
            FolderRole::Junk => "Spam",
            FolderRole::Archive => "Archiv",
            FolderRole::Other => "Ordner",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub account_id: String,
    /// Name auf dem Server, z. B. "INBOX" oder "Archiv/2024".
    pub path: String,
    /// Anzeigename (letztes Segment).
    pub name: String,
    /// Pfad des übergeordneten Ordners, falls es einen gibt.
    pub parent_path: Option<String>,
    /// Tiefe im Baum (0 = oberste Ebene).
    pub depth: u32,
    pub role: FolderRole,
    pub total: i64,
    pub unread: i64,
    pub sync_enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderUpdate {
    pub role: Option<FolderRole>,
    pub sync_enabled: Option<bool>,
}

// =================================================================== Nachrichten

/// Automatische Einteilung im Posteingang.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    #[default]
    Primary,
    Newsletter,
    Social,
    Notification,
    Other,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Primary,
        Category::Newsletter,
        Category::Social,
        Category::Notification,
        Category::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Primary => "primary",
            Category::Newsletter => "newsletter",
            Category::Social => "social",
            Category::Notification => "notification",
            Category::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Primary => "Allgemein",
            Category::Newsletter => "Newsletter",
            Category::Social => "Soziale Netzwerke",
            Category::Notification => "Benachrichtigungen",
            Category::Other => "Andere",
        }
    }
}

/// Ansicht in der Seitenleiste – über alle Postfächer, oder mit `account` nur eines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailView {
    #[default]
    Inbox,
    Unread,
    Starred,
    Sent,
    Drafts,
    Archive,
    Junk,
    Trash,
    /// Alles, auch was nur noch im lokalen Archiv liegt (inkl. Papierkorb und Spam)
    All,
    /// Nachrichten mit einem Anmelde- bzw. Bestätigungscode (6 oder 8 Ziffern)
    #[serde(rename = "2fa")]
    TwoFactor,
}

impl MailView {
    pub const ALL: [MailView; 10] = [
        MailView::Inbox,
        MailView::Unread,
        MailView::Starred,
        MailView::Sent,
        MailView::Drafts,
        MailView::Archive,
        MailView::Junk,
        MailView::Trash,
        MailView::All,
        MailView::TwoFactor,
    ];

    /// Ansichten in der Seitenleiste (Ungelesen steckt im Posteingang, Entwürfe und
    /// Archiviert gibt es nur noch als Ordner bzw. unter „Alle Nachrichten“).
    pub const SIDEBAR: [MailView; 7] = [
        MailView::Inbox,
        MailView::TwoFactor,
        MailView::Starred,
        MailView::Sent,
        MailView::Junk,
        MailView::Trash,
        MailView::All,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MailView::Inbox => "inbox",
            MailView::Unread => "unread",
            MailView::Starred => "starred",
            MailView::Sent => "sent",
            MailView::Drafts => "drafts",
            MailView::Archive => "archive",
            MailView::Junk => "junk",
            MailView::Trash => "trash",
            MailView::All => "all",
            MailView::TwoFactor => "2fa",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            MailView::Inbox => "Posteingang",
            MailView::Unread => "Ungelesen",
            MailView::Starred => "Markiert",
            MailView::Sent => "Gesendet",
            MailView::Drafts => "Entwürfe",
            MailView::Archive => "Archiviert",
            MailView::Junk => "Spam",
            MailView::Trash => "Papierkorb",
            MailView::All => "Alle Nachrichten",
            MailView::TwoFactor => "2FA-Codes",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Address {
    pub name: String,
    pub address: String,
}

impl Address {
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            address: address.into(),
        }
    }

    /// Name, sonst Adresse.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.address
        } else {
            &self.name
        }
    }

    /// `Name <adresse>` bzw. nur die Adresse.
    pub fn display(&self) -> String {
        if self.name.trim().is_empty() {
            self.address.clone()
        } else if self.name.contains([',', '<', '>', '"', ';']) {
            format!("\"{}\" <{}>", self.name.replace('"', ""), self.address)
        } else {
            format!("{} <{}>", self.name, self.address)
        }
    }

    /// Sieht die Adresse gültig aus? (einfache Prüfung: etwas@etwas.tld, keine Leerzeichen)
    pub fn is_valid(&self) -> bool {
        let a = self.address.trim();
        match a.split_once('@') {
            Some((local, domain)) => {
                !local.is_empty()
                    && domain.contains('.')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
                    && !a.contains(char::is_whitespace)
            }
            None => false,
        }
    }
}

/// Liest Empfänger aus einem Eingabefeld: `a@b.ch, Name <c@d.ch>; "Nach, Name" <e@f.ch>`.
pub fn parse_addresses(input: &str) -> Vec<Address> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut in_angle = false;
    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '<' if !in_quotes => {
                in_angle = true;
                current.push(ch);
            }
            '>' if !in_quotes => {
                in_angle = false;
                current.push(ch);
            }
            ',' | ';' | '\n' if !in_quotes && !in_angle => {
                if let Some(a) = parse_one(&current) {
                    out.push(a);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if let Some(a) = parse_one(&current) {
        out.push(a);
    }
    out
}

fn parse_one(part: &str) -> Option<Address> {
    let part = part.trim();
    if part.is_empty() {
        return None;
    }
    if let (Some(start), Some(end)) = (part.rfind('<'), part.rfind('>')) {
        if start < end {
            let address = part[start + 1..end].trim().to_string();
            let name = part[..start].trim().trim_matches('"').trim().to_string();
            return Some(Address { name, address });
        }
    }
    Some(Address {
        name: String::new(),
        address: part.trim_matches(['<', '>']).to_string(),
    })
}

/// Mehrere Adressen für ein Eingabefeld.
pub fn format_addresses(list: &[Address]) -> String {
    list.iter()
        .map(Address::display)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Filter für `GET /messages` und `GET /export`. Als Abfrage: [`MessageQuery::to_query`].
///
/// `q` versteht neben freiem Text: `from:`, `to:`, `subject:`, `has:attachment`, `is:unread`,
/// `is:starred`, `is:sent`, `before:2024-12-31`, `after:2024-01-01`, `folder:`, `account:`,
/// `category:newsletter`, Wörter in Anführungszeichen und `-wort` (ausschliessen).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageQuery {
    pub view: Option<MailView>,
    pub account: Option<String>,
    pub folder: Option<String>,
    pub category: Option<Category>,
    pub q: Option<String>,
    pub unread: Option<bool>,
    pub flagged: Option<bool>,
    pub attachments: Option<bool>,
    /// Nur Nachrichten ab diesem Zeitpunkt (Unix-Sekunden)
    pub after: Option<i64>,
    /// Nur Nachrichten vor diesem Zeitpunkt (Unix-Sekunden)
    pub before: Option<i64>,
    /// Zu Unterhaltungen bündeln (je Unterhaltung die neueste Nachricht)
    pub threads: Option<bool>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

impl MessageQuery {
    /// Als URL-Abfrage ohne `?`, z. B. `view=inbox&q=rechnung&limit=50`.
    pub fn to_query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut push = |k: &str, v: String| parts.push(format!("{k}={}", encode(&v)));
        if let Some(v) = self.view {
            push("view", v.as_str().into());
        }
        if let Some(v) = &self.account {
            push("account", v.clone());
        }
        if let Some(v) = &self.folder {
            push("folder", v.clone());
        }
        if let Some(v) = self.category {
            push("category", v.as_str().into());
        }
        if let Some(v) = self.q.as_ref().filter(|q| !q.trim().is_empty()) {
            push("q", v.clone());
        }
        for (k, v) in [
            ("unread", self.unread),
            ("flagged", self.flagged),
            ("attachments", self.attachments),
            ("threads", self.threads),
        ] {
            if let Some(v) = v {
                push(k, v.to_string());
            }
        }
        for (k, v) in [
            ("after", self.after),
            ("before", self.before),
            ("offset", self.offset),
            ("limit", self.limit),
        ] {
            if let Some(v) = v {
                push(k, v.to_string());
            }
        }
        parts.join("&")
    }
}

/// Prozent-Kodierung für Abfragewerte (UTF-8, alles ausser A–Z a–z 0–9 - _ . ~).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub id: String,
    pub account_id: String,
    pub folder_id: Option<String>,
    pub thread_id: String,
    /// Nachrichten in der Unterhaltung (1 = allein).
    pub thread_count: u32,
    pub subject: String,
    pub from: Address,
    pub to: Vec<Address>,
    /// Anfang des Textes (ca. 200 Zeichen, ohne Zitate).
    pub snippet: String,
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub has_attachments: bool,
    pub category: Category,
    pub size: i64,
    /// Liegt die Nachricht noch auf dem Server? (false = nur noch im lokalen Archiv)
    pub on_server: bool,
    /// Vom Benutzer gesendet.
    pub is_sent: bool,
    /// Erkannter Anmelde-/Bestätigungscode (6 oder 8 Ziffern), sonst leer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub otp_code: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageList {
    pub items: Vec<MessageSummary>,
    /// Treffer insgesamt (für „1–50 von 1234“).
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub message_id: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    /// Im HTML eingebettet (cid:)
    pub inline: bool,
    pub content_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDetail {
    #[serde(flatten)]
    pub summary: MessageSummary,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// `Message-ID`-Header
    pub message_id_header: String,
    pub in_reply_to: String,
    pub text_body: String,
    /// Bereinigtes HTML (ohne Skripte, Formulare, Event-Handler; `cid:` zeigt auf
    /// `/api/v1/attachments/<id>?inline=true`; externe Bilder als `data-src`, bis der Benutzer sie lädt).
    pub html_body: Option<String>,
    /// Enthält das HTML externe Bilder?
    pub has_remote_images: bool,
    pub attachments: Vec<Attachment>,
    /// Abmelde-Link aus `List-Unsubscribe` (https oder mailto)
    pub list_unsubscribe: Option<String>,
    /// Zusammenfassung, falls schon einmal erstellt
    pub ai_summary: Option<String>,
    pub account_email: String,
    pub folder_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub subject: String,
    /// Älteste zuerst
    pub messages: Vec<MessageDetail>,
}

/// Aktion auf Nachrichten. Als JSON z. B. `{"type":"markRead"}` oder `{"type":"move","folderId":"…"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MessageAction {
    MarkRead,
    MarkUnread,
    Flag,
    Unflag,
    /// In den Archiv-Ordner des Postfachs
    Archive,
    /// In den Papierkorb
    Trash,
    /// Endgültig löschen (Server und lokal)
    Delete,
    /// Aus Papierkorb/Spam/Archiv zurück in den Posteingang
    Restore,
    Spam,
    NotSpam,
    Move {
        folder_id: String,
    },
    SetCategory {
        category: Category,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkAction {
    pub ids: Vec<String>,
    pub action: MessageAction,
}

/// Ergebnis von „Abmelden“.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UnsubscribeResult {
    /// Abmeldung abgeschickt (One-Click, Mail oder Formular)
    pub done: bool,
    /// Wie: `one-click`, `mailto`, `form`, `link` oder `manual`
    pub method: String,
    /// Für Menschen
    pub message: String,
    /// Seite, die der Benutzer selbst öffnen muss (wenn es nicht automatisch ging)
    pub open_url: Option<String>,
    /// Bisherige Nachrichten des Absenders, die aufgeräumt wurden
    pub cleaned: u32,
}

/// Bekannte Adresse für die Autovervollständigung.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub name: String,
    pub address: String,
    /// Wie oft die Adresse vorkam
    pub count: i64,
    pub last_seen: i64,
}

// =================================================================== Senden

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub content_type: String,
    /// Inhalt als Base64 (Standard-Alphabet)
    pub data_base64: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Compose {
    pub account_id: String,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    /// Text (immer mitgeschickt)
    pub text: String,
    /// Optional HTML-Fassung
    pub html: Option<String>,
    /// Antwort auf diese Nachricht (lokale ID): setzt In-Reply-To/References, markiert als beantwortet
    pub in_reply_to: Option<String>,
    /// Weiterleitung dieser Nachricht (lokale ID)
    pub forward_of: Option<String>,
    pub attachments: Vec<OutgoingAttachment>,
    /// Anhänge vorhandener Nachrichten mitschicken (Attachment-IDs, z. B. beim Weiterleiten)
    pub forward_attachments: Vec<String>,
    /// Später senden (Unix-Sekunden); `None` = sofort
    pub send_at: Option<i64>,
    /// Entwurf, der damit erledigt ist (wird nach dem Senden gelöscht)
    pub draft_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxStatus {
    #[default]
    Draft,
    Scheduled,
    Sending,
    Sent,
    Failed,
}

impl OutboxStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboxStatus::Draft => "draft",
            OutboxStatus::Scheduled => "scheduled",
            OutboxStatus::Sending => "sending",
            OutboxStatus::Sent => "sent",
            OutboxStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            OutboxStatus::Draft,
            OutboxStatus::Scheduled,
            OutboxStatus::Sending,
            OutboxStatus::Sent,
            OutboxStatus::Failed,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    pub status: OutboxStatus,
    pub outbox_id: String,
    /// Lokale ID der gesendeten Nachricht (im Gesendet-Ordner), sobald gesendet
    pub message_id: Option<String>,
    /// Sendeverzögerung: bis dahin lässt sich das Senden mit `POST /outbox/<id>/undo`
    /// zurückziehen (Unix-Sekunden). `None` = sofort gesendet oder fest geplant.
    #[serde(default)]
    pub undo_until: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxItem {
    pub id: String,
    pub account_id: String,
    pub status: OutboxStatus,
    pub compose: Compose,
    pub send_at: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

// =================================================================== Archiv

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PeriodCount {
    /// "2024" oder "2024-03"
    pub label: String,
    pub count: i64,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveAccount {
    pub account: MailAccount,
    pub folders: Vec<Folder>,
    pub years: Vec<PeriodCount>,
    pub message_count: i64,
    pub size_bytes: i64,
    pub attachment_count: i64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    /// Nachrichten, die nur noch lokal liegen (auf dem Server gelöscht)
    pub local_only_count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArchiveOverview {
    pub accounts: Vec<ArchiveAccount>,
    pub total_messages: i64,
    pub total_size_bytes: i64,
    /// Grösse der Datenbankdatei
    pub database_bytes: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportResult {
    pub imported: u32,
    pub duplicates: u32,
    pub failed: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RestoreRequest {
    pub ids: Vec<String>,
    /// Zielordner auf dem Server
    pub folder_id: String,
}

// =================================================================== Statistik

/// Abfrage für `GET /stats`: `account=&days=&tz=`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StatsQuery {
    pub account: Option<String>,
    /// Zeitraum in Tagen bis heute; 0 = alles
    pub days: u32,
    /// Zeitzone des Browsers: Minuten östlich von UTC (Schweiz im Sommer: 120)
    pub tz: i32,
}

impl StatsQuery {
    pub fn to_query(&self) -> String {
        let mut q = format!("days={}&tz={}", self.days, self.tz);
        if let Some(a) = &self.account {
            q += &format!("&account={}", encode(a));
        }
        q
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StatsTotals {
    pub messages: i64,
    pub received: i64,
    pub sent: i64,
    pub unread: i64,
    pub flagged: i64,
    pub with_attachments: i64,
    pub threads: i64,
    pub size_bytes: i64,
    pub accounts: i64,
    /// Empfangene je Tag im Durchschnitt
    pub avg_per_day: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DayCount {
    /// "2026-10-31" (in der Zeitzone der Abfrage)
    pub date: String,
    pub received: i64,
    pub sent: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MonthCount {
    /// "2026-10"
    pub month: String,
    pub received: i64,
    pub sent: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SenderCount {
    pub name: String,
    pub address: String,
    pub count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DomainCount {
    pub domain: String,
    pub count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CategoryCount {
    pub category: Category,
    pub count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderCount {
    pub folder_id: String,
    pub name: String,
    pub account_name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountCount {
    pub account_id: String,
    pub name: String,
    pub color: String,
    pub count: i64,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Stats {
    /// Beginn des Zeitraums (Unix-Sekunden; bei `days=0` die älteste Nachricht)
    pub from: i64,
    pub to: i64,
    pub totals: StatsTotals,
    /// Jeder Tag im Zeitraum, auch ohne Nachrichten (bei `days=0` höchstens die letzten 365)
    pub per_day: Vec<DayCount>,
    /// Jeder Monat im Zeitraum
    pub per_month: Vec<MonthCount>,
    /// Empfangene je Stunde 0–23
    pub per_hour: Vec<i64>,
    /// Empfangene je Wochentag, Montag = 0
    pub per_weekday: Vec<i64>,
    /// 7 × 24: Wochentag (Montag = 0) × Stunde, empfangene
    pub heatmap: Vec<Vec<i64>>,
    pub top_senders: Vec<SenderCount>,
    pub top_recipients: Vec<SenderCount>,
    pub top_domains: Vec<DomainCount>,
    pub categories: Vec<CategoryCount>,
    pub folders: Vec<FolderCount>,
    pub accounts: Vec<AccountCount>,
    /// Tag mit den meisten empfangenen Nachrichten
    pub busiest_day: Option<DayCount>,
}

// =================================================================== KI

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProvider {
    pub id: String,
    pub name: String,
    pub default_url: String,
    pub default_model: String,
    pub needs_key: bool,
    /// Läuft auf dem eigenen Rechner (Ollama, LM Studio, …)
    pub local: bool,
    /// Wo es Schlüssel gibt (leer = keine Seite bekannt)
    #[serde(default)]
    pub keys_url: String,
    /// Programm auf dem Server statt einer API (Claude Code, OpenCode): `default_url` ist der
    /// Befehl, der Schlüssel ist optional (sonst gilt die Anmeldung des Programms)
    #[serde(default)]
    pub cli: bool,
}

/// KI-Einstellungen der Instanz (nur für Admins sichtbar).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    pub enabled: bool,
    pub provider: String,
    /// Wirksames Modell (eigenes oder die Vorgabe des Anbieters bzw. der Umgebung)
    pub model: String,
    /// Eigene Adresse der API (leer = Vorgabe des Anbieters)
    pub base_url: String,
    /// Hinweis auf den Schlüssel wie `sk-or-v1-0…cdef`, leer = keiner
    pub key_hint: String,
    /// Woher der Schlüssel kommt: `settings`, `environment`, `builtin` oder `none`
    pub key_source: String,
    /// Neue Nachrichten automatisch einteilen (Allgemein/Newsletter/…)
    pub auto_categorize: bool,
    /// Name des Assistenten in der Oberfläche
    pub assistant_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettingsUpdate {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    /// `None` = Schlüssel unverändert lassen, `Some("")` = Schlüssel entfernen
    pub api_key: Option<String>,
    pub auto_categorize: bool,
    pub assistant_name: String,
}

/// Für alle Benutzer: ist KI nutzbar?
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiStatus {
    pub available: bool,
    pub provider: String,
    pub model: String,
    pub assistant_name: String,
    /// Warum nicht verfügbar (z. B. „Kein Schlüssel eingetragen“)
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiModel {
    pub id: String,
    pub name: String,
    /// Kostenlos (nur bei OpenRouter bekannt)
    pub free: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiTestResult {
    pub ok: bool,
    pub message: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SummarizeRequest {
    pub message_id: Option<String>,
    /// Ganze Unterhaltung zusammenfassen
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiText {
    pub text: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DraftReplyRequest {
    pub message_id: String,
    /// Was die Antwort sagen soll, z. B. „zusagen, aber erst ab Dienstag“ (leer = KI entscheidet)
    pub instruction: String,
    /// z. B. „freundlich“, „förmlich“, „kurz“
    pub tone: String,
}

/// Neue Nachricht schreiben oder einen Text überarbeiten.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WriteRequest {
    /// Auftrag, z. B. „Termin mit Anna für nächste Woche vorschlagen“ oder „auf Englisch übersetzen“
    pub instruction: String,
    /// Bisheriger Text (zum Überarbeiten), leer = neu schreiben
    pub text: String,
    pub subject: String,
    pub to: Vec<Address>,
    pub account_id: Option<String>,
    /// Antwort auf diese Nachricht: die KI liest die ganze Unterhaltung mit
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiDraft {
    pub subject: String,
    pub text: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    #[default]
    User,
    Assistant,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatRequest {
    /// Bisheriger Verlauf, die letzte Nachricht ist die Frage
    pub messages: Vec<ChatMessage>,
    /// Gerade geöffnete Nachricht als Kontext
    pub message_id: Option<String>,
    /// Zeitzone des Browsers in Minuten östlich von UTC – für „heute“, „letzte Woche“, „im März“
    pub tz: i32,
}

/// Nachricht, auf die sich eine Antwort des Assistenten stützt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageRef {
    pub id: String,
    pub subject: String,
    pub from: Address,
    pub date: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatReply {
    pub reply: String,
    pub sources: Vec<MessageRef>,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CategorizeRequest {
    pub ids: Vec<String>,
}

// =================================================================== Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_address_lists() {
        let list = parse_addresses(
            r#"a@b.ch, Anna Muster <anna@example.com>; "Muster, Hans" <hans@example.com>"#,
        );
        assert_eq!(
            list,
            vec![
                Address::new("", "a@b.ch"),
                Address::new("Anna Muster", "anna@example.com"),
                Address::new("Muster, Hans", "hans@example.com"),
            ]
        );
        assert!(list.iter().all(Address::is_valid));
        assert!(!Address::new("", "kaputt").is_valid());
        assert_eq!(
            format_addresses(&list[1..]),
            r#"Anna Muster <anna@example.com>, "Muster, Hans" <hans@example.com>"#
        );
    }

    #[test]
    fn action_json_shape() {
        let json = r#"{"ids":["1"],"action":{"type":"move","folderId":"f1"}}"#;
        let a: BulkAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            a.action,
            MessageAction::Move {
                folder_id: "f1".into()
            }
        );
        let s = serde_json::to_string(&MessageAction::MarkRead).unwrap();
        assert_eq!(s, r#"{"type":"markRead"}"#);
    }

    #[test]
    fn query_string() {
        let q = MessageQuery {
            view: Some(MailView::Inbox),
            q: Some("from:anna rechnung".into()),
            limit: Some(50),
            ..Default::default()
        };
        assert_eq!(q.to_query(), "view=inbox&q=from%3Aanna%20rechnung&limit=50");
        assert_eq!(preset_for_email("x@GMAIL.com").unwrap().id, "gmail");
    }
}
