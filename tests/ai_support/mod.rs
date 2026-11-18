//! Helfer der KI-Tests: Server mit leerem Datenordner, Benutzer mit Sitzung, Postfächer und
//! Nachrichten direkt per SQL (samt Volltextindex) und ein gefälschter KI-Server, der
//! Anfragen aufzeichnet und nach Wunsch antwortet (OpenAI- wie Anthropic-Format).
#![allow(dead_code)]

use rocket::figment::Figment;
use rocket::http::{Accept, ContentType, Cookie, Status};
use rocket::local::asynchronous::Client;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const DB_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub fn figment(dir: &tempfile::TempDir) -> Figment {
    nrzmail::config::figment()
        .merge(("data_dir", dir.path()))
        .merge(("db_key", DB_KEY))
        .merge(("log_level", "off"))
        .merge(("background", false))
        .merge(("ai_provider", ""))
        .merge(("ai_url", ""))
        .merge(("ai_model", ""))
}

pub async fn server_with(adjust: impl FnOnce(Figment) -> Figment) -> (tempfile::TempDir, Client) {
    let dir = tempfile::tempdir().unwrap();
    let rocket = nrzmail::build_with(adjust(figment(&dir))).await.unwrap();
    (dir, Client::tracked(rocket).await.unwrap())
}

pub async fn server() -> (tempfile::TempDir, Client) {
    server_with(|f| f).await
}

pub fn pool(c: &Client) -> &SqlitePool {
    c.rocket().state::<SqlitePool>().unwrap()
}

pub fn config(c: &Client) -> &nrzmail::config::Config {
    c.rocket().state::<nrzmail::config::Config>().unwrap()
}

/// Benutzer mit Sitzung: (ID, Sitzungs-Token).
pub async fn user(c: &Client, name: &str, admin: bool) -> (String, String) {
    let u = nrzmail::services::users::create(
        pool(c),
        &nrzmail::services::users::NewAccount {
            username: name,
            password: "geheim-123",
            display_name: &format!("{name} Muster"),
            email: &format!("{name}@example.com"),
            is_admin: admin,
        },
    )
    .await
    .unwrap();
    let token = nrzmail::auth::session::create(pool(c), &u.id, "test")
        .await
        .unwrap();
    (u.id, token)
}

/// JSON-Aufruf mit der Sitzung `token` (leer = ohne Anmeldung).
pub async fn call(
    c: &Client,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (Status, Value) {
    let req = match method {
        "GET" => c.get(uri.to_string()),
        "POST" => c.post(uri.to_string()),
        "PUT" => c.put(uri.to_string()),
        "DELETE" => c.delete(uri.to_string()),
        _ => unreachable!(),
    }
    .header(Accept::JSON);
    let req = if token.is_empty() {
        req
    } else {
        req.private_cookie(Cookie::new(
            nrzmail::auth::session::COOKIE,
            token.to_string(),
        ))
    };
    let req = match body {
        Some(b) => req.header(ContentType::JSON).body(b.to_string()),
        None => req,
    };
    let res = req.dispatch().await;
    let status = res.status();
    (
        status,
        res.into_json::<Value>().await.unwrap_or(Value::Null),
    )
}

pub fn now() -> i64 {
    nrzmail::db::models::now()
}

/// Postfach mit Posteingang und Gesendet: (Postfach, Posteingang, Gesendet).
pub async fn account(
    pool: &SqlitePool,
    user_id: &str,
    email: &str,
    signature: &str,
) -> (String, String, String) {
    let id = nrzmail::db::models::new_id();
    sqlx::query(
        "INSERT INTO mail_accounts (id, user_id, kind, name, email, display_name, signature, created_at, updated_at)
         VALUES (?, ?, 'import', 'Privat', ?, 'Nico Muster', ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(email)
    .bind(signature)
    .bind(now())
    .bind(now())
    .execute(pool)
    .await
    .unwrap();
    let mut folders = Vec::new();
    for (path, role) in [("INBOX", "inbox"), ("Sent", "sent")] {
        let fid = nrzmail::db::models::new_id();
        sqlx::query(
            "INSERT INTO folders (id, account_id, path, name, role) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&fid)
        .bind(&id)
        .bind(path)
        .bind(path)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
        folders.push(fid);
    }
    (id, folders.remove(0), folders.remove(0))
}

/// Nachricht zum Einfügen.
#[derive(Clone)]
pub struct Msg {
    pub thread: String,
    pub subject: String,
    pub from_name: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub html: String,
    pub date: i64,
    pub sent: bool,
    pub category: String,
    pub source: String,
    pub unsubscribe: String,
}

impl Msg {
    pub fn new(subject: &str, from_name: &str, from: &str, body: &str) -> Self {
        Msg {
            thread: nrzmail::db::models::new_id(),
            subject: subject.into(),
            from_name: from_name.into(),
            from: from.into(),
            to: "nico@example.com".into(),
            body: body.into(),
            html: String::new(),
            date: now() - 3600,
            sent: false,
            category: "primary".into(),
            source: "auto".into(),
            unsubscribe: String::new(),
        }
    }

    pub fn thread(mut self, t: &str) -> Self {
        self.thread = t.into();
        self
    }

    pub fn date(mut self, d: i64) -> Self {
        self.date = d;
        self
    }

    pub fn sent(mut self) -> Self {
        self.sent = true;
        self
    }

    pub fn source(mut self, s: &str) -> Self {
        self.source = s.into();
        self
    }

    pub fn html(mut self, h: &str) -> Self {
        self.html = h.into();
        self
    }

    pub fn unsubscribe(mut self) -> Self {
        self.unsubscribe = "https://example.com/abmelden".into();
        self
    }
}

/// Nachricht samt Volltext-Eintrag einfügen; gibt die ID zurück.
pub async fn message(pool: &SqlitePool, account: &str, folder: &str, m: Msg) -> String {
    let id = nrzmail::db::models::new_id();
    let to = json!([{ "name": "", "address": m.to }]).to_string();
    sqlx::query(
        "INSERT INTO messages (id, account_id, folder_id, thread_id, subject, from_name, from_address, to_addrs,
             date, received_at, snippet, text_body, html_body, is_sent, category, category_source, list_unsubscribe, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(account)
    .bind(folder)
    .bind(&m.thread)
    .bind(&m.subject)
    .bind(&m.from_name)
    .bind(&m.from)
    .bind(&to)
    .bind(m.date)
    .bind(m.date)
    .bind(m.body.chars().take(200).collect::<String>())
    .bind(&m.body)
    .bind(&m.html)
    .bind(m.sent)
    .bind(&m.category)
    .bind(&m.source)
    .bind(&m.unsubscribe)
    .bind(now())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO messages_fts (message_id, subject, sender, recipients, body, attachment_names) VALUES (?, ?, ?, ?, ?, '')")
        .bind(&id)
        .bind(&m.subject)
        .bind(format!("{} {}", m.from_name, m.from))
        .bind(&m.to)
        .bind(&m.body)
        .execute(pool)
        .await
        .unwrap();
    id
}

// ------------------------------------------------------------------ Gefälschter KI-Server

/// Eine aufgezeichnete Anfrage: Methode, Pfad, Kopfzeilen (klein geschrieben), Body.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub head: String,
    pub body: Value,
}

impl Seen {
    /// Systemprompt (OpenAI: erste Nachricht, Anthropic: `system`).
    pub fn system(&self) -> String {
        self.body["system"]
            .as_str()
            .or_else(|| {
                self.body["messages"][0]["content"]
                    .as_str()
                    .filter(|_| self.body["messages"][0]["role"] == "system")
            })
            .unwrap_or_default()
            .to_string()
    }

    /// Letzte Nachricht des Benutzers.
    pub fn prompt(&self) -> String {
        self.body["messages"]
            .as_array()
            .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default()
            .to_string()
    }
}

pub type Handler = Arc<dyn Fn(&Seen) -> (u16, Value) + Send + Sync>;

pub struct FakeAi {
    pub url: String,
    pub log: Arc<Mutex<Vec<Seen>>>,
}

impl FakeAi {
    pub fn last(&self) -> Seen {
        self.log
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("keine Anfrage")
    }

    pub fn count(&self) -> usize {
        self.log.lock().unwrap().len()
    }
}

/// Startet den gefälschten Server; `handler` bekommt jede Anfrage und gibt Status und JSON zurück.
pub async fn fake_ai(handler: Handler) -> FakeAi {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let log = log2.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 16384];
                let (head_end, len) = loop {
                    let n = match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map(|v| v.trim().parse::<usize>().unwrap())
                            .unwrap_or(0);
                        break (pos + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut first = head.lines().next().unwrap_or("").split(' ');
                let (method, path) = (
                    first.next().unwrap_or("").to_string(),
                    first.next().unwrap_or("").to_string(),
                );
                let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                let seen = Seen {
                    method,
                    path,
                    head: head.to_lowercase(),
                    body,
                };
                let (status, reply) = handler(&seen);
                log.lock().unwrap().push(seen);
                let reply = reply.to_string();
                let out = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = sock.write_all(out.as_bytes()).await;
            });
        }
    });
    FakeAi { url, log }
}

/// Antwort im OpenAI-Format.
pub fn openai_text(model: &str, text: &str) -> Value {
    json!({ "model": model, "choices": [{ "message": { "role": "assistant", "content": text } }] })
}

/// Antwort im Anthropic-Format.
pub fn anthropic_text(model: &str, text: &str) -> Value {
    json!({ "model": model, "content": [{ "type": "text", "text": text }] })
}

/// KI als Admin einrichten: Anbieter, Adresse, Modell, Schlüssel.
pub async fn configure(
    c: &Client,
    admin: &str,
    provider: &str,
    url: &str,
    model: &str,
    key: Option<&str>,
) -> Value {
    let (s, v) = call(
        c,
        admin,
        "PUT",
        "/api/v1/ai/settings",
        Some(json!({ "enabled": true, "provider": provider, "baseUrl": url, "model": model, "apiKey": key, "autoCategorize": false, "assistantName": "" })),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    v
}
