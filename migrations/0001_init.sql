-- nrzMail – Grundschema. Zeitpunkte als Unix-Sekunden (UTC), IDs als Text (UUIDv7),
-- Wahrheitswerte als INTEGER 0/1, Listen als JSON-Text.
-- Weitere Migrationen: 0010–0019 Kern (Benutzer, Anmeldung), 0020–0029 Mail, 0030–0039 KI.

-- ------------------------------------------------------------------ Benutzer & Anmeldung

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL DEFAULT '',
    email         TEXT NOT NULL DEFAULT '',
    password_hash TEXT NOT NULL,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    disabled      INTEGER NOT NULL DEFAULT 0,
    language      TEXT NOT NULL DEFAULT 'de',
    -- Persönliche Einstellungen der Oberfläche (nrzmail_api::Preferences als JSON)
    preferences   TEXT NOT NULL DEFAULT '{}',
    last_login_at INTEGER,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE sessions (
    token_hash   TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    user_agent   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX sessions_user ON sessions(user_id);

-- Instanzweite Einstellungen (JSON-Werte): registration_open, cookie_secret, ai, …
CREATE TABLE app_settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Zugriffsprotokoll: Anmeldungen, Exporte, Löschungen, Änderungen an Postfächern
CREATE TABLE activity_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id    TEXT REFERENCES users(id) ON DELETE SET NULL,
    action     TEXT NOT NULL,
    detail     TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
CREATE INDEX activity_log_created ON activity_log(created_at DESC);

-- ------------------------------------------------------------------ Postfächer & Ordner

CREATE TABLE mail_accounts (
    id                    TEXT PRIMARY KEY,
    user_id               TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind                  TEXT NOT NULL DEFAULT 'imap' CHECK (kind IN ('imap', 'import')),
    name                  TEXT NOT NULL DEFAULT '',
    email                 TEXT NOT NULL,
    display_name          TEXT NOT NULL DEFAULT '',
    color                 TEXT NOT NULL DEFAULT '',
    signature             TEXT NOT NULL DEFAULT '',
    imap_host             TEXT NOT NULL DEFAULT '',
    imap_port             INTEGER NOT NULL DEFAULT 993,
    imap_security         TEXT NOT NULL DEFAULT 'tls' CHECK (imap_security IN ('tls', 'starttls', 'none')),
    imap_username         TEXT NOT NULL DEFAULT '',
    imap_password         TEXT NOT NULL DEFAULT '',
    smtp_host             TEXT NOT NULL DEFAULT '',
    smtp_port             INTEGER NOT NULL DEFAULT 465,
    smtp_security         TEXT NOT NULL DEFAULT 'tls' CHECK (smtp_security IN ('tls', 'starttls', 'none')),
    smtp_username         TEXT NOT NULL DEFAULT '',
    smtp_password         TEXT NOT NULL DEFAULT '',
    sync_enabled          INTEGER NOT NULL DEFAULT 1,
    sync_interval_minutes INTEGER NOT NULL DEFAULT 5,
    -- NULL = nie vom Server löschen
    delete_from_server_after_days INTEGER,
    -- NULL = im lokalen Archiv für immer behalten
    archive_retention_days INTEGER,
    last_sync_at          INTEGER,
    last_error            TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);
CREATE INDEX mail_accounts_user ON mail_accounts(user_id);

CREATE TABLE folders (
    id            TEXT PRIMARY KEY,
    account_id    TEXT NOT NULL REFERENCES mail_accounts(id) ON DELETE CASCADE,
    -- Name auf dem Server (z. B. "INBOX", "Archiv/2024"); bei Import-Postfächern frei gewählt
    path          TEXT NOT NULL,
    name          TEXT NOT NULL,
    delimiter     TEXT NOT NULL DEFAULT '/',
    role          TEXT NOT NULL DEFAULT 'other'
                  CHECK (role IN ('inbox', 'sent', 'drafts', 'trash', 'junk', 'archive', 'other')),
    -- IMAP-Stand für den schrittweisen Abgleich
    uid_validity  INTEGER,
    uid_next      INTEGER,
    highest_modseq INTEGER,
    sync_enabled  INTEGER NOT NULL DEFAULT 1,
    -- 0 = gibt es auf dem Server nicht mehr (Nachrichten bleiben im Archiv)
    on_server     INTEGER NOT NULL DEFAULT 1,
    last_sync_at  INTEGER,
    UNIQUE (account_id, path)
);

-- ------------------------------------------------------------------ Nachrichten

CREATE TABLE messages (
    id               TEXT PRIMARY KEY,
    account_id       TEXT NOT NULL REFERENCES mail_accounts(id) ON DELETE CASCADE,
    folder_id        TEXT REFERENCES folders(id) ON DELETE SET NULL,
    -- IMAP-UID im Ordner; NULL = nur lokal (importiert oder vom Server gelöscht)
    uid              INTEGER,
    -- 0 = auf dem Server gelöscht, liegt nur noch im lokalen Archiv
    on_server        INTEGER NOT NULL DEFAULT 1,
    message_id       TEXT NOT NULL DEFAULT '',   -- Message-ID-Header ohne <>
    in_reply_to      TEXT NOT NULL DEFAULT '',
    refs             TEXT NOT NULL DEFAULT '',   -- References-Header, Leerzeichen-getrennt
    thread_id        TEXT NOT NULL,
    subject          TEXT NOT NULL DEFAULT '',
    from_name        TEXT NOT NULL DEFAULT '',
    from_address     TEXT NOT NULL DEFAULT '',
    to_addrs         TEXT NOT NULL DEFAULT '[]', -- JSON [{name,address}]
    cc_addrs         TEXT NOT NULL DEFAULT '[]',
    bcc_addrs        TEXT NOT NULL DEFAULT '[]',
    reply_to         TEXT NOT NULL DEFAULT '[]',
    -- Date-Header (sonst Empfangszeit)
    date             INTEGER NOT NULL,
    -- Wann nrzMail die Nachricht übernommen hat
    received_at      INTEGER NOT NULL,
    snippet          TEXT NOT NULL DEFAULT '',
    text_body        TEXT NOT NULL DEFAULT '',
    html_body        TEXT NOT NULL DEFAULT '',   -- unverändert; bereinigt wird bei der Ausgabe
    size             INTEGER NOT NULL DEFAULT 0,
    seen             INTEGER NOT NULL DEFAULT 0,
    flagged          INTEGER NOT NULL DEFAULT 0,
    answered         INTEGER NOT NULL DEFAULT 0,
    draft            INTEGER NOT NULL DEFAULT 0,
    -- Vom Benutzer gesendet (Gesendet-Ordner oder Absender = Postfach-Adresse)
    is_sent          INTEGER NOT NULL DEFAULT 0,
    has_attachments  INTEGER NOT NULL DEFAULT 0,
    category         TEXT NOT NULL DEFAULT 'primary'
                     CHECK (category IN ('primary', 'newsletter', 'social', 'notification', 'other')),
    -- Wer die Kategorie gesetzt hat: 'auto' (Regeln beim Abholen), 'ai' oder 'user'
    category_source  TEXT NOT NULL DEFAULT 'auto' CHECK (category_source IN ('auto', 'ai', 'user')),
    ai_categorized_at INTEGER,
    ai_summary       TEXT,
    list_unsubscribe TEXT NOT NULL DEFAULT '',
    -- SHA-256 der Rohdaten: erkennt Duplikate (Import, mehrfach abgeholt)
    sha256           TEXT NOT NULL DEFAULT '',
    created_at       INTEGER NOT NULL
);
CREATE INDEX messages_folder_date ON messages(folder_id, date DESC);
CREATE INDEX messages_account_date ON messages(account_id, date DESC);
CREATE INDEX messages_thread ON messages(thread_id);
CREATE INDEX messages_message_id ON messages(message_id);
CREATE INDEX messages_from ON messages(from_address);
CREATE INDEX messages_sha ON messages(account_id, sha256);
CREATE UNIQUE INDEX messages_folder_uid ON messages(folder_id, uid) WHERE uid IS NOT NULL;

-- Rohdaten (RFC 822), zlib-komprimiert – Quelle für Anhänge, .eml-Export und Wiederherstellung
CREATE TABLE message_raw (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    compressed INTEGER NOT NULL DEFAULT 1,
    data       BLOB NOT NULL
);

-- Anhänge: nur Metadaten; der Inhalt wird beim Herunterladen aus den Rohdaten gelesen
CREATE TABLE attachments (
    id           TEXT PRIMARY KEY,
    message_id   TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    filename     TEXT NOT NULL DEFAULT '',
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    size         INTEGER NOT NULL DEFAULT 0,
    content_id   TEXT NOT NULL DEFAULT '',
    inline       INTEGER NOT NULL DEFAULT 0,
    -- Index des Teils in mail_parser::Message::parts
    part_index   INTEGER NOT NULL
);
CREATE INDEX attachments_message ON attachments(message_id);

-- Volltextsuche (Umlaute und Akzente gefaltet, Präfixe ab 2 Zeichen)
CREATE VIRTUAL TABLE messages_fts USING fts5(
    message_id UNINDEXED,
    subject,
    sender,
    recipients,
    body,
    attachment_names,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

-- ------------------------------------------------------------------ Senden

-- Entwürfe, geplante und fehlgeschlagene Nachrichten (nrzmail_api::Compose als JSON)
CREATE TABLE outbox (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    account_id      TEXT NOT NULL REFERENCES mail_accounts(id) ON DELETE CASCADE,
    status          TEXT NOT NULL DEFAULT 'draft'
                    CHECK (status IN ('draft', 'scheduled', 'sending', 'sent', 'failed')),
    payload         TEXT NOT NULL,
    send_at         INTEGER,
    error           TEXT,
    sent_message_id TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX outbox_due ON outbox(status, send_at);
