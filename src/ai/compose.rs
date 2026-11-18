//! Schreiben mit KI: Antwortentwurf auf eine Nachricht (mit Verlauf, Anweisung und Ton, in der
//! Sprache des Originals) und neue Nachrichten schreiben bzw. einen Text überarbeiten.
//! Entwürfe kommen ohne Signatur und ohne zitierten Originaltext – das fügt das Programm an.

use super::client::{self, Prompt};
use super::context;
use super::error::{AiError, AiResult};
use super::settings::AiConfig;
use super::store::{self, Account};
use super::text::{self, Fence};
use crate::api;
use crate::db::models::User;
use crate::error::AppError;
use sqlx::SqlitePool;
use std::sync::LazyLock;

fn check_len(value: &str, max: usize, what: &str) -> Result<(), AppError> {
    if value.chars().count() > max {
        return Err(AppError::bad(format!(
            "{what} ist zu lang (höchstens {max} Zeichen)."
        )));
    }
    Ok(())
}

/// Wie der Entwurf endet: mit Signatur ohne Namen, sonst mit dem Namen.
fn closing_rule(me: &api::Address, account: Option<&Account>) -> String {
    if account.is_some_and(|a| !a.signature.trim().is_empty()) {
        "Ende mit einer passenden Grussformel, aber ohne Namen und ohne Signatur – die Signatur fügt das Programm an.".into()
    } else {
        format!(
            "Ende mit einer passenden Grussformel und dem Namen „{}“, ohne weitere Signatur (keine Kontaktdaten).",
            me.label()
        )
    }
}

const COMMON_RULES: &str = "Keine Betreffzeile im Text, keine Erklärungen oder Kommentare zum Entwurf, \
    keine Platzhalter wie [Name] oder [Datum] – fehlt eine Angabe, formuliere so, dass sie nicht nötig ist. \
    Erfinde keine Fakten, Zusagen oder Termine, die nicht in der Anweisung oder den Nachrichten stehen.";

/// Antwortentwurf auf eine Nachricht.
pub async fn reply(
    pool: &SqlitePool,
    ai: &AiConfig,
    user: &User,
    req: &api::DraftReplyRequest,
) -> AiResult<api::AiDraft> {
    let id = req.message_id.trim();
    if id.is_empty() {
        return Err(AppError::bad("Bitte die Nachricht angeben, auf die geantwortet wird.").into());
    }
    check_len(&req.instruction, 2000, "Die Anweisung")?;
    check_len(&req.tone, 100, "Der Ton")?;
    let mail = store::message(pool, &user.id, id).await?;
    if let Some(why) = ai.unavailable() {
        return Err(AiError::Unavailable(why));
    }
    let account = store::account(pool, &user.id, &mail.account_id).await.ok();
    let me = store::me(user, account.as_ref());
    let signature = account
        .as_ref()
        .map(|a| a.signature.clone())
        .unwrap_or_default();

    // Der ganze frühere Verlauf der Unterhaltung als Hintergrund (bei sehr langen die
    // neuesten 40; `context::conversation` kürzt auf das Budget, die neuesten zuerst)
    let thread = store::thread(pool, &user.id, &mail.thread_id)
        .await
        .unwrap_or_default();
    let earlier: Vec<_> = thread
        .into_iter()
        .filter(|m| m.id != mail.id && m.date <= mail.date)
        .collect();
    let earlier = &earlier[earlier.len().saturating_sub(40)..];

    let fence = Fence::new();
    let system = format!(
        "Du entwirfst Antworten auf E-Mails für {me}. Schreibe nur den Text der Antwort: Anrede, Inhalt, Grussformel. \
         Schreibe in der Sprache der Nachricht, auf die geantwortet wird (nicht in der Sprache der Anweisung), \
         und übernimm Du oder Sie so, wie es in der Unterhaltung üblich ist. \
         Setze die Anweisung von {name} um; ohne Anweisung schreibe eine passende, hilfreiche Antwort. \
         Kein zitierter Originaltext, keine Signatur. {closing} {common}\n\n{rule}",
        me = me.display(),
        name = me.label(),
        closing = closing_rule(&me, account.as_ref()),
        common = COMMON_RULES,
        rule = fence.rule(),
    );
    let mut content = format!(
        "Ich: {}\nAnweisung: {}\nTon: {}\n",
        me.display(),
        if req.instruction.trim().is_empty() {
            "(keine – passende Antwort schreiben)"
        } else {
            req.instruction.trim()
        },
        if req.tone.trim().is_empty() {
            "freundlich und natürlich"
        } else {
            req.tone.trim()
        },
    );
    if !earlier.is_empty() {
        content.push_str(&format!(
            "\nFrüherer Verlauf (älteste zuerst):\n\n{}\n",
            context::conversation(&fence, earlier, ai.budget(24_000), "VERLAUF")
        ));
    }
    content.push_str(&format!(
        "\nNachricht, auf die ich antworte:\n\n{}",
        context::mail_block(
            &fence,
            "NACHRICHT",
            &mail,
            &text::truncate(&mail.own_text(), ai.budget(12_000))
        )
    ));
    let prompt = Prompt::new(system, content)
        .max_tokens(2000)
        .temperature(0.5);
    let c = client::complete(ai, &prompt).await?;
    let body = text::clean_draft(&c.text, &signature);
    if body.is_empty() {
        return Err(AiError::Failed(
            "Die KI hat keinen Entwurf geliefert – bitte nochmals versuchen.".into(),
        ));
    }
    Ok(api::AiDraft {
        subject: text::reply_subject(&mail.subject),
        text: body,
        model: c.model,
    })
}

/// Neue Nachricht schreiben oder einen Text überarbeiten.
pub async fn write(
    pool: &SqlitePool,
    ai: &AiConfig,
    user: &User,
    req: &api::WriteRequest,
) -> AiResult<api::AiDraft> {
    let instruction = req.instruction.trim();
    let draft = req.text.trim();
    if instruction.is_empty() && draft.is_empty() {
        return Err(AppError::bad("Bitte beschreiben, was geschrieben werden soll.").into());
    }
    check_len(instruction, 2000, "Die Anweisung")?;
    check_len(draft, 20_000, "Der Text")?;
    check_len(&req.subject, 500, "Der Betreff")?;
    let account = match req
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(id) => Some(store::account(pool, &user.id, id).await?),
        None => store::default_account(pool, user).await?,
    };
    if let Some(why) = ai.unavailable() {
        return Err(AiError::Unavailable(why));
    }
    let me = store::me(user, account.as_ref());
    let signature = account
        .as_ref()
        .map(|a| a.signature.clone())
        .unwrap_or_default();
    let want_subject = req.subject.trim().is_empty();
    let revise = !draft.is_empty();
    let fence = Fence::new();

    let task = if revise {
        format!(
            "Du überarbeitest E-Mail-Texte für {me}. Setze die Anweisung um (z. B. verbessern, kürzen, förmlicher, \
             freundlicher, übersetzen). Ohne Anweisung: Rechtschreibung, Grammatik und Klarheit verbessern und \
             Inhalt, Ton und Sprache beibehalten. Behalte die Sprache des Textes bei, ausser die Anweisung verlangt \
             eine andere. Behalte Anrede und Grussformel bei, wenn der Text welche hat; füge keine Signatur hinzu.",
            me = me.display()
        )
    } else {
        format!(
            "Du schreibst E-Mails für {me} nach dessen Anweisung. Schreibe in der Sprache der Anweisung, ausser sie \
             verlangt eine andere. Anrede passend zu den Empfängern (Du oder Sie je nach Anweisung, im Zweifel Sie). \
             {closing}",
            me = me.display(),
            closing = closing_rule(&me, account.as_ref()),
        )
    };
    let output = if want_subject {
        "Antworte nur mit JSON: {\"subject\": \"kurzer, treffender Betreff\", \"text\": \"Text der Nachricht\"}"
    } else {
        "Gib nur den Text der Nachricht zurück."
    };
    let system = format!("{task} {COMMON_RULES}\n{output}\n\n{}", fence.rule());
    let to = req
        .to
        .iter()
        .take(10)
        .map(|a| text::one_line(&a.display(), 120))
        .collect::<Vec<_>>()
        .join(", ");
    let mut content = format!(
        "Absender: {}\nEmpfänger: {}\nBetreff: {}\nAnweisung: {}\n",
        me.display(),
        if to.is_empty() {
            "(noch keine)".to_string()
        } else {
            to
        },
        if want_subject {
            "(noch keiner)".to_string()
        } else {
            text::one_line(&req.subject, 300)
        },
        if instruction.is_empty() {
            "(keine – Text verbessern)"
        } else {
            instruction
        },
    );
    if let Some(reply_id) = req
        .reply_to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Antwort: die ganze Unterhaltung bis zur Nachricht, auf die geantwortet wird
        let mail = store::message(pool, &user.id, reply_id).await?;
        let thread = store::thread(pool, &user.id, &mail.thread_id)
            .await
            .unwrap_or_default();
        let earlier: Vec<_> = thread
            .into_iter()
            .filter(|m| m.id != mail.id && m.date <= mail.date)
            .collect();
        let earlier = &earlier[earlier.len().saturating_sub(40)..];
        if !earlier.is_empty() {
            content.push_str(&format!(
                "\nBisheriger Verlauf der Unterhaltung (älteste zuerst):\n\n{}\n",
                context::conversation(&fence, earlier, ai.budget(20_000), "VERLAUF")
            ));
        }
        content.push_str(&format!(
            "\nNachricht, auf die ich antworte (in ihrer Sprache und mit passendem Du/Sie antworten):\n\n{}\n",
            context::mail_block(
                &fence,
                "NACHRICHT",
                &mail,
                &text::truncate(&mail.own_text(), ai.budget(12_000))
            )
        ));
    }
    if revise {
        content.push_str(&format!(
            "\nBisheriger Text:\n\n{}",
            fence.wrap("TEXT", draft)
        ));
    }
    let prompt = Prompt::new(system, content)
        .max_tokens(2500)
        .temperature(0.5);
    let c = client::complete(ai, &prompt).await?;
    let (subject, body) = if want_subject {
        subject_and_text(&c.text)
    } else {
        (req.subject.trim().to_string(), c.text.clone())
    };
    let body = text::clean_draft(&body, &signature);
    if body.is_empty() {
        return Err(AiError::Failed(
            "Die KI hat keinen Text geliefert – bitte nochmals versuchen.".into(),
        ));
    }
    Ok(api::AiDraft {
        subject: text::one_line(&subject, 300),
        text: body,
        model: c.model,
    })
}

/// Betreff und Text aus der Antwort: JSON, sonst „Betreff: …“ in der ersten Zeile, sonst nur Text.
pub fn subject_and_text(raw: &str) -> (String, String) {
    let field = |v: &serde_json::Value, names: &[&str]| -> Option<String> {
        names
            .iter()
            .find_map(|n| v[*n].as_str())
            .map(|s| s.trim().to_string())
    };
    if let Some(v) = text::extract_json(raw) {
        if let Some(body) =
            field(&v, &["text", "body", "content", "message"]).filter(|b| !b.is_empty())
        {
            return (
                field(&v, &["subject", "betreff", "title"]).unwrap_or_default(),
                body,
            );
        }
    }
    // JSON mit echten Zeilenumbrüchen in den Werten (ungültig, aber häufig)
    static LOOSE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"(?s)"subject"\s*:\s*"((?:[^"\\]|\\.)*)"\s*,\s*"text"\s*:\s*"(.*)"\s*\}"#,
        )
        .expect("Regex")
    });
    if let Some(c) = LOOSE.captures(raw) {
        let unescape = |s: &str| {
            s.replace("\\n", "\n")
                .replace("\\t", " ")
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
        };
        return (
            unescape(&c[1]).trim().to_string(),
            unescape(&c[2]).trim().to_string(),
        );
    }
    let raw = text::strip_code_fence(raw);
    let mut lines = raw.lines();
    if let Some(first) = lines.next() {
        let lower = first.trim().to_lowercase();
        for prefix in ["betreff:", "subject:"] {
            if lower.starts_with(prefix) {
                let subject = first.trim()[prefix.len()..].trim().to_string();
                return (
                    subject,
                    lines.collect::<Vec<_>>().join("\n").trim().to_string(),
                );
            }
        }
    }
    (String::new(), raw.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::subject_and_text;

    #[test]
    fn subject_and_text_from_answers() {
        let (s, t) = subject_and_text("```json\n{\"subject\": \"Termin\", \"text\": \"Hallo Anna,\\n\\nwie wäre Dienstag?\"}\n```");
        assert_eq!(
            (s.as_str(), t.as_str()),
            ("Termin", "Hallo Anna,\n\nwie wäre Dienstag?")
        );
        let (s, t) = subject_and_text(
            "{\"subject\": \"Termin\", \"text\": \"Hallo Anna,\n\nwie wäre \\\"Dienstag\\\"?\"}",
        );
        assert_eq!(
            (s.as_str(), t.as_str()),
            ("Termin", "Hallo Anna,\n\nwie wäre \"Dienstag\"?")
        );
        let (s, t) = subject_and_text("Betreff: Termin\n\nHallo Anna");
        assert_eq!((s.as_str(), t.as_str()), ("Termin", "Hallo Anna"));
        let (s, t) = subject_and_text("Hallo Anna");
        assert_eq!((s.as_str(), t.as_str()), ("", "Hallo Anna"));
    }
}
