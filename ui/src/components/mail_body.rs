//! Inhalt einer Nachricht. HTML (vom Server bereinigt) läuft in einem `iframe` mit `srcdoc`
//! und `sandbox` **ohne** `allow-scripts`; Links öffnen in einem neuen Tab (`<base target>`),
//! die Höhe folgt dem Inhalt, der Hintergrund bleibt weiss. Externe Bilder kommen vom Server
//! als `data-src` und werden erst auf Wunsch nach `src` getauscht. Text-Mails erscheinen
//! vorformatiert mit klickbaren Links und gedämpften Zitaten.

use crate::components::compose;
use dioxus::prelude::*;

/// Grundstil im Rahmen – hell, wie eine Mail im Mailprogramm.
const FRAME_CSS: &str = "html{background:#fff;color:#1f1f1f;color-scheme:light}\
body{margin:0;padding:20px 22px;font:14px/1.55 Inter,-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;overflow-wrap:break-word}\
img{max-width:100%;height:auto}\
pre{white-space:pre-wrap}\
blockquote{margin:0 0 0 .8ex;border-left:2px solid #d4d4d4;padding-left:1ex;color:#525252}\
a{color:#1d4ed8}\
@media (max-width:520px){body{padding:14px 12px;font-size:15px}}";

/// Vollständiges Dokument für `srcdoc`.
fn document(html: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"light\">\
         <base target=\"_blank\"><style>{FRAME_CSS}</style></head><body>{html}</body></html>"
    )
}

/// Höhe anpassen, Links absichern, auf Wunsch externe Bilder laden – vom Elternfenster aus
/// (im Rahmen selbst laufen keine Skripte).
fn wire_frame(id: &str, load_images: bool) {
    let js = format!(
        r#"
const f = document.getElementById({id:?});
if (f && !f.__nrzWired) {{
  f.__nrzWired = true;
  const fit = () => {{
    try {{
      const d = f.contentDocument;
      if (!d || !d.body) return;
      // Höhe des Inhalts (nicht scrollHeight: das wüchse mit dem Rahmen mit)
      const root = d.documentElement;
      // Breite Mails (feste Tabellen, 600 px) wie im Mailprogramm auf die Breite verkleinern;
      // bei neuer Rahmenbreite (Drehen) neu messen
      const b = d.body;
      if (f.__nrzW !== f.clientWidth) {{ f.__nrzW = f.clientWidth; b.style.zoom = ''; }}
      const z = parseFloat(b.style.zoom) || 1;
      const natural = root.scrollWidth / z;
      const want = natural > root.clientWidth + 2 ? Math.max(0.5, root.clientWidth / natural) : 1;
      if (Math.abs(want - z) > 0.01) b.style.zoom = want === 1 ? '' : String(want);
      let h = Math.ceil(root.getBoundingClientRect().height);
      if (root.scrollWidth > root.clientWidth + 1) h += 16;
      if (Math.abs((parseInt(f.style.height, 10) || 0) - h) > 1) f.style.height = h + 'px';
    }} catch (e) {{}}
  }};
  const setup = () => {{
    const d = f.contentDocument;
    if (!d || !d.body || !d.body.childNodes.length) return;
    d.querySelectorAll('a[href]').forEach(a => {{ a.target = '_blank'; a.rel = 'noopener noreferrer'; }});
    if (f.dataset.images === 'on') swap(d);
    d.querySelectorAll('img').forEach(i => i.addEventListener('load', fit));
    try {{ new ResizeObserver(fit).observe(d.documentElement); }} catch (e) {{}}
    fit();
    setTimeout(fit, 250);
    setTimeout(fit, 1200);
  }};
  f.__nrzSwap = () => {{ const d = f.contentDocument; if (d) {{ swap(d); setTimeout(fit, 300); }} }};
  const swap = (d) => {{
    d.querySelectorAll('[data-src]').forEach(el => {{ el.setAttribute('src', el.getAttribute('data-src')); el.removeAttribute('data-src'); }});
    d.querySelectorAll('[data-srcset]').forEach(el => {{ el.setAttribute('srcset', el.getAttribute('data-srcset')); el.removeAttribute('data-srcset'); }});
  }};
  f.addEventListener('load', setup);
  try {{ new ResizeObserver(fit).observe(f); }} catch (e) {{}}
  setup();
}}
if (f) {{
  f.dataset.images = {images:?};
  if ({images:?} === 'on' && f.__nrzSwap) f.__nrzSwap();
}}
"#,
        images = if load_images { "on" } else { "off" },
    );
    document::eval(&js);
}

#[component]
pub fn MailBody(
    #[props(into)] id: String,
    html: Option<String>,
    #[props(into)] text: String,
    load_images: bool,
) -> Element {
    match html.filter(|h| !h.trim().is_empty()) {
        Some(h) => rsx! { HtmlFrame { id, html: h, load_images } },
        None => rsx! { TextBody { text } },
    }
}

#[component]
fn HtmlFrame(id: String, html: String, load_images: bool) -> Element {
    let frame_id = format!("frame-{id}");
    let doc = document(&html);
    let fid = frame_id.clone();
    // Bilder-Schalter nach dem ersten Verdrahten weiterreichen
    let fid2 = frame_id.clone();
    use_effect(use_reactive!(|load_images| wire_frame(&fid2, load_images)));
    rsx! {
        div { class: "overflow-hidden rounded-lg bg-white",
            iframe {
                id: "{frame_id}",
                class: "block w-full border-0 bg-white",
                style: "height: 160px",
                title: "Inhalt der Nachricht",
                // Nie allow-scripts: Mails dürfen nichts ausführen
                "sandbox": "allow-same-origin allow-popups allow-popups-to-escape-sandbox",
                referrerpolicy: "no-referrer",
                srcdoc: "{doc}",
                onmounted: move |_| wire_frame(&fid, load_images),
            }
        }
    }
}

/// Stück Text oder Link.
enum Piece {
    Text(String),
    Link(String),
}

/// Links in einem Text finden (http, https, www., mailto:).
fn pieces(line: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut rest = line;
    loop {
        let found = ["https://", "http://", "mailto:", "www."]
            .iter()
            .filter_map(|p| rest.find(p).map(|i| (i, *p)))
            // www. nur am Wortanfang
            .filter(|(i, p)| *p != "www." || *i == 0 || !rest[..*i].ends_with(|c: char| c.is_alphanumeric() || c == '/' || c == '.'))
            .min_by_key(|(i, _)| *i);
        let Some((start, _)) = found else {
            if !rest.is_empty() {
                out.push(Piece::Text(rest.to_string()));
            }
            break;
        };
        let tail = &rest[start..];
        let mut end = tail.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '«' | '»')).unwrap_or(tail.len());
        // Satzzeichen am Ende gehören nicht zum Link
        while end > 0 && tail[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', ']', '\'']) {
            let last = tail[..end].chars().last().map(char::len_utf8).unwrap_or(1);
            end -= last;
        }
        if end <= 8 {
            out.push(Piece::Text(rest[..start + end.max(1)].to_string()));
            rest = &rest[start + end.max(1)..];
            continue;
        }
        if start > 0 {
            out.push(Piece::Text(rest[..start].to_string()));
        }
        out.push(Piece::Link(tail[..end].to_string()));
        rest = &tail[end..];
    }
    out
}

fn linked(text: &str) -> Element {
    let parts = pieces(text);
    rsx! {
        for p in parts {
            match p {
                Piece::Text(t) => rsx! { "{t}" },
                Piece::Link(l) if l.starts_with("mailto:") => {
                    let url = l.clone();
                    rsx! {
                        a { href: "{l}", class: "text-foreground",
                            onclick: move |e| { e.prevent_default(); compose::open_mailto(&url); },
                            {l.trim_start_matches("mailto:").to_string()}
                        }
                    }
                }
                Piece::Link(l) => {
                    let href = if l.starts_with("www.") { format!("https://{l}") } else { l.clone() };
                    rsx! { a { href: "{href}", target: "_blank", rel: "noopener noreferrer", class: "text-foreground", "{l}" } }
                }
            }
        }
    }
}

#[component]
fn TextBody(text: String) -> Element {
    // Zitate (Zeilen mit „>“) zu Blöcken bündeln
    let mut blocks: Vec<(bool, String)> = Vec::new();
    for line in text.trim_end().lines() {
        let quoted = line.trim_start().starts_with('>');
        match blocks.last_mut() {
            Some((q, s)) if *q == quoted => {
                s.push('\n');
                s.push_str(line);
            }
            _ => blocks.push((quoted, line.to_string())),
        }
    }
    if blocks.is_empty() {
        return rsx! { p { class: "text-sm italic text-muted-foreground", "(kein Text)" } };
    }
    rsx! {
        div { class: "mail-text px-1",
            for (i , (quoted , chunk)) in blocks.into_iter().enumerate() {
                if quoted {
                    div { key: "{i}", class: "my-1 border-l-2 pl-3 text-muted-foreground", {linked(&chunk)} }
                } else {
                    div { key: "{i}", {linked(&chunk)} }
                }
            }
        }
    }
}
