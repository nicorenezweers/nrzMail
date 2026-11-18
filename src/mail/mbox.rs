//! mbox lesen und schreiben (mboxrd): Nachrichten beginnen mit einer `From `-Zeile; Zeilen
//! im Text, die mit `From ` (oder `>From `, `>>From ` …) beginnen, bekommen beim Schreiben
//! ein `>` vorangestellt, das beim Lesen wieder entfernt wird.

use std::io::{self, BufRead, Write};

/// Eine Nachricht aus der mbox mit dem Datum ihrer `From `-Zeile.
#[derive(Debug, Clone, PartialEq)]
pub struct MboxEntry {
    pub raw: Vec<u8>,
    /// Aus der `From `-Zeile (UTC), falls lesbar
    pub date: Option<i64>,
}

/// Liest Nachrichten nacheinander aus einer mbox-Datei (ohne alles in den Speicher zu laden).
pub struct MboxReader<R: BufRead> {
    reader: R,
    current: Vec<u8>,
    date: Option<i64>,
    started: bool,
    prev_blank: bool,
    done: bool,
}

impl<R: BufRead> MboxReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            current: Vec::new(),
            date: None,
            started: false,
            prev_blank: true,
            done: false,
        }
    }

    fn take(&mut self) -> MboxEntry {
        let mut msg = std::mem::take(&mut self.current);
        // Die Leerzeile vor der nächsten From_-Zeile gehört zum Trenner.
        if msg.ends_with(b"\r\n\r\n") {
            msg.truncate(msg.len() - 2);
        } else if msg.ends_with(b"\n\n") {
            msg.truncate(msg.len() - 1);
        }
        MboxEntry {
            raw: msg,
            date: self.date.take(),
        }
    }
}

/// Datum aus `From absender Sat Feb  1 12:00:00 2025` (asctime, UTC).
pub fn separator_date(line: &[u8]) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let text = String::from_utf8_lossy(line);
    let parts: Vec<&str> = text.split_whitespace().skip(2).collect();
    let month_pos = parts
        .iter()
        .position(|p| MONTHS.contains(&p.to_ascii_lowercase().as_str()))?;
    let month = MONTHS
        .iter()
        .position(|m| *m == parts[month_pos].to_ascii_lowercase())? as u8
        + 1;
    let day: u8 = parts.get(month_pos + 1)?.parse().ok()?;
    let mut hms = parts
        .get(month_pos + 2)?
        .split(':')
        .map(|v| v.parse::<u8>().ok());
    let (h, mi, sec) = (
        hms.next()??,
        hms.next()??,
        hms.next().flatten().unwrap_or(0),
    );
    let year: i32 = parts[month_pos + 3..]
        .iter()
        .find(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_digit()))?
        .parse()
        .ok()?;
    let date =
        time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    let t = time::Time::from_hms(h, mi, sec).ok()?;
    Some(
        time::PrimitiveDateTime::new(date, t)
            .assume_utc()
            .unix_timestamp(),
    )
}

/// `From absender datum` – Anfang einer neuen Nachricht?
fn is_separator(line: &[u8]) -> bool {
    line.starts_with(b"From ") && line.iter().any(u8::is_ascii_digit)
}

fn is_blank(line: &[u8]) -> bool {
    line == b"\n" || line == b"\r\n"
}

/// `>From ` → `From `, `>>From ` → `>From ` usw.
fn unquote(line: &[u8]) -> &[u8] {
    let gt = line.iter().take_while(|b| **b == b'>').count();
    if gt > 0 && line[gt..].starts_with(b"From ") {
        &line[1..]
    } else {
        line
    }
}

impl<R: BufRead> Iterator for MboxReader<R> {
    type Item = io::Result<MboxEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut line = Vec::new();
        loop {
            line.clear();
            match self.reader.read_until(b'\n', &mut line) {
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
                Ok(0) => {
                    self.done = true;
                    let msg = self.take();
                    return (!msg.raw.iter().all(u8::is_ascii_whitespace)).then_some(Ok(msg));
                }
                Ok(_) => {}
            }
            if is_separator(&line) && self.prev_blank {
                self.prev_blank = false;
                let date = separator_date(&line);
                if self.started && !self.current.is_empty() {
                    let msg = self.take();
                    self.date = date;
                    return Some(Ok(msg));
                }
                self.date = date;
                self.started = true;
                continue;
            }
            self.started = true;
            self.prev_blank = is_blank(&line);
            self.current.extend_from_slice(unquote(&line));
        }
    }
}

/// Datum im Format von `asctime`: `Mon Jan  1 09:05:00 2024` (UTC).
pub fn asctime(ts: i64) -> String {
    let dt =
        time::OffsetDateTime::from_unix_timestamp(ts).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        DAYS[dt.weekday().number_days_from_monday() as usize],
        MONTHS[dt.month() as usize - 1],
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.year()
    )
}

/// Eine Nachricht an eine mbox anhängen.
pub fn write_message<W: Write>(w: &mut W, raw: &[u8], sender: &str, date: i64) -> io::Result<()> {
    let sender: String = sender
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    let sender = if sender.is_empty() {
        "MAILER-DAEMON".to_string()
    } else {
        sender
    };
    writeln!(w, "From {sender} {}", asctime(date))?;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let gt = line.iter().take_while(|b| **b == b'>').count();
        if line[gt..].starts_with(b"From ") {
            w.write_all(b">")?;
        }
        w.write_all(line)?;
    }
    if !raw.ends_with(b"\n") {
        w.write_all(b"\n")?;
    }
    w.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_messages_and_unquotes() {
        let mbox = b"From anna@example.ch Mon Jan  1 09:00:00 2024\nSubject: Eins\n\nHallo\n>From vorne\n\nFrom bruno@example.ch Tue Jan  2 10:00:00 2024\nSubject: Zwei\n\nText\n>>From doppelt\n";
        let entries: Vec<MboxEntry> = MboxReader::new(&mbox[..]).map(Result::unwrap).collect();
        assert_eq!(entries[0].date, Some(1_704_099_600), "2024-01-01 09:00 UTC");
        let msgs: Vec<Vec<u8>> = entries.into_iter().map(|e| e.raw).collect();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0], b"Subject: Eins\n\nHallo\nFrom vorne\n");
        assert_eq!(msgs[1], b"Subject: Zwei\n\nText\n>From doppelt\n");
    }

    #[test]
    fn from_in_body_without_blank_line_is_not_a_separator() {
        let mbox =
            b"From a@b.ch Mon Jan  1 09:00:00 2024\nSubject: x\n\nZeile\nFrom 2024 an gilt\n";
        let msgs: Vec<_> = MboxReader::new(&mbox[..]).collect();
        assert_eq!(msgs.len(), 1);
        // Ohne From_-Zeile am Anfang: die ganze Datei ist eine Nachricht
        let msgs: Vec<_> = MboxReader::new(&b"Subject: y\n\nText\n"[..]).collect();
        assert_eq!(msgs.len(), 1);
        assert_eq!(MboxReader::new(&b""[..]).count(), 0);
    }

    #[test]
    fn round_trip() {
        let originals: Vec<&[u8]> = vec![
            b"Subject: A\r\n\r\nFrom the start\r\n>From quoted\r\n\r\n",
            b"Subject: B\n\nletzte Zeile\n",
            b"Subject: C\n\n\n\nmit Leerzeilen\n\n",
        ];
        let mut out = Vec::new();
        for (i, m) in originals.iter().enumerate() {
            write_message(&mut out, m, "x@y.ch", 1_700_000_000 + i as i64).unwrap();
        }
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.starts_with("From x@y.ch Tue Nov 14 22:13:20 2023\n"),
            "{text}"
        );
        assert!(text.contains("\n>From the start") && text.contains("\n>>From quoted"));
        let back: Vec<Vec<u8>> = MboxReader::new(&out[..]).map(|e| e.unwrap().raw).collect();
        assert_eq!(
            separator_date(b"From x@y.ch Tue Nov 14 22:13:20 2023\n"),
            Some(1_700_000_000)
        );
        assert_eq!(separator_date(b"From x@y.ch irgendwas\n"), None);
        assert_eq!(back.len(), 3);
        for (a, b) in originals.iter().zip(&back) {
            assert_eq!(*a, &b[..], "{}", String::from_utf8_lossy(b));
        }
    }
}
