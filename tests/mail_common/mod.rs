//! Helfer für die Mail-Tests: Server mit leerem Datenordner, Benutzer mit Sitzung (direkt in
//! der Datenbank, ohne Anmelde-API), JSON-Aufrufe sowie ein Schein-IMAP- und ein
//! Schein-SMTP-Server (unverschlüsselt, auf 127.0.0.1).
#![allow(dead_code)]

use rocket::http::{Accept, ContentType, Cookie, Status};
use rocket::local::asynchronous::{Client, LocalResponse};
use serde_json::Value;
use sqlx::SqlitePool;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub async fn client_with(
    adjust: impl FnOnce(rocket::figment::Figment) -> rocket::figment::Figment,
) -> (tempfile::TempDir, Client) {
    let dir = tempfile::tempdir().unwrap();
    let figment = nrzmail::config::figment()
        .merge(("data_dir", dir.path()))
        .merge(("db_key", KEY))
        .merge(("log_level", "off"))
        .merge(("background", false));
    let rocket = nrzmail::build_with(adjust(figment)).await.unwrap();
    (dir, Client::tracked(rocket).await.unwrap())
}

pub async fn client() -> (tempfile::TempDir, Client) {
    client_with(|f| f).await
}

pub fn pool(c: &Client) -> &SqlitePool {
    c.rocket().state::<SqlitePool>().unwrap()
}

pub fn config(c: &Client) -> &nrzmail::config::Config {
    c.rocket().state::<nrzmail::config::Config>().unwrap()
}

/// Angemeldeter Benutzer: ID und Sitzungs-Token.
#[derive(Clone)]
pub struct User {
    pub id: String,
    pub token: String,
}

/// Benutzer direkt anlegen (ohne teures Passwort-Hashing) und eine Sitzung öffnen.
pub async fn user(c: &Client, name: &str) -> User {
    let id = uuid::Uuid::now_v7().to_string();
    let ts = time::OffsetDateTime::now_utc().unix_timestamp();
    sqlx::query("INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES (?, ?, 'x', ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(ts)
        .bind(ts)
        .execute(pool(c))
        .await
        .unwrap();
    let token = nrzmail::auth::session::create(pool(c), &id, "test")
        .await
        .unwrap();
    User { id, token }
}

fn request<'c>(
    c: &'c Client,
    u: Option<&User>,
    method: &str,
    uri: &str,
) -> rocket::local::asynchronous::LocalRequest<'c> {
    let req = match method {
        "GET" => c.get(uri.to_string()),
        "POST" => c.post(uri.to_string()),
        "PUT" => c.put(uri.to_string()),
        "DELETE" => c.delete(uri.to_string()),
        _ => unreachable!(),
    }
    .header(Accept::JSON);
    match u {
        Some(u) => req.private_cookie(Cookie::new(nrzmail::auth::session::COOKIE, u.token.clone())),
        None => req,
    }
}

/// JSON-Aufruf als Benutzer.
pub async fn call(
    c: &Client,
    u: &User,
    method: &str,
    uri: impl AsRef<str>,
    body: Option<Value>,
) -> (Status, Value) {
    let req = request(c, Some(u), method, &format!("/api/v1{}", uri.as_ref()));
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

/// Ohne Anmeldung.
pub async fn call_anon(c: &Client, method: &str, uri: &str) -> Status {
    request(c, None, method, &format!("/api/v1{uri}"))
        .dispatch()
        .await
        .status()
}

/// Rohantwort (Dateien).
pub async fn get_raw<'c>(c: &'c Client, u: &User, uri: &str) -> LocalResponse<'c> {
    request(c, Some(u), "GET", &format!("/api/v1{uri}"))
        .dispatch()
        .await
}

/// Rohdaten hochladen (Import).
pub async fn post_bytes(c: &Client, u: &User, uri: &str, body: Vec<u8>) -> (Status, Value) {
    let res = request(c, Some(u), "POST", &format!("/api/v1{uri}"))
        .header(ContentType::Binary)
        .body(body)
        .dispatch()
        .await;
    let status = res.status();
    (
        status,
        res.into_json::<Value>().await.unwrap_or(Value::Null),
    )
}

// ------------------------------------------------------------------ Schein-IMAP

#[derive(Debug, Clone)]
pub struct FakeMessage {
    pub uid: u32,
    pub flags: Vec<String>,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct FakeFolder {
    pub name: String,
    pub attrs: String,
    pub validity: u32,
    pub next_uid: u32,
    pub messages: Vec<FakeMessage>,
}

impl FakeFolder {
    pub fn new(name: &str, attrs: &str) -> Self {
        Self {
            name: name.into(),
            attrs: attrs.into(),
            validity: 1,
            next_uid: 1,
            messages: Vec::new(),
        }
    }

    pub fn add(&mut self, raw: &[u8], flags: &[&str]) -> u32 {
        let uid = self.next_uid;
        self.next_uid += 1;
        self.messages.push(FakeMessage {
            uid,
            flags: flags.iter().map(|f| f.to_string()).collect(),
            raw: raw.to_vec(),
        });
        uid
    }
}

#[derive(Debug, Default)]
pub struct ImapState {
    pub password: String,
    pub folders: Vec<FakeFolder>,
    /// Alle Befehle (ohne Tag)
    pub log: Vec<String>,
}

impl ImapState {
    pub fn folder(&mut self, name: &str) -> &mut FakeFolder {
        self.folders.iter_mut().find(|f| f.name == name).unwrap()
    }
}

pub type Imap = Arc<Mutex<ImapState>>;

/// Argumente eines IMAP-Befehls: Atome, "Strings" (mit \-Maskierung), (Listen) als Rohtext.
fn args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            ' ' => i += 1,
            '"' => {
                let mut v = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    v.push(chars[i]);
                    i += 1;
                }
                i += 1;
                out.push(v);
            }
            '(' => {
                let mut depth = 0;
                let mut v = String::new();
                while i < chars.len() {
                    v.push(chars[i]);
                    if chars[i] == '(' {
                        depth += 1;
                    } else if chars[i] == ')' {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
                out.push(v);
            }
            _ => {
                let mut v = String::new();
                while i < chars.len() && chars[i] != ' ' {
                    v.push(chars[i]);
                    i += 1;
                }
                out.push(v);
            }
        }
    }
    out
}

/// `1:3,5,7:*` gegen die grösste UID.
fn in_set(set: &str, uid: u32, max: u32) -> bool {
    set.split(',').any(|part| {
        let num = |s: &str| {
            if s == "*" {
                max
            } else {
                s.parse().unwrap_or(0)
            }
        };
        match part.split_once(':') {
            Some((a, b)) => {
                let (a, b) = (num(a), num(b));
                (a.min(b)..=a.max(b)).contains(&uid)
            }
            None => num(part) == uid,
        }
    })
}

fn message_id_header(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let line = text
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .find(|l| l.to_ascii_lowercase().starts_with("message-id:"))
        .unwrap_or("");
    if line.is_empty() {
        b"\r\n".to_vec()
    } else {
        format!("{}\r\n\r\n", line.trim_end()).into_bytes()
    }
}

/// Startet den Schein-IMAP-Server; gibt den Port zurück.
pub async fn fake_imap(state: Imap) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                let _ = w
                    .write_all(b"* OK [CAPABILITY IMAP4rev1 MOVE UIDPLUS SPECIAL-USE] Schein-IMAP bereit\r\n")
                    .await;
                let mut selected: Option<String> = None;
                loop {
                    let mut line = String::new();
                    match r.read_line(&mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let line = line.trim_end().to_string();
                    let (tag, cmd) = line.split_once(' ').unwrap_or((&line, ""));
                    let tag = tag.to_string();
                    let a = args(cmd);
                    let upper: Vec<String> = a.iter().map(|s| s.to_uppercase()).collect();
                    state.lock().unwrap().log.push(cmd.to_string());
                    let mut out: Vec<u8> = Vec::new();
                    let word = upper.first().map(String::as_str).unwrap_or("");
                    match word {
                        "CAPABILITY" => out.extend_from_slice(
                            format!("* CAPABILITY IMAP4rev1 MOVE UIDPLUS SPECIAL-USE\r\n{tag} OK CAPABILITY completed\r\n").as_bytes(),
                        ),
                        "LOGIN" => {
                            let ok = a.get(2).map(String::as_str) == Some(state.lock().unwrap().password.as_str());
                            out.extend_from_slice(if ok {
                                format!("{tag} OK LOGIN completed\r\n")
                            } else {
                                format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n")
                            }.as_bytes());
                        }
                        "LIST" => {
                            let s = state.lock().unwrap();
                            for f in &s.folders {
                                out.extend_from_slice(format!("* LIST ({}) \"/\" \"{}\"\r\n", f.attrs, f.name).as_bytes());
                            }
                            out.extend_from_slice(format!("{tag} OK LIST completed\r\n").as_bytes());
                        }
                        "SELECT" | "EXAMINE" => {
                            let name = a.get(1).cloned().unwrap_or_default();
                            let s = state.lock().unwrap();
                            match s.folders.iter().find(|f| f.name == name) {
                                Some(f) => {
                                    out.extend_from_slice(format!(
                                        "* {} EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY {}] UIDs valid\r\n* OK [UIDNEXT {}] next\r\n* FLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft)\r\n{tag} OK [READ-WRITE] SELECT completed\r\n",
                                        f.messages.len(), f.validity, f.next_uid
                                    ).as_bytes());
                                    selected = Some(name);
                                }
                                None => out.extend_from_slice(format!("{tag} NO Mailbox does not exist\r\n").as_bytes()),
                            }
                        }
                        "CREATE" => {
                            let name = a.get(1).cloned().unwrap_or_default();
                            let mut s = state.lock().unwrap();
                            if s.folders.iter().any(|f| f.name == name) {
                                out.extend_from_slice(format!("{tag} NO [ALREADYEXISTS] Mailbox exists\r\n").as_bytes());
                            } else {
                                s.folders.push(FakeFolder::new(&name, "\\HasNoChildren"));
                                out.extend_from_slice(format!("{tag} OK CREATE completed\r\n").as_bytes());
                            }
                        }
                        "APPEND" => {
                            let name = a.get(1).cloned().unwrap_or_default();
                            let flags: Vec<String> = a
                                .iter()
                                .find(|x| x.starts_with('('))
                                .map(|x| x.trim_matches(['(', ')']).split_whitespace().map(String::from).collect())
                                .unwrap_or_default();
                            let len: usize = a
                                .last()
                                .and_then(|x| x.strip_prefix('{'))
                                .and_then(|x| x.strip_suffix('}'))
                                .and_then(|x| x.parse().ok())
                                .unwrap_or(0);
                            if w.write_all(b"+ Ready for literal data\r\n").await.is_err() {
                                return;
                            }
                            let mut data = vec![0u8; len];
                            if r.read_exact(&mut data).await.is_err() {
                                return;
                            }
                            let mut crlf = String::new();
                            let _ = r.read_line(&mut crlf).await;
                            let mut s = state.lock().unwrap();
                            match s.folders.iter_mut().find(|f| f.name == name) {
                                Some(f) => {
                                    let flags: Vec<&str> = flags.iter().map(String::as_str).collect();
                                    let uid = f.add(&data, &flags);
                                    out.extend_from_slice(format!("{tag} OK [APPENDUID {} {uid}] APPEND completed\r\n", f.validity).as_bytes());
                                }
                                None => out.extend_from_slice(format!("{tag} NO [TRYCREATE] no such mailbox\r\n").as_bytes()),
                            }
                        }
                        "UID" => {
                            let sub = upper.get(1).map(String::as_str).unwrap_or("");
                            let set = a.get(2).cloned().unwrap_or_default();
                            let mut s = state.lock().unwrap();
                            let Some(sel) = selected.clone() else {
                                out.extend_from_slice(format!("{tag} BAD No mailbox selected\r\n").as_bytes());
                                drop(s);
                                let _ = w.write_all(&out).await;
                                continue;
                            };
                            match sub {
                                "FETCH" => {
                                    let items = upper[3..].join(" ");
                                    let f = s.folder(&sel);
                                    let max = f.messages.iter().map(|m| m.uid).max().unwrap_or(0);
                                    for (i, m) in f.messages.iter().enumerate() {
                                        if !in_set(&set, m.uid, max) {
                                            continue;
                                        }
                                        out.extend_from_slice(format!("* {} FETCH (UID {}", i + 1, m.uid).as_bytes());
                                        if items.contains("FLAGS") {
                                            out.extend_from_slice(format!(" FLAGS ({})", m.flags.join(" ")).as_bytes());
                                        }
                                        if items.contains("INTERNALDATE") {
                                            out.extend_from_slice(b" INTERNALDATE \"01-Jan-2025 10:00:00 +0000\"");
                                        }
                                        if items.contains("RFC822.SIZE") {
                                            out.extend_from_slice(format!(" RFC822.SIZE {}", m.raw.len()).as_bytes());
                                        }
                                        if items.contains("HEADER.FIELDS") {
                                            let h = message_id_header(&m.raw);
                                            out.extend_from_slice(format!(" BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}}}\r\n", h.len()).as_bytes());
                                            out.extend_from_slice(&h);
                                        }
                                        if items.contains("BODY.PEEK[]") {
                                            out.extend_from_slice(format!(" BODY[] {{{}}}\r\n", m.raw.len()).as_bytes());
                                            out.extend_from_slice(&m.raw);
                                        }
                                        out.extend_from_slice(b")\r\n");
                                    }
                                    out.extend_from_slice(format!("{tag} OK FETCH completed\r\n").as_bytes());
                                }
                                "STORE" => {
                                    let op = upper.get(3).cloned().unwrap_or_default();
                                    let flags: Vec<String> = a.get(4).map(|x| x.trim_matches(['(', ')']).split_whitespace().map(String::from).collect()).unwrap_or_default();
                                    let silent = op.contains("SILENT");
                                    let f = s.folder(&sel);
                                    let max = f.messages.iter().map(|m| m.uid).max().unwrap_or(0);
                                    for (i, m) in f.messages.iter_mut().enumerate() {
                                        if !in_set(&set, m.uid, max) {
                                            continue;
                                        }
                                        for fl in &flags {
                                            if op.starts_with('+') {
                                                if !m.flags.contains(fl) {
                                                    m.flags.push(fl.clone());
                                                }
                                            } else if op.starts_with('-') {
                                                m.flags.retain(|x| x != fl);
                                            }
                                        }
                                        if !silent {
                                            out.extend_from_slice(format!("* {} FETCH (UID {} FLAGS ({}))\r\n", i + 1, m.uid, m.flags.join(" ")).as_bytes());
                                        }
                                    }
                                    out.extend_from_slice(format!("{tag} OK STORE completed\r\n").as_bytes());
                                }
                                "MOVE" | "COPY" => {
                                    let target = a.get(3).cloned().unwrap_or_default();
                                    let source = s.folder(&sel).clone();
                                    let max = source.messages.iter().map(|m| m.uid).max().unwrap_or(0);
                                    let picked: Vec<FakeMessage> = source.messages.iter().filter(|m| in_set(&set, m.uid, max)).cloned().collect();
                                    if !s.folders.iter().any(|f| f.name == target) {
                                        out.extend_from_slice(format!("{tag} NO [TRYCREATE] no such mailbox\r\n").as_bytes());
                                    } else {
                                        for m in &picked {
                                            let flags: Vec<&str> = m.flags.iter().map(String::as_str).filter(|f| *f != "\\Deleted").collect();
                                            s.folder(&target).add(&m.raw, &flags);
                                        }
                                        if sub == "MOVE" {
                                            s.folder(&sel).messages.retain(|m| !in_set(&set, m.uid, max));
                                        }
                                        out.extend_from_slice(format!("{tag} OK {sub} completed\r\n").as_bytes());
                                    }
                                }
                                "EXPUNGE" => {
                                    let f = s.folder(&sel);
                                    let max = f.messages.iter().map(|m| m.uid).max().unwrap_or(0);
                                    f.messages.retain(|m| !(in_set(&set, m.uid, max) && m.flags.iter().any(|x| x == "\\Deleted")));
                                    out.extend_from_slice(format!("{tag} OK EXPUNGE completed\r\n").as_bytes());
                                }
                                _ => out.extend_from_slice(format!("{tag} BAD unbekannt\r\n").as_bytes()),
                            }
                        }
                        "EXPUNGE" => {
                            if let Some(sel) = &selected {
                                state.lock().unwrap().folder(sel).messages.retain(|m| !m.flags.iter().any(|x| x == "\\Deleted"));
                            }
                            out.extend_from_slice(format!("{tag} OK EXPUNGE completed\r\n").as_bytes());
                        }
                        "NOOP" => out.extend_from_slice(format!("{tag} OK NOOP completed\r\n").as_bytes()),
                        "LOGOUT" => {
                            let _ = w.write_all(format!("* BYE\r\n{tag} OK LOGOUT completed\r\n").as_bytes()).await;
                            return;
                        }
                        _ => out.extend_from_slice(format!("{tag} BAD unbekannt\r\n").as_bytes()),
                    }
                    if w.write_all(&out).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// Einfache Testnachricht.
pub fn mail(from: &str, subject: &str, message_id: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: nico@example.ch\r\nSubject: {subject}\r\nDate: Mon, 6 Oct 2025 10:00:00 +0200\r\nMessage-ID: <{message_id}>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

// ------------------------------------------------------------------ Schein-SMTP

#[derive(Debug, Default, Clone)]
pub struct SmtpMail {
    pub from: String,
    pub rcpt: Vec<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SmtpState {
    /// Erwartetes `AUTH PLAIN`-Passwort
    pub password: String,
    pub mails: Vec<SmtpMail>,
}

pub type Smtp = Arc<Mutex<SmtpState>>;

/// Startet den Schein-SMTP-Server (ohne TLS, AUTH PLAIN); gibt den Port zurück.
pub async fn fake_smtp(state: Smtp) -> u16 {
    use base64::Engine;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                let _ = w.write_all(b"220 schein.smtp ESMTP bereit\r\n").await;
                let mut current = SmtpMail::default();
                loop {
                    let mut line = String::new();
                    match r.read_line(&mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let cmd = line.trim_end().to_string();
                    let upper = cmd.to_uppercase();
                    let reply = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        "250-schein.smtp\r\n250-AUTH PLAIN LOGIN\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n".to_string()
                    } else if upper.starts_with("AUTH PLAIN") {
                        let b64 = cmd.split_whitespace().nth(2).unwrap_or("");
                        let decoded = base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .unwrap_or_default();
                        let parts: Vec<&[u8]> = decoded.split(|b| *b == 0).collect();
                        let password = parts
                            .get(2)
                            .map(|p| String::from_utf8_lossy(p).to_string())
                            .unwrap_or_default();
                        if password == state.lock().unwrap().password {
                            "235 2.7.0 Authentication successful\r\n".into()
                        } else {
                            "535 5.7.8 Authentication credentials invalid\r\n".into()
                        }
                    } else if upper.starts_with("MAIL FROM:") {
                        current = SmtpMail::default();
                        current.from = cmd[10..].trim().to_string();
                        "250 OK\r\n".into()
                    } else if upper.starts_with("RCPT TO:") {
                        current.rcpt.push(cmd[8..].trim().to_string());
                        "250 OK\r\n".into()
                    } else if upper == "DATA" {
                        if w.write_all(b"354 Start mail input\r\n").await.is_err() {
                            return;
                        }
                        let mut data = String::new();
                        loop {
                            let mut l = String::new();
                            if r.read_line(&mut l).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if l == ".\r\n" {
                                break;
                            }
                            data.push_str(
                                l.strip_prefix('.')
                                    .filter(|_| l.starts_with(".."))
                                    .unwrap_or(&l),
                            );
                        }
                        current.data = data;
                        state
                            .lock()
                            .unwrap()
                            .mails
                            .push(std::mem::take(&mut current));
                        "250 OK queued\r\n".into()
                    } else if upper == "QUIT" {
                        let _ = w.write_all(b"221 Bye\r\n").await;
                        return;
                    } else if upper == "RSET" || upper == "NOOP" {
                        "250 OK\r\n".into()
                    } else {
                        "502 Command not implemented\r\n".into()
                    };
                    if w.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

/// Ordnernamen → Rollen aus `GET /folders`.
pub fn roles(folders: &Value) -> BTreeMap<String, String> {
    folders
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap().to_string(),
                f["role"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}
