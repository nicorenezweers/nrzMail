//! Suche in Nachrichten: eine kleine Abfragesprache, übersetzt in FTS5 (`messages_fts`) und
//! SQL-Filter.
//!
//! | Eingabe | Bedeutung |
//! |---|---|
//! | `rechnung zürich` | beide Wörter (Anfang genügt: `rech` findet „Rechnung“), Umlaute egal |
//! | `"mobile abo"` | genau diese Wortfolge |
//! | `-werbung` | ohne dieses Wort |
//! | `from:anna` / `von:` · `to:` / `an:` · `subject:` / `betreff:` · `body:` · `filename:` | nur in diesem Feld |
//! | `has:attachment` (`anhang`) | mit Anhang |
//! | `is:unread` / `is:read` / `is:starred` / `is:sent` / `is:answered` | Zustand |
//! | `before:2024-12-31` / `after:2024-01-01` (auch `31.12.2024`, `2024-12`, `2024`) | Datum |
//! | `folder:Rechnungen` / `ordner:` · `account:privat` / `konto:` | Ordner bzw. Postfach |
//! | `category:newsletter` (`allgemein`, `sozial`, `benachrichtigungen`, …) | Kategorie |
//!
//! Werte gehen über `bind` in die Abfrage; jeder FTS-Begriff steht vollständig in
//! Anführungszeichen, damit Eingaben keine FTS-Syntax einschleusen können.

use crate::api::Category;
use time::{Date, Month, PrimitiveDateTime, Time};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Search {
    /// FTS-Ausdrücke, die alle passen müssen
    pub terms: Vec<String>,
    /// FTS-Ausdrücke, die nicht passen dürfen
    pub not_terms: Vec<String>,
    pub has_attachment: Option<bool>,
    pub unread: Option<bool>,
    pub flagged: Option<bool>,
    pub sent: Option<bool>,
    pub answered: Option<bool>,
    /// Unix-Sekunden, einschliesslich
    pub after: Option<i64>,
    /// Unix-Sekunden, ausschliesslich
    pub before: Option<i64>,
    pub folders: Vec<String>,
    pub accounts: Vec<String>,
    pub categories: Vec<Category>,
    pub not_categories: Vec<Category>,
}

impl Search {
    pub fn is_empty(&self) -> bool {
        *self == Search::default()
    }

    /// Alle positiven Begriffe als ein FTS5-Ausdruck.
    pub fn fts_match(&self) -> Option<String> {
        (!self.terms.is_empty()).then(|| self.terms.join(" AND "))
    }

    /// Die auszuschliessenden Begriffe als ein FTS5-Ausdruck (ODER-verknüpft).
    pub fn fts_exclude(&self) -> Option<String> {
        (!self.not_terms.is_empty()).then(|| self.not_terms.join(" OR "))
    }
}

/// Zerlegt die Eingabe; Anführungszeichen halten Leerzeichen zusammen.
fn tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Ein Begriff als FTS5-Ausdruck: immer in Anführungszeichen, einzelne Wörter ab zwei
/// Zeichen mit Präfixsuche. `None`, wenn nichts Suchbares übrig bleibt.
pub fn fts_term(term: &str) -> Option<String> {
    let phrase = term.starts_with('"') && term.ends_with('"') && term.len() > 1;
    let inner: String = term.chars().filter(|c| *c != '"').collect();
    let inner = inner.trim();
    if inner.is_empty() || !inner.chars().any(char::is_alphanumeric) {
        return None;
    }
    Some(
        if phrase || inner.contains(char::is_whitespace) || inner.chars().count() < 2 {
            format!("\"{inner}\"")
        } else {
            format!("\"{inner}\"*")
        },
    )
}

/// `2024`, `2024-06`, `2024-06-15`, `15.06.2024` → (Beginn, Ende) in Unix-Sekunden (UTC).
pub fn date_range(s: &str) -> Option<(i64, i64)> {
    let ts = |y: i32, m: u8, d: u8| -> Option<i64> {
        let date = Date::from_calendar_date(y, Month::try_from(m).ok()?, d).ok()?;
        Some(
            PrimitiveDateTime::new(date, Time::MIDNIGHT)
                .assume_utc()
                .unix_timestamp(),
        )
    };
    let parts: Vec<&str> = if s.contains('.') {
        let mut p: Vec<&str> = s.split('.').filter(|p| !p.is_empty()).collect();
        p.reverse();
        p
    } else {
        s.split('-').collect()
    };
    let year: i32 = parts.first()?.parse().ok()?;
    match parts.len() {
        1 => Some((ts(year, 1, 1)?, ts(year + 1, 1, 1)?)),
        2 => {
            let m: u8 = parts[1].parse().ok()?;
            let (ny, nm) = if m == 12 {
                (year + 1, 1)
            } else {
                (year, m + 1)
            };
            Some((ts(year, m, 1)?, ts(ny, nm, 1)?))
        }
        3 => {
            let start = ts(year, parts[1].parse().ok()?, parts[2].parse().ok()?)?;
            Some((start, start + 86_400))
        }
        _ => None,
    }
}

fn category(value: &str) -> Option<Category> {
    Some(match value.to_lowercase().as_str() {
        "primary" | "allgemein" | "primär" | "general" => Category::Primary,
        "newsletter" | "newsletters" | "promotions" | "werbung" => Category::Newsletter,
        "social" | "sozial" | "soziale" | "netzwerke" => Category::Social,
        "notification" | "notifications" | "benachrichtigung" | "benachrichtigungen"
        | "updates" => Category::Notification,
        "other" | "andere" | "sonstige" => Category::Other,
        _ => return None,
    })
}

pub fn parse(input: &str) -> Search {
    let mut s = Search::default();
    for token in tokens(input) {
        let (negated, body) = match token.strip_prefix('-') {
            Some(rest) if !rest.is_empty() => (true, rest.to_string()),
            _ => (false, token.clone()),
        };
        let Some((key, value)) = body
            .split_once(':')
            .filter(|(k, v)| !k.is_empty() && !k.starts_with('"') && !v.is_empty())
        else {
            if let Some(t) = fts_term(&body) {
                if negated {
                    s.not_terms.push(t);
                } else {
                    s.terms.push(t);
                }
            }
            continue;
        };
        let raw_value = value;
        let value = value.trim_matches('"').trim().to_string();
        let column = |c: &str| fts_term(raw_value).map(|t| format!("{c} : {t}"));
        let mut push_fts = |expr: Option<String>| {
            if let Some(e) = expr {
                if negated {
                    s.not_terms.push(e);
                } else {
                    s.terms.push(e);
                }
            }
        };
        match key.to_lowercase().as_str() {
            "from" | "von" | "absender" => push_fts(column("sender")),
            "to" | "an" | "cc" | "bcc" | "empfänger" => push_fts(column("recipients")),
            "subject" | "betreff" => push_fts(column("subject")),
            "body" | "text" | "inhalt" => push_fts(column("body")),
            "filename" | "datei" | "anhangname" => push_fts(column("attachment_names")),
            "has" | "hat" => {
                if matches!(
                    value.to_lowercase().as_str(),
                    "attachment" | "attachments" | "anhang" | "anhänge"
                ) {
                    s.has_attachment = Some(!negated);
                }
            }
            "is" | "ist" => match value.to_lowercase().as_str() {
                "unread" | "ungelesen" => s.unread = Some(!negated),
                "read" | "gelesen" => s.unread = Some(negated),
                "starred" | "flagged" | "markiert" => s.flagged = Some(!negated),
                "sent" | "gesendet" => s.sent = Some(!negated),
                "answered" | "beantwortet" => s.answered = Some(!negated),
                _ => {}
            },
            "before" | "vor" | "bis" => {
                if let Some((start, _)) = date_range(&value) {
                    s.before = Some(start);
                }
            }
            "after" | "nach" | "ab" | "seit" => {
                if let Some((start, _)) = date_range(&value) {
                    s.after = Some(start);
                }
            }
            "folder" | "ordner" | "in" => s.folders.push(value),
            "account" | "konto" | "postfach" => s.accounts.push(value),
            "category" | "kategorie" => {
                if let Some(c) = category(&value) {
                    if negated {
                        s.not_categories.push(c);
                    } else {
                        s.categories.push(c);
                    }
                }
            }
            // Unbekannter Filter (z. B. eine Uhrzeit „10:30“): als normalen Text suchen
            _ => push_fts(fts_term(&body)),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_words_phrases_and_filters() {
        let s = parse(
            r#"rechnung "mobile abo" -werbung from:anna to:"Bruno Meier" subject:offerte has:attachment is:unread is:starred category:newsletter -category:social folder:Rechnungen account:privat"#,
        );
        assert_eq!(
            s.fts_match().unwrap(),
            r#""rechnung"* AND "mobile abo" AND sender : "anna"* AND recipients : "Bruno Meier" AND subject : "offerte"*"#
        );
        assert_eq!(s.fts_exclude().unwrap(), r#""werbung"*"#);
        assert_eq!(s.has_attachment, Some(true));
        assert_eq!(s.unread, Some(true));
        assert_eq!(s.flagged, Some(true));
        assert_eq!(s.categories, vec![Category::Newsletter]);
        assert_eq!(s.not_categories, vec![Category::Social]);
        assert_eq!(s.folders, vec!["Rechnungen"]);
        assert_eq!(s.accounts, vec!["privat"]);
        assert!(parse("   ").is_empty());
        assert_eq!(parse("is:read").unread, Some(false));
        assert_eq!(parse("-has:attachment").has_attachment, Some(false));
        assert_eq!(
            parse("-from:newsletter").fts_exclude().unwrap(),
            r#"sender : "newsletter"*"#
        );
    }

    #[test]
    fn parses_dates() {
        let y2024 = 1_704_067_200; // 2024-01-01T00:00:00Z
        assert_eq!(parse("after:2024").after, Some(y2024));
        assert_eq!(parse("after:2024-01-01").after, Some(y2024));
        assert_eq!(parse("after:01.01.2024").after, Some(y2024));
        assert_eq!(parse("before:2024-02").before, Some(y2024 + 31 * 86_400));
        assert_eq!(parse("before:kaputt").before, None);
        assert_eq!(date_range("2024-12").unwrap().1, 1_735_689_600);
    }

    #[test]
    fn fts_is_escaped() {
        assert_eq!(fts_term("a"), Some("\"a\"".into()));
        assert_eq!(fts_term("***"), None);
        assert_eq!(
            fts_term("anna@example.com"),
            Some("\"anna@example.com\"*".into())
        );
        for input in [
            r#"a*" OR body:x NEAR( ^"#,
            "foo OR bar",
            "x AND NOT y",
            "{subject}: z",
            r#"""""#,
            "10:30",
        ] {
            let s = parse(input);
            for part in s.terms.iter().chain(&s.not_terms) {
                let part = part
                    .split_once(" : ")
                    .map(|(_, t)| t)
                    .unwrap_or(part)
                    .trim_end_matches('*');
                assert!(
                    part.starts_with('"')
                        && part.ends_with('"')
                        && !part[1..part.len() - 1].contains('"'),
                    "{input} → {part}"
                );
            }
        }
    }
}
