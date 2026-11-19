//! Was die App selbst ins Fenster bringt: Seiten der App (Start, Verbinden, Server nicht
//! erreichbar) und die Skripte, die jede Seite der Oberfläche bekommt (Titelleiste, Milchglas,
//! App-Gefühl). Die Seiten sprechen mit der App über `window.ipc.postMessage` (siehe `main.rs`).

/// Welche Seite der App.
pub enum Page<'a> {
    /// Lokaler Server startet bzw. Verbindung wird geprüft
    Loading(&'a str),
    /// Willkommen / mit einem Server verbinden (`current`: bisherige Adresse)
    Connect { current: Option<&'a str>, error: Option<&'a str>, first: bool },
    /// Server nicht erreichbar
    Offline { server: &'a str, error: &'a str },
    /// Fehler ohne Ausweg (lokaler Server startet nicht)
    Failed(&'a str),
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// JSON-String für eingebettetes JavaScript.
pub fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

const PAGE_CSS: &str = r#"
:root { color-scheme: dark; }
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; font: 14px -apple-system, BlinkMacSystemFont, "Inter", sans-serif; color: #f4f4f5; background: transparent; -webkit-user-select: none; cursor: default; overflow: hidden; }
html:not(.nrz-glass), html:not(.nrz-glass) body { background: #18181b; }
.drag { position: fixed; inset: 0 0 auto 0; height: 52px; }
.wrap { height: 100%; display: flex; align-items: center; justify-content: center; padding: 56px 24px 24px; }
.card { width: min(440px, 100%); padding: 32px 30px 26px; border-radius: 22px; background: rgba(28,28,32,.42); border: 1px solid rgba(255,255,255,.12); box-shadow: 0 30px 60px -30px rgba(0,0,0,.6); -webkit-backdrop-filter: blur(24px) saturate(160%); }
html:not(.nrz-glass) .card { background: #222226; }
.logo { width: 56px; height: 56px; border-radius: 14px; display: block; margin: 0 auto 14px; box-shadow: 0 10px 24px -10px rgba(0,0,0,.6); }
h1 { margin: 0; text-align: center; font-size: 21px; font-weight: 650; letter-spacing: -.01em; }
p.sub { margin: 8px 0 22px; text-align: center; color: #a1a1aa; line-height: 1.5; }
label { display: block; font-size: 12px; font-weight: 600; color: #d4d4d8; margin: 0 0 6px 2px; }
input { width: 100%; height: 38px; padding: 0 12px; border-radius: 10px; border: 1px solid rgba(255,255,255,.14); background: rgba(255,255,255,.06); color: #fafafa; font: inherit; outline: none; -webkit-user-select: text; }
input:focus { border-color: rgba(255,255,255,.34); background: rgba(255,255,255,.09); }
input::placeholder { color: #71717a; }
.hint { margin: 7px 2px 0; font-size: 12px; color: #8b8b94; line-height: 1.45; }
button { width: 100%; height: 38px; margin-top: 14px; border: 0; border-radius: 10px; font: inherit; font-weight: 600; cursor: default; }
.primary { background: #ececee; color: #18181b; }
.primary:hover { background: #fff; }
.primary:disabled { opacity: .6; }
.ghost { background: rgba(255,255,255,.07); color: #e4e4e7; border: 1px solid rgba(255,255,255,.1); }
.ghost:hover { background: rgba(255,255,255,.12); }
.or { display: flex; align-items: center; gap: 10px; margin: 18px 0 2px; color: #71717a; font-size: 12px; }
.or::before, .or::after { content: ""; flex: 1; height: 1px; background: rgba(255,255,255,.1); }
.err { min-height: 18px; margin: 10px 2px 0; font-size: 12.5px; color: #fca5a5; line-height: 1.45; -webkit-user-select: text; }
.err:empty { display: none; }
.badge { display: inline-block; margin-left: 6px; padding: 1px 7px; border-radius: 999px; background: rgba(255,255,255,.1); color: #d4d4d8; font-size: 10.5px; font-weight: 600; vertical-align: 1px; }
.mono { font-family: ui-monospace, "SF Mono", Menlo, monospace; color: #d4d4d8; -webkit-user-select: text; }
.spin { width: 22px; height: 22px; margin: 0 auto 14px; border-radius: 50%; border: 2px solid currentColor; border-right-color: transparent; animation: r .8s linear infinite; opacity: .6; }
.center { text-align: center; color: #a1a1aa; }
@keyframes r { to { transform: rotate(360deg); } }
"#;

const PAGE_JS: &str = r#"
function send(m) { try { window.ipc.postMessage(m); } catch (e) {} }
window.__nrzConnectState = function (status, text) {
  var b = document.getElementById('go'), e = document.getElementById('err');
  if (b) { b.disabled = status === 'busy'; b.textContent = status === 'busy' ? 'Verbinde …' : 'Verbinden'; }
  if (e) e.textContent = status === 'error' ? text : '';
};
function connect(ev) {
  if (ev) ev.preventDefault();
  var v = document.getElementById('url').value;
  window.__nrzConnectState('busy', '');
  send('connect:' + v);
}
document.addEventListener('DOMContentLoaded', function () {
  var u = document.getElementById('url');
  if (u) { u.focus(); u.select(); }
});
"#;

/// Seite der App (geladen mit `load_html`).
pub fn page(p: Page<'_>) -> String {
    let body = match p {
        Page::Loading(text) => format!(r#"<div class="card"><div class="spin"></div><div class="center">{}</div></div>"#, html_escape(text)),
        Page::Failed(e) => format!(
            r#"<div class="card"><h1>nrzMail konnte nicht starten</h1><p class="sub mono">{}</p>
<button class="ghost" onclick="send('connect:show')">Mit einem Server verbinden …</button></div>"#,
            html_escape(e)
        ),
        Page::Connect { current, error, first } => {
            let value = html_escape(current.unwrap_or(""));
            let title = if first { "Willkommen bei nrzMail" } else { "Mit nrzMail-Server verbinden" };
            let sub = "Verbinde die App mit deinem nrzMail-Server – Postfächer, Mails und Einstellungen kommen von dort, nichts muss neu eingerichtet werden.";
            let local = if first { "Ohne Server verwenden" } else { "Lokal verwenden (ohne Server)" };
            format!(
                r#"<form class="card" onsubmit="connect(event)">
<img class="logo" src="data:image/png;base64,{icon}" alt="">
<h1>{title}</h1>
<p class="sub">{sub}</p>
<label for="url">Adresse des Servers <span class="badge">empfohlen</span></label>
<input id="url" type="text" spellcheck="false" autocomplete="off" autocapitalize="off" placeholder="z. B. umbrel.local:8360" value="{value}">
<div class="hint">Wie im Browser, z. B. <span class="mono">http://umbrel.local:8360</span> oder <span class="mono">https://mail.example.ch</span>. Angemeldet wirst du einmal – danach merkt sich die App die Sitzung.</div>
<div class="err" id="err">{err}</div>
<button class="primary" id="go" type="submit">Verbinden</button>
<div class="or">oder</div>
<button class="ghost" type="button" onclick="send('local')">{local}</button>
<div class="hint" style="text-align:center">Lokal speichert die App Postfächer und Mails nur auf diesem Mac.</div>
</form>"#,
                icon = icon_b64(),
                err = html_escape(error.unwrap_or("")),
            )
        }
        Page::Offline { server, error } => format!(
            r#"<div class="card">
<img class="logo" src="data:image/png;base64,{icon}" alt="">
<h1>Server nicht erreichbar</h1>
<p class="sub"><span class="mono">{server}</span><br>{error}</p>
<div class="err" id="err"></div>
<button class="primary" id="go" onclick="window.__nrzConnectState('busy','');send('retry')">Erneut versuchen</button>
<button class="ghost" onclick="send('local')">Lokal weiterarbeiten</button>
<button class="ghost" onclick="send('connect:show')">Anderen Server …</button>
<div class="hint" style="text-align:center">Die App versucht es alle 20 Sekunden selbst wieder.</div>
</div>
<script>setInterval(function () {{ send('retry:quiet'); }}, 20000);</script>"#,
            icon = icon_b64(),
            server = html_escape(server),
            error = html_escape(error),
        ),
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>{PAGE_CSS}</style><script>{PAGE_JS}</script></head>
<body><div class="drag" data-drag></div><div class="wrap">{body}</div></body></html>"#
    )
}

fn icon_b64() -> &'static str {
    use std::sync::OnceLock;
    static ICON: OnceLock<String> = OnceLock::new();
    ICON.get_or_init(|| b64(include_bytes!("../../static/icon.png")))
}

/// Base64 (Standard-Alphabet) – für das Symbol auf den Seiten der App.
fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Titelleiste der App in der Oberfläche (Klasse `nrz-mac` – Platz für die Ampel), Fenster an
/// `[data-drag]` und an den Kopfzeilen ziehen bzw. mit Doppelklick zoomen, native Mitteilungen.
pub const WINDOW_SCRIPT: &str = r#"
(function () {
  if (window.__nrzNative) return;
  function send(m) { try { window.ipc.postMessage(m); } catch (e) {} }
  function mark() { document.documentElement.classList.add('nrz-mac'); }
  function watch() {
    mark();
    new MutationObserver(function () { if (!document.documentElement.classList.contains('nrz-mac')) mark(); })
      .observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
  }
  if (document.documentElement) watch(); else document.addEventListener('DOMContentLoaded', watch);
  var INTERACTIVE = 'button,a,input,select,textarea,label,summary,[role=button],[role=tab],[role=menuitem],[role=listitem],[contenteditable=true],[contenteditable=""]';
  // Kopfzeilen der Oberfläche (Seitenleiste oben, Kopf der Liste, Kopf der Seiten)
  var HEADS = '[data-drag],.nrz-titlebar,main .h-14.shrink-0,main .h-12.shrink-0,aside .h-14.shrink-0';
  function dragZone(e) {
    if (e.button !== 0 || !e.target.closest || e.target.closest(INTERACTIVE)) return false;
    if (e.target.closest(HEADS)) return true;
    // Seiten ohne Kopfzeile (Anmeldung, Laden): oberster Streifen
    return e.clientY < 32;
  }
  window.addEventListener('mousedown', function (e) {
    if (!dragZone(e)) return;
    if (e.detail === 2) { send('window:zoom'); return; }
    send('window:drag');
  }, true);
  window.__nrzNative = {
    app: 'nrzMail',
    notify: function (title, body) { send('notify:' + JSON.stringify({ title: String(title || ''), body: String(body || '') })); }
  };
})();
"#;

/// Milchglas hinter dem Fenster: `html.nrz-glass` schon vor dem ersten Zeichnen, damit nichts
/// deckend aufblitzt (die Oberfläche tönt dann nur noch, siehe `ui/tailwind.css`).
pub const GLASS_SCRIPT: &str = r#"
(function () {
  window.__nrzGlass = true;
  function mark() { document.documentElement.classList.add('nrz-glass'); }
  function watch() {
    mark();
    new MutationObserver(function () { if (window.__nrzGlass && !document.documentElement.classList.contains('nrz-glass')) mark(); })
      .observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
  }
  if (document.documentElement) watch(); else document.addEventListener('DOMContentLoaded', watch);
})();
"#;

/// Wie eine native App, nicht wie eine Webseite: nichts ziehbar, kein Browser-Kontextmenü
/// (ausser in Eingabefeldern und Mails), kein Neuladen/Vor/Zurück per Kürzel, keine Auswahl auf
/// Bedienelementen, Links auf fremde Seiten im Browser des Systems.
pub const NATIVE_SCRIPT: &str = r#"
(function () {
  if (window.__nrzFeel) return;
  window.__nrzFeel = true;
  var css = 'img,svg,a,video,canvas{-webkit-user-drag:none}'
    + 'button,[role=button],[role=menuitem],[role=tab],[role=option],label,nav,svg,[data-drag],.nrz-titlebar{-webkit-user-select:none;user-select:none}'
    + 'html,body{overscroll-behavior:none}'
    // Wie eine App: Bedienoberfläche nicht markierbar, Pfeil statt Hand; markierbar bleiben
    // Eingaben, Mailtexte und der Kopf einer geöffneten Mail
    + 'body{-webkit-user-select:none;user-select:none;cursor:default}'
    + 'input,textarea,[contenteditable],.mail-text,.mail-text *,article,article *,header h1,header h2,[data-selectable],[data-selectable] *{-webkit-user-select:text;user-select:text}'
    + 'a,[role=link],button{cursor:default}'
    + 'aside a,nav a{-webkit-user-select:none;user-select:none}'
    + ':focus:not(:focus-visible){outline:none}';
  function style() {
    if (document.getElementById('nrz-feel-css')) return;
    var s = document.createElement('style'); s.id = 'nrz-feel-css'; s.textContent = css;
    (document.head || document.documentElement).appendChild(s);
  }
  if (document.documentElement) style();
  document.addEventListener('DOMContentLoaded', style);
  function editable(t) {
    return !!(t && t.closest && t.closest('input,textarea,select,[contenteditable=""],[contenteditable=true],.mail-text,iframe'));
  }
  window.addEventListener('dragstart', function (e) {
    var t = e.target;
    if (t && t.nodeType === 1 && t.closest && t.closest('[draggable="true"]')) return;
    if (editable(t)) return;
    e.preventDefault();
  }, true);
  window.addEventListener('contextmenu', function (e) {
    if (editable(e.target) || String(window.getSelection() || '').length) return;
    e.preventDefault();
  }, true);
  window.addEventListener('keydown', function (e) {
    var k = (e.key || '').toLowerCase(), mod = e.metaKey || e.ctrlKey;
    var block = k === 'f5' || k === 'browserrefresh' || k === 'browserback' || k === 'browserforward'
      || (mod && !e.altKey && k === 'r')
      || (e.metaKey && (k === '[' || k === ']'))
      || (e.altKey && !mod && (k === 'arrowleft' || k === 'arrowright') && !editable(e.target));
    if (block) e.preventDefault();
  }, true);
  ['mouseup', 'mousedown', 'auxclick'].forEach(function (t) {
    window.addEventListener(t, function (e) { if (e.button === 3 || e.button === 4) e.preventDefault(); }, true);
  });
  // Links auf fremde Seiten nie im Fenster der App – window.open geht über die App nach draussen
  function foreignLink(e) {
    if ((e.type === 'click' && e.button !== 0) || (e.type === 'auxclick' && e.button !== 1)) return;
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a || a.hasAttribute('download')) return;
    var u;
    try { u = new URL(a.getAttribute('href'), location.href); } catch (x) { return; }
    if (u.origin === location.origin || !/^(https?|mailto|tel):$/.test(u.protocol)) return;
    e.preventDefault();
    e.stopPropagation();
    window.open(u.href, '_blank');
  }
  window.addEventListener('click', foreignLink, true);
  window.addEventListener('auxclick', foreignLink, true);
  window.addEventListener('dragover', function (e) { if (!e.defaultPrevented) e.preventDefault(); });
  window.addEventListener('drop', function (e) { if (!e.defaultPrevented) e.preventDefault(); });
})();
"#;

#[cfg(test)]
mod tests {
    #[test]
    fn base64() {
        assert_eq!(super::b64(b"Man"), "TWFu");
        assert_eq!(super::b64(b"Ma"), "TWE=");
        assert_eq!(super::b64(b"M"), "TQ==");
    }
}

/// Hinweis auf ein Update unten links im Fenster – auf jeder Seite (lokal, Server, Seiten der App),
/// unabhängig von der Version des Servers. Die App meldet den Stand über
/// `window.__nrzUpdateState(status, text)`: `available` (Version), `progress` (0–1, -1 unbekannt),
/// `installing`, `failed` (Text), `current` (Version, nach einer Prüfung von Hand), `hidden`.
/// Ein Klick schickt `update:install` (bei `failed` erneut).
pub const UPDATE_SCRIPT: &str = r#"
(function () {
  if (window.__nrzUpdateState) return;
  function send(m) { try { window.ipc.postMessage(m); } catch (e) {} }
  var css = '#nrz-update{position:fixed;left:12px;bottom:12px;z-index:2147483000;display:flex;align-items:center;gap:8px;max-width:240px;padding:8px 13px 8px 10px;border-radius:999px;border:1px solid rgba(255,255,255,.16);background:rgba(36,36,40,.72);color:#fafafa;font:600 12.5px -apple-system,BlinkMacSystemFont,"Inter",sans-serif;box-shadow:0 12px 30px -12px rgba(0,0,0,.7);-webkit-backdrop-filter:blur(24px) saturate(170%);cursor:default;-webkit-user-select:none;transition:transform .15s,background .15s;animation:nrzUpIn .25s ease-out}'
    + '#nrz-update:hover{background:rgba(52,52,58,.82);transform:translateY(-1px)}'
    + '#nrz-update .dot{width:8px;height:8px;border-radius:50%;background:#4ade80;box-shadow:0 0 0 3px rgba(74,222,128,.22);flex:none}'
    + '#nrz-update.err .dot{background:#f87171;box-shadow:0 0 0 3px rgba(248,113,113,.22)}'
    + '#nrz-update .bar{position:absolute;left:12px;right:12px;bottom:3px;height:2px;border-radius:2px;background:rgba(255,255,255,.12);overflow:hidden}'
    + '#nrz-update .bar i{display:block;height:100%;background:#fafafa;width:0;transition:width .2s}'
    + '#nrz-update .x{margin-left:2px;opacity:.55;font-weight:400;padding:0 2px}'
    + '#nrz-update .x:hover{opacity:1}'
    + '@keyframes nrzUpIn{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:none}}';
  var state = null, el = null, hideTimer = null;
  function ensure() {
    if (!document.body) return null;
    if (!document.getElementById('nrz-update-css')) {
      var s = document.createElement('style'); s.id = 'nrz-update-css'; s.textContent = css;
      (document.head || document.documentElement).appendChild(s);
    }
    if (!el || !el.isConnected) {
      el = document.createElement('div'); el.id = 'nrz-update'; el.setAttribute('role', 'button');
      el.addEventListener('click', function (e) {
        if (e.target.classList.contains('x')) { state = null; render(); return; }
        if (state && (state.s === 'available' || state.s === 'failed')) send('update:install');
      });
      document.body.appendChild(el);
    }
    return el;
  }
  function render() {
    if (!state || state.s === 'hidden') { if (el) el.remove(); el = null; return; }
    var e = ensure(); if (!e) return;
    var s = state.s, t = state.t, html = '';
    e.classList.toggle('err', s === 'failed');
    e.style.position = 'fixed';
    if (s === 'available') { html = '<span class="dot"></span><span>Update auf ' + esc(t) + ' installieren</span><span class="x" title="Später">✕</span>'; e.title = 'Neue Version von nrzMail laden und neu starten'; }
    else if (s === 'progress') { var p = parseFloat(t); html = '<span class="dot"></span><span>Update wird geladen' + (p >= 0 ? ' … ' + Math.round(p * 100) + ' %' : ' …') + '</span><span class="bar"><i style="width:' + (p >= 0 ? p * 100 : 30) + '%"></i></span>'; e.title = ''; }
    else if (s === 'installing') { html = '<span class="dot"></span><span>Wird installiert – nrzMail startet neu …</span>'; e.title = ''; }
    else if (s === 'failed') { html = '<span class="dot"></span><span>Update fehlgeschlagen – erneut versuchen</span><span class="x" title="Schliessen">✕</span>'; e.title = t; }
    else if (s === 'current') { html = '<span class="dot"></span><span>nrzMail ' + esc(t) + ' ist aktuell</span>'; e.title = ''; }
    e.innerHTML = html;
  }
  function esc(s) { return String(s).replace(/[&<>"]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; }); }
  window.__nrzUpdateState = function (s, t) {
    state = { s: s, t: t };
    clearTimeout(hideTimer);
    if (s === 'current') hideTimer = setTimeout(function () { state = null; render(); }, 4000);
    if (document.body) render(); else document.addEventListener('DOMContentLoaded', render);
  };
  // Seite (neu) geladen: ein schon gefundenes Update holt sich der Hinweis selbst
  function ask() { send('update:state'); }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', ask); else ask();
})();
"#;
