# Aufbau

nrzMail ist ein einziges Rust-Programm. Es liefert die API, die Oberfläche (eine Dioxus/WebAssembly-App, beim Übersetzen eingebettet) und erledigt alle Hintergrundarbeit: Postfächer abgleichen, geplant senden, Aufbewahrungsfristen, KI-Einteilung und Aufräumen. Alle Daten – Nachrichten samt Rohdaten, Anhänge, Suchindex, Benutzer und Einstellungen – liegen in einer einzigen SQLCipher-verschlüsselten SQLite-Datei.

```
┌──────────────────── Browser ────────────────────┐
│  Dioxus-App (WASM): Posteingang, Leser, Editor, │
│  Archiv, Statistik, Einstellungen, Dieter       │
└───────────────────────┬─────────────────────────┘
                        │ /api/v1 (Sitzungs-Cookie, JSON-Typen aus shared/)
┌───────────────────────▼──────────────────────────────────────────────────┐
│ Rocket 0.5                                                               │
│  routes/api/auth, admin   Anmeldung, Konto, Verwaltung                   │
│  routes/api/mail/*        Postfächer, Ordner, Nachrichten, Senden,       │
│                           Archiv, Import/Export, Statistik               │
│  routes/api/ai            Zusammenfassen, Antworten, Schreiben, Chat     │
│  routes/spa, assets       eingebettete Oberfläche, Schrift, Icons        │
│  auth/        Argon2id, Sitzungen, Guards, Anmeldebremse                 │
│  services/    Benutzer, Instanz-Einstellungen, Zugriffsprotokoll         │
│  mail/        IMAP-Abgleich, SMTP, Zerlegen, HTML bereinigen, Suche,     │
│               mbox/eml, Aufbewahrung, Statistik                          │
│  ai/          Anbieter (OpenAI-kompatibel, Anthropic), Dieter            │
│  fairings/    Sicherheits-Header, Protokoll, Hintergrund, DB schliessen  │
└──────┬──────────────────────────┬──────────────────────────┬─────────────┘
       │ sqlx                     │ IMAP / SMTP (rustls)     │ HTTPS
┌──────▼──────────────────────┐ ┌─▼──────────────────┐ ┌─────▼────────────┐
│ nrzmail.db (SQLCipher, FTS5)│ │ Mail-Server        │ │ KI-Anbieter      │
│ Benutzer, Postfächer,       │ └────────────────────┘ │ (oder lokal,     │
│ Nachrichten + Rohdaten, …   │                        │  z. B. Ollama)   │
└─────────────────────────────┘                        └──────────────────┘
```

Die macOS-App (`macos/`, tao/wry/muda wie nrzX) zeigt dieselbe Oberfläche in einem WKWebView mit Milchglas (`html.nrz-glass`, `html.nrz-mac` – gesetzt von Skripten der App, Stile in `ui/tailwind.css`). Sie verbindet sich entweder mit einer Server-Instanz (das Fenster lädt deren Oberfläche) oder startet den Server als Bibliothek im eigenen Prozess (`nrzmail::build()`, nur `127.0.0.1`). Für den lokalen Server setzt sie `NRZMAIL_DESKTOP_TOKEN`; nur damit gibt es `GET /__desktop/login` (`routes/desktop.rs`), das ohne Passwort anmeldet und beim ersten Mal das Konto anlegt.

## Entscheidungen

| Bereich | Wahl | Warum |
|---|---|---|
| Server | Rocket 0.5.1 | Typisierte Guards für Sitzungen und Rechte, sauberes Herunterfahren |
| Speicher | Eine SQLite-Datei mit **SQLCipher** (gebündelt, OpenSSL mitübersetzt) | Sicherung = eine Datei plus Schlüssel; nichts liegt unverschlüsselt auf der Platte |
| Rohdaten | Jede Nachricht zusätzlich als RFC 822, zlib-komprimiert (`message_raw`); Anhänge nur als Metadaten | Anhänge, `.eml`-Export und Wiederherstellen auf den Server kommen aus dem Original – nichts geht beim Zerlegen verloren |
| Archiv | Nachrichten bleiben lokal, wenn sie auf dem Server verschwinden (`on_server = 0`); Import und Export als mbox, eml und zip | Das Archiv ist unabhängig vom Mail-Anbieter |
| Suche | SQLite **FTS5** (Umlaute und Akzente gefaltet, Präfixe) mit einer kleinen Abfragesprache (`from:`, `has:attachment`, `is:unread`, `before:` …) | Kein zusätzlicher Dienst |
| Mail | `async-imap` mit `tokio-rustls`, `lettre` (SMTP), `mail-parser`, `ammonia` | Reines Rust bis auf SQLCipher; HTML wird beim Ausliefern bereinigt, externe Bilder erst auf Wunsch |
| API-Vertrag | Crate `nrzmail-api` (`shared/`) mit allen Anfrage- und Antworttypen, von Server und Oberfläche benutzt | Beide Seiten übersetzen gegen dieselben Typen – kein Auseinanderlaufen |
| Oberfläche | **Dioxus 0.7** (Rust → WebAssembly) mit Komponenten aus **rust-ui**; `dx bundle` baut sie, `build.rs` bettet sie ein (gzip) | Ein Programm, kein CDN, läuft offline; die Oberfläche spricht nur mit `/api/v1` |
| KI | OpenAI-Chat-Protokoll (OpenRouter, OpenAI, Gemini, Mistral, Ollama, …) und Anthropic Messages | Cloud und lokale Modelle; Schlüssel nur auf dem Server |
| Hintergrund | Tokio-Aufgaben, gestartet beim Hochfahren, beendet mit dem Server | Kein zweiter Prozess, keine Warteschlange von aussen |
| Anmeldebremse | Fehlversuche je IP-Adresse und Benutzername im Speicher | Einfach und ohne Schreibzugriffe; ein Neustart setzt sie zurück |

## Datenmodell (Migrationen)

- `0001_init`: Benutzer, Sitzungen, Instanz-Einstellungen (`app_settings`, JSON-Werte), Zugriffsprotokoll, Postfächer, Ordner, Nachrichten, Rohdaten, Anhänge, Volltextindex (`messages_fts`), Ausgang (Entwürfe, geplante und fehlgeschlagene Nachrichten)
- `0010–0019` Kern, `0020–0029` Mail (u. a. feste Nummer jeder Nachricht für den Suchindex, Papierkorb-Zeitpunkt und ein Trigger, der Einträge im Suchindex mit ihrer Nachricht löscht), `0030–0039` KI

Jede Nachricht gehört über `mail_accounts.user_id` zu genau einem Benutzer; jede Abfrage ist auf die Postfächer des angemeldeten Benutzers beschränkt. Wird ein Benutzer gelöscht, verschwinden seine Postfächer, Nachrichten, Rohdaten, Anhänge und Sitzungen per `ON DELETE CASCADE`; seine Einträge im Zugriffsprotokoll bleiben ohne Benutzer erhalten.

## Sicherheit

- **Datenbank**: Schlüssel aus `NRZMAIL_DB_KEY` (64 Hex-Zeichen direkt, sonst Passphrase mit PBKDF2) oder aus `nrzmail.key` (beim ersten Start erzeugt, nur für den eigenen Benutzer lesbar). Ein falscher Schlüssel stoppt den Start.
- **Passwörter** als Argon2id-Hash; bei unbekanntem Namen wird ein Platzhalter-Hash geprüft, damit die Antwortzeit nichts verrät. Gesperrte Konten bekommen dieselbe Meldung wie ein falsches Passwort.
- **Sitzungen**: 32 zufällige Bytes in einem privaten (von Rocket verschlüsselten) Cookie, `HttpOnly`, `SameSite=Lax`, optional `Secure`; in der Datenbank nur der SHA-256. 30 Tage, verlängern sich bei Benutzung. Der Cookie-Schlüssel liegt verschlüsselt in der Datenbank.
- **Anmeldebremse**: nach `NRZMAIL_LOGIN_MAX_FAILURES` Fehlversuchen (Vorgabe 10) je IP-Adresse und Benutzername in `NRZMAIL_LOGIN_WINDOW_MINUTES` (Vorgabe 15) antwortet die Anmeldung mit 429 und `Retry-After`, ohne das Passwort zu prüfen. Ebenso beim Ändern des eigenen Passworts. Die IP kommt aus `X-Real-IP` (Rockets `ip_header`) oder der Verbindung.
- **CSRF**: `SameSite=Lax` plus Prüfung des `Origin` bei jeder ändernden Anfrage (`CurrentUser`, `AdminUser`, `SameOrigin`); hinter einem Proxy zählt auch `X-Forwarded-Host`.
- **Header**: Content-Security-Policy mit Skripten nur vom eigenen Server (plus `wasm-unsafe-eval` und `unsafe-eval` für die WASM-App), keine Inline-Skripte, Bilder auch von fremden Servern (externe Bilder in HTML-Mails, nur auf Wunsch geladen), `frame-ancestors 'none'`, `Referrer-Policy: same-origin`. Rockets Shield ist durch ein leeres ersetzt, alle Header kommen aus `fairings/security.rs`.
- **Verwaltung**: Admins sperren, löschen oder entrechten sich nicht selbst; dass der letzte aktive Administrator bleibt, steht als Bedingung direkt in den `UPDATE`- und `DELETE`-Anweisungen und hält so auch bei gleichzeitigen Änderungen. Sperren und neue Passwörter beenden die Sitzungen des Kontos.
- **Zugangsdaten**: Passwörter der Postfächer und KI-Schlüssel liegen nur in der verschlüsselten Datenbank und gehen nie an den Browser (nur ein Hinweis wie `ab…yz`). Ein eingebauter OpenRouter-Schlüssel kann beim eigenen Bauen als BuildKit-Secret dazukommen (`--secret id=openrouter,env=NRZ_OPENROUTER_KEY`); er steht nie im Quelltext. Das öffentliche Image enthält keinen.

## Build

`build.rs` bettet die Migrationen und die gebaute Oberfläche (`ui/dist/public`, Text und WebAssembly zusätzlich gzip) ins Programm ein. Ohne Oberfläche übersetzt der Server trotzdem und zeigt auf der Startseite einen Hinweis.

Stufen im `Dockerfile`:

1. `ui`: installiert `dioxus-cli` 0.7.10 und baut die WebAssembly-Oberfläche (`dx bundle --release`) – einmal auf dem Build-Rechner, für alle Architekturen
2. `build`: Rust-Release-Build, für amd64 und arm64 quer-übersetzt (nicht emuliert), bettet die Oberfläche ein
3. `runtime`: Debian bookworm-slim mit `ca-certificates`, Benutzer 1000, Volume `/data`, Port 8330, Health-Check über `nrzmail healthcheck`

GitHub Actions: `rust.yml` übersetzt und testet den Server bei jedem Push und Pull Request auf `main`; `release.yml` baut bei einem Tag `v*` das Multi-Arch-Image `ghcr.io/<owner>/nrz-mail:<version>` (und `:latest`) und schreibt den Digest der Manifest-Liste in die Zusammenfassung.

## Tests

`cargo test` führt Unit-Tests und Integrationstests mit Rockets lokalem Client aus, jeder gegen einen leeren Datenordner mit eigener verschlüsselter Datenbank und ohne Hintergrundarbeiten:

- `tests/core_*.rs`: Einrichtung (nur einmal, auch bei gleichzeitigen Anfragen), Anmeldung und Abmeldung, falsche Passwörter, gesperrte Konten, Anmeldebremse, Registrierung, Passwortänderung beendet andere Sitzungen, Profil und Einstellungen, Admin-Rechte, Regeln für den letzten Administrator, Zugriffsprotokoll, fremder `Origin`, Health-Check, Fehler als JSON, Rückfall auf die Oberfläche, Sicherheits-Header und Icons
- `tests/mail*.rs` und `tests/ai*.rs`: Mail- und KI-Bereich mit eigenen Helfern
