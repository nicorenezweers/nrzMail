//! Benutzerkonten: anlegen (auch das erste Konto bei der Einrichtung), anmelden, Profil,
//! Passwort und Einstellungen der Oberfläche, Verwaltung durch Administratoren.
//!
//! Regeln der Verwaltung: Niemand löscht oder sperrt sich selbst oder nimmt sich das
//! Admin-Recht, und der letzte aktive Administrator bleibt. Die letzte Regel steckt direkt in
//! den SQL-Anweisungen, damit sie auch bei gleichzeitigen Änderungen hält.

use crate::api;
use crate::auth::{password, session};
use crate::config::Config;
use crate::db::models::{new_id, now, User};
use crate::error::{AppError, AppResult};
use crate::services::settings;
use sqlx::SqlitePool;

/// SQL-Bedingung „es gibt einen anderen aktiven Administrator als die Zeile `users.id`“
/// (als Makro, damit `concat!` daraus feste Anweisungen baut).
macro_rules! other_active_admin {
    () => {
        "EXISTS (SELECT 1 FROM users o WHERE o.id != users.id AND o.is_admin = 1 AND o.disabled = 0)"
    };
}

macro_rules! insert_user {
    () => {
        "INSERT INTO users (id, username, display_name, email, password_hash, is_admin, created_at, updated_at)
         SELECT ?, ?, ?, ?, ?, ?, ?, ?"
    };
}

pub async fn by_id(pool: &SqlitePool, id: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn by_username(pool: &SqlitePool, username: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as("SELECT * FROM users WHERE username = ?")
        .bind(username.trim())
        .fetch_optional(pool)
        .await?)
}

pub async fn count(pool: &SqlitePool) -> AppResult<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await?)
}

/// Alle Benutzer, älteste zuerst.
pub async fn list(pool: &SqlitePool) -> AppResult<Vec<User>> {
    Ok(
        sqlx::query_as("SELECT * FROM users ORDER BY created_at, username")
            .fetch_all(pool)
            .await?,
    )
}

/// Dürfen sich neue Benutzer selbst registrieren? Die Einstellung der Verwaltung hat Vorrang,
/// sonst gilt `NRZMAIL_REGISTRATION_OPEN`.
pub async fn registration_open(pool: &SqlitePool, config: &Config) -> AppResult<bool> {
    Ok(settings::get::<bool>(pool, settings::REGISTRATION_OPEN)
        .await?
        .unwrap_or(config.registration_open))
}

// ------------------------------------------------------------------ Prüfungen

/// Benutzername: 2–64 Zeichen, Buchstaben, Ziffern und `.-_@`.
pub fn check_username(username: &str) -> AppResult<()> {
    let ok = (2..=64).contains(&username.chars().count())
        && username
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'));
    if ok {
        Ok(())
    } else {
        Err(AppError::bad(
            "Benutzername: 2–64 Zeichen, nur Buchstaben, Ziffern und . - _ @",
        ))
    }
}

/// Anzeigename: höchstens 100 Zeichen, keine Steuerzeichen.
pub fn clean_display_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(AppError::bad("Anzeigename: höchstens 100 Zeichen."));
    }
    Ok(name.to_string())
}

/// E-Mail-Adresse: leer oder `name@domain`, ohne Leerzeichen, höchstens 254 Zeichen.
pub fn clean_email(email: &str) -> AppResult<String> {
    let email = email.trim();
    if email.is_empty() {
        return Ok(String::new());
    }
    let ok = email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.contains('@')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        });
    if ok {
        Ok(email.to_string())
    } else {
        Err(AppError::bad("Diese E-Mail-Adresse ist ungültig."))
    }
}

/// Sprache der Oberfläche: Kürzel wie `de`, `en` oder `de-CH`.
pub fn clean_language(language: &str) -> AppResult<String> {
    let language = language.trim();
    let (base, region) = match language.split_once('-') {
        Some((b, r)) => (b, Some(r)),
        None => (language, None),
    };
    let ok = (2..=3).contains(&base.len())
        && base.chars().all(|c| c.is_ascii_lowercase())
        && region.is_none_or(|r| {
            (2..=3).contains(&r.len()) && r.chars().all(|c| c.is_ascii_alphanumeric())
        });
    if ok {
        Ok(language.to_string())
    } else {
        Err(AppError::bad("Diese Sprache gibt es nicht."))
    }
}

// ------------------------------------------------------------------ Anlegen & Anmelden

/// Daten für ein neues Konto.
pub struct NewAccount<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub display_name: &'a str,
    pub email: &'a str,
    pub is_admin: bool,
}

/// Neues Konto anlegen (prüft alle Angaben).
pub async fn create(pool: &SqlitePool, account: &NewAccount<'_>) -> AppResult<User> {
    insert(pool, account, false)
        .await?
        .ok_or(AppError::NotFound)
}

/// Das erste Konto bei der Einrichtung – immer Administrator. Nur solange es noch gar keinen
/// Benutzer gibt; die Bedingung steht in der INSERT-Anweisung, zwei gleichzeitige
/// Einrichtungen ergeben also nie zwei Konten.
pub async fn create_first(pool: &SqlitePool, account: &NewAccount<'_>) -> AppResult<User> {
    let account = NewAccount {
        is_admin: true,
        ..*account
    };
    insert(pool, &account, true)
        .await?
        .ok_or_else(|| AppError::Conflict("nrzMail ist schon eingerichtet.".into()))
}

async fn insert(
    pool: &SqlitePool,
    account: &NewAccount<'_>,
    only_if_empty: bool,
) -> AppResult<Option<User>> {
    let username = account.username.trim();
    check_username(username)?;
    password::check_strength(account.password)?;
    let display_name = clean_display_name(account.display_name)?;
    let email = clean_email(account.email)?;
    let hash = password::hash(account.password.to_string()).await?;
    let id = new_id();
    let ts = now();
    let sql = if only_if_empty {
        concat!(insert_user!(), " WHERE NOT EXISTS (SELECT 1 FROM users)")
    } else {
        insert_user!()
    };
    let inserted = sqlx::query(sql)
        .bind(&id)
        .bind(username)
        .bind(display_name)
        .bind(email)
        .bind(hash)
        .bind(account.is_admin)
        .bind(ts)
        .bind(ts)
        .execute(pool)
        .await
        .map_err(|e| AppError::unique(e, "Diesen Benutzernamen gibt es schon."))?
        .rows_affected();
    if inserted == 0 {
        return Ok(None);
    }
    by_id(pool, &id).await
}

/// Anmeldedaten prüfen. Auch bei unbekanntem Namen wird ein Hash geprüft, damit die
/// Antwortzeit nicht verrät, ob es das Konto gibt; gesperrte Konten gelten als falsch.
/// Ergebnis: das Konto (falls es den Namen gibt) und ob die Anmeldung gelingt.
pub async fn authenticate(
    pool: &SqlitePool,
    username: &str,
    password_plain: &str,
) -> AppResult<(Option<User>, bool)> {
    let user = by_username(pool, username).await?;
    let hash = user
        .as_ref()
        .map(|u| u.password_hash.clone())
        .unwrap_or_else(|| password::DUMMY_HASH.clone());
    let ok = password::verify(password_plain.to_string(), hash).await;
    let ok = ok && user.as_ref().is_some_and(|u| !u.disabled);
    Ok((user, ok))
}

/// Zeitpunkt der letzten Anmeldung festhalten.
pub async fn touch_login(pool: &SqlitePool, id: &str) -> AppResult<User> {
    sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
        .bind(now())
        .bind(id)
        .execute(pool)
        .await?;
    by_id(pool, id).await?.ok_or(AppError::NotFound)
}

// ------------------------------------------------------------------ Eigenes Konto

/// Anzeigename, E-Mail und Sprache ändern (leere Sprache = unverändert).
pub async fn update_profile(
    pool: &SqlitePool,
    user: &User,
    profile: &api::UpdateProfile,
) -> AppResult<User> {
    let display_name = clean_display_name(&profile.display_name)?;
    let email = clean_email(&profile.email)?;
    let language = if profile.language.trim().is_empty() {
        user.language.clone()
    } else {
        clean_language(&profile.language)?
    };
    sqlx::query(
        "UPDATE users SET display_name = ?, email = ?, language = ?, updated_at = ? WHERE id = ?",
    )
    .bind(display_name)
    .bind(email)
    .bind(language)
    .bind(now())
    .bind(&user.id)
    .execute(pool)
    .await?;
    by_id(pool, &user.id).await?.ok_or(AppError::NotFound)
}

/// Eigenes Passwort ändern. `Ok(false)`, wenn das aktuelle nicht stimmt. Danach enden alle
/// anderen Sitzungen; `keep` (die laufende) bleibt.
pub async fn change_password(
    pool: &SqlitePool,
    user: &User,
    current: &str,
    new: &str,
    keep: Option<&str>,
) -> AppResult<bool> {
    if !password::verify(current.to_string(), user.password_hash.clone()).await {
        return Ok(false);
    }
    password::check_strength(new)?;
    if new == current {
        return Err(AppError::bad(
            "Das neue Passwort muss sich vom bisherigen unterscheiden.",
        ));
    }
    let hash = password::hash(new.to_string()).await?;
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(hash)
        .bind(now())
        .bind(&user.id)
        .execute(pool)
        .await?;
    session::destroy_others(pool, &user.id, keep).await?;
    Ok(true)
}

/// Einstellungen der Oberfläche speichern. Ein Standard-Postfach, das es (nicht mehr) gibt oder
/// das nicht dem Benutzer gehört, wird verworfen.
pub async fn set_preferences(
    pool: &SqlitePool,
    user: &User,
    preferences: &api::Preferences,
) -> AppResult<api::Preferences> {
    let mut preferences = preferences.clone();
    preferences.undo_send_seconds = preferences.undo_send_seconds.min(api::UNDO_SEND_MAX);
    if preferences.auto_refresh_seconds > 0 {
        preferences.auto_refresh_seconds = preferences
            .auto_refresh_seconds
            .clamp(api::AUTO_REFRESH_MIN, 3600);
    }
    if let Some(account) = preferences
        .default_account
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        let own: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM mail_accounts WHERE id = ? AND user_id = ?")
                .bind(account)
                .bind(&user.id)
                .fetch_optional(pool)
                .await?;
        preferences.default_account = own.map(|_| account.to_string());
    } else {
        preferences.default_account = None;
    }
    let json = serde_json::to_string(&preferences)
        .map_err(|e| AppError::Config(format!("Einstellungen: {e}")))?;
    sqlx::query("UPDATE users SET preferences = ?, updated_at = ? WHERE id = ?")
        .bind(json)
        .bind(now())
        .bind(&user.id)
        .execute(pool)
        .await?;
    Ok(preferences)
}

// ------------------------------------------------------------------ Verwaltung

/// Was eine Änderung durch einen Administrator bewirkt hat (für Protokoll und Sitzungen).
pub struct AdminChange {
    pub user: User,
    /// Kurzbeschreibung der geänderten Felder, z. B. „gesperrt, Passwort“
    pub summary: String,
}

/// Benutzer ändern (nur Felder, die gesetzt sind). Gesperrte Konten und neue Passwörter
/// beenden die Sitzungen des Benutzers – bei sich selbst bleibt die laufende (`keep`).
pub async fn admin_update(
    pool: &SqlitePool,
    actor: &User,
    id: &str,
    change: &api::UpdateUser,
    keep: Option<&str>,
) -> AppResult<AdminChange> {
    let target = by_id(pool, id).await?.ok_or(AppError::NotFound)?;
    let is_self = target.id == actor.id;
    if is_self && change.is_admin == Some(false) {
        return Err(AppError::Conflict(
            "Du kannst dir das Admin-Recht nicht selbst nehmen.".into(),
        ));
    }
    if is_self && change.disabled == Some(true) {
        return Err(AppError::Conflict(
            "Du kannst dein eigenes Konto nicht sperren.".into(),
        ));
    }

    let display_name = match &change.display_name {
        Some(n) => clean_display_name(n)?,
        None => target.display_name.clone(),
    };
    let email = match &change.email {
        Some(e) => clean_email(e)?,
        None => target.email.clone(),
    };
    let is_admin = change.is_admin.unwrap_or(target.is_admin);
    let disabled = change.disabled.unwrap_or(target.disabled);
    let new_hash = match change.password.as_deref() {
        Some(p) => {
            password::check_strength(p)?;
            Some(password::hash(p.to_string()).await?)
        }
        None => None,
    };

    // Bleibt kein aktiver Administrator übrig, ändert die Anweisung nichts.
    let stays_active_admin = is_admin && !disabled;
    let updated = sqlx::query(concat!(
        "UPDATE users SET display_name = ?, email = ?, is_admin = ?, disabled = ?,
                password_hash = COALESCE(?, password_hash), updated_at = ?
         WHERE id = ? AND (? OR NOT (is_admin = 1 AND disabled = 0) OR ",
        other_active_admin!(),
        ")"
    ))
    .bind(&display_name)
    .bind(&email)
    .bind(is_admin)
    .bind(disabled)
    .bind(&new_hash)
    .bind(now())
    .bind(id)
    .bind(stays_active_admin)
    .execute(pool)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(AppError::Conflict(
            "Der letzte aktive Administrator muss bleiben.".into(),
        ));
    }

    if disabled || new_hash.is_some() {
        let keep = if is_self { keep } else { None };
        session::destroy_others(pool, id, keep).await?;
    }

    let mut parts = Vec::new();
    if display_name != target.display_name {
        parts.push("Anzeigename");
    }
    if email != target.email {
        parts.push("E-Mail");
    }
    if is_admin != target.is_admin {
        parts.push(if is_admin { "Admin" } else { "kein Admin" });
    }
    if disabled != target.disabled {
        parts.push(if disabled { "gesperrt" } else { "entsperrt" });
    }
    if new_hash.is_some() {
        parts.push("Passwort");
    }
    let user = by_id(pool, id).await?.ok_or(AppError::NotFound)?;
    Ok(AdminChange {
        summary: format!("{}: {}", user.username, parts.join(", ")),
        user,
    })
}

/// Benutzer löschen – mit allen Postfächern und dem lokalen Archiv (per `ON DELETE CASCADE`;
/// den Suchindex räumt der Trigger der Mail-Migration mit auf). Nicht sich selbst und nicht den
/// letzten aktiven Administrator.
pub async fn delete(pool: &SqlitePool, actor: &User, id: &str) -> AppResult<User> {
    if id == actor.id {
        return Err(AppError::Conflict(
            "Du kannst dein eigenes Konto nicht löschen.".into(),
        ));
    }
    let target = by_id(pool, id).await?.ok_or(AppError::NotFound)?;
    let deleted = sqlx::query(concat!(
        "DELETE FROM users WHERE id = ? AND (NOT (is_admin = 1 AND disabled = 0) OR ",
        other_active_admin!(),
        ")"
    ))
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(AppError::Conflict(
            "Der letzte aktive Administrator muss bleiben.".into(),
        ));
    }
    Ok(target)
}
