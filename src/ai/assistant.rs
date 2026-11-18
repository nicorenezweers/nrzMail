//! Der Assistent („Dieter“, Name einstellbar) beantwortet Fragen zu den eigenen Mails – mit
//! Quellen. Ablauf ohne Vektor-Datenbank, nur mit der vorhandenen Volltextsuche:
//! 1. Aus der Frage werden Suchbegriffe, Absender und ein Zeitraum ([`query::parse`]).
//! 2. Kontext: die geöffnete Nachricht, Nachrichten der genannten Absender, Treffer der
//!    Volltextsuche (FTS5) und die neuesten Nachrichten – nur aus den Postfächern des Benutzers,
//!    nie aus dem Spam. Je Nachricht die Textstellen um die Treffer, insgesamt mit Budget.
//! 3. Fragt jemand nach Mengen, kommen exakte Kennzahlen aus der Datenbank dazu.
//! 4. Die KI antwortet nur anhand dieser Nachrichten und belegt mit [M1], [M2] …; daraus
//!    werden die Quellen, die die Oberfläche verlinkt.

use super::client::{self, Prompt, Turn};
use super::context;
use super::error::{AiError, AiResult};
use super::query::{self, Question};
use super::settings::AiConfig;
use super::store::{self, Mail, SELECT};
use super::text::{self, Fence};
use crate::api;
use crate::db::models::{now, User};
use crate::error::AppError;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::LazyLock;

/// So viele Nachrichten gehen höchstens an die KI, so viel Text insgesamt.
const MAX_MAILS: usize = 20;
const BUDGET: usize = 30_000;
const MAX_SOURCES: usize = 6;

/// Woher eine Nachricht im Kontext stammt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Opened,
    Match,
    Recent,
}

struct Found {
    mails: Vec<(Mail, Origin)>,
    seen: HashSet<String>,
}

impl Found {
    fn add(&mut self, list: Vec<Mail>, origin: Origin) {
        for m in list {
            if self.mails.len() >= MAX_MAILS {
                return;
            }
            if self.seen.insert(m.id.clone()) {
                self.mails.push((m, origin));
            }
        }
    }
}

/// Nur Postfächer des Benutzers, kein Spam; optional im Zeitraum.
fn scope(range: Option<&query::Range>) -> String {
    let mut s = String::from(" a.user_id = ? AND COALESCE(f.role, '') <> 'junk'");
    if range.is_some() {
        s.push_str(" AND m.date >= ? AND m.date < ?");
    }
    s
}

async fn fetch(
    pool: &SqlitePool,
    sql: &str,
    binds: &[String],
    user_id: &str,
    range: Option<&query::Range>,
    limit: i64,
) -> Vec<Mail> {
    let mut q = sqlx::query_as::<_, Mail>(store::sql(sql.to_string()));
    for b in binds {
        q = q.bind(b);
    }
    q = q.bind(user_id);
    if let Some(r) = range {
        q = q.bind(r.from).bind(r.to);
    }
    match q.bind(limit).fetch_all(pool).await {
        Ok(list) => list,
        Err(e) => {
            // Z. B. eine Volltext-Abfrage, die FTS5 nicht mag – dann eben ohne diese Treffer
            tracing::warn!("Assistent: Suche fehlgeschlagen: {e}");
            Vec::new()
        }
    }
}

/// Nachrichten von bzw. an genannte Adressen und Namen, neueste zuerst.
async fn by_people(pool: &SqlitePool, user_id: &str, q: &Question, limit: i64) -> Vec<Mail> {
    let mut out = Vec::new();
    let range = q.range.as_ref();
    for a in &q.addresses {
        let sql = format!(
            "{SELECT} WHERE (lower(m.from_address) = ? OR lower(m.to_addrs) LIKE ? OR lower(m.cc_addrs) LIKE ?) AND{} ORDER BY m.date DESC LIMIT ?",
            scope(range)
        );
        let like = format!("%\"{}\"%", a.replace(['%', '_'], ""));
        out.extend(
            fetch(
                pool,
                &sql,
                &[a.clone(), like.clone(), like],
                user_id,
                range,
                limit,
            )
            .await,
        );
    }
    for s in &q.senders {
        let sql = format!(
            "{SELECT} WHERE (lower(m.from_name) LIKE ? OR lower(m.from_address) LIKE ?) AND{} ORDER BY m.date DESC LIMIT ?",
            scope(range)
        );
        let like = format!("%{}%", s.to_lowercase().replace(['%', '_'], ""));
        out.extend(fetch(pool, &sql, &[like.clone(), like], user_id, range, limit).await);
    }
    out
}

/// Treffer der Volltextsuche, beste zuerst.
async fn search(pool: &SqlitePool, user_id: &str, q: &Question, limit: i64) -> Vec<Mail> {
    let Some(fts) = query::fts_query(&q.terms) else {
        return Vec::new();
    };
    let sql = format!(
        "WITH hits AS (SELECT message_id, bm25(messages_fts) AS score FROM messages_fts WHERE messages_fts MATCH ?)
         {SELECT} JOIN hits h ON h.message_id = m.id WHERE{} ORDER BY h.score, m.date DESC LIMIT ?",
        scope(q.range.as_ref())
    );
    fetch(pool, &sql, &[fts], user_id, q.range.as_ref(), limit).await
}

/// Neueste empfangene Nachrichten (ohne Papierkorb).
async fn recent(
    pool: &SqlitePool,
    user_id: &str,
    range: Option<&query::Range>,
    limit: i64,
) -> Vec<Mail> {
    let sql = format!(
        "{SELECT} WHERE m.is_sent = 0 AND COALESCE(f.role, '') <> 'trash' AND{} ORDER BY m.date DESC LIMIT ?",
        scope(range)
    );
    fetch(pool, &sql, &[], user_id, range, limit).await
}

async fn count(
    pool: &SqlitePool,
    sql: &str,
    binds: &[String],
    user_id: &str,
    range: Option<&query::Range>,
) -> Option<i64> {
    let mut q = sqlx::query_scalar::<_, i64>(store::sql(sql.to_string()));
    for b in binds {
        q = q.bind(b);
    }
    q = q.bind(user_id);
    if let Some(r) = range {
        q = q.bind(r.from).bind(r.to);
    }
    q.fetch_one(pool).await.ok()
}

/// Exakte Kennzahlen für Fragen nach Mengen.
async fn stats(pool: &SqlitePool, user_id: &str, q: &Question) -> Vec<String> {
    let range = q.range.as_ref();
    let when = range
        .map(|r| format!(" ({})", r.label))
        .unwrap_or_else(|| " (insgesamt)".into());
    let base = "SELECT count(*) FROM messages m JOIN mail_accounts a ON a.id = m.account_id LEFT JOIN folders f ON f.id = m.folder_id";
    let mut lines = Vec::new();
    for (what, cond) in [
        ("Empfangene Nachrichten", "m.is_sent = 0"),
        ("Gesendete Nachrichten", "m.is_sent = 1"),
    ] {
        let sql = format!("{base} WHERE {cond} AND{}", scope(range));
        if let Some(n) = count(pool, &sql, &[], user_id, range).await {
            lines.push(format!("- {what}{when}: {n}"));
        }
    }
    for a in &q.addresses {
        let sql = format!("{base} WHERE lower(m.from_address) = ? AND{}", scope(range));
        if let Some(n) = count(pool, &sql, std::slice::from_ref(a), user_id, range).await {
            lines.push(format!("- Nachrichten von {a}{when}: {n}"));
        }
    }
    for s in &q.senders {
        let like = format!("%{}%", s.to_lowercase().replace(['%', '_'], ""));
        let sql = format!(
            "{base} WHERE (lower(m.from_name) LIKE ? OR lower(m.from_address) LIKE ?) AND{}",
            scope(range)
        );
        if let Some(n) = count(pool, &sql, &[like.clone(), like], user_id, range).await {
            lines.push(format!("- Nachrichten von „{s}“{when}: {n}"));
        }
    }
    if let Some(fts) = query::fts_query(&q.terms) {
        let sql = format!(
            "WITH hits AS (SELECT message_id FROM messages_fts WHERE messages_fts MATCH ?)
             {base} JOIN hits h ON h.message_id = m.id WHERE{}",
            scope(range)
        );
        if let Some(n) = count(pool, &sql, &[fts], user_id, range).await {
            lines.push(format!(
                "- Nachrichten mit „{}“{when}: {n}",
                q.words.join("“ oder „")
            ));
        }
    }
    let sql = format!(
        "{base} WHERE m.seen = 0 AND m.is_sent = 0 AND COALESCE(f.role, '') = 'inbox' AND{}",
        scope(None)
    );
    if let Some(n) = count(pool, &sql, &[], user_id, None).await {
        lines.push(format!("- Ungelesen im Posteingang (jetzt): {n}"));
    }
    lines
}

/// Verweise wie [M3], (M1, M2) im Text: als Quellen nehmen und aus dem Text entfernen.
pub fn citations(answer: &str) -> (String, Vec<usize>) {
    static CITE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\s*[\[(]\s*M\d+(?:\s*[,;/]\s*(?:M\s*)?\d+)*\s*[\])]").expect("Regex")
    });
    static NUM: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\d+").expect("Regex"));
    let mut refs = Vec::new();
    for m in CITE.find_iter(answer) {
        for n in NUM.find_iter(m.as_str()) {
            if let Ok(n) = n.as_str().parse::<usize>() {
                if !refs.contains(&n) {
                    refs.push(n);
                }
            }
        }
    }
    (CITE.replace_all(answer, "").trim().to_string(), refs)
}

/// Verlauf für die KI: die letzten Nachrichten, gekürzt (bei lokalen Modellen kürzer).
fn history(ai: &AiConfig, messages: &[api::ChatMessage]) -> Vec<Turn> {
    let earlier = &messages[..messages.len().saturating_sub(1)];
    let keep = if ai.provider.local { 4 } else { 8 };
    earlier[earlier.len().saturating_sub(keep)..]
        .iter()
        .map(|m| {
            let content = text::truncate(m.content.trim(), ai.budget(2000).max(600));
            match m.role {
                api::ChatRole::User => Turn::user(content),
                api::ChatRole::Assistant => Turn::assistant(content),
            }
        })
        .collect()
}

pub async fn chat(
    pool: &SqlitePool,
    ai: &AiConfig,
    user: &User,
    req: &api::ChatRequest,
) -> AiResult<api::ChatReply> {
    if req.messages.len() > 40 {
        return Err(
            AppError::bad("Der Verlauf ist zu lang – bitte ein neues Gespräch beginnen.").into(),
        );
    }
    let question = match req.messages.last() {
        Some(m) if m.role == api::ChatRole::User && !m.content.trim().is_empty() => {
            m.content.trim().to_string()
        }
        _ => return Err(AppError::bad("Bitte eine Frage stellen.").into()),
    };
    if question.chars().count() > 2000 {
        return Err(AppError::bad("Die Frage ist zu lang (höchstens 2000 Zeichen).").into());
    }
    let opened = match req
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(id) => Some(store::message(pool, &user.id, id).await?),
        None => None,
    };
    if let Some(why) = ai.unavailable() {
        return Err(AiError::Unavailable(why));
    }

    // 1. Suchen – ohne eigene Begriffe (z. B. „und im März?“) gelten die der vorigen Frage.
    // Zeitangaben gelten in der Zeitzone des Browsers: auf dessen Uhrzeit rechnen, dann den
    // Zeitraum zurück nach UTC schieben.
    let shift = i64::from(req.tz.clamp(-14 * 60, 14 * 60)) * 60;
    // Uhrzeit beim Benutzer (für Zeitangaben und „heute ist …“ im Prompt)
    let ts = now() + shift;
    let parse = |text: &str| {
        let mut q = query::parse(text, ts);
        if let Some(r) = q.range.as_mut() {
            r.from -= shift;
            r.to -= shift;
        }
        q
    };
    let mut q = parse(&question);
    if q.is_vague() {
        if let Some(prev) = req.messages[..req.messages.len() - 1]
            .iter()
            .rev()
            .find(|m| m.role == api::ChatRole::User)
        {
            let p = parse(&prev.content);
            q.words = p.words;
            q.terms = p.terms;
            q.addresses = p.addresses;
            q.senders = p.senders;
            if q.range.is_none() {
                q.range = p.range;
            }
        }
    }
    let mut found = Found {
        mails: Vec::new(),
        seen: HashSet::new(),
    };
    if let Some(m) = opened {
        found.add(vec![m], Origin::Opened);
    }
    found.add(by_people(pool, &user.id, &q, 8).await, Origin::Match);
    found.add(search(pool, &user.id, &q, 15).await, Origin::Match);
    let fill = if q.is_vague() { 15 } else { 5 };
    found.add(
        recent(pool, &user.id, q.range.as_ref(), fill).await,
        Origin::Recent,
    );
    let figures = if q.counting {
        stats(pool, &user.id, &q).await
    } else {
        Vec::new()
    };

    // 2. Kontext: Textstellen um die Treffer, Budget je Nachricht
    let fence = Fence::new();
    let n = found.mails.len();
    let per_mail = ai
        .budget(BUDGET)
        .checked_div(n)
        .unwrap_or(0)
        .clamp(400, 6000);
    let mut blocks = Vec::new();
    for (i, (m, origin)) in found.mails.iter().enumerate() {
        let body = m.own_text();
        let excerpt = if *origin == Origin::Opened {
            text::truncate(&body, per_mail.max(4000))
        } else {
            text::snippets(&body, &q.terms, per_mail)
        };
        let mut block = format!(
            "Postfach: {} · Ordner: {} · {}{}\n",
            text::one_line(&m.account_name, 60),
            m.folder_label(),
            if m.seen { "gelesen" } else { "ungelesen" },
            if *origin == Origin::Opened {
                " · gerade geöffnet"
            } else {
                ""
            },
        );
        block.push_str(&context::mail_block(
            &fence,
            &format!("MAIL M{}", i + 1),
            m,
            &excerpt,
        ));
        blocks.push(block);
    }

    // 3. Antworten
    let account = match found.mails.first() {
        Some((m, _)) => store::account(pool, &user.id, &m.account_id).await.ok(),
        None => store::default_account(pool, user).await?,
    };
    let me = store::me(user, account.as_ref());
    let system = format!(
        "Du bist {name}, der Assistent in nrzMail, dem Mail-Programm von {me}. Heute ist {weekday}, {today} (UTC).\n\
         Du beantwortest Fragen zu den E-Mails von {me} – ausschliesslich anhand der Nachrichten (M1, M2, …) und \
         Kennzahlen, die du mit der Frage bekommst. Erfinde nichts. Steht die Antwort nicht darin, sag das ehrlich \
         und schlage vor, wonach man suchen könnte.\n\
         Antworte hilfsbereit, knapp und konkret in der Sprache der Frage (meist Deutsch). Übernimm Namen, Beträge, \
         Daten und Nummern genau. Markdown ist erlaubt (Listen mit „- “).\n\
         Belege Aussagen mit dem Kürzel der Nachricht in eckigen Klammern direkt dahinter, z. B. [M2] oder [M1, M4]. \
         Die Kennzahlen stammen aus der Datenbank und sind exakt – nutze sie für Fragen nach Mengen, statt selbst \
         zu zählen; die Nachrichten sind nur eine Auswahl.\n\
         Du kannst keine Mails senden, löschen oder verschieben und keine Links öffnen.\n\n{rule}",
        name = ai.assistant_name,
        me = me.display(),
        weekday = text::weekday_de(ts),
        today = text::date(ts),
        rule = fence.rule(),
    );
    let mut content = String::new();
    if blocks.is_empty() {
        content.push_str("Nachrichten: (keine passenden gefunden)\n");
    } else {
        content.push_str(&format!(
            "Nachrichten ({n}, die passendsten und neuesten{}):\n\n{}\n",
            q.range
                .as_ref()
                .map(|r| format!(", Zeitraum {}", r.label))
                .unwrap_or_default(),
            blocks.join("\n\n")
        ));
    }
    if !figures.is_empty() {
        content.push_str(&format!("\nKennzahlen:\n{}\n", figures.join("\n")));
    }
    content.push_str(&format!("\nFrage: {question}"));
    let mut turns = history(ai, &req.messages);
    turns.push(Turn::user(content));
    let prompt = Prompt {
        turns,
        ..Prompt::new(system, "").max_tokens(2000).temperature(0.2)
    };
    let c = client::complete(ai, &prompt).await?;

    let (answer, refs) = citations(&text::strip_code_fence(&c.text));
    let mut sources: Vec<api::MessageRef> = refs
        .iter()
        .filter(|n| **n >= 1 && **n <= found.mails.len())
        .map(|n| found.mails[n - 1].0.to_ref())
        .take(MAX_SOURCES)
        .collect();
    if sources.is_empty() {
        // Keine Belege genannt: die gezielt gefundenen Nachrichten
        sources = found
            .mails
            .iter()
            .filter(|(_, o)| *o != Origin::Recent)
            .take(MAX_SOURCES.min(3))
            .map(|(m, _)| m.to_ref())
            .collect();
    }
    Ok(api::ChatReply {
        reply: if answer.is_empty() {
            "Dazu habe ich in deinen Mails nichts gefunden.".into()
        } else {
            answer
        },
        sources,
        model: c.model,
    })
}

#[cfg(test)]
mod tests {
    use super::citations;

    #[test]
    fn citations_become_sources() {
        let (text, refs) =
            citations("Die Rechnung kam am 3. März [M2]. Anna schrieb zweimal (M1, M4) [M2].");
        assert_eq!(text, "Die Rechnung kam am 3. März. Anna schrieb zweimal.");
        assert_eq!(refs, vec![2, 1, 4]);
        let (text, refs) = citations("Nichts gefunden.");
        assert_eq!((text.as_str(), refs.len()), ("Nichts gefunden.", 0));
    }
}
