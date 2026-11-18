//! Globaler Zustand: angemeldeter Benutzer, persönliche Einstellungen, Postfächer mit Ordnern
//! (für Seitenleiste und Zähler), KI-Status und die Zähler, mit denen Listen neu laden.

use crate::api;
use dioxus::prelude::*;
use nrzmail_api::{AiStatus, Folder, FolderRole, MailAccount, Preferences, User};

/// Angemeldeter Benutzer (None = noch nicht geladen / abgemeldet).
pub static ME: GlobalSignal<Option<User>> = Signal::global(|| None);

/// Persönliche Einstellungen der Oberfläche.
pub static PREFS: GlobalSignal<Preferences> = Signal::global(Preferences::default);

/// Postfächer des Benutzers.
pub static ACCOUNTS: GlobalSignal<Vec<MailAccount>> = Signal::global(Vec::new);

/// Ordner aller Postfächer (mit Zählern).
pub static FOLDERS: GlobalSignal<Vec<Folder>> = Signal::global(Vec::new);

/// Sind Postfächer und Ordner einmal geladen?
pub static ACCOUNTS_LOADED: GlobalSignal<bool> = Signal::global(|| false);

/// Ist KI nutzbar? (Name des Assistenten, Modell)
pub static AI: GlobalSignal<AiStatus> = Signal::global(AiStatus::default);

/// Erhöhen, damit die Nachrichtenliste im Hintergrund neu lädt (neue Mails, Abgleich).
pub static LIST_REFRESH: GlobalSignal<u64> = Signal::global(|| 0);

/// Oben links gewähltes Postfach ("" = alle Postfächer zusammen). Die Ansichten der
/// Seitenleiste (Posteingang, Ungelesen, …) zeigen dann nur dieses Postfach.
pub static CURRENT_ACCOUNT: GlobalSignal<String> =
    Signal::global(|| crate::util::storage_get("nrzmail.account").unwrap_or_default());

/// Gewähltes Postfach, falls es (noch) existiert; sonst "".
pub fn current_account() -> String {
    let id = CURRENT_ACCOUNT.read().clone();
    if id.is_empty() || !*ACCOUNTS_LOADED.read() || ACCOUNTS.read().iter().any(|a| a.id == id) {
        id
    } else {
        String::new()
    }
}

pub fn set_current_account(id: &str) {
    *CURRENT_ACCOUNT.write() = id.to_string();
    crate::util::storage_set("nrzmail.account", id);
}

/// Seitenleiste eingeklappt (Desktop).
pub static SIDEBAR_COLLAPSED: GlobalSignal<bool> =
    Signal::global(|| crate::util::storage_get("nrzmail.sidebar") == Some("collapsed".into()));

/// Seitenleiste als Blatt geöffnet (Handy).
pub static MOBILE_NAV: GlobalSignal<bool> = Signal::global(|| false);

/// Schmaler Bildschirm (Handy, unter 768 px) – folgt Drehen und Fenstergrösse.
pub static PHONE: GlobalSignal<bool> = Signal::global(|| {
    web_sys::window()
        .and_then(|w| w.match_media("(max-width: 767px)").ok().flatten())
        .is_some_and(|m| m.matches())
});

/// `PHONE` aktuell halten (einmal beim Start der App).
pub fn watch_phone() {
    spawn(async move {
        let mut e = document::eval(
            "const m = matchMedia('(max-width: 767px)'); dioxus.send(m.matches); \
             m.addEventListener('change', ev => dioxus.send(ev.matches)); await new Promise(() => {});",
        );
        while let Ok(v) = e.recv::<bool>().await {
            if *PHONE.peek() != v {
                *PHONE.write() = v;
            }
        }
    });
}

pub fn refresh_list() {
    *LIST_REFRESH.write() += 1;
}

/// Nach 401: zur Anmeldung (ausser auf den Anmeldeseiten selbst).
pub fn on_unauthorized() {
    let Some(window) = web_sys::window() else { return };
    let location = window.location();
    let path = location.pathname().unwrap_or_default();
    if path.starts_with("/login") || path.starts_with("/setup") || path.starts_with("/register") {
        return;
    }
    let search = location.search().unwrap_or_default();
    *ME.write() = None;
    let next = api::enc(&format!("{path}{search}"));
    let _ = location.assign(&format!("/login?next={next}"));
}

/// Benutzer laden; danach Einstellungen, Postfächer und KI-Status im Hintergrund.
pub async fn load_me() -> Option<User> {
    let me = api::get::<User>("/me").await.ok()?;
    *ME.write() = Some(me.clone());
    load_prefs().await;
    spawn(async {
        load_accounts().await;
    });
    spawn(async {
        load_ai().await;
    });
    Some(me)
}

pub async fn load_prefs() {
    if let Ok(p) = api::get::<Preferences>("/me/preferences").await {
        *PREFS.write() = p;
    }
}

/// Einstellungen speichern (sofort lokal übernehmen, bei Fehler zurück).
pub async fn save_prefs(p: Preferences) -> api::ApiResult<()> {
    let before = PREFS.peek().clone();
    *PREFS.write() = p.clone();
    match api::put::<Preferences, _>("/me/preferences", &p).await {
        Ok(saved) => {
            *PREFS.write() = saved;
            Ok(())
        }
        Err(e) => {
            *PREFS.write() = before;
            Err(e)
        }
    }
}

pub async fn load_ai() {
    if let Ok(s) = api::get::<AiStatus>("/ai/status").await {
        *AI.write() = s;
    }
}

/// Postfächer und ihre Ordner neu laden. Gibt `true` zurück, wenn sich Zähler geändert haben.
pub async fn load_accounts() -> bool {
    let Ok(accounts) = api::get::<Vec<MailAccount>>("/accounts").await else {
        return false;
    };
    let paths: Vec<String> = accounts.iter().map(|a| format!("/folders?account={}", api::enc(&a.id))).collect();
    let lists = futures_util::future::join_all(paths.iter().map(|p| api::get::<Vec<Folder>>(p))).await;
    let mut folders: Vec<Folder> = Vec::new();
    for (a, list) in accounts.iter().zip(lists) {
        match list {
            Ok(list) => folders.extend(list.into_iter().filter(|f| f.account_id == a.id)),
            // Fehler beim Laden der Ordner: die bisherigen behalten
            Err(_) => folders.extend(FOLDERS.peek().iter().filter(|f| f.account_id == a.id).cloned()),
        }
    }
    folders.sort_by(|a, b| (a.account_id.as_str(), a.path.to_lowercase()).cmp(&(b.account_id.as_str(), b.path.to_lowercase())));
    let fingerprint = |acc: &[MailAccount], f: &[Folder]| -> (i64, i64, i64) {
        (
            acc.iter().map(|a| a.message_count).sum(),
            f.iter().map(|f| f.unread + f.total).sum(),
            acc.iter().filter_map(|a| a.last_sync_at).max().unwrap_or(0),
        )
    };
    let changed = *ACCOUNTS_LOADED.peek()
        && fingerprint(&ACCOUNTS.peek(), &FOLDERS.peek()) != fingerprint(&accounts, &folders);
    if *ACCOUNTS.peek() != accounts {
        *ACCOUNTS.write() = accounts;
    }
    if *FOLDERS.peek() != folders {
        *FOLDERS.write() = folders;
    }
    if !*ACCOUNTS_LOADED.peek() {
        *ACCOUNTS_LOADED.write() = true;
    }
    changed
}

pub fn account(id: &str) -> Option<MailAccount> {
    ACCOUNTS.read().iter().find(|a| a.id == id).cloned()
}

/// Farbe eines Postfachs (leer = Grau).
pub fn account_color(id: &str) -> String {
    ACCOUNTS
        .read()
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.color.clone())
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "#a3a3a3".into())
}

/// Postfach, mit dem „Neue Nachricht“ startet: Vorgabe aus den Einstellungen, sonst das erste,
/// das senden kann.
pub fn default_account() -> Option<MailAccount> {
    let accounts = ACCOUNTS.read();
    let current = Some(current_account()).filter(|c| !c.is_empty());
    let pref = current.or_else(|| PREFS.read().default_account.clone());
    pref.and_then(|id| accounts.iter().find(|a| a.id == id && a.kind == nrzmail_api::AccountKind::Imap).cloned())
        .or_else(|| accounts.iter().find(|a| a.kind == nrzmail_api::AccountKind::Imap).cloned())
        .or_else(|| accounts.first().cloned())
}

/// Ordner eines Postfachs mit einer bestimmten Rolle.
pub fn folder_with_role(account_id: &str, role: FolderRole) -> Option<Folder> {
    FOLDERS.read().iter().find(|f| f.account_id == account_id && f.role == role).cloned()
}

/// Summe ungelesener Nachrichten in Ordnern einer Rolle (über alle Postfächer).
pub fn unread_in_role(role: FolderRole) -> i64 {
    FOLDERS.read().iter().filter(|f| f.role == role).map(|f| f.unread).sum()
}

/// Wie [`unread_in_role`], mit `account` ("" = alle) nur in einem Postfach.
pub fn unread_in_role_of(role: FolderRole, account: &str) -> i64 {
    FOLDERS.read().iter().filter(|f| f.role == role && (account.is_empty() || f.account_id == account)).map(|f| f.unread).sum()
}

pub fn total_in_role_of(role: FolderRole, account: &str) -> i64 {
    FOLDERS.read().iter().filter(|f| f.role == role && (account.is_empty() || f.account_id == account)).map(|f| f.total).sum()
}
