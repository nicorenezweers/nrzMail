//! Bremse gegen das Durchprobieren von Passwörtern. Fehlversuche werden je Schlüssel
//! (IP-Adresse und Benutzername) im Speicher gezählt; sind es in einem Zeitfenster zu viele,
//! wird jeder weitere Versuch bis zum Ende des Fensters abgewiesen – ohne das Passwort zu
//! prüfen. Eine erfolgreiche Anmeldung setzt den Zähler zurück, ein Neustart alle.
//!
//! Den Zustand legt beim Start das Fairing [`crate::fairings::SecurityHeaders`] an
//! (`State<LoginThrottle>`); Grenze und Fenster kommen aus der Konfiguration.

use crate::config::Config;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Höchstens so viele Schlüssel merken – schützt vor Fluten mit erfundenen Namen.
const MAX_ENTRIES: usize = 10_000;

/// Grenze und Zeitfenster.
#[derive(Debug, Clone, Copy)]
pub struct Limit {
    /// Erlaubte Fehlversuche je Fenster (0 = Bremse aus)
    pub max_failures: u32,
    pub window: Duration,
}

impl Limit {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_failures: config.login_max_failures,
            window: Duration::from_secs(u64::from(config.login_window_minutes.max(1)) * 60),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    /// Beginn des Zeitfensters (erster Fehlversuch)
    since: Instant,
}

#[derive(Default)]
pub struct LoginThrottle {
    entries: Mutex<HashMap<String, Entry>>,
}

impl LoginThrottle {
    /// Schlüssel aus IP-Adresse und Benutzername (Gross-/Kleinschreibung egal, wie bei der Anmeldung).
    pub fn key(ip: Option<IpAddr>, username: &str) -> String {
        let ip = ip.map_or_else(|| "-".to_string(), |ip| ip.to_string());
        let name: String = username.trim().to_lowercase().chars().take(64).collect();
        format!("{ip}|{name}")
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        // Ein Panik-Abbruch anderswo darf die Anmeldung nicht dauerhaft lahmlegen.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ist der Schlüssel gerade gesperrt? Dann die verbleibende Wartezeit.
    pub fn blocked(&self, key: &str, limit: Limit) -> Option<Duration> {
        if limit.max_failures == 0 {
            return None;
        }
        let mut entries = self.entries();
        let entry = *entries.get(key)?;
        let elapsed = entry.since.elapsed();
        if elapsed >= limit.window {
            entries.remove(key);
            return None;
        }
        (entry.failures >= limit.max_failures).then(|| limit.window - elapsed)
    }

    /// Fehlversuch zählen. `true`, wenn damit die Grenze gerade erreicht wurde.
    pub fn failure(&self, key: &str, limit: Limit) -> bool {
        if limit.max_failures == 0 {
            return false;
        }
        let mut entries = self.entries();
        let now = Instant::now();
        if !entries.contains_key(key) && entries.len() >= MAX_ENTRIES {
            entries.retain(|_, e| now.duration_since(e.since) < limit.window);
            if entries.len() >= MAX_ENTRIES {
                // Immer noch voll: den ältesten Eintrag opfern statt den neuen nicht zu zählen.
                if let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, e)| e.since)
                    .map(|(k, _)| k.clone())
                {
                    entries.remove(&oldest);
                }
            }
        }
        let entry = entries.entry(key.to_string()).or_insert(Entry {
            failures: 0,
            since: now,
        });
        if now.duration_since(entry.since) >= limit.window {
            *entry = Entry {
                failures: 0,
                since: now,
            };
        }
        entry.failures += 1;
        entry.failures == limit.max_failures
    }

    /// Nach erfolgreicher Anmeldung: Zähler löschen.
    pub fn reset(&self, key: &str) {
        self.entries().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(max: u32, secs: u64) -> Limit {
        Limit {
            max_failures: max,
            window: Duration::from_secs(secs),
        }
    }

    #[test]
    fn blocks_after_the_limit_and_resets() {
        let t = LoginThrottle::default();
        let k = LoginThrottle::key(None, " Alice ");
        assert_eq!(k, LoginThrottle::key(None, "alice"));
        assert!(!t.failure(&k, limit(3, 60)));
        assert!(!t.failure(&k, limit(3, 60)));
        assert!(t.blocked(&k, limit(3, 60)).is_none());
        assert!(t.failure(&k, limit(3, 60)));
        assert!(t.blocked(&k, limit(3, 60)).is_some());
        // Andere IP bzw. anderer Name sind nicht betroffen
        let other = LoginThrottle::key(Some([10, 0, 0, 1].into()), "alice");
        assert!(t.blocked(&other, limit(3, 60)).is_none());
        t.reset(&k);
        assert!(t.blocked(&k, limit(3, 60)).is_none());
    }

    #[test]
    fn window_expires_and_zero_disables() {
        let t = LoginThrottle::default();
        let k = LoginThrottle::key(None, "bob");
        for _ in 0..5 {
            t.failure(&k, limit(1, 0));
        }
        // Fenster von 0 Sekunden ist sofort abgelaufen
        assert!(t.blocked(&k, limit(1, 0)).is_none());
        for _ in 0..5 {
            assert!(!t.failure(&k, limit(0, 60)));
        }
        assert!(t.blocked(&k, limit(0, 60)).is_none());
    }

    #[test]
    fn number_of_entries_is_bounded() {
        let t = LoginThrottle::default();
        for i in 0..(MAX_ENTRIES + 50) {
            t.failure(&format!("-|u{i}"), limit(10, 600));
        }
        assert!(t.entries().len() <= MAX_ENTRIES);
    }
}
