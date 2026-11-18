//! HTML-Mails für die Anzeige bereinigen (ammonia): keine Skripte, Formulare, iframes,
//! Event-Handler oder `javascript:`-Links. `style`-Attribute und `<style>` bleiben (Mails
//! sind ohne sie kaum lesbar). Eingebettete Bilder (`cid:`) zeigen auf die Anhänge,
//! externe Bilder werden zu `data-src` (bis der Benutzer sie lädt), Links öffnen in
//! einem neuen Fenster ohne `opener`. Die Oberfläche zeigt das Ergebnis in einem iframe.

use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

/// Markierung für externe Bilder; wird nach dem Bereinigen zu `data-src`.
const REMOTE_MARK: &str = "nrzmail-remote-image:";

/// Bereinigtes HTML.
#[derive(Debug, Clone, PartialEq)]
pub struct Cleaned {
    pub html: String,
    pub has_remote_images: bool,
}

static REMOTE_SRC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r#"\ssrc="{REMOTE_MARK}"#)).expect("Regex"));
static CSS_REMOTE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)url\(\s*(?:&quot;|["'])?\s*(?:https?:)?//[^)]*\)"#).expect("Regex")
});
static CSS_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)@import[^;]*;?"#).expect("Regex"));
static STYLE_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)(<style[^>]*>)(.*?)(</style>)"#).expect("Regex"));

/// Externe Adressen in CSS (`url(https://…)`, `@import`) entfernen; `true`, wenn welche da waren.
fn neutralize_css(css: &str) -> (String, bool) {
    let found = CSS_REMOTE_URL.is_match(css) || CSS_IMPORT.is_match(css);
    if !found {
        return (css.to_string(), false);
    }
    let out = CSS_REMOTE_URL.replace_all(css, "url()");
    let out = CSS_IMPORT.replace_all(&out, "");
    (out.into_owned(), true)
}

/// `%40` → `@` usw. (Content-IDs in `cid:`-Adressen sind manchmal kodiert).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_remote(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://") || u.starts_with("//")
}

/// HTML einer Nachricht bereinigen. `cid_map`: Content-ID → Anhang-ID.
pub fn clean(html: &str, cid_map: &HashMap<String, String>) -> Cleaned {
    let remote = Arc::new(AtomicBool::new(false));
    let cids = cid_map.clone();
    let flag = remote.clone();

    let mut b = ammonia::Builder::default();
    b.add_tags([
        "style",
        "font",
        "center",
        "span",
        "div",
        "section",
        "header",
        "footer",
        "article",
        "main",
        "nav",
        "figure",
        "figcaption",
        "picture",
        "caption",
        "col",
        "colgroup",
        "big",
        "small",
        "u",
        "s",
        "mark",
    ])
    .rm_clean_content_tags(["style"])
    .add_clean_content_tags(["title", "head"])
    .add_generic_attributes([
        "style", "class", "align", "valign", "bgcolor", "width", "height", "dir", "lang", "title",
        "border",
    ])
    .add_tag_attributes("font", ["color", "face", "size"])
    .add_tag_attributes(
        "table",
        [
            "cellpadding",
            "cellspacing",
            "border",
            "width",
            "bgcolor",
            "role",
        ],
    )
    .add_tag_attributes("td", ["colspan", "rowspan", "nowrap", "width", "height"])
    .add_tag_attributes("th", ["colspan", "rowspan", "nowrap", "width", "height"])
    .add_tag_attributes("img", ["border", "hspace", "vspace"])
    .add_tag_attributes("a", ["name"])
    .add_url_schemes(["cid", "data"])
    .set_tag_attribute_value("a", "target", "_blank")
    .link_rel(Some("noopener noreferrer"))
    .strip_comments(true)
    .attribute_filter(
        move |element, attribute, value| match (element, attribute) {
            ("img", "src") => {
                let v = value.trim();
                let lower = v.to_ascii_lowercase();
                if lower.starts_with("cid:") {
                    let cid = percent_decode(v[4..].trim().trim_matches(['<', '>']));
                    let id = cids.get(&cid).or_else(|| {
                        cids.iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case(&cid))
                            .map(|(_, id)| id)
                    });
                    return id
                        .map(|id| Cow::Owned(format!("/api/v1/attachments/{id}?inline=true")));
                }
                if lower.starts_with("data:image/") && !lower.starts_with("data:image/svg") {
                    return Some(Cow::Owned(v.to_string()));
                }
                if is_remote(v) {
                    flag.store(true, Ordering::Relaxed);
                    let url = if v.starts_with("//") {
                        format!("https:{v}")
                    } else {
                        v.to_string()
                    };
                    return Some(Cow::Owned(format!("{REMOTE_MARK}{url}")));
                }
                None
            }
            (_, "src") => None,
            (_, "href") => {
                let lower = value.trim().to_ascii_lowercase();
                if lower.starts_with("http://")
                    || lower.starts_with("https://")
                    || lower.starts_with("mailto:")
                    || lower.starts_with("tel:")
                    || lower.starts_with('#')
                {
                    Some(Cow::Owned(value.to_string()))
                } else {
                    None
                }
            }
            (_, "style") => {
                let (css, found) = neutralize_css(value);
                if found {
                    flag.store(true, Ordering::Relaxed);
                }
                Some(Cow::Owned(css))
            }
            _ => Some(Cow::Owned(value.to_string())),
        },
    );
    let cleaned = b.clean(html).to_string();

    // Externe Bilder: src → data-src
    let out = REMOTE_SRC.replace_all(&cleaned, " data-src=\"");
    // <style>-Blöcke: externe Adressen entfernen
    let mut style_remote = false;
    let out = STYLE_BLOCK.replace_all(&out, |c: &regex::Captures<'_>| {
        let (css, found) = neutralize_css(&c[2]);
        style_remote |= found;
        format!("{}{}{}", &c[1], css, &c[3])
    });
    Cleaned {
        html: out.into_owned(),
        has_remote_images: remote.load(Ordering::Relaxed) || style_remote,
    }
}

/// Text als einfaches HTML (für Mails ohne HTML-Teil, z. B. beim Weiterleiten).
pub fn text_to_html(text: &str) -> String {
    let esc = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!("<pre style=\"white-space:pre-wrap;font-family:inherit\">{esc}</pre>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(html: &str) -> Cleaned {
        let mut cids = HashMap::new();
        cids.insert("logo@x".to_string(), "att-1".to_string());
        clean(html, &cids)
    }

    #[test]
    fn removes_scripts_forms_and_handlers() {
        let c = run(
            r#"<p onclick="alert(1)">Hallo</p><script>alert(2)</script><form action="https://x"><input name=a></form><iframe src="https://evil"></iframe><a href="javascript:alert(3)">x</a><object data="x"></object>"#,
        );
        let h = &c.html;
        for bad in [
            "onclick",
            "<script",
            "alert",
            "<form",
            "<input",
            "<iframe",
            "javascript:",
            "<object",
        ] {
            assert!(!h.contains(bad), "{bad} in {h}");
        }
        assert!(h.contains("<p>Hallo</p>"), "{h}");
        assert!(!c.has_remote_images);
    }

    #[test]
    fn keeps_styles_and_rewrites_images_and_links() {
        let c = run(
            r#"<html><head><title>Titel</title><style>.a{color:red;background:url(https://track.example/x.gif)} @import url(https://x/y.css);</style></head><body><table cellpadding="0" style="width:100%"><tr><td class="a" style="color:#333">Text</td></tr></table><img src="cid:logo@x" alt="Logo"><img src="https://img.example/p.png?a=1&amp;b=2" width="1"><a href="https://example.com">Link</a></body></html>"#,
        );
        let h = &c.html;
        assert!(c.has_remote_images);
        assert!(h.contains("<style>"), "{h}");
        assert!(h.contains("color:red"), "{h}");
        assert!(
            !h.contains("track.example") && !h.contains("@import"),
            "{h}"
        );
        assert!(h.contains(r#"style="color:#333""#), "{h}");
        assert!(h.contains(r#"cellpadding="0""#), "{h}");
        assert!(
            h.contains(r#"src="/api/v1/attachments/att-1?inline=true""#),
            "{h}"
        );
        assert!(
            h.contains(r#"data-src="https://img.example/p.png?a=1&amp;b=2""#),
            "{h}"
        );
        assert!(!h.contains(r#" src="https://img"#), "{h}");
        assert!(
            h.contains(r#"target="_blank""#) && h.contains("noopener"),
            "{h}"
        );
        assert!(!h.contains("Titel"), "{h}");
    }

    #[test]
    fn remote_css_in_style_attribute_counts() {
        let c = run(r#"<div style="background-image:url('http://x/y.png')">a</div>"#);
        assert!(c.has_remote_images);
        assert!(!c.html.contains("http://x"), "{}", c.html);
        let c = run(
            r#"<img src="cid:unbekannt"><img src="data:image/png;base64,AAAA"><img src="data:image/svg+xml;base64,AAAA">"#,
        );
        assert!(!c.html.contains("unbekannt"));
        assert!(c.html.contains("data:image/png"));
        assert!(!c.html.contains("svg"));
    }
}
