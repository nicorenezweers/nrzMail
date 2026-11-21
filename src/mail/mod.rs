//! Mail: Postfächer, Abgleich per IMAP, Senden per SMTP, Speichern und Suchen der
//! Nachrichten, Archiv (Import/Export, Aufbewahrung) und Statistik.
//!
//! Hintergrund ([`spawn`]): alle ~20 s fällige oder angeforderte Postfächer abgleichen
//! (höchstens `sync_workers` gleichzeitig), geplante Nachrichten senden, stündlich die
//! Aufbewahrungsfristen anwenden.

pub mod accounts;
pub mod actions;
pub mod archive;
pub mod folders;
pub mod html;
pub mod imap;
pub mod mbox;
pub mod messages;
pub mod model;
pub mod otp;
pub mod parse;
pub mod pdf;
pub mod retention;
pub mod search;
pub mod send;
pub mod smtp;
pub mod stats;
pub mod store;
pub mod unsubscribe;
pub mod sync;

use crate::config::Config;
use rocket::Shutdown;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};

/// Gemeinsamer Zustand des Abgleichs (von Rocket verwaltet, `State<SyncState>`):
/// welche Postfächer gerade abgeglichen werden und ein Weckruf für „jetzt abgleichen“.
#[derive(Clone, Default)]
pub struct SyncState {
    inner: Arc<SyncInner>,
}

#[derive(Default)]
struct SyncInner {
    running: Mutex<HashSet<String>>,
    requested: Mutex<HashSet<String>>,
    wake: Notify,
}

impl SyncState {
    /// Läuft gerade ein Abgleich für dieses Postfach?
    pub fn is_syncing(&self, account_id: &str) -> bool {
        self.inner
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(account_id)
    }

    /// Abgleich so bald wie möglich starten (z. B. nach „Jetzt abgleichen“ oder einem neuen Postfach).
    pub fn request(&self, account_id: &str) {
        self.inner
            .requested
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id.to_string());
        self.inner.wake.notify_one();
    }

    /// Angeforderte Postfächer abholen (und die Liste leeren).
    pub fn take_requested(&self) -> Vec<String> {
        self.inner
            .requested
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect()
    }

    /// Wartet, bis ein Abgleich angefordert wird.
    pub async fn woken(&self) {
        self.inner.wake.notified().await
    }

    /// Markiert ein Postfach als „wird abgeglichen“; `false`, wenn schon ein Abgleich läuft.
    pub fn start(&self, account_id: &str) -> bool {
        self.inner
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(account_id.to_string())
    }

    pub fn finish(&self, account_id: &str) {
        self.inner
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(account_id);
    }
}

/// Meldet das Postfach beim Verlassen ab – auch wenn der Abgleich abbricht.
struct Running(SyncState, String);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.finish(&self.1);
    }
}

/// Startet die Hintergrundarbeiten der Mail: Abgleich fälliger Postfächer, geplantes Senden,
/// Aufbewahrungsfristen. Alles endet mit `shutdown`.
pub fn spawn(pool: SqlitePool, config: Config, sync: SyncState, shutdown: Shutdown) {
    // Abgleich
    {
        let (pool, config, sync, mut stop) =
            (pool.clone(), config.clone(), sync.clone(), shutdown.clone());
        tokio::spawn(async move {
            let workers = Arc::new(Semaphore::new(config.sync_workers.max(1)));
            loop {
                let mut ids = sync.take_requested();
                match sync::due(&pool, &config).await {
                    Ok(due) => ids.extend(due),
                    Err(e) => tracing::warn!("Fällige Postfächer: {e}"),
                }
                ids.sort();
                ids.dedup();
                for id in ids {
                    if !sync.start(&id) {
                        continue;
                    }
                    let guard = Running(sync.clone(), id.clone());
                    let (pool, config, workers, stop) =
                        (pool.clone(), config.clone(), workers.clone(), stop.clone());
                    tokio::spawn(async move {
                        let _guard = guard;
                        let Ok(_permit) = workers.acquire_owned().await else {
                            return;
                        };
                        tokio::select! {
                            _ = sync::run(&pool, &config, &id) => {}
                            _ = stop => {}
                        }
                    });
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(20)) => {}
                    _ = sync.woken() => {}
                    _ = &mut stop => break,
                }
            }
        });
    }

    // Geplantes Senden
    {
        let (pool, config, mut stop) = (pool.clone(), config.clone(), shutdown.clone());
        tokio::spawn(async move {
            if let Err(e) = send::recover_stuck(&pool).await {
                tracing::warn!("Postausgang: {e}");
            }
            loop {
                match send::send_due(&pool, &config).await {
                    Ok(n) if n > 0 => tracing::info!("{n} geplante Nachrichten gesendet"),
                    Err(e) => tracing::warn!("Geplantes Senden: {e}"),
                    _ => {}
                }
                // Kurz, damit die Sendeverzögerung (ab einer Minute) pünktlich endet
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = &mut stop => break,
                }
            }
        });
    }

    // Aufbewahrung, stündlich (erstmals kurz nach dem Start)
    {
        let mut stop = shutdown;
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = &mut stop => return,
            }
            loop {
                if let Err(e) = retention::run_local(&pool, &config).await {
                    tracing::warn!("Aufbewahrung: {e}");
                }
                archive::clean_tmp(&config);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
                    _ = &mut stop => break,
                }
            }
        });
    }
}
