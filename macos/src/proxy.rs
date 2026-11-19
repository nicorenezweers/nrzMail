//! Server-Modus ohne Webseite des Servers: Die App zeigt immer ihre **eigene** Oberfläche (in das
//! Programm eingebettet, mit Milchglas und Titelleiste – unabhängig davon, welche Version auf dem
//! Server läuft). Vom Server holt sie nur Daten: Dieser kleine Server auf `127.0.0.1` liefert die
//! eingebettete Oberfläche und reicht alles unter `/api/` an die verbundene nrzMail-Instanz weiter.
//!
//! Die Sitzung beim Server (Cookie `nrzmail_session`) bleibt in der App: Der Proxy nimmt
//! `Set-Cookie` des Servers an sich, schickt die Cookies bei jeder Anfrage mit und speichert sie
//! (`session.json`, nur für den Benutzer lesbar) – nach einem Neustart ist man weiter angemeldet.

use rocket::data::{Data, ToByteUnit};
use rocket::http::{Header, Method, Status};
use rocket::route::{Handler, Outcome, Route};
use rocket::{Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Gespeicherte Sitzung: Cookies je Server (Ursprung).
#[derive(Default, Serialize, Deserialize)]
struct Sessions {
    #[serde(default)]
    servers: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Default)]
struct State {
    /// Adresse der Instanz (ohne `/` am Ende), `None` = nicht verbunden
    base: Option<String>,
    sessions: Sessions,
}

/// Verbindung zur Instanz – geteilt zwischen App und Proxy.
#[derive(Clone)]
pub struct Remote {
    state: Arc<Mutex<State>>,
    client: reqwest::Client,
}

fn sessions_path() -> std::path::PathBuf {
    crate::server::data_dir().join("session.json")
}

impl Remote {
    pub fn new() -> Self {
        let sessions = std::fs::read(sessions_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let client = reqwest::Client::builder()
            .user_agent(concat!("nrzMail-macOS/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(8))
            // KI-Antworten und grosse Importe dürfen dauern
            .timeout(Duration::from_secs(15 * 60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP-Client");
        Remote { state: Arc::new(Mutex::new(State { base: None, sessions })), client }
    }

    /// Mit dieser Instanz arbeiten (die gespeicherte Sitzung dazu gilt weiter).
    pub fn set_base(&self, base: Option<String>) {
        self.state.lock().unwrap().base = base;
    }

    fn save(state: &State) {
        let path = sessions_path();
        if let Ok(json) = serde_json::to_vec_pretty(&state.sessions) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
                }
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    /// `Set-Cookie` des Servers übernehmen (Löschen = leerer Wert bzw. `Max-Age=0`).
    fn take_cookies(&self, origin: &str, headers: &reqwest::header::HeaderMap) {
        let mut changed = false;
        let mut st = self.state.lock().unwrap();
        let jar = st.sessions.servers.entry(origin.to_string()).or_default();
        for v in headers.get_all(reqwest::header::SET_COOKIE) {
            let Ok(v) = v.to_str() else { continue };
            let Some((name, value, remove)) = parse_set_cookie(v) else { continue };
            if remove {
                changed |= jar.remove(&name).is_some();
            } else if jar.get(&name) != Some(&value) {
                jar.insert(name, value);
                changed = true;
            }
        }
        if changed {
            Self::save(&st);
        }
    }

    fn cookie_header(&self, origin: &str) -> Option<String> {
        let st = self.state.lock().unwrap();
        let jar = st.sessions.servers.get(origin)?;
        (!jar.is_empty()).then(|| jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "))
    }
}

/// `name=value; Path=/; Max-Age=0 …` → (Name, Wert, löschen?)
fn parse_set_cookie(v: &str) -> Option<(String, String, bool)> {
    let mut parts = v.split(';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let value = value.trim().trim_matches('"').to_string();
    let expired = parts.any(|a| {
        let a = a.trim().to_ascii_lowercase();
        a.strip_prefix("max-age=").is_some_and(|n| n.trim().parse::<i64>().map(|n| n <= 0).unwrap_or(false)) || a.contains("1970")
    });
    let remove = expired || value.is_empty();
    Some((name, value, remove))
}

/// Header, die nicht weitergereicht werden (Verbindung, Länge, Cookies, Herkunft).
const HOP: &[&str] = &[
    "host", "connection", "keep-alive", "transfer-encoding", "te", "trailer", "upgrade", "proxy-authorization",
    "proxy-connection", "content-length", "cookie", "origin", "referer", "accept-encoding",
];

#[derive(Clone)]
struct Forward(Remote);

#[rocket::async_trait]
impl Handler for Forward {
    async fn handle<'r>(&self, req: &'r Request<'_>, data: Data<'r>) -> Outcome<'r> {
        let base = self.0.state.lock().unwrap().base.clone();
        let Some(base) = base else {
            return Outcome::Success(error(Status::ServiceUnavailable, "Die App ist mit keinem nrzMail-Server verbunden."));
        };
        let origin = crate::settings::origin(&base).unwrap_or_else(|| base.clone());
        let url = format!("{base}{}", req.uri());
        let method = match reqwest::Method::from_bytes(req.method().as_str().as_bytes()) {
            Ok(m) => m,
            Err(_) => return Outcome::Success(error(Status::MethodNotAllowed, "Methode nicht unterstützt.")),
        };
        let body = match data.open(4.gibibytes()).into_bytes().await {
            Ok(b) if b.is_complete() => b.into_inner(),
            Ok(_) => return Outcome::Success(error(Status::PayloadTooLarge, "Die Datei ist zu gross.")),
            Err(e) => return Outcome::Success(error(Status::BadRequest, &format!("Anfrage unvollständig: {e}"))),
        };
        let mut rb = self.0.client.request(method, &url);
        for h in req.headers().iter() {
            if !HOP.contains(&h.name().as_str().to_ascii_lowercase().as_str()) {
                rb = rb.header(h.name().as_str(), h.value());
            }
        }
        if let Some(c) = self.0.cookie_header(&origin) {
            rb = rb.header(reqwest::header::COOKIE, c);
        }
        if !body.is_empty() {
            rb = rb.body(body);
        }
        let res = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = if e.is_timeout() { "Der Server antwortet nicht rechtzeitig.".to_string() } else { format!("Server nicht erreichbar: {e}") };
                return Outcome::Success(error(Status::BadGateway, &msg));
            }
        };
        self.0.take_cookies(&origin, res.headers());
        let status = Status::new(res.status().as_u16());
        let headers: Vec<(String, String)> = res
            .headers()
            .iter()
            .filter(|(k, _)| {
                let k = k.as_str();
                !HOP.contains(&k) && k != "set-cookie" && k != "content-encoding"
            })
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let bytes = match res.bytes().await {
            Ok(b) => b,
            Err(e) => return Outcome::Success(error(Status::BadGateway, &format!("Antwort abgebrochen: {e}"))),
        };
        let mut out = Response::build();
        out.status(status);
        for (k, v) in headers {
            out.header_adjoin(Header::new(k, v));
        }
        out.sized_body(bytes.len(), Cursor::new(bytes.to_vec()));
        Outcome::Success(out.finalize())
    }
}

/// Fehler im Format der API (`{"error":{"code","message"}}`) – die Oberfläche zeigt ihn an.
fn error(status: Status, message: &str) -> Response<'static> {
    let body = serde_json::json!({ "error": { "code": "proxy", "message": message } }).to_string();
    Response::build()
        .status(status)
        .header(rocket::http::ContentType::JSON)
        .sized_body(body.len(), Cursor::new(body))
        .finalize()
}

/// Startet den Proxy auf einem eigenen Thread und wartet, bis er antwortet; Ergebnis: Port.
pub fn start(remote: Remote) -> Result<u16, String> {
    let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())?;
    let mut shutdown = rocket::config::Shutdown { ctrlc: false, grace: 1, mercy: 1, ..Default::default() };
    shutdown.signals.clear();
    let mut key = [0u8; 64];
    getrandom::getrandom(&mut key).map_err(|e| e.to_string())?;
    let config = rocket::Config {
        address: std::net::Ipv4Addr::LOCALHOST.into(),
        port,
        ident: rocket::config::Ident::try_new("nrzMail").map_err(|e| e.to_string())?,
        log_level: rocket::config::LogLevel::Off,
        secret_key: rocket::config::SecretKey::from(&key[..]),
        limits: rocket::data::Limits::default()
            .limit("bytes", 4.gibibytes())
            .limit("data-form", 4.gibibytes())
            .limit("file", 4.gibibytes())
            .limit("json", 64.mebibytes())
            .limit("form", 64.kibibytes()),
        shutdown,
        ..rocket::Config::release_default()
    };
    let methods = [Method::Get, Method::Post, Method::Put, Method::Patch, Method::Delete, Method::Head, Method::Options];
    let forward: Vec<Route> = methods.iter().map(|m| Route::ranked(-10, *m, "/api/<path..>", Forward(remote.clone()))).collect();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("nrzmail-proxy".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("Tokio");
            rt.block_on(async move {
                let r = rocket::custom(config)
                    .attach(nrzmail::fairings::SecurityHeaders)
                    .mount("/", nrzmail::routes::assets::routes())
                    .mount("/", nrzmail::routes::spa::routes())
                    .mount("/", forward)
                    .launch()
                    .await;
                if let Err(e) = r {
                    tracing::error!("Proxy beendet: {e}");
                    let _ = tx.send(e.to_string());
                }
            });
        })
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    while TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200)).is_err() {
        if let Ok(e) = rx.try_recv() {
            return Err(e);
        }
        if start.elapsed() > Duration::from_secs(20) {
            return Err("Proxy startet nicht".into());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::parse_set_cookie;

    #[test]
    fn cookies() {
        assert_eq!(parse_set_cookie("nrzmail_session=abc%2B; Path=/; HttpOnly; Max-Age=2592000"), Some(("nrzmail_session".into(), "abc%2B".into(), false)));
        assert_eq!(parse_set_cookie("nrzmail_session=; Path=/; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT").unwrap().2, true);
        assert_eq!(parse_set_cookie("x=1; Max-Age=0").unwrap().2, true);
        assert_eq!(parse_set_cookie("garbage"), None);
    }
}
