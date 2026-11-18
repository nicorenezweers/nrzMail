//! Der lokale nrzMail-Server im selben Prozess – damit die App auch ohne Server-Instanz geht.
//! Umgebung vorbereiten (Datenordner, Port, Token der App), dann Rocket auf einem eigenen
//! Tokio-Thread starten. Die Einstellungen gehen über `NRZMAIL_*`-Umgebungsvariablen, die
//! `nrzmail::config::figment()` liest.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Bevorzugter Port der App – nicht der des Servers (8330), damit beide nebeneinander laufen.
const PREFERRED_PORT: u16 = 18330;

pub struct Server {
    pub port: u16,
    shutdown: Option<rocket::Shutdown>,
}

impl Server {
    /// Sauber beenden (laufende Anfragen abschliessen, Datenbank schliessen), höchstens `wait`.
    pub fn stop(&mut self, wait: Duration) {
        if let Some(s) = self.shutdown.take() {
            s.notify();
            let start = Instant::now();
            while start.elapsed() < wait && port_open(self.port) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Adresse, über die sich die App ohne Passwort am lokalen Server anmeldet.
pub fn login_url(port: u16) -> String {
    let token = std::env::var("NRZMAIL_DESKTOP_TOKEN").unwrap_or_default();
    format!("http://127.0.0.1:{port}/__desktop/login?token={token}")
}

/// `~/Library/Application Support/nrzMail`
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NRZMAIL_APP_DIR") {
        return PathBuf::from(dir);
    }
    home().join("Library/Application Support/nrzMail")
}

/// `~/Library/Logs/nrzMail`
pub fn log_dir() -> PathBuf {
    home().join("Library/Logs/nrzMail")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("Zufallszahlen");
    hex::encode(buf)
}

fn full_name() -> String {
    std::process::Command::new("/usr/bin/id")
        .arg("-F")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Umgebung setzen. Muss vor dem Start weiterer Threads laufen (`set_var`).
pub fn prepare_env() -> Result<u16, String> {
    let data = data_dir();
    let local = data.join("local");
    std::fs::create_dir_all(&local).map_err(|e| format!("{}: {e}", local.display()))?;

    // Rocket.toml des Projekts (Limits) im Datenordner ablegen und benutzen
    let config = data.join("Rocket.toml");
    std::fs::write(&config, include_str!("../../Rocket.toml")).map_err(|e| format!("{}: {e}", config.display()))?;

    let port = free_port(PREFERRED_PORT).ok_or("Kein freier Port für den lokalen Server gefunden")?;
    let set = |k: &str, v: &str| std::env::set_var(k, v);
    set("NRZMAIL_CONFIG", &config.to_string_lossy());
    set("NRZMAIL_PROFILE", "release");
    set("NRZMAIL_DATA_DIR", &local.to_string_lossy());
    set("NRZMAIL_ADDRESS", "127.0.0.1");
    set("NRZMAIL_PORT", &port.to_string());
    // Kein Ctrl-C/Signal-Handling durch Rocket – das Beenden steuert die App
    set("NRZMAIL_SHUTDOWN", "{ctrlc=false,signals=[],grace=3,mercy=2}");
    // Anmeldung ohne Passwort über ein Token, das nur diese App kennt
    set("NRZMAIL_DESKTOP_TOKEN", &random_hex(32));
    set("NRZMAIL_DESKTOP_USER", &std::env::var("USER").unwrap_or_default());
    set("NRZMAIL_DESKTOP_FULL_NAME", &full_name());
    Ok(port)
}

/// Startet den Server auf einem eigenen Thread und wartet, bis er antwortet.
pub fn start(port: u16) -> Result<Server, String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<rocket::Shutdown, String>>();
    std::thread::Builder::new()
        .name("nrzmail-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            };
            rt.block_on(async move {
                let rocket = match nrzmail::build().await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let rocket = match rocket.ignite().await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let _ = tx.send(Ok(rocket.shutdown()));
                if let Err(e) = rocket.launch().await {
                    tracing::error!("Lokaler Server beendet: {e}");
                }
            });
        })
        .map_err(|e| e.to_string())?;

    let shutdown = rx.recv_timeout(Duration::from_secs(60)).map_err(|_| "Lokaler Server startet nicht".to_string())??;
    let start = Instant::now();
    while !port_open(port) {
        if start.elapsed() > Duration::from_secs(30) {
            return Err(format!("Lokaler Server antwortet nicht auf Port {port}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(Server { port, shutdown: Some(shutdown) })
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200)).is_ok()
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).is_ok()
}

/// Bevorzugten Port nehmen, sonst die nächsten zwanzig probieren, sonst einen vom System.
fn free_port(preferred: u16) -> Option<u16> {
    (preferred..preferred.saturating_add(20))
        .find(|p| port_free(*p))
        .or_else(|| TcpListener::bind("127.0.0.1:0").ok().and_then(|l| l.local_addr().ok()).map(|a| a.port()))
}
