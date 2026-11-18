//! Nachrichten und Unterhaltungen als PDF – ein kleiner, eigener PDF-Schreiber ohne weitere
//! Abhängigkeiten: A4, Helvetica (eingebaute Standardschrift, WinAnsi – deckt Deutsch,
//! Französisch, Italienisch ab), Zeilenumbruch nach Zeichenbreiten, Seitenumbruch, Kopf je
//! Nachricht (Von, An, Cc, Datum, Anhänge) und Seitenzahlen. Zeichen ausserhalb von WinAnsi
//! werden ersetzt (z. B. Emojis).

use crate::api;
use time::{OffsetDateTime, UtcOffset};

const PAGE_W: f32 = 595.0;
const PAGE_H: f32 = 842.0;
const MARGIN: f32 = 56.0;
const TEXT_W: f32 = PAGE_W - 2.0 * MARGIN;

/// Breiten der Helvetica (1/1000 em) für ASCII 32–126.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // 32–47
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 48–63
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // 64–79
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // 80–95
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // 96–111
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, // 112–126
];

/// Zeichen → Byte in WinAnsiEncoding (cp1252); `None` = nicht darstellbar.
fn winansi(c: char) -> Option<u8> {
    let u = c as u32;
    match u {
        0x20..=0x7e => Some(u as u8),
        0xa0..=0xff => Some(u as u8),
        _ => Some(match c {
            '€' => 0x80,
            '‚' => 0x82,
            'ƒ' => 0x83,
            '„' => 0x84,
            '…' => 0x85,
            '†' => 0x86,
            '‡' => 0x87,
            'ˆ' => 0x88,
            '‰' => 0x89,
            'Š' => 0x8a,
            '‹' => 0x8b,
            'Œ' => 0x8c,
            'Ž' => 0x8e,
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '˜' => 0x98,
            '™' => 0x99,
            'š' => 0x9a,
            '›' => 0x9b,
            'œ' => 0x9c,
            'ž' => 0x9e,
            'Ÿ' => 0x9f,
            _ => return None,
        }),
    }
}

/// Ersatz für Zeichen ohne WinAnsi-Gegenstück.
fn substitute(c: char) -> &'static str {
    match c {
        '\u{2009}' | '\u{202f}' | '\u{2002}' | '\u{2003}' => " ",
        '\u{2010}' | '\u{2011}' | '\u{2212}' => "-",
        '→' => "->",
        '←' => "<-",
        '✓' | '✔' => "v",
        '\u{200b}'..='\u{200f}' | '\u{feff}' | '\u{fe0f}' | '\u{00ad}' => "",
        _ => "?",
    }
}

/// Breite eines Bytes (WinAnsi) in Punkten.
fn width(b: u8, size: f32, bold: bool) -> f32 {
    let w = match b {
        32..=126 => HELVETICA[(b - 32) as usize],
        0xc0..=0xc5 | 0xc8..=0xcb | 0xd2..=0xd6 | 0xd8..=0xdd => 700,
        0xe0..=0xe5 | 0xe8..=0xeb | 0xf2..=0xf6 | 0xf8..=0xfd | 0xff | 0xdf => 556,
        0xc6 => 1000,
        0xe6 => 889,
        0xc7 => 722,
        0xe7 => 500,
        0xcc..=0xcf | 0xec..=0xef => 278,
        0xa0 => 278,
        0x85 | 0x97 => 1000,
        0x96 => 556,
        0x91..=0x94 | 0x82 | 0x84 => 333,
        _ => 556,
    } as f32;
    w * size / 1000.0 * if bold { 1.07 } else { 1.0 }
}

fn encode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.extend_from_slice(b"    "),
            c => match winansi(c) {
                Some(b) => out.push(b),
                None => out.extend(substitute(c).bytes()),
            },
        }
    }
    out
}

fn text_width(line: &[u8], size: f32, bold: bool) -> f32 {
    line.iter().map(|b| width(*b, size, bold)).sum()
}

/// Zeile nach Breite umbrechen (an Leerzeichen, überlange Wörter hart).
fn wrap(text: &[u8], size: f32, bold: bool, max: f32) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut line_w = 0.0;
    for word in text.split(|b| *b == b' ') {
        let ww = text_width(word, size, bold);
        let space = if line.is_empty() { 0.0 } else { width(b' ', size, bold) };
        if line_w + space + ww <= max {
            if !line.is_empty() {
                line.push(b' ');
            }
            line.extend_from_slice(word);
            line_w += space + ww;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line_w = 0.0;
        }
        if ww <= max {
            line.extend_from_slice(word);
            line_w = ww;
        } else {
            for b in word {
                let w = width(*b, size, bold);
                if line_w + w > max && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    line_w = 0.0;
                }
                line.push(*b);
                line_w += w;
            }
        }
    }
    lines.push(line);
    lines
}

fn pdf_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() + 2);
    s.push('(');
    for b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                s.push('\\');
                s.push(*b as char);
            }
            32..=126 => s.push(*b as char),
            _ => s.push_str(&format!("\\{b:03o}")),
        }
    }
    s.push(')');
    s
}

/// Seiten mit Inhalt (PDF-Operatoren), Zeile für Zeile gefüllt.
struct Writer {
    pages: Vec<String>,
    current: String,
    y: f32,
}

impl Writer {
    fn new() -> Self {
        Writer { pages: Vec::new(), current: String::new(), y: PAGE_H - MARGIN }
    }

    fn new_page(&mut self) {
        let page = std::mem::take(&mut self.current);
        self.pages.push(page);
        self.y = PAGE_H - MARGIN;
    }

    fn ensure(&mut self, h: f32) {
        if self.y - h < MARGIN + 20.0 {
            self.new_page();
        }
    }

    fn line(&mut self, bytes: &[u8], x: f32, size: f32, bold: bool, gray: f32) {
        let lead = size * 1.38;
        self.ensure(lead);
        self.y -= lead;
        let font = if bold { "F2" } else { "F1" };
        self.current.push_str(&format!(
            "BT {gray:.2} g /{font} {size} Tf {x:.2} {:.2} Td {} Tj ET\n",
            self.y,
            pdf_string(bytes)
        ));
    }

    fn paragraph(&mut self, text: &str, x: f32, size: f32, bold: bool, gray: f32) {
        for raw in text.lines() {
            let enc = encode(raw.trim_end());
            for l in wrap(&enc, size, bold, TEXT_W - (x - MARGIN)) {
                self.line(&l, x, size, bold, gray);
            }
        }
    }

    fn gap(&mut self, h: f32) {
        self.y -= h;
    }

    fn rule(&mut self) {
        self.ensure(12.0);
        self.y -= 6.0;
        self.current.push_str(&format!(
            "0.8 G 0.5 w {MARGIN:.2} {y:.2} m {:.2} {y:.2} l S\n",
            PAGE_W - MARGIN,
            y = self.y
        ));
        self.y -= 8.0;
    }

    /// „Von:“ fett, Wert daneben (umbrochen).
    fn field(&mut self, label: &str, value: &str) {
        if value.trim().is_empty() {
            return;
        }
        let size = 9.5;
        let indent = MARGIN + 52.0;
        let enc = encode(value);
        let lines = wrap(&enc, size, false, TEXT_W - 52.0);
        for (i, l) in lines.iter().enumerate() {
            let lead = size * 1.38;
            self.ensure(lead);
            self.y -= lead;
            if i == 0 {
                self.current.push_str(&format!(
                    "BT 0.40 g /F2 {size} Tf {MARGIN:.2} {:.2} Td {} Tj ET\n",
                    self.y,
                    pdf_string(&encode(label))
                ));
            }
            self.current.push_str(&format!(
                "BT 0.15 g /F1 {size} Tf {indent:.2} {:.2} Td {} Tj ET\n",
                self.y,
                pdf_string(l)
            ));
        }
    }

    fn finish(mut self, title: &str) -> Vec<u8> {
        if !self.current.is_empty() || self.pages.is_empty() {
            self.new_page();
        }
        let n = self.pages.len();
        let footer_title = {
            let enc = encode(title);
            let mut l = wrap(&enc, 8.0, false, TEXT_W - 80.0).into_iter().next().unwrap_or_default();
            if l.len() < enc.len() {
                l.extend_from_slice(&[0x85]);
            }
            l
        };
        // Objekte: 1 Katalog, 2 Seiten, 3 F1, 4 F2, 5 Info, dann je Seite (Seite, Inhalt)
        let mut objects: Vec<Vec<u8>> = Vec::new();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 6 + 2 * i)).collect();
        objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
        objects.push(format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes());
        objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec());
        objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_vec());
        let mut info = b"<< /Producer (nrzMail) /Title ".to_vec();
        info.extend(pdf_string(&encode(title)).into_bytes());
        info.extend(b" >>");
        objects.push(info);
        for (i, page) in self.pages.iter().enumerate() {
            let mut content = page.clone();
            content.push_str(&format!(
                "BT 0.55 g /F1 8 Tf {MARGIN:.2} 30 Td {} Tj ET\n",
                pdf_string(&footer_title)
            ));
            let num = encode(&format!("Seite {} von {n}", i + 1));
            let w = text_width(&num, 8.0, false);
            content.push_str(&format!(
                "BT 0.55 g /F1 8 Tf {:.2} 30 Td {} Tj ET\n",
                PAGE_W - MARGIN - w,
                pdf_string(&num)
            ));
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {} 0 R >>",
                    7 + 2 * i
                )
                .into_bytes(),
            );
            let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
            stream.extend(content.into_bytes());
            stream.extend(b"\nendstream");
            objects.push(stream);
        }
        let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
            out.extend(o);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
        for off in offsets {
            out.extend(format!("{off:010} 00000 n \n").into_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .into_bytes(),
        );
        out
    }
}

fn date(ts: i64, tz_minutes: i32) -> String {
    let offset = UtcOffset::from_whole_seconds(tz_minutes.clamp(-14 * 60, 14 * 60) * 60).unwrap_or(UtcOffset::UTC);
    match OffsetDateTime::from_unix_timestamp(ts) {
        Ok(d) => {
            let d = d.to_offset(offset);
            format!(
                "{:02}.{:02}.{} {:02}:{:02}",
                d.day(),
                u8::from(d.month()),
                d.year(),
                d.hour(),
                d.minute()
            )
        }
        Err(_) => String::new(),
    }
}

/// Eine oder mehrere Nachrichten (älteste zuerst) als PDF.
pub fn render(subject: &str, messages: &[api::MessageDetail], tz_minutes: i32) -> Vec<u8> {
    let title = if subject.trim().is_empty() { "(kein Betreff)" } else { subject.trim() };
    let mut w = Writer::new();
    w.paragraph(title, MARGIN, 16.0, true, 0.1);
    if messages.len() > 1 {
        w.paragraph(
            &format!(
                "Unterhaltung mit {} Nachrichten, {} bis {}",
                messages.len(),
                messages.first().map(|m| date(m.summary.date, tz_minutes)).unwrap_or_default(),
                messages.last().map(|m| date(m.summary.date, tz_minutes)).unwrap_or_default()
            ),
            MARGIN,
            9.0,
            false,
            0.45,
        );
    }
    w.gap(6.0);
    for (i, m) in messages.iter().enumerate() {
        if i > 0 || messages.len() > 1 {
            w.rule();
        }
        if messages.len() > 1 && m.summary.subject.trim() != title {
            w.paragraph(&m.summary.subject, MARGIN, 11.0, true, 0.15);
        }
        w.field("Von", &m.summary.from.display());
        w.field("An", &api::format_addresses(&m.summary.to));
        w.field("Cc", &api::format_addresses(&m.cc));
        w.field("Datum", &date(m.summary.date, tz_minutes));
        let files: Vec<String> = m
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| a.filename.clone())
            .collect();
        w.field("Anhänge", &files.join(", "));
        w.gap(8.0);
        let body = m.text_body.trim();
        w.paragraph(if body.is_empty() { "(kein Text)" } else { body }, MARGIN, 10.0, false, 0.1);
        w.gap(4.0);
    }
    w.finish(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_valid_looking_pdf() {
        let long = "Grüezi Anna, das ist ein längerer Text mit Umlauten (äöü ÄÖÜ ß) und € – „Zitat“. ".repeat(200);
        let pdf = Writer::new();
        let mut pdf = pdf;
        pdf.paragraph(&long, MARGIN, 10.0, false, 0.0);
        let bytes = pdf.finish("Test");
        assert!(bytes.starts_with(b"%PDF-1.4"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Count 3") || text.contains("/Count 4"), "mehrere Seiten");
        assert!(text.contains("Seite 1 von"));
        assert_eq!(encode("ä€😀"), vec![0xe4, 0x80, b'?']);
        let lines = wrap(&encode(&"wort ".repeat(100)), 10.0, false, 200.0);
        assert!(lines.len() > 5 && lines.iter().all(|l| text_width(l, 10.0, false) <= 200.0));
    }
}
