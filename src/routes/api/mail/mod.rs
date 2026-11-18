//! Mail-API: Postfächer, Ordner, Nachrichten, Senden, Archiv, Import/Export und Statistik.

mod accounts;
mod archive;
mod messages;
mod send;
mod stats;

use rocket::http::{ContentType, Status};
use rocket::request::Request;
use rocket::response::{self, Responder, Response};
use rocket::Route;
use std::io::Cursor;

pub fn routes() -> Vec<Route> {
    let mut r = accounts::routes();
    r.extend(messages::routes());
    r.extend(send::routes());
    r.extend(archive::routes());
    r.extend(stats::routes());
    r
}

/// Inhalt einer Datei-Antwort.
pub enum Body {
    Bytes(Vec<u8>),
    File(rocket::tokio::fs::File),
}

/// Datei zum Herunterladen (oder mit `inline` zum Anzeigen). Eigene, strenge CSP: Inhalte
/// aus Mails dürfen im Browser nichts ausführen.
pub struct Download {
    pub body: Body,
    pub content_type: ContentType,
    pub filename: String,
    pub inline: bool,
}

/// `Content-Disposition` mit ASCII-Ersatz und UTF-8-Namen (RFC 6266/5987).
pub fn content_disposition(filename: &str, inline: bool) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in filename.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char)
            }
            _ => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    format!(
        "{}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}",
        if inline { "inline" } else { "attachment" }
    )
}

impl<'r> Responder<'r, 'static> for Download {
    fn respond_to(self, _req: &'r Request<'_>) -> response::Result<'static> {
        let mut b = Response::build();
        b.status(Status::Ok)
            .header(self.content_type)
            .raw_header(
                "Content-Disposition",
                content_disposition(&self.filename, self.inline),
            )
            .raw_header(
                "Content-Security-Policy",
                "default-src 'none'; img-src data:; style-src 'unsafe-inline'; sandbox",
            )
            .raw_header("Cache-Control", "private, no-store");
        match self.body {
            Body::Bytes(v) => {
                b.sized_body(v.len(), Cursor::new(v));
            }
            Body::File(f) => {
                b.sized_body(None, f);
            }
        }
        b.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_is_safe() {
        assert_eq!(
            content_disposition("Übersicht \"neu\".pdf", false),
            "attachment; filename=\"_bersicht _neu_.pdf\"; filename*=UTF-8''%C3%9Cbersicht%20%22neu%22.pdf"
        );
        assert!(
            content_disposition("a\r\nb.png", true).starts_with("inline; filename=\"a__b.png\"")
        );
    }
}
