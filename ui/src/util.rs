//! Kleine Helfer: Datum (relativ und absolut), Grössen, Zahlen, Browser-Speicher, Dateien.

use wasm_bindgen::JsValue;

/// Sprache und Region für Datum und Zahlen.
pub const LOCALE: &str = "de-CH";

fn js_date(ts: i64) -> js_sys::Date {
    js_sys::Date::new(&JsValue::from_f64(ts as f64 * 1000.0))
}

fn opts(pairs: &[(&str, &str)]) -> js_sys::Object {
    let o = js_sys::Object::new();
    for (k, v) in pairs {
        let _ = js_sys::Reflect::set(&o, &(*k).into(), &(*v).into());
    }
    o
}

/// Unix-Sekunden → "24.10.2026, 14:05" in der Zeitzone des Browsers.
pub fn datetime(ts: i64) -> String {
    js_date(ts)
        .to_locale_string(LOCALE, &opts(&[("dateStyle", "medium"), ("timeStyle", "short")]))
        .into()
}

/// Unix-Sekunden → "Sa., 24. Okt. 2026, 14:05" (Kopf einer Nachricht).
pub fn datetime_long(ts: i64) -> String {
    js_date(ts)
        .to_locale_string(
            LOCALE,
            &opts(&[
                ("weekday", "short"),
                ("day", "numeric"),
                ("month", "short"),
                ("year", "numeric"),
                ("hour", "2-digit"),
                ("minute", "2-digit"),
            ]),
        )
        .into()
}

/// Unix-Sekunden → "24.10.2026".
pub fn date(ts: i64) -> String {
    js_date(ts).to_locale_date_string(LOCALE, &opts(&[("dateStyle", "medium")])).into()
}

/// Unix-Sekunden → "14:05".
pub fn time(ts: i64) -> String {
    js_date(ts).to_locale_time_string_with_options(LOCALE, &opts(&[("hour", "2-digit"), ("minute", "2-digit")])).into()
}

/// Kurz für Listen: heute nur die Uhrzeit, dieses Jahr Tag und Monat, sonst das Datum.
pub fn relative(ts: i64) -> String {
    let d = js_date(ts);
    let now = js_sys::Date::new_0();
    let same_day = d.get_full_year() == now.get_full_year()
        && d.get_month() == now.get_month()
        && d.get_date() == now.get_date();
    if same_day {
        return d.to_locale_time_string_with_options(LOCALE, &opts(&[("hour", "2-digit"), ("minute", "2-digit")])).into();
    }
    let yesterday = js_sys::Date::new(&JsValue::from_f64(now.get_time() - 86_400_000.0));
    if d.get_full_year() == yesterday.get_full_year()
        && d.get_month() == yesterday.get_month()
        && d.get_date() == yesterday.get_date()
    {
        return "Gestern".into();
    }
    if d.get_full_year() == now.get_full_year() {
        return d.to_locale_date_string(LOCALE, &opts(&[("day", "numeric"), ("month", "short")])).into();
    }
    d.to_locale_date_string(LOCALE, &opts(&[("day", "2-digit"), ("month", "2-digit"), ("year", "numeric")])).into()
}

/// "vor 5 Min.", "vor 3 Std.", sonst das Datum – für „letzter Abgleich“.
pub fn ago(ts: i64) -> String {
    let diff = now() - ts;
    match diff {
        i64::MIN..=44 => "gerade eben".into(),
        45..=3599 => format!("vor {} Min.", (diff / 60).max(1)),
        3600..=86_399 => format!("vor {} Std.", diff / 3600),
        _ => datetime(ts),
    }
}

pub fn now() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

/// Zeitzone des Browsers in Minuten östlich von UTC (Schweiz im Sommer: 120).
/// `getTimezoneOffset` zählt umgekehrt (westlich positiv), darum das Vorzeichen drehen.
pub fn tz_offset_minutes() -> i32 {
    -(js_sys::Date::new_0().get_timezone_offset() as i32)
}

/// Wert eines `<input type="datetime-local">` (Ortszeit) → Unix-Sekunden.
pub fn parse_local_datetime(value: &str) -> Option<i64> {
    if value.trim().is_empty() {
        return None;
    }
    let ms = js_sys::Date::new(&JsValue::from_str(value)).get_time();
    (!ms.is_nan()).then_some((ms / 1000.0) as i64)
}

/// Wert eines `<input type="date">` (Ortszeit, Tagesbeginn) → Unix-Sekunden.
pub fn parse_local_date(value: &str) -> Option<i64> {
    if value.trim().is_empty() {
        return None;
    }
    parse_local_datetime(&format!("{value}T00:00"))
}

/// Unix-Sekunden → Wert für `<input type="datetime-local">`.
pub fn to_local_input(ts: i64) -> String {
    let d = js_date(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes()
    )
}

fn number_format(min_frac: u8, max_frac: u8) -> js_sys::Intl::NumberFormat {
    let o = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&o, &"minimumFractionDigits".into(), &min_frac.into());
    let _ = js_sys::Reflect::set(&o, &"maximumFractionDigits".into(), &max_frac.into());
    js_sys::Intl::NumberFormat::new(&js_sys::Array::of1(&LOCALE.into()), &o)
}

fn format_with(f: js_sys::Intl::NumberFormat, x: f64, fallback: String) -> String {
    f.format()
        .call1(&JsValue::NULL, &x.into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or(fallback)
}

/// 12’345 (Tausender getrennt).
pub fn num(n: i64) -> String {
    format_with(number_format(0, 0), n as f64, n.to_string())
}

/// Zahl mit einer Nachkommastelle, z. B. 3.4
pub fn decimal(x: f64) -> String {
    format_with(number_format(0, 1), x, format!("{x:.1}"))
}

/// 812 B, 14.2 KB, 3.1 MB, 1.2 GB
pub fn size(bytes: i64) -> String {
    let b = bytes as f64;
    let one = |x: f64| format_with(number_format(1, 1), x, format!("{x:.1}"));
    if b >= 1_073_741_824.0 {
        format!("{} GB", one(b / 1_073_741_824.0))
    } else if b >= 1_048_576.0 {
        format!("{} MB", one(b / 1_048_576.0))
    } else if b >= 1024.0 {
        format!("{} KB", one(b / 1024.0))
    } else {
        format!("{bytes} B")
    }
}

/// „1 Nachricht“ / „3 Nachrichten“
pub fn plural(n: i64, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{} {many}", num(n)) }
}

/// Anfangsbuchstabe für runde Abzeichen.
pub fn initial(s: &str) -> String {
    s.trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

pub fn storage_get(key: &str) -> Option<String> {
    web_sys::window()?.local_storage().ok()??.get_item(key).ok()?
}

pub fn storage_set(key: &str, value: &str) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.set_item(key, value);
    }
}

/// Neues Fenster/Tab öffnen (Abmelde-Links, Hilfe).
pub fn open_external(url: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer");
    }
}

/// Element mit dieser ID fokussieren (nach dem nächsten Zeichnen).
/// Kurzes Vibrieren (Android; iPhone ignoriert es).
pub fn vibrate(ms: u32) {
    dioxus::prelude::document::eval(&format!("try {{ navigator.vibrate && navigator.vibrate({ms}); }} catch (e) {{}}"));
}

pub fn focus(id: &str) {
    dioxus::prelude::document::eval(&format!(
        "requestAnimationFrame(() => {{ const e = document.getElementById({id:?}); if (e) {{ e.focus(); if (e.select) e.select(); }} }})"
    ));
}

/// Element mit dieser ID in den sichtbaren Bereich holen.
/// Läuft die App als installierte Web-App (Home-Bildschirm, ohne Browser-Leiste)?
pub fn standalone() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(display-mode: standalone)").ok().flatten())
        .is_some_and(|m| m.matches())
}

/// Wie weit ein Element gescrollt ist (px; 0 = ganz oben).
pub fn scroll_top(id: &str) -> f64 {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(id))
        .map(|e| f64::from(e.scroll_top()))
        .unwrap_or(0.0)
}

pub fn scroll_into_view(id: &str) {
    dioxus::prelude::document::eval(&format!(
        "requestAnimationFrame(() => document.getElementById({id:?})?.scrollIntoView({{ block: 'nearest' }}))"
    ));
}

/// Dateien aus einem <input type=file> (onchange); leert das Feld danach.
pub fn input_files(e: &dioxus::prelude::FormEvent) -> Vec<web_sys::File> {
    use wasm_bindgen::JsCast;
    let input = e
        .data()
        .downcast::<web_sys::Event>()
        .and_then(|ev| ev.target())
        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok());
    match input {
        Some(i) => {
            let files = files_of(i.files());
            i.set_value("");
            files
        }
        None => Vec::new(),
    }
}

/// Dateien aus einem Drop-Ereignis (Drag & Drop vom Desktop).
pub fn dropped_files(e: &dioxus::prelude::DragEvent) -> Vec<web_sys::File> {
    use wasm_bindgen::JsCast;
    e.data()
        .downcast::<web_sys::Event>()
        .and_then(|ev| ev.dyn_ref::<web_sys::DragEvent>())
        .and_then(|d| d.data_transfer())
        .map(|dt| files_of(dt.files()))
        .unwrap_or_default()
}

fn files_of(list: Option<web_sys::FileList>) -> Vec<web_sys::File> {
    let Some(list) = list else { return Vec::new() };
    (0..list.length()).filter_map(|i| list.get(i)).collect()
}

/// Inhalt einer Datei als Base64 (Standard-Alphabet) – für Anhänge beim Senden.
pub async fn file_base64(file: &web_sys::File) -> Option<String> {
    use base64::Engine;
    let buf = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await.ok()?;
    let bytes = js_sys::Uint8Array::new(&buf).to_vec();
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Ungefähre Grösse von Base64-Daten in Bytes.
pub fn base64_size(data: &str) -> i64 {
    (data.len() as i64 * 3) / 4
}

/// Ist die Seite gerade sichtbar (nicht in einem Hintergrund-Tab)?
pub fn page_visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| js_sys::Reflect::get(&d, &"hidden".into()).ok())
        .and_then(|v| v.as_bool())
        .map(|hidden| !hidden)
        .unwrap_or(true)
}
