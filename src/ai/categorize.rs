//! Einteilen per KI in Allgemein / Newsletter / Soziale Netzwerke / Benachrichtigungen /
//! Andere – auf Wunsch für ausgewählte Nachrichten und (wenn eingeschaltet) im Hintergrund
//! für neue Nachrichten im Posteingang. Von Hand gesetzte Kategorien (`category_source =
//! 'user'`) bleiben immer, wie sie sind.

use super::client::{self, Prompt};
use super::error::{AiError, AiResult};
use super::settings::AiConfig;
use super::store::{self, sql, Mail, SELECT};
use super::text::{self, Fence};
use crate::api::{self, Category};
use crate::config::Config;
use crate::db::models::{now, User};
use crate::error::AppError;
use serde_json::Value;
use sqlx::SqlitePool;
use std::sync::LazyLock;
use std::time::Duration;

/// Nachrichten je Anfrage an die KI.
pub const BATCH: usize = 15;
/// Höchstens so viele Nachrichten je Durchlauf im Hintergrund.
const PER_RUN: i64 = 60;

/// Kategorie aus der Antwort des Modells (auch deutsch oder mit Synonymen).
pub fn category_from(s: &str) -> Option<Category> {
    let s = s
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if let Some(c) = Category::parse(&s) {
        return Some(c);
    }
    Some(match s.as_str() {
        "allgemein" | "persönlich" | "personal" | "general" | "wichtig" | "important" | "inbox" => {
            Category::Primary
        }
        "newsletters" | "werbung" | "promotion" | "promotions" | "marketing" | "angebot"
        | "angebote" | "news" | "advertising" | "ads" => Category::Newsletter,
        "soziale netzwerke" | "sozial" | "soziales" | "social media" | "socials" | "community" => {
            Category::Social
        }
        "benachrichtigung" | "benachrichtigungen" | "notifications" | "updates" | "update"
        | "automatisch" | "transaktion" | "transactional" | "system" | "rechnung"
        | "bestellung" | "versand" => Category::Notification,
        "andere" | "sonstige" | "sonstiges" | "misc" | "spam" | "unknown" => Category::Other,
        _ => return None,
    })
}

/// Antwort des Modells lesen: `{"1":"newsletter",…}`, `[{"n":1,"category":"…"},…]`,
/// `["primary",…]` oder Zeilen wie `1: newsletter`. Ergebnis je Nummer 1..=n.
pub fn parse_answer(raw: &str, n: usize) -> Vec<Option<Category>> {
    let mut out = vec![None; n];
    let mut set = |i: Option<usize>, c: Option<&str>| {
        if let (Some(i), Some(c)) = (i, c.and_then(category_from)) {
            if (1..=n).contains(&i) && out[i - 1].is_none() {
                out[i - 1] = Some(c);
            }
        }
    };
    let index = |v: &Value| -> Option<usize> {
        v.as_u64().map(|x| x as usize).or_else(|| {
            v.as_str()
                .and_then(|s| s.trim().trim_start_matches(['#', 'M', 'm']).parse().ok())
        })
    };
    match text::extract_json(raw) {
        Some(Value::Object(map)) => {
            // Auch {"categories": {...}} bzw. {"results": [...]}
            let inner = map
                .values()
                .find(|v| v.is_object() || v.is_array())
                .filter(|_| map.len() == 1)
                .cloned();
            match inner {
                Some(Value::Array(items)) => parse_items(&items, &mut set, &index),
                Some(Value::Object(m)) => {
                    for (k, v) in m {
                        set(index(&Value::String(k)), v.as_str());
                    }
                }
                _ => {
                    for (k, v) in &map {
                        set(index(&Value::String(k.clone())), v.as_str());
                    }
                }
            }
        }
        Some(Value::Array(items)) => parse_items(&items, &mut set, &index),
        _ => {}
    }
    if out.iter().all(Option::is_none) {
        static LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"(?m)^\W*(?:M|Nr\.?\s*)?(\d{1,3})\W+([A-Za-zÄÖÜäöü ]{3,30})")
                .expect("Regex")
        });
        for c in LINE.captures_iter(raw) {
            let i = c[1].parse::<usize>().ok();
            let cat = c[2].trim().to_string();
            if let (Some(i), Some(c)) = (i, category_from(&cat)) {
                if (1..=n).contains(&i) && out[i - 1].is_none() {
                    out[i - 1] = Some(c);
                }
            }
        }
    }
    out
}

fn parse_items(
    items: &[Value],
    set: &mut impl FnMut(Option<usize>, Option<&str>),
    index: &impl Fn(&Value) -> Option<usize>,
) {
    for (pos, item) in items.iter().enumerate() {
        match item {
            Value::String(s) => set(Some(pos + 1), Some(s)),
            Value::Object(o) => {
                let i = ["n", "nr", "id", "index", "number", "mail"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(index))
                    .or(Some(pos + 1));
                let c = ["category", "kategorie", "label", "cat"]
                    .iter()
                    .find_map(|k| o.get(*k).and_then(Value::as_str));
                set(i, c);
            }
            _ => {}
        }
    }
}

/// Einen Stapel einteilen (je Nachricht eine Kategorie oder `None`, wenn das Modell keine nennt).
pub async fn classify(ai: &AiConfig, mails: &[Mail]) -> AiResult<Vec<Option<Category>>> {
    if mails.is_empty() {
        return Ok(Vec::new());
    }
    let fence = Fence::new();
    let system = format!(
        "Du teilst E-Mails im Posteingang in genau eine von fünf Kategorien ein:\n\
         - primary: persönliche oder geschäftliche Nachrichten von Menschen, auf die man reagieren könnte\n\
         - newsletter: Newsletter, Werbung, Angebote, Marketing, Blogs, Digests\n\
         - social: Mitteilungen sozialer Netzwerke und Communities (LinkedIn, Facebook, Instagram, X, Mastodon, Discord, Foren)\n\
         - notification: automatische Mitteilungen wie Bestellungen, Versand, Rechnungen, Kontoauszüge, Termine, \
           Sicherheits- und Systemmeldungen, Tickets\n\
         - other: passt nirgends\n\
         Antworte nur mit JSON, für jede Nummer genau eine Kategorie, z. B. {{\"1\": \"newsletter\", \"2\": \"primary\"}}.\n\n{}",
        fence.rule()
    );
    let items = mails
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut s = format!(
                "Von: {}\nBetreff: {}\n",
                text::one_line(&m.from().display(), 160),
                text::one_line(&m.subject, 200)
            );
            if !m.list_unsubscribe.trim().is_empty() {
                s.push_str("Abmelde-Link: ja\n");
            }
            let body = m.own_text();
            let excerpt = if body.trim().is_empty() {
                m.snippet.clone()
            } else {
                body
            };
            s.push_str(&text::one_line(&excerpt, 400));
            fence.wrap(&format!("MAIL {}", i + 1), &s)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let prompt = Prompt::new(system, format!("Nachrichten:\n\n{items}"))
        .max_tokens(1500)
        .temperature(0.0)
        .timeout(Duration::from_secs(90));
    let c = client::complete(ai, &prompt).await?;
    Ok(parse_answer(&c.text, mails.len()))
}

/// Kategorie speichern – nie über eine von Hand gesetzte. `true`, wenn geändert.
async fn store_category(pool: &SqlitePool, id: &str, c: Category, ts: i64) -> sqlx::Result<bool> {
    Ok(sqlx::query(
        "UPDATE messages SET category = ?, category_source = 'ai', ai_categorized_at = ?
         WHERE id = ? AND category_source <> 'user'",
    )
    .bind(c.as_str())
    .bind(ts)
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

/// `POST /ai/categorize`: ausgewählte Nachrichten des Benutzers einteilen.
pub async fn categorize(
    pool: &SqlitePool,
    ai: &AiConfig,
    user: &User,
    req: &api::CategorizeRequest,
) -> AiResult<api::BulkResult> {
    let mut ids: Vec<String> = req
        .ids
        .iter()
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err(AppError::bad("Keine Nachrichten ausgewählt.").into());
    }
    if ids.len() > 500 {
        return Err(AppError::bad("Höchstens 500 Nachrichten auf einmal.").into());
    }
    let mails = store::by_ids(pool, &user.id, &ids).await?;
    if mails.is_empty() {
        return Err(AppError::NotFound.into());
    }
    if let Some(why) = ai.unavailable() {
        return Err(AiError::Unavailable(why));
    }
    let unknown = (ids.len() - mails.len()) as u32;
    let (manual, todo): (Vec<Mail>, Vec<Mail>) =
        mails.into_iter().partition(|m| m.category_source == "user");
    let mut result = api::BulkResult {
        updated: 0,
        failed: unknown,
        message: None,
    };
    let mut notes = Vec::new();
    let ts = now();
    for chunk in todo.chunks(BATCH) {
        match classify(ai, chunk).await {
            Ok(cats) => {
                for (m, c) in chunk.iter().zip(cats) {
                    match c {
                        Some(c) if store_category(pool, &m.id, c, ts).await? => result.updated += 1,
                        _ => result.failed += 1,
                    }
                }
            }
            // Ganz am Anfang gescheitert: Fehler direkt melden (z. B. falscher Schlüssel)
            Err(e) if result.updated == 0 && result.failed == unknown => return Err(e),
            Err(e) => {
                result.failed += chunk.len() as u32;
                notes.push(e.to_string());
                break;
            }
        }
    }
    if !manual.is_empty() {
        notes.insert(
            0,
            format!(
                "{} von Hand eingeteilte Nachrichten blieben unverändert.",
                manual.len()
            ),
        );
    }
    if unknown > 0 {
        notes.push(format!("{unknown} Nachrichten nicht gefunden."));
    }
    if !notes.is_empty() {
        result.message = Some(notes.join(" "));
    }
    Ok(result)
}

/// Ein Durchlauf im Hintergrund: neue Nachrichten im Posteingang (letzte 7 Tage, noch nicht
/// von der KI gesehen, Kategorie nur aus den Regeln) einteilen. Gibt die Zahl der
/// bearbeiteten Nachrichten zurück; 0, wenn die KI aus ist oder `autoCategorize` nicht gilt.
pub async fn run_pending(pool: &SqlitePool, config: &Config) -> AiResult<usize> {
    let ai = AiConfig::load(pool, config).await?;
    if !ai.auto_categorize || !ai.available() {
        return Ok(0);
    }
    let ts = now();
    let mails = sqlx::query_as::<_, Mail>(sql(format!(
        "{SELECT} WHERE m.ai_categorized_at IS NULL AND m.category_source = 'auto' AND m.is_sent = 0
           AND f.role = 'inbox' AND m.date >= ?
         ORDER BY a.user_id, m.account_id, m.date DESC LIMIT ?"
    )))
    .bind(ts - 7 * 86_400)
    .bind(PER_RUN)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    // Stapel je Postfach – Nachrichten verschiedener Benutzer landen nie in derselben Anfrage
    for group in mails.chunk_by(|a, b| a.account_id == b.account_id) {
        for chunk in group.chunks(BATCH) {
            let cats = classify(&ai, chunk).await?;
            for (m, c) in chunk.iter().zip(cats) {
                match c {
                    Some(c) => {
                        store_category(pool, &m.id, c, ts).await?;
                    }
                    // Keine Kategorie genannt: als gesehen merken, damit sie nicht ewig wiederkommt
                    None => {
                        sqlx::query("UPDATE messages SET ai_categorized_at = ? WHERE id = ? AND ai_categorized_at IS NULL")
                            .bind(ts)
                            .bind(&m.id)
                            .execute(pool)
                            .await?;
                    }
                }
                done += 1;
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_in_many_shapes() {
        use Category::*;
        assert_eq!(
            parse_answer(
                "```json\n{\"1\": \"newsletter\", \"2\": \"Primary\", \"3\": \"quatsch\"}\n```",
                3
            ),
            vec![Some(Newsletter), Some(Primary), None]
        );
        assert_eq!(
            parse_answer("{\"categories\": [{\"n\": 2, \"category\": \"social\"}, {\"n\": 1, \"category\": \"Benachrichtigungen\"}]}", 2),
            vec![Some(Notification), Some(Social)]
        );
        assert_eq!(
            parse_answer("[\"promotions\", \"other\"]", 2),
            vec![Some(Newsletter), Some(Other)]
        );
        assert_eq!(
            parse_answer("1: newsletter\n2 - primary\n9: social", 2),
            vec![Some(Newsletter), Some(Primary)]
        );
        assert_eq!(parse_answer("keine Ahnung", 2), vec![None, None]);
    }
}
