//! IMAP-Verbindung (async-imap über tokio, TLS mit rustls): verbinden mit TLS, STARTTLS
//! oder unverschlüsselt, anmelden, Ordner auflisten und die Befehle, die Abgleich und
//! Aktionen brauchen. Fehler kommen als verständliche deutsche Meldung.

use crate::api::{FolderRole, Security};
use crate::config::Config;
use crate::mail::model::AccountRow;
use async_imap::types::{Flag, NameAttribute};
use futures::StreamExt;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::rustls;

/// Zeitlimit für Verbindungsaufbau, TLS und Anmeldung.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Zeitlimit für einen einzelnen Befehl (grosse Abrufe brauchen länger).
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(180);

/// Ein Datenstrom für async-imap – TLS oder unverschlüsselt.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug> Io for T {}

pub type Session = async_imap::Session<Box<dyn Io>>;

/// Zugangsdaten für eine Verbindung.
#[derive(Debug, Clone)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub password: String,
    pub allow_invalid_certs: bool,
}

impl Server {
    pub fn for_account(account: &AccountRow, config: &Config) -> Self {
        Self {
            host: account.imap_host.trim().to_string(),
            port: u16::try_from(account.imap_port).unwrap_or(993),
            security: account.imap_security(),
            username: if account.imap_username.trim().is_empty() {
                account.email.trim().to_string()
            } else {
                account.imap_username.trim().to_string()
            },
            password: account.imap_password.clone(),
            allow_invalid_certs: config.allow_invalid_certs,
        }
    }
}

/// Nimmt jedes Zertifikat an (nur mit `allow_invalid_certs`, für eigene Server im LAN).
#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(allow_invalid: bool) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("TLS-Versionen");
    let config = if allow_invalid {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    Arc::new(config)
}

static TLS_STRICT: LazyLock<tokio_rustls::TlsConnector> =
    LazyLock::new(|| tokio_rustls::TlsConnector::from(tls_config(false)));
static TLS_ANY: LazyLock<tokio_rustls::TlsConnector> =
    LazyLock::new(|| tokio_rustls::TlsConnector::from(tls_config(true)));

/// TLS-Fehler verständlich machen.
pub fn tls_error(e: &dyn std::fmt::Display) -> String {
    let s = e.to_string();
    let lower = s.to_lowercase();
    if lower.contains("certificate")
        || lower.contains("unknownissuer")
        || lower.contains("notvalidforname")
        || lower.contains("expired")
    {
        format!(
            "Das Zertifikat des Servers ist ungültig ({s}). Bei einem eigenen Server im lokalen Netz lässt sich das mit NRZMAIL_ALLOW_INVALID_CERTS=true erlauben."
        )
    } else {
        format!("TLS-Verbindung fehlgeschlagen ({s}) – passen Port und Verschlüsselung? (993 = TLS, 143 = STARTTLS)")
    }
}

/// Verbindungsfehler (TCP) verständlich machen.
pub fn connect_error(host: &str, port: u16, e: &std::io::Error) -> String {
    let s = e.to_string();
    let lower = s.to_lowercase();
    if e.kind() == std::io::ErrorKind::ConnectionRefused {
        format!("{host}:{port} lehnt die Verbindung ab – stimmt der Port?")
    } else if lower.contains("lookup")
        || lower.contains("nodename")
        || lower.contains("name or service")
        || lower.contains("no such host")
        || lower.contains("not known")
    {
        format!("Server „{host}“ nicht gefunden – Adresse prüfen.")
    } else {
        format!("Keine Verbindung zu {host}:{port}: {s}")
    }
}

/// Anmeldefehler verständlich machen.
fn login_error(e: &async_imap::error::Error) -> String {
    let s = e.to_string();
    format!("Anmeldung abgelehnt – Benutzername und Passwort prüfen (bei Gmail, iCloud und Outlook ist ein App-Passwort nötig). Server: {}", s.trim_start_matches("no response: ").trim_start_matches("bad response: "))
}

/// TCP-Verbindung mit Zeitlimit.
pub async fn tcp(host: &str, port: u16) -> Result<tokio::net::TcpStream, String> {
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    {
        Err(_) => Err(format!(
            "Zeitüberschreitung: {host}:{port} antwortet nicht (Firewall oder falscher Port?)."
        )),
        Ok(Err(e)) => Err(connect_error(host, port, &e)),
        Ok(Ok(s)) => Ok(s),
    }
}

async fn greeting<T: Io>(client: &mut async_imap::Client<T>) -> Result<(), String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, client.read_response()).await {
        Ok(Some(Ok(_))) => Ok(()),
        Ok(Some(Err(e))) => Err(format!("Der Server antwortet unverständlich: {e}")),
        Ok(None) => Err("Der Server hat die Verbindung geschlossen.".into()),
        Err(_) => {
            Err("Der Server meldet sich nicht – falscher Port oder falsche Verschlüsselung?".into())
        }
    }
}

async fn tls_wrap(server: &Server, tcp: tokio::net::TcpStream) -> Result<Box<dyn Io>, String> {
    let name = rustls::pki_types::ServerName::try_from(server.host.clone())
        .map_err(|_| format!("Ungültiger Servername „{}“.", server.host))?;
    let connector = if server.allow_invalid_certs {
        &*TLS_ANY
    } else {
        &*TLS_STRICT
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name, tcp)).await {
        Err(_) => Err(
            "Zeitüberschreitung beim TLS-Handshake – passt die Verschlüsselung zum Port?".into(),
        ),
        Ok(Err(e)) => Err(tls_error(&e)),
        Ok(Ok(s)) => Ok(Box::new(s)),
    }
}

/// Verbinden und anmelden.
pub async fn connect(server: &Server) -> Result<Session, String> {
    if server.host.is_empty() {
        return Err("Kein IMAP-Server eingetragen.".into());
    }
    let tcp = tcp(&server.host, server.port).await?;
    let client = match server.security {
        Security::Tls => {
            let mut c = async_imap::Client::new(tls_wrap(server, tcp).await?);
            greeting(&mut c).await?;
            c
        }
        Security::Starttls => {
            let mut c = async_imap::Client::new(tcp);
            greeting(&mut c).await?;
            match tokio::time::timeout(
                CONNECT_TIMEOUT,
                c.run_command_and_check_ok("STARTTLS", None),
            )
            .await
            {
                Err(_) => return Err("Zeitüberschreitung bei STARTTLS.".into()),
                Ok(Err(e)) => return Err(format!("Der Server kann kein STARTTLS ({e}).")),
                Ok(Ok(())) => {}
            }
            async_imap::Client::new(tls_wrap(server, c.into_inner()).await?)
        }
        Security::None => {
            let mut c = async_imap::Client::new(Box::new(tcp) as Box<dyn Io>);
            greeting(&mut c).await?;
            c
        }
    };
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        client.login(&server.username, &server.password),
    )
    .await
    {
        Err(_) => Err("Zeitüberschreitung bei der Anmeldung.".into()),
        Ok(Err((e, _))) => Err(login_error(&e)),
        Ok(Ok(s)) => Ok(s),
    }
}

/// Mit Zeitlimit ausführen; Fehler als Text.
pub async fn timed<T, E: std::fmt::Display>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(COMMAND_TIMEOUT, fut).await {
        Err(_) => Err(format!("{what}: Zeitüberschreitung.")),
        Ok(Err(e)) => Err(format!("{what}: {e}")),
        Ok(Ok(v)) => Ok(v),
    }
}

/// Ordner auf dem Server.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub path: String,
    pub delimiter: String,
    /// Rolle aus SPECIAL-USE (`\Sent`, `\Trash`, …)
    pub special: Option<FolderRole>,
    /// `\All`, `\Flagged`, `\Important`: Sammelordner, nicht doppelt abgleichen
    pub virtual_folder: bool,
    pub selectable: bool,
}

pub async fn list(session: &mut Session) -> Result<Vec<Listed>, String> {
    let names: Vec<_> = {
        let stream = timed("Ordner auflisten", session.list(Some(""), Some("*"))).await?;
        let collected: Vec<_> = tokio::time::timeout(COMMAND_TIMEOUT, stream.collect::<Vec<_>>())
            .await
            .map_err(|_| "Ordner auflisten: Zeitüberschreitung.".to_string())?;
        collected
    };
    let mut out = Vec::new();
    for n in names {
        let n = n.map_err(|e| format!("Ordner auflisten: {e}"))?;
        let mut special = None;
        let mut virtual_folder = false;
        let mut selectable = true;
        for a in n.attributes() {
            match a {
                NameAttribute::Sent => special = Some(FolderRole::Sent),
                NameAttribute::Trash => special = Some(FolderRole::Trash),
                NameAttribute::Junk => special = Some(FolderRole::Junk),
                NameAttribute::Drafts => special = Some(FolderRole::Drafts),
                NameAttribute::Archive => special = Some(FolderRole::Archive),
                NameAttribute::All | NameAttribute::Flagged => virtual_folder = true,
                NameAttribute::NoSelect => selectable = false,
                NameAttribute::Extension(e) => {
                    let e = e.to_ascii_lowercase();
                    if e == "\\important" {
                        virtual_folder = true;
                    } else if e == "\\nonexistent" {
                        selectable = false;
                    }
                }
                _ => {}
            }
        }
        out.push(Listed {
            path: n.name().to_string(),
            delimiter: n.delimiter().unwrap_or("").to_string(),
            special,
            virtual_folder,
            selectable,
        });
    }
    Ok(out)
}

/// Fähigkeiten des Servers, die nrzMail nutzt.
#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub r#move: bool,
    pub uidplus: bool,
}

pub async fn caps(session: &mut Session) -> Caps {
    match timed("CAPABILITY", session.capabilities()).await {
        Ok(c) => Caps {
            r#move: c.has_str("MOVE"),
            uidplus: c.has_str("UIDPLUS"),
        },
        Err(_) => Caps::default(),
    }
}

/// UIDs als kompakte Menge: `1:5,7,9:12` (Eingabe beliebig, Duplikate egal).
pub fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}:{end}")
        });
        i += 1;
    }
    parts.join(",")
}

/// Ordnername als IMAP-String (in Anführungszeichen, `\` und `"` maskiert).
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Flags aus einer FETCH-Antwort.
pub fn flags_of(fetch: &async_imap::types::Fetch) -> crate::mail::store::Flags {
    let mut f = crate::mail::store::Flags::default();
    for flag in fetch.flags() {
        match flag {
            Flag::Seen => f.seen = true,
            Flag::Flagged => f.flagged = true,
            Flag::Answered => f.answered = true,
            Flag::Draft => f.draft = true,
            _ => {}
        }
    }
    f
}

/// Ordner auswählen.
pub async fn select(
    session: &mut Session,
    path: &str,
) -> Result<async_imap::types::Mailbox, String> {
    timed(
        &format!("Ordner „{}“", crate::mail::folders::decode_mutf7(path)),
        session.select(path),
    )
    .await
}

/// Flags setzen oder entfernen (`+FLAGS.SILENT (\Seen)` usw.) – im ausgewählten Ordner.
pub async fn store(session: &mut Session, uids: &[u32], query: &str) -> Result<(), String> {
    for chunk in uids.chunks(500) {
        let stream = timed("Flags setzen", session.uid_store(uid_set(chunk), query)).await?;
        let results: Vec<_> = tokio::time::timeout(COMMAND_TIMEOUT, stream.collect::<Vec<_>>())
            .await
            .map_err(|_| "Flags setzen: Zeitüberschreitung.".to_string())?;
        for r in results {
            r.map_err(|e| format!("Flags setzen: {e}"))?;
        }
    }
    Ok(())
}

/// Endgültig löschen (`\Deleted` + UID EXPUNGE bzw. EXPUNGE) – im ausgewählten Ordner.
pub async fn expunge(session: &mut Session, caps: Caps, uids: &[u32]) -> Result<(), String> {
    store(session, uids, "+FLAGS.SILENT (\\Deleted)").await?;
    if caps.uidplus {
        for chunk in uids.chunks(500) {
            let stream = timed("Löschen", session.uid_expunge(uid_set(chunk))).await?;
            let results: Vec<_> = tokio::time::timeout(COMMAND_TIMEOUT, stream.collect::<Vec<_>>())
                .await
                .map_err(|_| "Löschen: Zeitüberschreitung.".to_string())?;
            for r in results {
                r.map_err(|e| format!("Löschen: {e}"))?;
            }
        }
    } else {
        let stream = timed("Löschen", session.expunge()).await?;
        let results: Vec<_> = tokio::time::timeout(COMMAND_TIMEOUT, stream.collect::<Vec<_>>())
            .await
            .map_err(|_| "Löschen: Zeitüberschreitung.".to_string())?;
        for r in results {
            r.map_err(|e| format!("Löschen: {e}"))?;
        }
    }
    Ok(())
}

/// Verschieben (UID MOVE, sonst COPY + `\Deleted` + EXPUNGE) – aus dem ausgewählten Ordner.
pub async fn move_to(
    session: &mut Session,
    caps: Caps,
    uids: &[u32],
    target: &str,
) -> Result<(), String> {
    for chunk in uids.chunks(500) {
        let set = uid_set(chunk);
        if caps.r#move {
            timed("Verschieben", session.uid_mv(&set, target)).await?;
        } else {
            timed("Kopieren", session.uid_copy(&set, quote(target))).await?;
            expunge(session, caps, chunk).await?;
        }
    }
    Ok(())
}

/// Nachricht in einen Ordner legen (APPEND), z. B. die Kopie im Gesendet-Ordner.
pub async fn append(
    session: &mut Session,
    path: &str,
    flags: &str,
    raw: &[u8],
) -> Result<(), String> {
    let flags = (!flags.is_empty()).then(|| format!("({flags})"));
    timed(
        "Hochladen",
        session.append(path, flags.as_deref(), None, raw),
    )
    .await
}

/// Ordner anlegen; „gibt es schon“ ist kein Fehler.
pub async fn create(session: &mut Session, path: &str) -> Result<(), String> {
    match timed("Ordner anlegen", session.create(path)).await {
        Ok(()) => Ok(()),
        Err(e) if e.to_lowercase().contains("exist") => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn logout(mut session: Session) {
    let _ = tokio::time::timeout(Duration::from_secs(5), session.logout()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_uid_sets() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 7, 9, 10, 11, 3]), "1:3,5,7,9:11");
        assert_eq!(uid_set(&[42]), "42");
        assert_eq!(uid_set(&[]), "");
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn readable_connection_errors() {
        let e = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert!(connect_error("h", 1, &e).contains("lehnt die Verbindung ab"));
        assert!(tls_error(&"invalid peer certificate: UnknownIssuer").contains("Zertifikat"));
    }
}
