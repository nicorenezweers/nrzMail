//! Vom Newsletter abmelden – möglichst automatisch, der Reihe nach:
//!
//! 1. **One-Click** (RFC 8058): `List-Unsubscribe` mit https und `List-Unsubscribe-Post:
//!    List-Unsubscribe=One-Click` → ein POST, fertig.
//! 2. **mailto**: die Abmelde-Mail geht sofort über das Postfach raus (ohne Sendeverzögerung).
//! 3. **Abmeldeseite** (aus dem Kopf oder, ohne Kopf, der Abmelde-Link im HTML): Seite laden,
//!    das Abmelde-Formular finden und abschicken (versteckte Felder, Adresse, „Abmelden“-Knopf).
//!    Meldet die Seite schon ohne Formular den Erfolg, genügt das.
//!
//! Klappt nichts davon, bekommt die Oberfläche die Seite zum selbst Öffnen. Anfragen gehen nur
//! an öffentliche Adressen (nie ins eigene Netz), mit Zeitlimit und ohne Cookies des Benutzers.

use crate::api::{self, Address, Compose, MessageAction, UnsubscribeCleanup};
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::mail::{actions, parse, send, store};
use regex::Regex;
use sqlx::SqlitePool;
use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 6 {
                attempt.error("zu viele Weiterleitungen")
            } else if !host_allowed_literal(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .user_agent(concat!("Mozilla/5.0 (compatible; nrzMail/", env!("CARGO_PKG_VERSION"), ")"))
        .build()
        .expect("HTTP-Client")
});

/// Was eine Nachricht zum Abmelden anbietet.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Offer {
    pub https: Vec<String>,
    pub mailto: Vec<String>,
    pub one_click: bool,
}

/// `List-Unsubscribe: <mailto:…>, <https://…>` und `List-Unsubscribe-Post`.
pub fn offer(list_unsubscribe: &str, list_post: &str) -> Offer {
    let mut o = Offer {
        one_click: list_post.to_ascii_lowercase().contains("one-click"),
        ..Offer::default()
    };
    for part in list_unsubscribe.split(',') {
        let link: String = part
            .trim()
            .trim_matches(['<', '>'])
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let lower = link.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") {
            o.https.push(link);
        } else if lower.starts_with("mailto:") {
            o.mailto.push(link);
        }
    }
    o
}

/// Abmelde-Link im HTML, wenn der Kopf fehlt: ein `<a>`, dessen Text oder Ziel nach Abmelden klingt.
pub fn link_in_html(html: &str) -> Option<String> {
    static A: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)<a\b[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).expect("Regex")
    });
    let mut best: Option<(u8, String)> = None;
    for c in A.captures_iter(html) {
        let href = decode_entities(c[1].trim());
        if !(href.starts_with("https://") || href.starts_with("http://")) {
            continue;
        }
        let text = strip_tags(&c[2]).to_lowercase();
        let h = href.to_lowercase();
        let score = if is_unsubscribe_word(&text) {
            2
        } else if is_unsubscribe_word(&h) {
            1
        } else {
            0
        };
        if score > 0 && best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, href));
        }
    }
    best.map(|(_, h)| h)
}

fn is_unsubscribe_word(s: &str) -> bool {
    [
        "unsubscribe",
        "abmelden",
        "abbestellen",
        "austragen",
        "abmeldung",
        "opt-out",
        "optout",
        "désabonner",
        "desabonner",
        "disiscriviti",
        "cancella iscrizione",
        "uitschrijven",
    ]
    .iter()
    .any(|w| s.contains(w))
}

fn is_success_text(s: &str) -> bool {
    let s = s.to_lowercase();
    [
        "successfully unsubscribed",
        "you have been unsubscribed",
        "you've been unsubscribed",
        "you are unsubscribed",
        "have been removed",
        "erfolgreich abgemeldet",
        "wurden abgemeldet",
        "sind abgemeldet",
        "wurde abgemeldet",
        "erfolgreich ausgetragen",
        "abmeldung war erfolgreich",
        "abmeldung erfolgreich",
        "désinscription confirmée",
        "unsubscribe successful",
    ]
    .iter()
    .any(|w| s.contains(w))
}

fn strip_tags(s: &str) -> String {
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").expect("Regex"));
    decode_entities(&TAG.replace_all(s, " ")).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

// ------------------------------------------------------------------ Netz-Schutz

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]) // CGNAT/Tailscale
                || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| private_ip(IpAddr::V4(v4)))
        }
    }
}

/// Schnelle Prüfung ohne DNS (für Weiterleitungen): keine IP im eigenen Netz, kein localhost.
fn host_allowed_literal(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "https" | "http") {
        return false;
    }
    let Some(host) = url.host_str() else { return false };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    match host.parse::<IpAddr>() {
        Ok(ip) => !private_ip(ip),
        Err(_) => !(host == "localhost"
            || host.ends_with(".localhost")
            || host.ends_with(".local")
            || host.ends_with(".internal")),
    }
}

/// Darf nrzMail diese Adresse abrufen? (öffentlich, auch nach DNS)
async fn allowed(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    if !host_allowed_literal(&u) {
        return false;
    }
    let Some(host) = u.host_str().map(|h| h.trim_matches(['[', ']']).to_string()) else { return false };
    let port = u.port_or_known_default().unwrap_or(443);
    let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let addrs: Vec<std::net::SocketAddr> = match tokio::net::lookup_host(target).await {
        Ok(a) => a.collect(),
        Err(_) => return false,
    };
    !addrs.is_empty() && addrs.iter().all(|a| !private_ip(a.ip()))
}

// ------------------------------------------------------------------ Formulare

#[derive(Debug, Default, Clone, PartialEq)]
struct Form {
    action: String,
    method: String,
    fields: Vec<(String, String)>,
    /// Text der Knöpfe und des Formulars (zum Erkennen)
    text: String,
    /// Name/Wert des Abmelde-Knopfs, falls er einen hat
    submit: Option<(String, String)>,
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"(?is)\b{name}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#)).ok()?;
    let c = re.captures(tag)?;
    c.get(1)
        .or_else(|| c.get(2))
        .or_else(|| c.get(3))
        .map(|m| decode_entities(m.as_str()))
}

/// Formulare der Seite (einfacher Zerleger – reicht für Abmeldeseiten).
fn forms(html: &str, email: &str) -> Vec<Form> {
    static FORM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<form\b([^>]*)>(.*?)</form>").expect("Regex"));
    static INPUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<input\b[^>]*>").expect("Regex"));
    static BUTTON: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<button\b([^>]*)>(.*?)</button>").expect("Regex"));
    static SELECT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<select\b([^>]*)>(.*?)</select>").expect("Regex"));
    static OPTION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<option\b([^>]*)>(.*?)</option>").expect("Regex"));
    let mut out = Vec::new();
    for f in FORM.captures_iter(html) {
        let head = &f[1];
        let inner = &f[2];
        let mut form = Form {
            action: attr(head, "action").unwrap_or_default(),
            method: attr(head, "method").unwrap_or_else(|| "get".into()).to_lowercase(),
            text: format!("{} {}", strip_tags(inner), attr(head, "action").unwrap_or_default()).to_lowercase(),
            ..Form::default()
        };
        for i in INPUT.find_iter(inner) {
            let tag = i.as_str();
            let kind = attr(tag, "type").unwrap_or_else(|| "text".into()).to_lowercase();
            let Some(name) = attr(tag, "name").filter(|n| !n.is_empty()) else {
                if kind == "submit" {
                    form.text.push(' ');
                    form.text.push_str(&attr(tag, "value").unwrap_or_default().to_lowercase());
                }
                continue;
            };
            let value = attr(tag, "value").unwrap_or_default();
            match kind.as_str() {
                "submit" | "image" => {
                    form.text.push(' ');
                    form.text.push_str(&value.to_lowercase());
                    if form.submit.is_none() || is_unsubscribe_word(&value.to_lowercase()) {
                        form.submit = Some((name, value));
                    }
                }
                "checkbox" | "radio" => {
                    // Abmelde-Optionen („von allen Listen abmelden“) einschalten, sonst wie vorgegeben
                    let checked = tag.to_lowercase().contains("checked");
                    let wanted = is_unsubscribe_word(&format!("{name} {value}").to_lowercase())
                        || name.to_lowercase().contains("all");
                    if checked || (wanted && kind == "checkbox") {
                        form.fields.push((name, if value.is_empty() { "on".into() } else { value }));
                    }
                }
                "button" | "reset" | "file" => {}
                "email" => form.fields.push((name, if value.is_empty() { email.into() } else { value })),
                _ => {
                    let v = if value.is_empty() && name.to_lowercase().contains("mail") {
                        email.to_string()
                    } else {
                        value
                    };
                    form.fields.push((name, v));
                }
            }
        }
        for b in BUTTON.captures_iter(inner) {
            let kind = attr(&b[1], "type").unwrap_or_else(|| "submit".into()).to_lowercase();
            if kind != "submit" {
                continue;
            }
            let label = strip_tags(&b[2]).to_lowercase();
            if let Some(name) = attr(&b[1], "name").filter(|n| !n.is_empty()) {
                if form.submit.is_none() || is_unsubscribe_word(&label) {
                    form.submit = Some((name, attr(&b[1], "value").unwrap_or_default()));
                }
            }
        }
        for sel in SELECT.captures_iter(inner) {
            if let Some(name) = attr(&sel[1], "name") {
                let opts: Vec<(String, String)> = OPTION
                    .captures_iter(&sel[2])
                    .map(|o| {
                        let label = strip_tags(&o[2]);
                        (attr(&o[1], "value").unwrap_or_else(|| label.clone()), label.to_lowercase())
                    })
                    .collect();
                let pick = opts
                    .iter()
                    .find(|(v, l)| is_unsubscribe_word(l) || is_unsubscribe_word(&v.to_lowercase()))
                    .or(opts.first());
                if let Some((v, _)) = pick {
                    form.fields.push((name, v.clone()));
                }
            }
        }
        out.push(form);
    }
    out
}

/// Formular der Abmeldeseite abschicken. `Ok(true)` = abgeschickt und die Antwort sieht gut aus.
async fn submit_form(page: &reqwest::Url, form: &Form) -> Result<bool, String> {
    let target = if form.action.trim().is_empty() {
        page.clone()
    } else {
        page.join(form.action.trim()).map_err(|e| e.to_string())?
    };
    if !allowed(target.as_str()).await {
        return Err("Formular zeigt auf eine nicht erlaubte Adresse".into());
    }
    let mut fields = form.fields.clone();
    if let Some(s) = &form.submit {
        fields.push(s.clone());
    }
    let req = if form.method == "post" {
        HTTP.post(target).form(&fields)
    } else {
        HTTP.get(target).query(&fields)
    };
    let resp = req.send().await.map_err(|e| e.without_url().to_string())?;
    let ok = resp.status().is_success() || resp.status().is_redirection();
    let body = resp.text().await.unwrap_or_default();
    // Fehlerseite? Sonst gilt es als erledigt.
    let lower = strip_tags(&body).to_lowercase();
    let failed = lower.contains("error") && !is_success_text(&lower) && lower.len() < 400;
    Ok(ok && !failed)
}

/// Abmeldeseite aufrufen und, wenn nötig, ihr Formular abschicken. (Erfolg, Methode)
async fn via_page(url: &str, email: &str) -> Result<(bool, &'static str), String> {
    if !allowed(url).await {
        return Err("Die Abmeldeseite liegt nicht im öffentlichen Netz".into());
    }
    let resp = HTTP.get(url).send().await.map_err(|e| e.without_url().to_string())?;
    let page = resp.url().clone();
    if !resp.status().is_success() {
        return Err(format!("Abmeldeseite antwortet mit HTTP {}", resp.status().as_u16()));
    }
    let html = resp.text().await.unwrap_or_default();
    let text = strip_tags(&html);
    let all = forms(&html, email);
    // Das Formular, das nach Abmelden aussieht (sonst das einzige)
    let form = all
        .iter()
        .find(|f| is_unsubscribe_word(&f.text))
        .or_else(|| (all.len() == 1).then(|| &all[0]));
    if let Some(form) = form {
        return submit_form(&page, form).await.map(|ok| (ok, "form"));
    }
    if is_success_text(&text) {
        return Ok((true, "link"));
    }
    // Abmelde-Knopf als Link auf der Seite („Jetzt abmelden“)
    if let Some(next) = link_in_html(&html) {
        if let Ok(next) = page.join(&next) {
            if next.as_str() != url && allowed(next.as_str()).await {
                let r = HTTP.get(next).send().await.map_err(|e| e.without_url().to_string())?;
                let ok = r.status().is_success();
                let body = r.text().await.unwrap_or_default();
                return Ok((ok && (is_success_text(&strip_tags(&body)) || forms(&body, email).is_empty()), "link"));
            }
        }
    }
    Ok((false, "manual"))
}

async fn one_click(url: &str) -> Result<bool, String> {
    if !allowed(url).await {
        return Err("Die Abmeldeadresse liegt nicht im öffentlichen Netz".into());
    }
    let r = HTTP
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    Ok(r.status().is_success() || r.status().is_redirection())
}

/// `mailto:liste@example.com?subject=unsubscribe&body=…` → Empfänger, Betreff, Text.
pub fn parse_mailto(link: &str) -> Option<(String, String, String)> {
    let rest = link.strip_prefix("mailto:").or_else(|| link.strip_prefix("MAILTO:"))?;
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    let to = percent_decode(to);
    if !to.contains('@') {
        return None;
    }
    let mut subject = "unsubscribe".to_string();
    let mut body = "unsubscribe".to_string();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k.to_ascii_lowercase().as_str() {
            "subject" if !v.is_empty() => subject = percent_decode(v),
            "body" if !v.is_empty() => body = percent_decode(v),
            _ => {}
        }
    }
    Some((to, subject, body))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------ Ablauf

struct Message {
    account_id: String,
    from_address: String,
    account_email: String,
}

/// Abmelden und (nach der Einstellung) die bisherigen Nachrichten des Absenders aufräumen.
pub async fn unsubscribe(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    message_id: &str,
    cleanup: UnsubscribeCleanup,
) -> AppResult<api::UnsubscribeResult> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT m.account_id, m.from_address, a.email, m.html_body FROM messages m
         JOIN mail_accounts a ON a.id = m.account_id WHERE m.id = ? AND a.user_id = ?",
    )
    .bind(message_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    let Some((account_id, from_address, account_email, html)) = row else {
        return Err(AppError::NotFound);
    };
    let msg = Message { account_id, from_address, account_email };
    let raw = store::raw(pool, message_id).await.unwrap_or_default();
    let (lu, lp) = match mail_parser::MessageParser::default().parse(&raw) {
        Some(m) => (
            parse::header_text(&m, "List-Unsubscribe"),
            parse::header_text(&m, "List-Unsubscribe-Post"),
        ),
        None => (String::new(), String::new()),
    };
    let mut offer = offer(&lu, &lp);
    if offer.https.is_empty() && offer.mailto.is_empty() {
        if let Some(link) = link_in_html(&html) {
            offer.https.push(link);
        }
    }
    if offer.https.is_empty() && offer.mailto.is_empty() {
        return Ok(api::UnsubscribeResult {
            method: "manual".into(),
            message: "Diese Nachricht enthält keinen Abmelde-Link.".into(),
            ..Default::default()
        });
    }

    let mut result = api::UnsubscribeResult::default();
    let mut notes: Vec<String> = Vec::new();

    // 1. One-Click
    if offer.one_click {
        if let Some(url) = offer.https.iter().find(|u| u.starts_with("https://")) {
            match one_click(url).await {
                Ok(true) => {
                    result.done = true;
                    result.method = "one-click".into();
                    result.message = "Abgemeldet (One-Click beim Absender).".into();
                }
                Ok(false) => notes.push("One-Click wurde abgelehnt".into()),
                Err(e) => notes.push(e),
            }
        }
    }
    // 2. mailto
    if !result.done {
        if let Some((to, subject, body)) = offer.mailto.iter().find_map(|m| parse_mailto(m)) {
            let compose = Compose {
                account_id: msg.account_id.clone(),
                to: vec![Address::new("", to.clone())],
                subject,
                text: body,
                ..Default::default()
            };
            match send::send(pool, config, user_id, &compose, 0).await {
                Ok(_) => {
                    result.done = true;
                    result.method = "mailto".into();
                    result.message = format!("Abmelde-Mail an {to} gesendet.");
                }
                Err(e) => notes.push(format!("Abmelde-Mail: {e}")),
            }
        }
    }
    // 3. Abmeldeseite samt Formular
    if !result.done {
        for url in &offer.https {
            match via_page(url, &msg.account_email).await {
                Ok((true, how)) => {
                    result.done = true;
                    result.method = how.into();
                    result.message = if how == "form" {
                        "Abgemeldet – das Formular auf der Abmeldeseite wurde automatisch abgeschickt.".into()
                    } else {
                        "Abgemeldet über die Abmeldeseite.".into()
                    };
                    result.open_url = Some(url.clone());
                    break;
                }
                Ok((false, _)) => {
                    result.open_url.get_or_insert_with(|| url.clone());
                }
                Err(e) => {
                    notes.push(e);
                    result.open_url.get_or_insert_with(|| url.clone());
                }
            }
        }
    }
    if !result.done {
        result.method = "manual".into();
        result.message = if notes.is_empty() {
            "Automatisch ging es nicht – bitte die Abmeldeseite selbst öffnen.".into()
        } else {
            format!("Automatisch ging es nicht ({}) – bitte die Abmeldeseite selbst öffnen.", notes.join("; "))
        };
        return Ok(result);
    }

    // Aufräumen: alle empfangenen Nachrichten des Absenders
    let action = match cleanup {
        UnsubscribeCleanup::None => None,
        UnsubscribeCleanup::Archive => Some(MessageAction::Archive),
        UnsubscribeCleanup::Trash => Some(MessageAction::Trash),
    };
    if let Some(action) = action.filter(|_| !msg.from_address.is_empty()) {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT m.id FROM messages m JOIN mail_accounts a ON a.id = m.account_id
             LEFT JOIN folders f ON f.id = m.folder_id
             WHERE a.user_id = ? AND lower(m.from_address) = lower(?) AND m.is_sent = 0
             AND COALESCE(f.role, 'other') NOT IN ('trash', 'junk', 'archive') AND m.on_server = 1",
        )
        .bind(user_id)
        .bind(&msg.from_address)
        .fetch_all(pool)
        .await?;
        if !ids.is_empty() {
            let r = actions::apply(pool, config, user_id, &api::BulkAction { ids, action: action.clone() }).await?;
            result.cleaned = r.updated;
            result.message.push_str(&format!(
                " {} bisherige Nachrichten {}.",
                r.updated,
                if action == MessageAction::Trash { "in den Papierkorb gelegt" } else { "archiviert" }
            ));
        }
    }
    // Künftige Nachrichten des Absenders landen ohnehin im Reiter Newsletter
    sqlx::query(
        "UPDATE messages SET category = 'newsletter', category_source = 'user' WHERE id = ? AND category = 'primary'",
    )
    .bind(message_id)
    .execute(pool)
    .await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_headers_and_mailto() {
        let o = offer(
            "<mailto:off@list.example?subject=Abmelden>, <https://list.example/u?id=1>",
            "List-Unsubscribe=One-Click",
        );
        assert!(o.one_click);
        assert_eq!(o.https, vec!["https://list.example/u?id=1"]);
        assert_eq!(
            parse_mailto(&o.mailto[0]),
            Some(("off@list.example".into(), "Abmelden".into(), "unsubscribe".into()))
        );
        assert_eq!(percent_decode("a%20b%C3%A4"), "a bä");
    }

    #[test]
    fn finds_links_and_forms() {
        let html = r#"<p><a href="https://shop.example/">Shop</a> <a href="https://n.example/x?u=1&amp;t=2">Newsletter abbestellen</a></p>"#;
        assert_eq!(link_in_html(html).as_deref(), Some("https://n.example/x?u=1&t=2"));
        let page = r#"<form action="/unsub" method="POST"><input type="hidden" name="t" value="abc">
            <input type="email" name="email"><input type="checkbox" name="all_lists" value="1">
            <button type="submit" name="go" value="1">Abmelden</button></form>"#;
        let f = &forms(page, "me@example.com")[0];
        assert_eq!(f.method, "post");
        assert!(f.fields.contains(&("t".into(), "abc".into())));
        assert!(f.fields.contains(&("email".into(), "me@example.com".into())));
        assert!(f.fields.contains(&("all_lists".into(), "1".into())));
        assert_eq!(f.submit, Some(("go".into(), "1".into())));
        assert!(is_unsubscribe_word(&f.text));
    }

    #[test]
    fn blocks_private_networks() {
        for u in ["http://127.0.0.1/x", "http://192.168.1.2/", "http://100.78.107.162/", "http://localhost/", "file:///etc/passwd"] {
            assert!(!host_allowed_literal(&reqwest::Url::parse(u).unwrap()), "{u}");
        }
        assert!(host_allowed_literal(&reqwest::Url::parse("https://list.example/u").unwrap()));
    }
}
