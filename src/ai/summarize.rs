//! Zusammenfassen: eine Nachricht oder eine ganze Unterhaltung als kurze Stichpunkte in der
//! Sprache der Mail – worum es geht, was von mir erwartet wird, Fristen und Termine.
//! Die Zusammenfassung einer einzelnen Nachricht wird in `messages.ai_summary` gemerkt.

use super::client::{self, Prompt};
use super::context;
use super::error::{AiError, AiResult};
use super::settings::AiConfig;
use super::store;
use super::text::{self, Fence};
use crate::api;
use crate::db::models::User;
use crate::error::AppError;
use sqlx::SqlitePool;

/// Zeichen Mail-Text, die höchstens an die KI gehen.
const BUDGET: usize = 24_000;

pub async fn summarize(
    pool: &SqlitePool,
    ai: &AiConfig,
    user: &User,
    req: &api::SummarizeRequest,
) -> AiResult<api::AiText> {
    let thread_id = req
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let message_id = req
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mails = match (thread_id, message_id) {
        (Some(t), _) => store::thread(pool, &user.id, t).await?,
        (None, Some(m)) => vec![store::message(pool, &user.id, m).await?],
        (None, None) => {
            return Err(AppError::bad("Bitte eine Nachricht oder Unterhaltung angeben.").into())
        }
    };
    if let Some(why) = ai.unavailable() {
        return Err(AiError::Unavailable(why));
    }
    let account = store::account(pool, &user.id, &mails[0].account_id)
        .await
        .ok();
    let me = store::me(user, account.as_ref());
    let fence = Fence::new();
    let single = mails.len() == 1;
    let budget = ai.budget(BUDGET);
    let system = format!(
        "Du fasst E-Mails für {me} zusammen (in den Nachrichten ist das „ich“ bzw. der Empfänger). \
         Schreibe in der Sprache der E-Mail – bei mehreren Sprachen in der Sprache der neuesten Nachricht. \
         Format: 2 bis 6 knappe Stichpunkte, jeder beginnt mit „- “, ohne Einleitung, ohne Überschrift, ohne Schlusssatz:\n\
         - worum es geht (wer will was){thread}\n\
         - was von {name} erwartet wird (z. B. antworten, entscheiden, bezahlen, unterschreiben) – oder dass nichts zu tun ist\n\
         - Fristen, Termine, Beträge und Nummern, falls vorhanden – genau übernehmen\n\
         Erfinde nichts und lass weg, was nicht in den Nachrichten steht.\n\n{rule}",
        me = me.display(),
        name = me.label(),
        thread = if single { "" } else { "; bei einer Unterhaltung der aktuelle Stand und was zuletzt gesagt wurde" },
        rule = fence.rule(),
    );
    let content = if single {
        let m = &mails[0];
        context::mail_block(&fence, "MAIL", m, &text::truncate(&m.body(), budget))
    } else {
        format!(
            "Unterhaltung mit {} Nachrichten, älteste zuerst:\n\n{}",
            mails.len(),
            context::conversation(&fence, &mails, budget, "MAIL")
        )
    };
    let prompt = Prompt::new(system, format!("Bitte zusammenfassen.\n\n{content}"))
        .max_tokens(1500)
        .temperature(0.2);
    let c = client::complete(ai, &prompt).await?;
    let summary = text::truncate(&text::strip_code_fence(&c.text), 4000);
    if single {
        sqlx::query("UPDATE messages SET ai_summary = ? WHERE id = ?")
            .bind(&summary)
            .bind(&mails[0].id)
            .execute(pool)
            .await?;
    }
    Ok(api::AiText {
        text: summary,
        model: c.model,
    })
}
