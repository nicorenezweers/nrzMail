//! KI: Anbieter (OpenAI-kompatibel und Anthropic), Einstellungen, Zusammenfassen,
//! Antworten entwerfen, Schreiben, Einteilen und der Chat-Assistent über die eigenen Mails.
//!
//! * [`provider`] – Katalog der Anbieter, [`settings`] – Einstellungen und wirksamer Zugang,
//!   [`client`] – Anfragen an die Anbieter, [`error`] – verständliche Fehler.
//! * [`summarize`], [`compose`] (Antworten, Schreiben), [`assistant`] (Chat), [`categorize`].
//! * [`store`] liest Nachrichten per SQL, immer nur aus den Postfächern des Benutzers;
//!   [`context`] und [`text`] bereiten sie für die KI auf.
//!
//! Mail-Inhalte gehen nur in markierten Blöcken an die KI, und der Systemprompt sagt jedes Mal,
//! dass Anweisungen darin nicht gelten (Schutz vor Prompt-Injection). Sie landen nie im Protokoll.

pub mod assistant;
pub mod categorize;
pub mod cli;
pub mod client;
pub mod compose;
pub mod context;
pub mod error;
pub mod provider;
pub mod query;
pub mod settings;
pub mod store;
pub mod summarize;
pub mod text;

pub use error::{AiError, AiResult};
pub use provider::BUILTIN_KEY;
pub use settings::AiConfig;

use crate::config::Config;
use rocket::Shutdown;
use sqlx::SqlitePool;
use std::time::Duration;

/// Abstand zwischen zwei Durchläufen der Einteilung im Hintergrund.
const INTERVAL: Duration = Duration::from_secs(120);
/// Längste Pause nach wiederholten Fehlern.
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// Wartezeit nach `failures` Fehlern in Folge: 2, 4, 8 … Minuten, höchstens eine Stunde.
fn backoff(failures: u32) -> Duration {
    INTERVAL
        .saturating_mul(2u32.saturating_pow(failures.min(10)))
        .min(MAX_BACKOFF)
}

/// Startet die Hintergrundarbeit der KI: alle ~2 Minuten neue Nachrichten im Posteingang
/// einteilen, wenn die KI eingeschaltet ist und „automatisch einteilen“ gilt. Bei Fehlern
/// (Anbieter weg, Rate-Limit) wird länger gewartet; protokolliert wird nur der erste Fehler
/// einer Serie und danach jeder zehnte. Endet mit `shutdown`.
pub fn spawn(pool: SqlitePool, config: Config, mut shutdown: Shutdown) {
    tokio::spawn(async move {
        let mut wait = Duration::from_secs(60);
        let mut failures = 0u32;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = &mut shutdown => break,
            }
            let run = tokio::select! {
                r = categorize::run_pending(&pool, &config) => r,
                _ = &mut shutdown => break,
            };
            match run {
                Ok(n) => {
                    if n > 0 {
                        tracing::debug!("KI: {n} neue Nachrichten eingeteilt");
                    }
                    if failures > 0 {
                        tracing::info!("KI-Einteilung läuft wieder");
                    }
                    failures = 0;
                    wait = INTERVAL;
                }
                Err(e) => {
                    failures += 1;
                    wait = backoff(failures);
                    if failures == 1 || failures % 10 == 0 {
                        tracing::warn!(
                            "KI-Einteilung fehlgeschlagen ({failures}×), nächster Versuch in {} min: {e}",
                            wait.as_secs() / 60
                        );
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_stops() {
        assert_eq!(backoff(1), Duration::from_secs(240));
        assert_eq!(backoff(2), Duration::from_secs(480));
        assert_eq!(backoff(50), MAX_BACKOFF);
    }
}
