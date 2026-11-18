//! Zugriff auf die JSON-API unter `/api/v1`. Anmeldung läuft über das Sitzungs-Cookie
//! (gleiche Herkunft); Fehler kommen als [`nrzmail_api::ApiError`]
//! (`{"error":{"code":…,"message":…}}`). Ein 401 schickt zur Anmeldung.

use futures_util::StreamExt;
use gloo_net::http::{Request, RequestBuilder, Response};
use serde::Serialize;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

pub const BASE: &str = "/api/v1";

/// Fehler einer Anfrage, so wie ihn die Oberfläche zeigt.
#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    /// HTTP-Status (0 = keine Verbindung)
    pub status: u16,
    /// Maschinenlesbarer Code des Servers, z. B. `conflict`
    pub code: String,
    /// Meldung für Menschen (deutsch)
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type ApiResult<T> = Result<T, Error>;

fn network(e: impl std::fmt::Display) -> Error {
    Error {
        status: 0,
        code: "network".into(),
        message: format!("Keine Verbindung zum Server ({e})."),
    }
}

/// Ganze Adresse eines API-Pfads, z. B. `/messages` → `/api/v1/messages`.
pub fn url(path: &str) -> String {
    if path.starts_with(BASE) || path.starts_with("/static") {
        path.to_string()
    } else {
        format!("{BASE}{path}")
    }
}

/// Antwort auswerten: 2xx → Inhalt (leer = `null`), sonst die Meldung aus `ApiError`.
fn parse<T: DeserializeOwned>(status: u16, text: &str) -> ApiResult<T> {
    if (200..300).contains(&status) {
        let value: Value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(text).map_err(|e| Error {
                status,
                code: "invalid".into(),
                message: format!("Unerwartete Antwort: {e}"),
            })?
        };
        return serde_json::from_value(value).map_err(|e| Error {
            status,
            code: "invalid".into(),
            message: format!("Unerwartete Antwort: {e}"),
        });
    }
    let body = serde_json::from_str::<nrzmail_api::ApiError>(text).ok();
    let message = body
        .as_ref()
        .map(|b| b.error.message.clone())
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| match status {
            401 => "Bitte anmelden.".into(),
            403 => "Keine Berechtigung.".into(),
            404 => "Nicht gefunden.".into(),
            413 => "Die Datei ist zu gross.".into(),
            502..=504 => "Der Server antwortet nicht.".into(),
            _ => format!("Fehler {status}"),
        });
    let code = body.map(|b| b.error.code).unwrap_or_default();
    if status == 401 {
        crate::state::on_unauthorized();
    }
    Err(Error { status, code, message })
}

async fn finish<T: DeserializeOwned>(res: Response) -> ApiResult<T> {
    let status = res.status();
    let text = if status == 204 {
        String::new()
    } else {
        res.text().await.map_err(network)?
    };
    parse(status, &text)
}

fn builder(method: &str, path: &str) -> RequestBuilder {
    let full = url(path);
    // gloo-net hängt an eine Adresse mit `?` immer noch ein `&` an – darum die Abfrage
    // getrennt übergeben (dekodiert; gloo-net kodiert sie selbst wieder).
    let (base, query) = full.split_once('?').unwrap_or((&full, ""));
    let decode = |s: &str| js_sys::decode_uri_component(&s.replace('+', " ")).map(String::from).unwrap_or_else(|_| s.to_string());
    let pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    let b = match method {
        "POST" => Request::post(base),
        "PUT" => Request::put(base),
        "DELETE" => Request::delete(base),
        _ => Request::get(base),
    };
    b.query(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str()))).header("Accept", "application/json")
}

pub async fn get<T: DeserializeOwned>(path: &str) -> ApiResult<T> {
    finish(builder("GET", path).send().await.map_err(network)?).await
}

async fn send<T: DeserializeOwned, B: Serialize>(method: &str, path: &str, body: &B) -> ApiResult<T> {
    let req = builder(method, path).json(body).map_err(network)?;
    finish(req.send().await.map_err(network)?).await
}

pub async fn post<T: DeserializeOwned, B: Serialize>(path: &str, body: &B) -> ApiResult<T> {
    send("POST", path, body).await
}

pub async fn put<T: DeserializeOwned, B: Serialize>(path: &str, body: &B) -> ApiResult<T> {
    send("PUT", path, body).await
}

/// POST ohne Inhalt (z. B. Abmelden, Abgleich starten); die Antwort wird ignoriert.
pub async fn action(path: &str) -> ApiResult<()> {
    let _: IgnoredAny = finish(builder("POST", path).send().await.map_err(network)?).await?;
    Ok(())
}

pub async fn delete(path: &str) -> ApiResult<()> {
    let _: IgnoredAny = finish(builder("DELETE", path).send().await.map_err(network)?).await?;
    Ok(())
}

enum XhrEvent {
    Progress(f64),
    Done,
}

/// Datei als Rohdaten hochladen (XHR, damit es einen Fortschritt gibt). `progress` bekommt
/// Werte von 0.0 bis 1.0 und läuft in der Aufgabe des Aufrufers (Signale setzen geht).
pub async fn upload_raw<T: DeserializeOwned>(
    path: &str,
    file: &web_sys::File,
    mut progress: impl FnMut(f64),
) -> ApiResult<T> {
    let fail = |_| network("XMLHttpRequest");
    let xhr = web_sys::XmlHttpRequest::new().map_err(fail)?;
    xhr.open_with_async("POST", &url(path), true).map_err(fail)?;
    xhr.set_request_header("Accept", "application/json").map_err(fail)?;
    let mime = if file.type_().is_empty() { "application/octet-stream".to_string() } else { file.type_() };
    xhr.set_request_header("Content-Type", &mime).map_err(fail)?;

    let (tx, mut rx) = futures_channel::mpsc::unbounded::<XhrEvent>();
    let tx_progress = tx.clone();
    let on_progress = Closure::<dyn FnMut(web_sys::ProgressEvent)>::new(move |e: web_sys::ProgressEvent| {
        if e.length_computable() && e.total() > 0.0 {
            let _ = tx_progress.unbounded_send(XhrEvent::Progress(e.loaded() / e.total()));
        }
    });
    let on_done = Closure::<dyn FnMut()>::new(move || {
        let _ = tx.unbounded_send(XhrEvent::Done);
    });
    xhr.upload().map_err(fail)?.set_onprogress(Some(on_progress.as_ref().unchecked_ref()));
    xhr.set_onloadend(Some(on_done.as_ref().unchecked_ref()));
    xhr.send_with_opt_blob(Some(file)).map_err(fail)?;

    while let Some(ev) = rx.next().await {
        match ev {
            XhrEvent::Progress(p) => progress(p.clamp(0.0, 1.0)),
            XhrEvent::Done => break,
        }
    }
    drop(on_progress);
    drop(on_done);
    let status = xhr.status().unwrap_or(0);
    if status == 0 {
        return Err(network("Upload abgebrochen"));
    }
    progress(1.0);
    let text = xhr.response_text().ok().flatten().unwrap_or_default();
    parse(status, &text)
}

/// URL-Kodierung für Pfad- und Abfrageteile.
pub fn enc(s: &str) -> String {
    nrzmail_api::encode(s)
}
