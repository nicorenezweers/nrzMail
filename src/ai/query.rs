//! Aus einer Frage an den Assistenten wird eine Suche: Stichwörter (mit Wortstämmen und
//! Schreibweisen mit/ohne Umlaut), Adressen und Namen nach „von …“, ein Zeitraum aus Angaben
//! wie „heute“, „letzte Woche“, „im März“, „2024“ oder „letzte 3 Tage“ und ob nach einer
//! Menge gefragt wird („wie viele …“). Zeiträume rechnet `parse` in der übergebenen Uhrzeit;
//! der Assistent verschiebt sie in die Zeitzone des Browsers.

use super::text::{stem, variants};
use std::collections::HashSet;
use std::sync::LazyLock;
use time::{Date, Duration, Month, OffsetDateTime};

fn re(pattern: &str) -> regex::Regex {
    regex::Regex::new(pattern).expect("Regex")
}

/// Zeitraum `[from, to)` in Unix-Sekunden mit einer lesbaren Bezeichnung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    pub from: i64,
    pub to: i64,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Question {
    /// Bedeutungstragende Wörter (klein), für Textstellen um Treffer
    pub words: Vec<String>,
    /// Begriffe für die Volltextsuche (mit Stämmen und Schreibweisen)
    pub terms: Vec<String>,
    pub range: Option<Range>,
    /// E-Mail-Adressen in der Frage
    pub addresses: Vec<String>,
    /// Namen bzw. Firmen nach „von“/„from“
    pub senders: Vec<String>,
    /// Fragt nach einer Menge („wie viele“, „Anzahl“, „how many“)
    pub counting: bool,
}

impl Question {
    /// Nichts, wonach gezielt gesucht werden könnte?
    pub fn is_vague(&self) -> bool {
        self.terms.is_empty() && self.addresses.is_empty() && self.senders.is_empty()
    }
}

/// Füllwörter (Deutsch, Englisch), Wörter rund um Mails und Zeit, Monatsnamen – je Zeile durch
/// Leerzeichen getrennt.
const STOPWORDS: &[&str] = &[
    // Deutsch
    "aber alle allem allen aller alles als also am an andere anderen auch auf aus bei beim",
    "bin bis bitte bist da damit dann das dass dem den denen der des dessen die dies diese",
    "diesem diesen dieser dieses doch dort du durch ein eine einem einen einer eines einige",
    "er es etwas euch euer für gab gibt gib hab habe haben hast hat hatte hätte ich ihm ihn",
    "ihnen ihr ihre im in ins ist ja jede jedem jeden jeder jedes jetzt kann kannst kein",
    "keine können man mehr mein meine meinem meinen meiner mich mir mit muss nach nicht",
    "nichts noch nun nur ob oder ohne schon sehr sein seine sich sie sind so soll sollte",
    "sondern über um und uns unser unsere unter viel viele vom von vor war waren was weil",
    "welche welchem welchen welcher welches wem wen wenn wer werden wie wieder will wir wird",
    "wo wurde würde zu zum zur zwischen wann warum weshalb wieso wieviel wieviele anzahl oft",
    "mal denn eigentlich genau gerade irgendwas irgendwelche worum geht ging gesagt steht",
    "stand drin darin dazu wegen bezüglich betreffend thema kam kamen kommt gekommen",
    "geantwortet antwort antworten",
    // Mail-Wörter
    "mail mails email emails e-mail nachricht nachrichten post posteingang postfach",
    "geschrieben schrieb schreibt schreiben geschickt schickte gesendet gesandt bekommen",
    "bekam erhalten zeig zeige zeigen finde finden such suche suchen liste auflisten",
    "zusammenfassen zusammenfassung erzähl sag sage inbox absender absenderin",
    // Zeit
    "heute gestern vorgestern morgen woche wochen monat monate monaten monats jahr jahre",
    "jahren jahres tag tage tagen letzte letzten letzter letztes letztem vergangene",
    "vergangenen vergangener vergangenes vorige vorigen voriger voriges seit zuletzt neueste",
    "neuesten neu neue neuen neues heuer kürzlich aktuell aktuelle",
    // Englisch
    "a an and are as at be by did do does for from has have how i is it me my of on or show",
    "the this that to was what when where which who whom why with you your any all about tell",
    "list find message messages last past week weeks month months year years today yesterday",
    "day days since recent recently latest new many often count number sent received get got",
    "there were can could please summarize summary wrote write came regarding reply replied",
    // Monate
    "januar jänner februar märz maerz april mai juni juli august september oktober november",
    "dezember january february march may june july october december",
];

static STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    STOPWORDS
        .iter()
        .flat_map(|l| l.split_whitespace())
        .collect()
});

pub fn is_stopword(w: &str) -> bool {
    STOP.contains(w.to_lowercase().as_str())
}

static EMAIL: LazyLock<regex::Regex> =
    LazyLock::new(|| re(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}"));
static SENDER: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(
        r"(?i:\b(?:von|vom|from|absender(?:in)?:?)\s+(?:der\s+|dem\s+|den\s+|the\s+|meiner\s+|meinem\s+|my\s+)?)([\p{L}][\p{L}\p{N}.&\-']{1,40})(?:\s+(\p{Lu}[\p{L}\-']{1,40}))?",
    )
});
static COUNTING: LazyLock<regex::Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:wie\s*viele?|wieviele?|anzahl|how\s+many|number\s+of|wie\s+oft|how\s+often|zähle|zählen|count)\b",
    )
});

/// Frage zerlegen; `now` = jetzt (Unix-Sekunden).
pub fn parse(question: &str, now: i64) -> Question {
    let range = time_range(question, now);
    let addresses: Vec<String> = EMAIL
        .find_iter(question)
        .map(|m| m.as_str().trim_end_matches('.').to_lowercase())
        .fold(Vec::new(), |mut acc, a| {
            if !acc.contains(&a) {
                acc.push(a);
            }
            acc
        });
    let without_addresses = EMAIL.replace_all(question, " ");
    let mut senders = Vec::new();
    for c in SENDER.captures_iter(&without_addresses) {
        let first = c[1].trim_end_matches(['.', '-', '\'']);
        if first.chars().count() < 2
            || is_stopword(first)
            || first.chars().all(|ch| ch.is_numeric())
        {
            continue;
        }
        let mut name = first.to_string();
        if let Some(last) = c.get(2).map(|m| m.as_str()).filter(|w| !is_stopword(w)) {
            name = format!("{name} {last}");
        }
        if !senders
            .iter()
            .any(|s: &String| s.eq_ignore_ascii_case(&name))
        {
            senders.push(name);
        }
    }
    let year_used = range.as_ref().map(|r| year_of(r.from)).unwrap_or(-1);
    let mut words: Vec<String> = Vec::new();
    for w in without_addresses.split(|c: char| !(c.is_alphanumeric() || c == '-')) {
        let w = w.trim_matches('-').to_lowercase();
        let numeric = w.chars().all(|c| c.is_ascii_digit());
        if w.chars().count() < 3 || is_stopword(&w) {
            continue;
        }
        // Jahreszahl, die schon als Zeitraum gilt, und kurze Zahlen (Tage, Uhrzeiten) sind keine Suchbegriffe
        if numeric && (w.len() < 4 || w.parse::<i32>().ok() == Some(year_used)) {
            continue;
        }
        if !words.contains(&w) {
            words.push(w);
        }
    }
    let mut terms: Vec<String> = Vec::new();
    for w in &words {
        // Wörter mit Bindestrich: auch die Teile (die Volltextsuche trennt dort)
        let parts: Vec<&str> = std::iter::once(w.as_str())
            .chain(w.split('-').filter(|p| p.chars().count() >= 3))
            .collect();
        for p in parts {
            if p.contains('-') {
                continue;
            }
            for v in variants(p).into_iter().chain(variants(&stem(p))) {
                if !terms.contains(&v) {
                    terms.push(v);
                }
            }
        }
    }
    terms.truncate(24);
    Question {
        words,
        terms,
        range,
        addresses,
        senders,
        counting: COUNTING.is_match(question),
    }
}

/// Volltext-Abfrage (FTS5): alle Begriffe als Präfix, mit ODER verknüpft.
pub fn fts_query(terms: &[String]) -> Option<String> {
    let parts: Vec<String> = terms
        .iter()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" OR "))
}

// ------------------------------------------------------------------ Zeiträume

fn year_of(ts: i64) -> i32 {
    OffsetDateTime::from_unix_timestamp(ts)
        .map(|d| d.year())
        .unwrap_or(0)
}

fn ts(d: Date) -> i64 {
    d.midnight().assume_utc().unix_timestamp()
}

fn fmt(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day())
}

/// Zeitraum über ganze Tage `[from, to)`.
fn days(from: Date, to_exclusive: Date, label: &str) -> Range {
    let last = to_exclusive.previous_day().unwrap_or(to_exclusive);
    let label = if from == last {
        format!("{label} ({})", fmt(from))
    } else {
        format!("{label} ({} – {})", fmt(from), fmt(last))
    };
    Range {
        from: ts(from),
        to: ts(to_exclusive),
        label,
    }
}

fn month_start(year: i32, month: Month) -> Option<Date> {
    Date::from_calendar_date(year, month, 1).ok()
}

fn next_month(d: Date) -> Option<Date> {
    let (y, m) = if d.month() == Month::December {
        (d.year() + 1, Month::January)
    } else {
        (d.year(), d.month().next())
    };
    month_start(y, m)
}

fn month_named(word: &str) -> Option<Month> {
    Some(match word {
        "januar" | "jänner" | "january" => Month::January,
        "februar" | "february" => Month::February,
        "märz" | "maerz" | "march" => Month::March,
        "april" => Month::April,
        "mai" | "may" => Month::May,
        "juni" | "june" => Month::June,
        "juli" | "july" => Month::July,
        "august" => Month::August,
        "september" => Month::September,
        "oktober" | "october" => Month::October,
        "november" => Month::November,
        "dezember" | "december" => Month::December,
        _ => return None,
    })
}

fn month_name_de(m: Month) -> &'static str {
    match m {
        Month::January => "Januar",
        Month::February => "Februar",
        Month::March => "März",
        Month::April => "April",
        Month::May => "Mai",
        Month::June => "Juni",
        Month::July => "Juli",
        Month::August => "August",
        Month::September => "September",
        Month::October => "Oktober",
        Month::November => "November",
        Month::December => "Dezember",
    }
}

/// Zeitangabe in der Frage (die genaueste zuerst).
pub fn time_range(question: &str, now: i64) -> Option<Range> {
    static ISO: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b"));
    static DOTTED: LazyLock<regex::Regex> =
        LazyLock::new(|| re(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4}|\d{2})?(?:\D|$)"));
    static LAST_N: LazyLock<regex::Regex> = LazyLock::new(|| {
        re(
            r"(?i)\b(?:(?:in\s+den\s+)?(?:letzte[nr]?|vergangene[nr]?|vorige[nr]?)|(?:in\s+the\s+)?(?:last|past)|seit|since|innerhalb\s+von|within)\s+(\d{1,3}|zwei|drei|vier|fünf|sechs|sieben|acht|neun|zehn|two|three|four|five|six|seven|eight|nine|ten)\s+(tage?n?|wochen?|monate?n?|jahre?n?|days?|weeks?|months?|years?)\b",
        )
    });
    static MONTH: LazyLock<regex::Regex> = LazyLock::new(|| {
        re(
            r"(?i)\b(januar|jänner|februar|märz|maerz|april|mai|juni|juli|august|september|oktober|november|dezember|january|february|march|may|june|july|october|december)\b(?:\s+(\d{4}))?",
        )
    });
    static YEAR: LazyLock<regex::Regex> = LazyLock::new(|| re(r"\b(19[7-9]\d|20\d\d)\b"));

    let now_dt = OffsetDateTime::from_unix_timestamp(now).ok()?;
    let today = now_dt.date();
    let tomorrow = today.next_day()?;
    let q = question.to_lowercase();
    let has = |pattern: &str| re(&format!(r"(?i)\b(?:{pattern})\b")).is_match(&q);

    // Genaues Datum
    if let Some(c) = ISO.captures(&q) {
        let (y, m, d) = (
            c[1].parse().ok()?,
            c[2].parse::<u8>().ok()?,
            c[3].parse().ok()?,
        );
        if let Some(date) = Month::try_from(m)
            .ok()
            .and_then(|m| Date::from_calendar_date(y, m, d).ok())
        {
            return Some(days(date, date.next_day()?, "am"));
        }
    }
    if let Some(c) = DOTTED.captures(&q) {
        let (d, m) = (c[1].parse().ok()?, c[2].parse::<u8>().ok()?);
        let y = match c.get(3).map(|y| y.as_str()) {
            Some(y) if y.len() == 2 => 2000 + y.parse::<i32>().ok()?,
            Some(y) => y.parse().ok()?,
            None => today.year(),
        };
        if let Some(mut date) = Month::try_from(m)
            .ok()
            .and_then(|m| Date::from_calendar_date(y, m, d).ok())
        {
            if c.get(3).is_none() && date > today {
                date = date.replace_year(y - 1).ok()?;
            }
            return Some(days(date, date.next_day()?, "am"));
        }
    }

    // Letzte N Tage/Wochen/Monate/Jahre
    if let Some(c) = LAST_N.captures(&q) {
        let n: i64 = match &c[1] {
            "zwei" | "two" => 2,
            "drei" | "three" => 3,
            "vier" | "four" => 4,
            "fünf" | "five" => 5,
            "sechs" | "six" => 6,
            "sieben" | "seven" => 7,
            "acht" | "eight" => 8,
            "neun" | "nine" => 9,
            "zehn" | "ten" => 10,
            x => x.parse().ok()?,
        };
        let unit = &c[2];
        let (days_back, unit) = if unit.starts_with("tag") || unit.starts_with("day") {
            (n, "Tage")
        } else if unit.starts_with("woche") || unit.starts_with("week") {
            (n * 7, "Wochen")
        } else if unit.starts_with("monat") || unit.starts_with("month") {
            (n * 30, "Monate")
        } else {
            (n * 365, "Jahre")
        };
        let from = today - Duration::days(days_back.clamp(1, 36_500) - 1);
        return Some(days(from, tomorrow, &format!("letzte {n} {unit}")));
    }

    if has("vorgestern") {
        let d = today - Duration::days(2);
        return Some(days(d, d.next_day()?, "vorgestern"));
    }
    if has("gestern|yesterday") {
        let d = today.previous_day()?;
        return Some(days(d, today, "gestern"));
    }
    if has("heute|today|heutige[nrs]?") {
        return Some(days(today, tomorrow, "heute"));
    }

    let week_start = today - Duration::days(today.weekday().number_days_from_monday() as i64);
    if has(r"(?:letzte[nr]?|vergangene[nr]?|vorige[nr]?)\s+woche|last\s+week|previous\s+week") {
        return Some(days(
            week_start - Duration::days(7),
            week_start,
            "letzte Woche",
        ));
    }
    if has(r"diese[rn]?\s+woche|this\s+week|in\s+der\s+woche") {
        return Some(days(week_start, tomorrow, "diese Woche"));
    }

    let this_month = month_start(today.year(), today.month())?;
    if has(r"(?:letzte[nmr]?|vergangene[nmr]?|vorige[nmr]?)\s+monat|last\s+month|previous\s+month")
    {
        let prev = (this_month - Duration::days(1)).replace_day(1).ok()?;
        return Some(days(
            prev,
            this_month,
            &format!("{} {}", month_name_de(prev.month()), prev.year()),
        ));
    }
    if has(r"diese[nmr]?\s+monat|this\s+month") {
        return Some(days(this_month, tomorrow, "diesen Monat"));
    }

    let this_year = Date::from_calendar_date(today.year(), Month::January, 1).ok()?;
    if has(
        r"(?:letzte[nms]?|vergangene[nms]?|vorige[nms]?)\s+jahr|last\s+year|previous\s+year|vorjahr",
    ) {
        let prev = this_year.replace_year(today.year() - 1).ok()?;
        return Some(days(prev, this_year, &format!("{}", prev.year())));
    }
    if has(r"diese[nms]?\s+jahr|this\s+year|heuer") {
        return Some(days(this_year, tomorrow, &format!("{}", today.year())));
    }

    // Monatsnamen („im März“, „März 2025“); „may“/„march“ nur mit „in“ davor oder Jahr dahinter
    for c in MONTH.captures_iter(&q) {
        let word = c.get(1)?;
        let year = c.get(2).and_then(|y| y.as_str().parse::<i32>().ok());
        if matches!(word.as_str(), "may" | "march") && year.is_none() {
            let before = q[..word.start()].trim_end();
            if !["in", "since", "during", "of", "im", "seit"]
                .iter()
                .any(|p| before.ends_with(p))
            {
                continue;
            }
        }
        let month = month_named(word.as_str())?;
        let y = year.unwrap_or_else(|| {
            if (month as u8) > (today.month() as u8) {
                today.year() - 1
            } else {
                today.year()
            }
        });
        let start = month_start(y, month)?;
        return Some(days(
            start,
            next_month(start)?,
            &format!("{} {y}", month_name_de(month)),
        ));
    }

    if let Some(c) = YEAR.captures(&q) {
        let y: i32 = c[1].parse().ok()?;
        if y <= today.year() + 1 {
            let start = Date::from_calendar_date(y, Month::January, 1).ok()?;
            let end = start.replace_year(y + 1).ok()?;
            return Some(days(start, end, &format!("{y}")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samstag, 2026-10-31 12:00 UTC
    const NOW: i64 = 1_793_404_800 + 12 * 3600;

    fn label(q: &str) -> String {
        time_range(q, NOW).map(|r| r.label).unwrap_or_default()
    }

    #[test]
    fn time_expressions() {
        assert_eq!(label("Was kam heute?"), "heute (2026-10-31)");
        assert_eq!(label("Mails von gestern"), "gestern (2026-10-30)");
        assert_eq!(
            label("Was schrieb Anna letzte Woche?"),
            "letzte Woche (2026-10-19 – 2026-10-25)"
        );
        assert_eq!(
            label("diese Woche"),
            "diese Woche (2026-10-26 – 2026-10-31)"
        );
        assert_eq!(
            label("Rechnungen im März"),
            "März 2026 (2026-03-01 – 2026-03-31)"
        );
        assert_eq!(
            label("Rechnungen im Dezember"),
            "Dezember 2025 (2025-12-01 – 2025-12-31)"
        );
        assert_eq!(label("März 2024"), "März 2024 (2024-03-01 – 2024-03-31)");
        assert_eq!(
            label("letzten Monat"),
            "September 2026 (2026-09-01 – 2026-09-30)"
        );
        assert_eq!(label("alles aus 2024"), "2024 (2024-01-01 – 2024-12-31)");
        assert_eq!(label("letztes Jahr"), "2025 (2025-01-01 – 2025-12-31)");
        assert_eq!(
            label("in den letzten 3 Tagen"),
            "letzte 3 Tage (2026-10-29 – 2026-10-31)"
        );
        assert_eq!(
            label("last two weeks"),
            "letzte 2 Wochen (2026-10-18 – 2026-10-31)"
        );
        assert_eq!(label("am 15.10.2026"), "am (2026-10-15)");
        assert_eq!(label("2026-01-05"), "am (2026-01-05)");
        assert_eq!(label("may I see the invoice?"), "");
        assert_eq!(label("emails in may"), "Mai 2026 (2026-05-01 – 2026-05-31)");
        assert_eq!(label("Rechnung 4711"), "");
        let r = time_range("heute", NOW).unwrap();
        assert_eq!((r.from, r.to), (1_793_404_800, 1_793_404_800 + 86_400));
    }

    #[test]
    fn words_senders_and_counting() {
        let q = parse(
            "Wie viele Mails hat mir Anna Müller letzte Woche wegen der Rechnungen geschrieben?",
            NOW,
        );
        assert!(q.counting);
        assert!(q.range.is_some());
        assert_eq!(q.words, vec!["anna", "müller", "rechnungen"]);
        assert!(q.terms.contains(&"rechnung".to_string()));
        assert!(q.terms.contains(&"mueller".to_string()));
        assert!(!q.terms.contains(&"woche".to_string()));

        let q = parse(
            "Was hat Swisscom geschickt? Und was kam von peter@example.com oder von Hans Meier?",
            NOW,
        );
        assert_eq!(q.addresses, vec!["peter@example.com"]);
        assert_eq!(q.senders, vec!["Hans Meier"]);
        assert!(!q.counting);
        assert!(q.words.contains(&"swisscom".to_string()));

        let q = parse("Mails von heute", NOW);
        assert!(q.senders.is_empty() && q.is_vague());
        assert_eq!(
            parse("Rechnung 2024 Nr. 4711", NOW).words,
            vec!["rechnung", "4711"]
        );
        assert_eq!(
            fts_query(&["rechnung".into(), "mü\"ller".into()]).unwrap(),
            "\"rechnung\"* OR \"müller\"*"
        );
        assert!(fts_query(&[]).is_none());
    }
}
