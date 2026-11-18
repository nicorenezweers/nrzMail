//! Startet beim Hochfahren (Liftoff) die Hintergrundarbeiten: Abgleich der Postfächer und
//! geplantes Senden (`crate::mail::spawn`), KI-Einteilung neuer Nachrichten (`crate::ai::spawn`)
//! und stündlich das Aufräumen abgelaufener Sitzungen. Alle enden mit dem Server.

use crate::auth::session;
use crate::config::Config;
use crate::mail::SyncState;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::{Orbit, Rocket};
use sqlx::SqlitePool;
use std::time::Duration;

pub struct Background;

#[rocket::async_trait]
impl Fairing for Background {
    fn info(&self) -> Info {
        Info {
            name: "Hintergrundarbeiten",
            kind: Kind::Liftoff,
        }
    }

    async fn on_liftoff(&self, rocket: &Rocket<Orbit>) {
        let (Some(pool), Some(config), Some(sync)) = (
            rocket.state::<SqlitePool>().cloned(),
            rocket.state::<Config>().cloned(),
            rocket.state::<SyncState>().cloned(),
        ) else {
            return;
        };
        if !config.background {
            return;
        }

        // Postfächer abgleichen, geplante Nachrichten senden, Aufbewahrungsfristen
        crate::mail::spawn(pool.clone(), config.clone(), sync, rocket.shutdown());

        // KI: neue Nachrichten einteilen (wenn eingeschaltet)
        crate::ai::spawn(pool.clone(), config.clone(), rocket.shutdown());

        // Aufräumen: abgelaufene Sitzungen stündlich
        let mut shutdown = rocket.shutdown();
        tokio::spawn(async move {
            loop {
                match session::purge_expired(&pool).await {
                    Ok(n) if n > 0 => tracing::info!("{n} abgelaufene Sitzungen gelöscht"),
                    Err(e) => tracing::warn!("Sitzungen aufräumen: {e}"),
                    _ => {}
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
                    _ = &mut shutdown => break,
                }
            }
        });
    }
}
