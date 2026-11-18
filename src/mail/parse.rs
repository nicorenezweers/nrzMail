//! Zerlegen einer Nachricht (RFC 822) mit mail-parser: Kopf, Adressen, Datum, Text und HTML,
//! Vorschautext, Anhänge (nur Metadaten mit `part_index`), Abmelde-Link und die Merkmale
//! für die automatische Einteilung. Kaputte Nachrichten (falsche Kodierung, fehlende Header)
//! ergeben leere Felder statt eines Fehlers.

use crate::api::{Address, Category};
use mail_parser::decoders::{base64::base64_decode, quoted_printable::quoted_printable_decode};
use mail_parser::{Encoding, HeaderValue, Message, MessageParser, MimeHeaders, PartType};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Ende eines Blocks im HTML – danach kommt beim Ableiten des Textes ein Zeilenumbruch,
/// sonst klebt „Herbst-Aktion“ (Überschrift) an „Nur bis Sonntag“ (Absatz).
static BLOCK_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)</(?:p|div|h[1-6]|li|tr|td|th|table|blockquote|section|article|header|footer|ul|ol)\s*>")
        .expect("gültiger Ausdruck")
});

/// Höchstens so viel Text geht in den Suchindex.
const MAX_INDEX_CHARS: usize = 200_000;
/// Länge des Vorschautexts.
const SNIPPET_CHARS: usize = 200;

/// Anhang (ohne Inhalt – der steht in den Rohdaten).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAttachment {
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub content_id: String,
    /// Im HTML über `cid:` eingebettet
    pub inline: bool,
    /// Index in `mail_parser::Message::parts`
    pub part_index: u32,
}

/// Was nrzMail aus einer Nachricht speichert.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Message-ID ohne `<>`
    pub message_id: String,
    pub in_reply_to: String,
    /// References, ohne `<>`
    pub references: Vec<String>,
    pub subject: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    /// Date-Header (nur wenn plausibel)
    pub date: Option<i64>,
    pub text: String,
    /// Nur echtes HTML (kein aus Text erzeugtes)
    pub html: String,
    pub snippet: String,
    pub attachments: Vec<ParsedAttachment>,
    pub list_unsubscribe: String,
    /// Merkmale für die Einteilung
    pub is_list: bool,
    pub precedence: String,
    pub auto_submitted: String,
}

impl Parsed {
    pub fn has_attachments(&self) -> bool {
        self.attachments.iter().any(|a| !a.inline)
    }

    /// Kategorie nach den Regeln (gesendete Nachrichten sind immer „Allgemein“).
    pub fn category(&self, is_sent: bool) -> Category {
        if is_sent {
            return Category::Primary;
        }
        categorize(
            &self.from.address,
            self.is_list,
            &self.precedence,
            &self.auto_submitted,
        )
    }

    /// Namen und Adressen aller Empfänger (für den Suchindex).
    pub fn recipients_text(&self) -> String {
        self.to
            .iter()
            .chain(&self.cc)
            .chain(&self.bcc)
            .map(|a| format!("{} {}", a.name, a.address))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Text für den Suchindex (gekürzt).
    pub fn index_text(&self) -> String {
        self.text.chars().take(MAX_INDEX_CHARS).collect()
    }

    pub fn attachment_names(&self) -> String {
        self.attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| a.filename.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// SHA-256 der Rohdaten als Hex.
pub fn sha256(raw: &[u8]) -> String {
    hex::encode(Sha256::digest(raw))
}

/// Nachricht zerlegen. Gibt nie einen Fehler – im schlimmsten Fall sind die Felder leer.
pub fn parse(raw: &[u8]) -> Parsed {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return Parsed {
            text: String::from_utf8_lossy(raw).chars().take(10_000).collect(),
            ..Parsed::default()
        };
    };

    let from = msg
        .from()
        .or_else(|| msg.sender())
        .map(addresses)
        .and_then(|l| l.into_iter().next())
        .unwrap_or_default();

    // Text: alle Textteile; fehlt Text (oder ist er leer), aus dem HTML ableiten.
    let mut html_parts = Vec::new();
    for &id in &msg.html_body {
        if let Some(PartType::Html(h)) = msg.parts.get(id as usize).map(|p| &p.body) {
            html_parts.push(h.as_ref());
        }
    }
    let html = html_parts.join("\n");
    let mut text = (0..msg.text_body_count())
        .filter_map(|i| body_text(&msg, i))
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.trim().is_empty() && !html.is_empty() {
        text = html_text(&html);
    }
    let text = text.replace("\r\n", "\n");

    let precedence = header_text(&msg, "Precedence").to_ascii_lowercase();
    let auto_submitted = header_text(&msg, "Auto-Submitted").to_ascii_lowercase();
    let list_unsubscribe_raw = header_text(&msg, "List-Unsubscribe");
    let is_list = !header_text(&msg, "List-Id").is_empty() || !list_unsubscribe_raw.is_empty();

    let mut attachments = Vec::new();
    for (n, &id) in msg.attachments.iter().enumerate() {
        let Some(part) = msg.parts.get(id as usize) else {
            continue;
        };
        let content_type = part
            .content_type()
            .map(|c| {
                format!(
                    "{}/{}",
                    c.ctype(),
                    c.subtype().unwrap_or(match c.ctype() {
                        "text" => "plain",
                        _ => "octet-stream",
                    })
                )
                .to_ascii_lowercase()
            })
            .unwrap_or_else(|| {
                match part.body {
                    PartType::Text(_) => "text/plain",
                    PartType::Html(_) => "text/html",
                    PartType::Message(_) => "message/rfc822",
                    _ => "application/octet-stream",
                }
                .into()
            });
        let content_id = part
            .content_id()
            .unwrap_or("")
            .trim()
            .trim_matches(['<', '>'])
            .to_string();
        let size = part.len() as i64;
        let explicit_attachment = part
            .content_disposition()
            .is_some_and(|d| d.is_attachment());
        let inline = !content_id.is_empty()
            && !explicit_attachment
            && html.contains(&format!("cid:{content_id}"));
        let filename = part
            .attachment_name()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| match &part.body {
                PartType::Message(inner) => {
                    let subject = inner.subject().unwrap_or("").trim();
                    if subject.is_empty() {
                        "Nachricht.eml".into()
                    } else {
                        format!("{subject}.eml")
                    }
                }
                _ => format!("Anhang-{}.{}", n + 1, extension(&content_type)),
            });
        if size == 0 && part.attachment_name().is_none() {
            continue;
        }
        attachments.push(ParsedAttachment {
            filename: filename.chars().take(255).collect(),
            content_type,
            size,
            content_id,
            inline,
            part_index: id,
        });
    }

    let date = msg
        .date()
        .map(|d| d.to_timestamp())
        .filter(|ts| *ts > 0 && *ts < crate::db::models::now() + 365 * 86_400);

    let snippet = snippet(&text);
    Parsed {
        message_id: msg.message_id().map(clean_id).unwrap_or_default(),
        in_reply_to: ids(msg.in_reply_to())
            .into_iter()
            .next()
            .unwrap_or_default(),
        references: ids(msg.references()),
        subject: collapse(msg.subject().unwrap_or("")),
        from,
        to: msg.to().map(addresses).unwrap_or_default(),
        cc: msg.cc().map(addresses).unwrap_or_default(),
        bcc: msg.bcc().map(addresses).unwrap_or_default(),
        reply_to: msg.reply_to().map(addresses).unwrap_or_default(),
        date,
        text,
        html,
        snippet,
        attachments,
        list_unsubscribe: unsubscribe_link(&list_unsubscribe_raw),
        is_list,
        precedence,
        auto_submitted,
    }
}

/// Text aus HTML, mit Zeilenumbruch nach jedem Block.
fn html_text(html: &str) -> String {
    mail_parser::decoders::html::html_to_text(&BLOCK_END.replace_all(html, "$0<br>"))
}

/// Text eines Textteils wie `Message::body_text`, aber mit Rückfall auf Latin-1: Ohne
/// Zeichensatz (oder mit falsch angegebenem UTF-8) liest mail-parser UTF-8, alte Mails sind
/// aber oft Latin-1 – dann stünde „K�se“ statt „Käse“ im Text. HTML-Teile (Mails ohne
/// Textfassung) über [`html_text`].
fn body_text(msg: &Message<'_>, index: usize) -> Option<String> {
    let part = msg
        .text_body
        .get(index)
        .and_then(|&id| msg.parts.get(id as usize));
    if let Some(PartType::Html(h)) = part.map(|p| &p.body) {
        return Some(html_text(h));
    }
    let text = msg.body_text(index)?;
    let Some(part) = part.filter(|p| matches!(p.body, PartType::Text(_))) else {
        return Some(text.into_owned());
    };
    let charset = part
        .content_type()
        .and_then(|c| c.attribute("charset"))
        .map(str::to_ascii_lowercase);
    let utf8_assumed = matches!(
        charset.as_deref(),
        None | Some("utf-8" | "utf8" | "us-ascii")
    );
    if !text.contains('\u{FFFD}') || !utf8_assumed {
        return Some(text.into_owned());
    }
    let raw = msg
        .raw_message()
        .get(part.offset_body as usize..part.offset_end as usize)?;
    let bytes = match part.encoding {
        Encoding::QuotedPrintable => quoted_printable_decode(raw),
        Encoding::Base64 => base64_decode(raw),
        Encoding::None => Some(raw.to_vec()),
    };
    match bytes {
        Some(b) if std::str::from_utf8(&b).is_err() => {
            Some(b.iter().map(|&c| char::from(c)).collect())
        }
        _ => Some(text.into_owned()),
    }
}

/// Rohwert eines Kopffelds (entfaltet, ohne Zeilenumbrüche).
pub(crate) fn header_text(msg: &mail_parser::Message<'_>, name: &str) -> String {
    msg.header_raw(name).map(collapse).unwrap_or_default()
}

fn clean_id(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).trim().to_string()
}

fn ids(v: &HeaderValue<'_>) -> Vec<String> {
    let list: Vec<String> = match v {
        HeaderValue::Text(t) => vec![t.to_string()],
        HeaderValue::TextList(l) => l.iter().map(|s| s.to_string()).collect(),
        _ => Vec::new(),
    };
    list.iter()
        .map(|s| clean_id(s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Adressen (Gruppen werden aufgelöst); Namen ohne Anführungszeichen, leere Einträge weg.
fn addresses(a: &mail_parser::Address<'_>) -> Vec<Address> {
    let list: Vec<&mail_parser::Addr<'_>> = match a {
        mail_parser::Address::List(l) => l.iter().collect(),
        mail_parser::Address::Group(g) => g.iter().flat_map(|g| g.addresses.iter()).collect(),
    };
    list.into_iter()
        .filter_map(|x| {
            let address = x.address.as_deref().unwrap_or("").trim().to_string();
            let mut name = collapse(x.name.as_deref().unwrap_or(""))
                .trim_matches(['"', '\''])
                .trim()
                .to_string();
            if name.eq_ignore_ascii_case(&address) {
                name.clear();
            }
            (!address.is_empty() || !name.is_empty()).then(|| Address::new(name, address))
        })
        .collect()
}

/// Mehrfache Leerzeichen und Zeilenumbrüche zu einem Leerzeichen.
pub fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extension(content_type: &str) -> &'static str {
    match content_type {
        "application/pdf" => "pdf",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/calendar" => "ics",
        "message/rfc822" => "eml",
        "application/zip" => "zip",
        _ => "bin",
    }
}

/// Abmelde-Link aus `List-Unsubscribe`: https bevorzugt, sonst http, sonst mailto.
pub fn unsubscribe_link(raw: &str) -> String {
    let mut links: Vec<String> = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        let link: String = rest[start + 1..start + end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if !link.is_empty() {
            links.push(link);
        }
        rest = &rest[start + end + 1..];
    }
    if links.is_empty() {
        let t = raw.trim();
        if t.starts_with("http") || t.starts_with("mailto:") {
            links.push(t.to_string());
        }
    }
    let lower = |l: &String| l.to_ascii_lowercase();
    links
        .iter()
        .find(|l| lower(l).starts_with("https://"))
        .or_else(|| links.iter().find(|l| lower(l).starts_with("http://")))
        .or_else(|| links.iter().find(|l| lower(l).starts_with("mailto:")))
        .cloned()
        .unwrap_or_default()
}

/// Absender-Domains sozialer Netzwerke.
const SOCIAL_DOMAINS: &[&str] = &[
    "facebookmail.com",
    "facebook.com",
    "instagram.com",
    "linkedin.com",
    "twitter.com",
    "x.com",
    "xing.com",
    "pinterest.com",
    "tiktok.com",
    "redditmail.com",
    "reddit.com",
    "youtube.com",
    "discord.com",
    "discordapp.com",
    "mastodon.social",
    "meetup.com",
    "quora.com",
    "tumblr.com",
    "snapchat.com",
    "whatsapp.com",
    "telegram.org",
    "strava.com",
    "nextdoor.com",
    "threads.net",
    "bsky.app",
    "flickr.com",
    "vk.com",
];

/// Automatische Einteilung nach Absender und Kopfzeilen.
pub fn categorize(
    from_address: &str,
    is_list: bool,
    precedence: &str,
    auto_submitted: &str,
) -> Category {
    let from = from_address.trim().to_ascii_lowercase();
    let (local, domain) = from.rsplit_once('@').unwrap_or((from.as_str(), ""));
    if !domain.is_empty()
        && SOCIAL_DOMAINS
            .iter()
            .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
    {
        return Category::Social;
    }
    let auto = auto_submitted.trim();
    if !auto.is_empty() && auto != "no" {
        return Category::Notification;
    }
    if [
        "notification",
        "notify",
        "alert",
        "mailer-daemon",
        "postmaster",
    ]
    .iter()
    .any(|p| local.contains(p))
    {
        return Category::Notification;
    }
    if is_list
        || matches!(precedence.trim(), "bulk" | "list" | "junk")
        || local.contains("newsletter")
    {
        return Category::Newsletter;
    }
    if [
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
    ]
    .iter()
    .any(|p| local.contains(p))
    {
        return Category::Notification;
    }
    Category::Primary
}

/// Unsichtbare Zeichen, mit denen Newsletter den Vorschautext auffüllen.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' | '\u{034F}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
    )
}

/// Zeile, mit der ein zitierter Text beginnt („Am … schrieb …:“, „On … wrote:“, …).
fn is_quote_start(line: &str) -> bool {
    let l = line.trim();
    let lower = l.to_lowercase();
    if l.starts_with("-----") && (lower.contains("original") || lower.contains("ursprüngliche")) {
        return true;
    }
    if l.len() >= 20 && l.chars().all(|c| c == '_') {
        return true;
    }
    lower.ends_with(':')
        && ((lower.starts_with("on ") && lower.contains("wrote"))
            || (lower.starts_with("am ") && lower.contains("schrieb"))
            || lower.contains("a écrit")
            || lower.contains("ha scritto"))
}

/// Vorschautext: Anfang des Textes ohne Zitate, Signatur, Links in spitzen Klammern und
/// Leerraum-Müll, höchstens ~200 Zeichen.
pub fn snippet(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut len = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('>') {
            continue;
        }
        if line.trim_end_matches('\r') == "-- " || trimmed == "--" || is_quote_start(trimmed) {
            break;
        }
        let cleaned: String = trimmed
            .chars()
            .map(|c| if c == '\u{00A0}' { ' ' } else { c })
            .filter(|c| !is_invisible(*c))
            .collect();
        for word in cleaned.split_whitespace() {
            if word.starts_with("<http") || word.starts_with("[http") {
                continue;
            }
            len += word.chars().count() + 1;
            words.push(word.to_string());
        }
        if len > SNIPPET_CHARS + 20 {
            break;
        }
    }
    let joined = words.join(" ");
    if joined.chars().count() <= SNIPPET_CHARS {
        return joined;
    }
    let cut: String = joined.chars().take(SNIPPET_CHARS).collect();
    let cut = match cut.rfind(' ') {
        Some(pos) if pos > SNIPPET_CHARS / 2 => cut[..pos].to_string(),
        _ => cut,
    };
    format!("{}…", cut.trim_end_matches([',', '.', ';', ':', ' ']))
}

/// Teil `part_index` aus den Rohdaten (dekodiert) – für das Herunterladen von Anhängen.
pub fn part_contents(raw: &[u8], part_index: u32) -> Option<Vec<u8>> {
    let msg = MessageParser::default().parse(raw)?;
    msg.parts
        .get(part_index as usize)
        .map(|p| p.contents().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTI: &str = "From: \"Anna Müller\" <anna@example.ch>\r\n\
To: Bruno <bruno@example.com>, carla@example.com\r\n\
Cc: =?UTF-8?Q?J=C3=BCrg?= <juerg@example.ch>\r\n\
Subject: =?UTF-8?Q?Gr=C3=BCsse_aus_Z=C3=BCrich?=\r\n\
Date: Tue, 14 Oct 2025 09:30:00 +0200\r\n\
Message-ID: <abc-1@example.ch>\r\n\
In-Reply-To: <root@example.com>\r\n\
References: <root@example.com> <mid@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"X\"\r\n\
\r\n\
--X\r\n\
Content-Type: multipart/related; boundary=\"R\"\r\n\
\r\n\
--R\r\n\
Content-Type: multipart/alternative; boundary=\"A\"\r\n\
\r\n\
--A\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Hallo Bruno,=0D=0Aanbei die =C3=9Cbersicht.\r\n\
\r\n\
Am 12.10.2025 um 10:00 schrieb Bruno:\r\n\
> Kannst du mir die Zahlen schicken?\r\n\
--A\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Hallo Bruno,</p><p>anbei die <b>Übersicht</b>.</p><img src=\"cid:logo@x\">\r\n\
--A--\r\n\
--R\r\n\
Content-Type: image/png\r\n\
Content-ID: <logo@x>\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--R--\r\n\
--X\r\n\
Content-Type: application/pdf; name=\"Übersicht.pdf\"\r\n\
Content-Disposition: attachment; filename=\"=?UTF-8?Q?=C3=9Cbersicht.pdf?=\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--X--\r\n";

    #[test]
    fn parses_headers_bodies_and_attachments() {
        let p = parse(MULTI.as_bytes());
        assert_eq!(p.subject, "Grüsse aus Zürich");
        assert_eq!(p.from, Address::new("Anna Müller", "anna@example.ch"));
        assert_eq!(
            p.to,
            vec![
                Address::new("Bruno", "bruno@example.com"),
                Address::new("", "carla@example.com")
            ]
        );
        assert_eq!(p.cc, vec![Address::new("Jürg", "juerg@example.ch")]);
        assert_eq!(p.message_id, "abc-1@example.ch");
        assert_eq!(p.in_reply_to, "root@example.com");
        assert_eq!(p.references, vec!["root@example.com", "mid@example.com"]);
        assert_eq!(p.date, Some(1_760_427_000));
        assert!(
            p.text.starts_with("Hallo Bruno,\nanbei die Übersicht."),
            "{}",
            p.text
        );
        assert!(p.html.contains("<b>Übersicht</b>"));
        assert_eq!(p.snippet, "Hallo Bruno, anbei die Übersicht.");
        assert_eq!(p.attachments.len(), 2, "{:?}", p.attachments);
        let logo = &p.attachments[0];
        assert!(logo.inline && logo.content_id == "logo@x" && logo.content_type == "image/png");
        let pdf = &p.attachments[1];
        assert_eq!(
            (pdf.filename.as_str(), pdf.content_type.as_str(), pdf.inline),
            ("Übersicht.pdf", "application/pdf", false)
        );
        assert!(p.has_attachments());
        assert_eq!(
            part_contents(MULTI.as_bytes(), pdf.part_index).unwrap(),
            b"%PDF-1.4\n"
        );
        assert_eq!(p.attachment_names(), "Übersicht.pdf");
        assert!(p.recipients_text().contains("Jürg juerg@example.ch"));
    }

    #[test]
    fn broken_mails_do_not_fail() {
        let p = parse(b"");
        assert_eq!(p.subject, "");
        let p = parse(b"Subject: nur ein Kopf\r\n");
        assert_eq!(p.subject, "nur ein Kopf");
        assert_eq!(p.from, Address::default());
        assert!(p.date.is_none());
        // Falsche Zeichensatz-Angabe und Latin-1-Bytes
        let p = parse(
            b"From: x@y.ch\r\nSubject: Gr\xfcsse\r\nContent-Type: text/plain; charset=gibtsnicht\r\n\r\nK\xe4se\r\n",
        );
        assert!(p.text.contains('K'));
        assert!(!p.subject.is_empty());
        let p = parse(b"Date: kein Datum\r\n\r\nText");
        assert!(p.date.is_none());
    }

    #[test]
    fn html_only_mail_gets_text() {
        let raw = "From: shop@example.com\r\nSubject: Angebot\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><head><style>p{color:red}</style></head><body><p>Nur heute&nbsp;20&#37; Rabatt</p></body></html>";
        let p = parse(raw.as_bytes());
        assert!(p.text.contains("Nur heute"), "{}", p.text);
        assert!(!p.text.contains("color"), "{}", p.text);
        assert!(p.html.contains("<p>"));
        assert!(p.snippet.starts_with("Nur heute"), "{}", p.snippet);

        // Überschrift und Absatz kleben im Vorschautext nicht zusammen
        let raw = "Content-Type: text/html\r\n\r\n<h1>Herbst-Aktion</h1><p>Nur bis Sonntag</p>";
        let p = parse(raw.as_bytes());
        assert!(!p.snippet.contains("Herbst-AktionNur"), "{}", p.snippet);
        assert!(p.snippet.contains("Nur bis Sonntag"), "{}", p.snippet);
    }

    #[test]
    fn latin1_without_charset_keeps_umlauts() {
        let p = parse(b"From: x@y.ch\r\nSubject: Notiz\r\nContent-Type: text/plain\r\n\r\nK\xe4se und Br\xf6tchen\r\n");
        assert!(p.text.contains("Käse und Brötchen"), "{}", p.text);
        // Quoted-Printable ohne Zeichensatz
        let p = parse(b"Subject: x\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nGr=FCsse\r\n");
        assert!(p.text.contains("Grüsse"), "{}", p.text);
        // Gültiges UTF-8 bleibt, wie es ist
        let p = parse("Subject: x\r\nContent-Type: text/plain\r\n\r\nGrüsse\r\n".as_bytes());
        assert!(p.text.contains("Grüsse"), "{}", p.text);
    }

    #[test]
    fn snippet_skips_quotes_and_padding() {
        let text = "\u{200C}\u{00A0}\u{034F} \n\n  Danke   für die Nachricht!  \n<https://example.com/x>\n\nOn Mon, 1 Jan 2024, Bob wrote:\n> alt\n";
        assert_eq!(snippet(text), "Danke für die Nachricht!");
        let text = "Erste Zeile\n-- \nSignatur";
        assert_eq!(snippet(text), "Erste Zeile");
        let long = "wort ".repeat(100);
        let s = snippet(&long);
        assert!(
            s.ends_with('…') && s.chars().count() <= SNIPPET_CHARS + 1,
            "{s}"
        );
    }

    #[test]
    fn category_rules() {
        assert_eq!(
            categorize("anna@example.ch", false, "", ""),
            Category::Primary
        );
        assert_eq!(
            categorize("news@shop.ch", true, "", ""),
            Category::Newsletter
        );
        assert_eq!(
            categorize("info@shop.ch", false, "bulk", ""),
            Category::Newsletter
        );
        assert_eq!(
            categorize("notification@facebookmail.com", true, "", ""),
            Category::Social
        );
        assert_eq!(
            categorize("messages-noreply@linkedin.com", true, "bulk", ""),
            Category::Social
        );
        assert_eq!(
            categorize("notifications@github.com", true, "list", ""),
            Category::Notification
        );
        assert_eq!(
            categorize("noreply@bank.ch", false, "", ""),
            Category::Notification
        );
        assert_eq!(
            categorize("x@example.ch", false, "", "auto-generated"),
            Category::Notification
        );
        assert_eq!(
            categorize("x@example.ch", false, "", "no"),
            Category::Primary
        );
        let p = Parsed {
            is_list: true,
            ..Parsed::default()
        };
        assert_eq!(p.category(true), Category::Primary, "gesendet");
    }

    #[test]
    fn unsubscribe_prefers_https() {
        assert_eq!(
            unsubscribe_link("<mailto:u@x.ch?subject=weg>, <https://x.ch/u?id=1>"),
            "https://x.ch/u?id=1"
        );
        assert_eq!(unsubscribe_link("<mailto:u@x.ch>"), "mailto:u@x.ch");
        assert_eq!(unsubscribe_link("https://x.ch/u"), "https://x.ch/u");
        assert_eq!(unsubscribe_link("kaputt"), "");
    }
}
