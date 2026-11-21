//! Anmelde- und Bestätigungscodes (2FA/OTP) in Nachrichten erkennen: 6 oder 8 Ziffern
//! (auch „123 456“ oder „1234-5678“) in der Nähe eines Stichworts wie „Code“ oder
//! „Verifizierung“. Bestell-, Kunden- und Telefonnummern werden ausgelassen.

use regex::Regex;
use std::sync::LazyLock;

/// Stichworte (klein geschrieben); dieselben grenzen in SQL die Kandidaten ein.
pub const KEYWORDS: &[&str] = &[
    "code",
    "verif",
    "bestätig",
    "otp",
    "2fa",
    "einmal",
    "passcode",
    "authent",
    "sign-in",
    "sign in",
    "login",
    "log-in",
    "anmeld",
    "sicherheit",
    "security",
    "kennwort",
    "password",
    "passwort",
    "pin",
    "tan",
];

/// Wörter kurz vor einer Zahl, die gegen einen Code sprechen.
const NOT_BEFORE: &[&str] = &[
    "nr", "no.", "#", "nummer", "number", "order", "bestell", "rechnung", "invoice", "kunden",
    "customer", "tel", "fax", "phone", "iban", "konto", "account", "plz", "ticket", "id:", "€",
    "$", "+",
];

static CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^0-9A-Za-z.,/:+])([0-9]{4}[ \-]?[0-9]{4}|[0-9]{3}[ \-]?[0-9]{3})(?:$|[^0-9A-Za-z/:%€$])")
        .expect("Regex")
});

/// Stärke eines Stichworts (kurze, mehrdeutige zählen nur mit Ziffern dahinter).
fn keyword_positions(lower: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for k in KEYWORDS {
        let mut start = 0;
        while let Some(i) = lower[start..].find(k) {
            let pos = start + i;
            // kurze Wörter nur als ganzes Wort („pin“, nicht „shopping“)
            let whole = k.len() > 3 || {
                let before = lower[..pos].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
                let after = lower[pos + k.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
                before && after
            };
            if whole {
                out.push(pos);
            }
            start = pos + k.len();
        }
    }
    out.sort_unstable();
    out
}

fn looks_like_date(code: &str) -> bool {
    if code.len() != 8 {
        return false;
    }
    let (y, m) = (&code[..4], &code[4..6]);
    (y.starts_with("19") || y.starts_with("20")) && ("01"..="12").contains(&m)
}

fn best_in(text: &str, max_distance: usize) -> Option<(usize, String)> {
    let lower = text.to_lowercase();
    let keys = keyword_positions(&lower);
    if keys.is_empty() {
        return None;
    }
    let mut best: Option<(usize, String)> = None;
    for cap in CANDIDATE.captures_iter(&lower) {
        let m = cap.get(1)?;
        let code: String = m.as_str().chars().filter(char::is_ascii_digit).collect();
        if looks_like_date(&code) || code.chars().all(|c| c == code.chars().next().unwrap_or('0')) {
            continue;
        }
        let before_start = lower[..m.start()].char_indices().rev().nth(13).map(|(i, _)| i).unwrap_or(0);
        let before = &lower[before_start..m.start()];
        if NOT_BEFORE.iter().any(|w| before.contains(w)) {
            continue;
        }
        // Abstand zum nächsten Stichwort (davor zählt voll, danach etwas mehr)
        let dist = keys
            .iter()
            .map(|&k| if k <= m.start() { m.start() - k } else { (k - m.end()) * 2 })
            .min()
            .unwrap_or(usize::MAX);
        if dist <= max_distance && best.as_ref().is_none_or(|(d, _)| dist < *d) {
            best = Some((dist, code));
        }
    }
    best
}

/// Code aus Betreff und Text, sonst `None`.
pub fn extract(subject: &str, body: &str) -> Option<String> {
    // Im Betreff („Dein Code: 123456“) zählt jede Nähe
    if let Some((_, c)) = best_in(subject, 200) {
        return Some(c);
    }
    let body: String = body.chars().take(4000).collect();
    let subject_hint = !keyword_positions(&subject.to_lowercase()).is_empty();
    best_in(&body, if subject_hint { 400 } else { 160 }).map(|(_, c)| c)
}

/// SQL-Bedingung für mögliche Code-Nachrichten (die genaue Prüfung macht [`extract`]).
pub fn sql_condition() -> String {
    let hay = "lower(m.subject || ' ' || substr(m.text_body, 1, 4000))";
    let keys = KEYWORDS
        .iter()
        .map(|k| format!("{hay} LIKE '%{}%'", k.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("(({keys}) AND {hay} GLOB '*[0-9][0-9][0-9]*')")
}

#[cfg(test)]
mod tests {
    use super::extract;

    #[test]
    fn finds_codes() {
        assert_eq!(extract("Dein Anmeldecode: 482913", ""), Some("482913".into()));
        assert_eq!(extract("Sign in", "Your verification code is 123 456. It expires in 10 minutes."), Some("123456".into()));
        assert_eq!(extract("Bestätigung", "Ihr Sicherheitscode lautet\n\n8812-4410\n"), Some("88124410".into()));
        assert_eq!(extract("G-552211 is your Google verification code", ""), Some("552211".into()));
    }

    #[test]
    fn skips_noise() {
        assert_eq!(extract("Deine Bestellung", "Bestellnr. 123456 – Gutscheincode SOMMER"), None);
        assert_eq!(extract("Newsletter", "Hallo, hier sind die News vom 20251121."), None);
        assert_eq!(extract("Rechnung", "Kundennummer 987654"), None);
    }
}
