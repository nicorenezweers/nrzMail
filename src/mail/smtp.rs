//! SMTP (lettre): Verbindung mit TLS, STARTTLS oder unverschlüsselt, Anmeldung, Senden und
//! das Zusammenbauen einer Nachricht (Text, optional HTML, Anhänge, Antwort-Header).

use crate::api::{Address, Security};
use crate::config::Config;
use crate::mail::model::AccountRow;
use lettre::message::header::{self, ContentType};
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);

fn transport(
    account: &AccountRow,
    config: &Config,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, String> {
    let host = account.smtp_host.trim();
    if host.is_empty() {
        return Err("Für dieses Postfach ist kein SMTP-Server eingetragen.".into());
    }
    let port = u16::try_from(account.smtp_port).unwrap_or(465);
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        .port(port)
        .timeout(Some(TIMEOUT));
    let params = || {
        TlsParameters::builder(host.to_string())
            .dangerous_accept_invalid_certs(config.allow_invalid_certs)
            .dangerous_accept_invalid_hostnames(config.allow_invalid_certs)
            .build()
            .map_err(|e| format!("TLS: {e}"))
    };
    builder = match account.smtp_security() {
        Security::Tls => builder.tls(Tls::Wrapper(params()?)),
        Security::Starttls => builder.tls(Tls::Required(params()?)),
        Security::None => builder.tls(Tls::None),
    };
    let (user, password) = account.smtp_login();
    if !user.is_empty() {
        builder = builder.credentials(Credentials::new(user.to_string(), password.to_string()));
    }
    Ok(builder.build())
}

/// SMTP-Fehler verständlich machen.
fn smtp_error(account: &AccountRow, e: &lettre::transport::smtp::Error) -> String {
    let s = e.to_string();
    let lower = s.to_lowercase();
    let host = format!("{}:{}", account.smtp_host.trim(), account.smtp_port);
    if lower.contains("certificate") || lower.contains("unknownissuer") {
        return crate::mail::imap::tls_error(&s);
    }
    if e.is_timeout() || lower.contains("timed out") {
        return format!("Zeitüberschreitung: der SMTP-Server {host} antwortet nicht.");
    }
    if lower.contains("connection refused") {
        return format!("{host} lehnt die Verbindung ab – stimmt der Port?");
    }
    if lower.contains("lookup") || lower.contains("nodename") || lower.contains("not known") {
        return format!(
            "SMTP-Server „{}“ nicht gefunden – Adresse prüfen.",
            account.smtp_host.trim()
        );
    }
    if e.status()
        .is_some_and(|c| c.to_string().starts_with("535") || c.to_string().starts_with("534"))
        || lower.contains("authentication")
    {
        return format!("SMTP-Anmeldung abgelehnt – Benutzername und Passwort prüfen (bei Gmail, iCloud und Outlook ist ein App-Passwort nötig). ({s})");
    }
    if e.is_tls() {
        return crate::mail::imap::tls_error(&s);
    }
    if e.is_permanent() {
        return format!("Der SMTP-Server hat die Nachricht abgelehnt: {s}");
    }
    format!("SMTP ({host}): {s}")
}

/// Verbinden und anmelden (Verbindungstest).
pub async fn test(account: &AccountRow, config: &Config) -> Result<(), String> {
    let t = transport(account, config)?;
    match tokio::time::timeout(Duration::from_secs(20), t.test_connection()).await {
        Err(_) => Err(format!(
            "Zeitüberschreitung: der SMTP-Server {}:{} antwortet nicht.",
            account.smtp_host.trim(),
            account.smtp_port
        )),
        Ok(Err(e)) => Err(smtp_error(account, &e)),
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err("Der SMTP-Server hat die Verbindung beendet.".into()),
    }
}

/// Eine zu sendende Nachricht.
pub struct Outgoing {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    /// `<id@domain>`
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    /// (Dateiname, Content-Type, Inhalt)
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

/// Fertige Nachricht: Rohdaten mit Bcc (für die eigene Kopie) und ohne (zum Senden).
pub struct Built {
    pub raw_with_bcc: Vec<u8>,
    pub raw: Vec<u8>,
    pub envelope: lettre::address::Envelope,
}

fn mailbox(a: &Address) -> Result<Mailbox, String> {
    let address = a
        .address
        .trim()
        .parse()
        .map_err(|_| format!("Ungültige Adresse: {}", a.address.trim()))?;
    let name = a.name.trim();
    Ok(Mailbox::new(
        (!name.is_empty()).then(|| name.to_string()),
        address,
    ))
}

/// Nachricht zusammenbauen (multipart/alternative bei HTML, multipart/mixed bei Anhängen).
pub fn build(o: &Outgoing) -> Result<Built, String> {
    let mut b = Message::builder()
        .from(mailbox(&o.from)?)
        .subject(o.subject.clone())
        .message_id(Some(o.message_id.clone()))
        .date_now()
        .user_agent(format!("nrzMail {}", env!("CARGO_PKG_VERSION")))
        .keep_bcc();
    for a in &o.to {
        b = b.to(mailbox(a)?);
    }
    for a in &o.cc {
        b = b.cc(mailbox(a)?);
    }
    for a in &o.bcc {
        b = b.bcc(mailbox(a)?);
    }
    if let Some(r) = &o.in_reply_to {
        b = b.in_reply_to(r.clone());
    }
    if let Some(r) = &o.references {
        b = b.references(r.clone());
    }
    let text = SinglePart::builder()
        .header(ContentType::TEXT_PLAIN)
        .body(o.text.clone());
    let body = match &o.html {
        Some(html) => MultiPart::alternative().singlepart(text).singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone()),
        ),
        None => MultiPart::mixed().singlepart(text),
    };
    let message = if o.attachments.is_empty() {
        match &o.html {
            Some(_) => b.multipart(body),
            None => b.singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(o.text.clone()),
            ),
        }
    } else {
        let mut mixed = match &o.html {
            Some(_) => MultiPart::mixed().multipart(body),
            None => body,
        };
        for (name, ctype, data) in &o.attachments {
            let ct = ContentType::parse(ctype).unwrap_or_else(|_| {
                ContentType::parse("application/octet-stream").expect("gültig")
            });
            mixed = mixed.singlepart(Attachment::new(name.clone()).body(data.clone(), ct));
        }
        b.multipart(mixed)
    }
    .map_err(|e| format!("Nachricht ungültig: {e}"))?;
    let raw_with_bcc = message.formatted();
    let envelope = message.envelope().clone();
    let mut message = message;
    message.headers_mut().remove::<header::Bcc>();
    Ok(Built {
        raw_with_bcc,
        raw: message.formatted(),
        envelope,
    })
}

/// Senden.
pub async fn send(account: &AccountRow, config: &Config, built: &Built) -> Result<(), String> {
    let t = transport(account, config)?;
    match tokio::time::timeout(
        Duration::from_secs(120),
        t.send_raw(&built.envelope, &built.raw),
    )
    .await
    {
        Err(_) => Err("Zeitüberschreitung beim Senden.".into()),
        Ok(Err(e)) => Err(smtp_error(account, &e)),
        Ok(Ok(_)) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outgoing() -> Outgoing {
        Outgoing {
            from: Address::new("Anna Müller", "anna@example.ch"),
            to: vec![Address::new("Bruno", "bruno@example.com")],
            cc: vec![],
            bcc: vec![Address::new("", "geheim@example.com")],
            subject: "Grüsse".into(),
            text: "Hallo Bruno".into(),
            html: Some("<p>Hallo <b>Bruno</b></p>".into()),
            message_id: "<x1@example.ch>".into(),
            in_reply_to: Some("<orig@example.com>".into()),
            references: Some("<root@example.com> <orig@example.com>".into()),
            attachments: vec![(
                "Plan.pdf".into(),
                "application/pdf".into(),
                b"%PDF-1.4".to_vec(),
            )],
        }
    }

    #[test]
    fn builds_mime_structure() {
        let b = build(&outgoing()).unwrap();
        let with = String::from_utf8_lossy(&b.raw_with_bcc);
        let without = String::from_utf8_lossy(&b.raw);
        assert!(with.contains("Bcc: geheim@example.com"), "{with}");
        assert!(!without.contains("geheim@example.com"), "{without}");
        assert!(without.contains("multipart/mixed") && without.contains("multipart/alternative"));
        assert!(without.contains("Message-ID: <x1@example.ch>"));
        assert!(without.contains("In-Reply-To: <orig@example.com>"));
        assert!(without.contains("References: <root@example.com> <orig@example.com>"));
        assert!(
            b.envelope.to().len() == 2,
            "Bcc bekommt die Nachricht trotzdem"
        );
        let p = crate::mail::parse::parse(&b.raw_with_bcc);
        assert_eq!(p.subject, "Grüsse");
        assert_eq!(p.from.name, "Anna Müller");
        assert_eq!(p.bcc.len(), 1);
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].filename, "Plan.pdf");
        assert!(p.html.contains("<b>Bruno</b>"));
        assert_eq!(p.in_reply_to, "orig@example.com");
    }

    #[test]
    fn rejects_bad_addresses() {
        let mut o = outgoing();
        o.to = vec![Address::new("", "kaputt")];
        assert!(build(&o).err().unwrap().contains("kaputt"));
        let mut o = outgoing();
        o.html = None;
        o.attachments.clear();
        let b = build(&o).unwrap();
        assert!(String::from_utf8_lossy(&b.raw).contains("text/plain"));
    }
}
