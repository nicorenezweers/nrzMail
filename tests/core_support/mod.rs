//! Helfer der Kern-Tests: Server mit leerem Datenordner (ohne Hintergrundarbeiten), JSON-Aufrufe
//! wie die Oberfläche, Einrichtung und Anmeldung.
#![allow(dead_code)]

use rocket::figment::Figment;
use rocket::http::{Accept, ContentType, Cookie, Header, Status};
use rocket::local::asynchronous::{Client, LocalRequest};
use serde_json::{json, Value};
use sqlx::SqlitePool;

pub const DB_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
pub const ADMIN: &str = "alice";
pub const ADMIN_PASSWORD: &str = "geheim-123";

pub fn figment(dir: &tempfile::TempDir) -> Figment {
    nrzmail::config::figment()
        .merge(("data_dir", dir.path()))
        .merge(("db_key", DB_KEY))
        .merge(("log_level", "off"))
        .merge(("background", false))
}

/// Server ohne Benutzer; `adjust` ändert die Konfiguration.
pub async fn server_with(adjust: impl FnOnce(Figment) -> Figment) -> (tempfile::TempDir, Client) {
    let dir = tempfile::tempdir().unwrap();
    let rocket = nrzmail::build_with(adjust(figment(&dir))).await.unwrap();
    (dir, Client::tracked(rocket).await.unwrap())
}

pub async fn server() -> (tempfile::TempDir, Client) {
    server_with(|f| f).await
}

/// Server mit eingerichtetem und angemeldetem Admin „alice“.
pub async fn with_admin() -> (tempfile::TempDir, Client) {
    let (dir, c) = server().await;
    setup(&c, ADMIN, ADMIN_PASSWORD).await;
    (dir, c)
}

pub fn pool(c: &Client) -> &SqlitePool {
    c.rocket().state::<SqlitePool>().unwrap()
}

pub fn request<'c>(c: &'c Client, method: &str, uri: &str) -> LocalRequest<'c> {
    let uri = uri.to_string();
    match method {
        "GET" => c.get(uri),
        "POST" => c.post(uri),
        "PUT" => c.put(uri),
        "DELETE" => c.delete(uri),
        _ => unreachable!("{method}"),
    }
    .header(Accept::JSON)
}

async fn send(req: LocalRequest<'_>, body: Option<Value>) -> (Status, Value) {
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

/// JSON-Aufruf mit der Sitzung des Clients.
pub async fn call(c: &Client, method: &str, uri: &str, body: Option<Value>) -> (Status, Value) {
    send(request(c, method, uri), body).await
}

/// JSON-Aufruf mit einer bestimmten (gemerkten) Sitzung statt der des Clients.
pub async fn call_as(
    c: &Client,
    session: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (Status, Value) {
    let req = request(c, method, uri).cookie(Cookie::new("nrzmail_session", session.to_string()));
    send(req, body).await
}

/// JSON-Aufruf von einer bestimmten IP-Adresse (über `X-Real-IP`, wie hinter einem Proxy).
pub async fn call_from(
    c: &Client,
    ip: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (Status, Value) {
    let req = request(c, method, uri).header(Header::new("X-Real-IP", ip.to_string()));
    send(req, body).await
}

/// Das (verschlüsselte) Sitzungs-Cookie, das der Client gerade hat.
pub fn session_cookie(c: &Client) -> String {
    c.cookies()
        .get("nrzmail_session")
        .expect("Sitzungs-Cookie")
        .value()
        .to_string()
}

pub async fn setup(c: &Client, user: &str, password: &str) -> Value {
    let (s, v) = call(
        c,
        "POST",
        "/api/v1/auth/setup",
        Some(json!({ "username": user, "password": password, "displayName": "Alice", "email": "alice@example.org" })),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    v
}

pub async fn login(c: &Client, user: &str, password: &str) -> (Status, Value) {
    call(
        c,
        "POST",
        "/api/v1/auth/login",
        Some(json!({ "username": user, "password": password })),
    )
    .await
}

pub async fn logout(c: &Client) {
    let (s, _) = call(c, "POST", "/api/v1/auth/logout", None).await;
    assert_eq!(s, Status::NoContent);
}

/// Benutzer über die Verwaltung anlegen (der Client muss als Admin angemeldet sein); gibt die ID.
pub async fn create_user(c: &Client, user: &str, password: &str, admin: bool) -> String {
    let (s, v) = call(
        c,
        "POST",
        "/api/v1/admin/users",
        Some(json!({ "username": user, "password": password, "isAdmin": admin })),
    )
    .await;
    assert_eq!(s, Status::Created, "{v}");
    v["id"].as_str().unwrap().to_string()
}

/// Aktionen im Zugriffsprotokoll (neueste zuerst) – direkt aus der Datenbank.
pub async fn actions(c: &Client) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM activity_log ORDER BY id DESC")
        .fetch_all(pool(c))
        .await
        .unwrap()
}
