//! Verfassen: Fenster unten rechts (auf dem Handy Vollbild). Absender-Postfach, An/Cc/Bcc mit
//! Autovervollständigung, Betreff, Text mit Signatur und Zitat, Anhänge (Base64), Senden,
//! später senden, Entwurf speichern und KI (schreiben, überarbeiten, übersetzen).

use crate::api;
use crate::components::common::*;
use crate::components::overlay::*;
use crate::state::{self, ACCOUNTS, AI, PREFS};
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{
    AccountKind, AiDraft, Address, Attachment, Compose, Contact, MessageDetail, OutboxItem, OutboxStatus,
    OutgoingAttachment, SendResult, WriteRequest, format_addresses, parse_addresses,
};

/// Was im Fenster steht, wenn es aufgeht.
#[derive(Clone, PartialEq, Default)]
pub struct Draft {
    /// Neue Instanz des Fensters je Aufruf
    key: u64,
    pub title: String,
    pub account_id: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub forward_of: Option<String>,
    /// Anhänge der weitergeleiteten Nachricht (alle vorausgewählt)
    pub forward_attachments: Vec<Attachment>,
    pub attachments: Vec<OutgoingAttachment>,
    /// Gespeicherter Entwurf, der damit bearbeitet wird
    pub draft_id: Option<String>,
    /// Ungespeicherte Änderung von Anfang an (z. B. KI-Entwurf)
    pub dirty: bool,
}

static COMPOSE: GlobalSignal<Option<Draft>> = Signal::global(|| None);
static NEXT: GlobalSignal<u64> = Signal::global(|| 0);
/// Hat das offene Fenster ungespeicherte Änderungen?
static UNSAVED: GlobalSignal<bool> = Signal::global(|| false);

/// Ist gerade ein Fenster offen? (Assistent blendet seinen Knopf dann aus)
pub fn is_open() -> bool {
    COMPOSE.read().is_some()
}

/// Fenster mit diesem Inhalt öffnen (ersetzt ein offenes).
pub fn open(mut d: Draft) {
    let key = {
        let mut n = NEXT.write();
        *n += 1;
        *n
    };
    d.key = key;
    if d.title.is_empty() {
        d.title = "Neue Nachricht".into();
    }
    // Ein offenes Fenster mit ungespeichertem Text nicht ohne Rückfrage ersetzen
    if COMPOSE.peek().is_some() && *UNSAVED.peek() {
        spawn(async move {
            let ask = Ask::new("Offene Nachricht ersetzen?", "Das Fenster enthält ungespeicherten Text. Zuerst als Entwurf speichern oder verwerfen – oder trotzdem ersetzen.")
                .confirm("Ersetzen")
                .destructive();
            if confirm(ask).await {
                *UNSAVED.write() = false;
                *COMPOSE.write() = Some(d);
            }
        });
        return;
    }
    *UNSAVED.write() = false;
    *COMPOSE.write() = Some(d);
}

/// Signatur-Block eines Postfachs (`-- ` + Signatur), leer ohne Signatur.
fn signature_block(account_id: &str) -> String {
    let sig = ACCOUNTS.peek().iter().find(|a| a.id == account_id).map(|a| a.signature.trim_end().to_string()).unwrap_or_default();
    if sig.trim().is_empty() { String::new() } else { format!("-- \n{sig}") }
}

/// Text mit Signatur und optionalem Zitat darunter.
fn body_with(account_id: &str, text: &str, quote: &str) -> String {
    let sig = signature_block(account_id);
    let mut body = text.to_string();
    if !sig.is_empty() {
        body.push_str("\n\n");
        body.push_str(&sig);
    }
    if !quote.is_empty() {
        body.push_str("\n\n");
        body.push_str(quote);
    }
    if text.is_empty() && (!sig.is_empty() || !quote.is_empty()) {
        // Platz für den eigenen Text oben lassen
        body = format!("\n{body}");
    }
    body
}

const FORWARD_MARK: &str = "---------- Weitergeleitete Nachricht ----------";

/// Teilt den Text in den eigenen Teil und das, was darunter steht (Signatur, Zitat).
fn split_tail(body: &str) -> (String, String) {
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let l = line.trim_end_matches(['\n', '\r']);
        let is_tail = l == "-- " || l == FORWARD_MARK || (l.starts_with("Am ") && l.contains(" schrieb ") && l.ends_with(':'));
        if is_tail {
            return (body[..offset].trim_end().to_string(), body[offset..].to_string());
        }
        offset += line.len();
    }
    (body.trim_end().to_string(), String::new())
}

/// Postfach, das senden kann – bevorzugt das gewünschte.
fn sending_account(preferred: &str) -> String {
    let accounts = ACCOUNTS.peek();
    accounts
        .iter()
        .find(|a| a.id == preferred && a.kind == AccountKind::Imap)
        .map(|a| a.id.clone())
        .or_else(|| state::default_account().map(|a| a.id))
        .unwrap_or_default()
}

pub fn open_new() {
    let account_id = state::default_account().map(|a| a.id).unwrap_or_default();
    let body = body_with(&account_id, "", "");
    open(Draft { account_id, body, ..Default::default() });
}

/// Neue Nachricht an eine Adresse (z. B. aus einem `mailto:`-Link).
pub fn open_mailto(url: &str) {
    let rest = url.trim_start_matches("mailto:");
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut subject = String::new();
    let mut text = String::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let v = js_sys::decode_uri_component(v).map(String::from).unwrap_or_else(|_| v.to_string());
            match k.to_ascii_lowercase().as_str() {
                "subject" => subject = v,
                "body" => text = v,
                _ => {}
            }
        }
    }
    let to = js_sys::decode_uri_component(to).map(String::from).unwrap_or_else(|_| to.to_string());
    let account_id = state::default_account().map(|a| a.id).unwrap_or_default();
    let body = body_with(&account_id, &text, "");
    open(Draft { account_id, to, subject, body, dirty: true, ..Default::default() });
}

fn prefixed(subject: &str, prefix: &str, known: &[&str]) -> String {
    let lower = subject.trim().to_lowercase();
    if known.iter().any(|p| lower.starts_with(p)) {
        subject.trim().to_string()
    } else {
        format!("{prefix} {}", subject.trim())
    }
}

fn quote_of(m: &MessageDetail) -> String {
    let who = m.summary.from.display();
    let mut q = format!("Am {} schrieb {who}:\n", util::datetime(m.summary.date));
    for line in m.text_body.trim_end().lines() {
        if line.starts_with('>') {
            q.push('>');
        } else {
            q.push_str("> ");
        }
        q.push_str(line);
        q.push('\n');
    }
    q
}

/// Eigene Adressen (alle Postfächer) – fallen beim „Allen antworten“ weg.
fn own_addresses() -> Vec<String> {
    ACCOUNTS.peek().iter().map(|a| a.email.to_lowercase()).collect()
}

/// Empfänger einer Antwort: (An, Cc).
fn reply_recipients(m: &MessageDetail, all: bool) -> (Vec<Address>, Vec<Address>) {
    let own = own_addresses();
    let to: Vec<Address> = if m.summary.is_sent {
        m.summary.to.clone()
    } else if !m.reply_to.is_empty() {
        m.reply_to.clone()
    } else {
        vec![m.summary.from.clone()]
    };
    if !all {
        return (to, Vec::new());
    }
    let mut seen: Vec<String> = to.iter().map(|a| a.address.to_lowercase()).collect();
    let mut cc = Vec::new();
    for a in m.summary.to.iter().chain(m.cc.iter()) {
        let key = a.address.to_lowercase();
        if own.contains(&key) || seen.contains(&key) || key.is_empty() {
            continue;
        }
        seen.push(key);
        cc.push(a.clone());
    }
    (to, cc)
}

/// Antwort vorbereiten; `text` ist ein vorgeschlagener Text (z. B. von der KI).
pub fn reply_with(m: &MessageDetail, all: bool, text: &str, subject: Option<String>) {
    let account_id = sending_account(&m.summary.account_id);
    let (to, cc) = reply_recipients(m, all);
    let subject = subject
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| prefixed(&m.summary.subject, "Re:", &["re:", "aw:", "antw:"]));
    let body = body_with(&account_id, text, &quote_of(m));
    open(Draft {
        title: if all { "Allen antworten".into() } else { "Antworten".into() },
        account_id,
        to: format_addresses(&to),
        cc: format_addresses(&cc),
        subject,
        body,
        in_reply_to: Some(m.summary.id.clone()),
        dirty: !text.is_empty(),
        ..Default::default()
    });
}

pub fn reply(m: &MessageDetail, all: bool) {
    reply_with(m, all, "", None);
}

/// Fertige Antwort für das Antwortfeld im Chat: gleiche Empfänger, Betreff, Signatur und
/// Zitat wie im Editor – sie geht als ganz normale E-Mail raus.
pub fn quick_reply(m: &MessageDetail, all: bool, text: &str) -> Compose {
    let account_id = sending_account(&m.summary.account_id);
    let (to, cc) = reply_recipients(m, all);
    Compose {
        text: body_with(&account_id, text.trim_end(), &quote_of(m)),
        account_id,
        to,
        cc,
        subject: prefixed(&m.summary.subject, "Re:", &["re:", "aw:", "antw:"]),
        in_reply_to: Some(m.summary.id.clone()),
        ..Default::default()
    }
}

/// Nach dem Senden: Hinweis, bei Sendeverzögerung mit „Rückgängig“.
pub fn after_send(r: &SendResult, scheduled_at: Option<i64>) {
    match (r.status, scheduled_at) {
        (OutboxStatus::Scheduled, None) if r.undo_until.is_some() => {
            let until = r.undo_until.unwrap_or_default();
            let ms = ((until - util::now()).clamp(3, nrzmail_api::UNDO_SEND_MAX as i64) * 1000) as u32;
            let id = r.outbox_id.clone();
            toast_action(format!("Wird um {} gesendet.", util::datetime(until)), "Rückgängig", ms, move || {
                let id = id.clone();
                spawn(async move {
                    match api::post::<OutboxItem, _>(&format!("/outbox/{}/undo", api::enc(&id)), &()).await {
                        Ok(item) => {
                            toast("Senden zurückgezogen.");
                            edit_outbox(&item);
                            state::refresh_list();
                        }
                        Err(e) => toast_error(e),
                    }
                });
            });
        }
        (OutboxStatus::Scheduled, Some(at)) => toast(format!("Geplant für {}.", util::datetime(at))),
        (OutboxStatus::Scheduled, None) => toast("Nachricht geplant."),
        (OutboxStatus::Failed, _) => toast_error("Senden fehlgeschlagen – die Nachricht liegt unter Entwürfe."),
        (OutboxStatus::Sent, _) => toast("Gesendet."),
        _ => toast("Wird gesendet …"),
    }
}

pub fn forward(m: &MessageDetail) {
    let account_id = sending_account(&m.summary.account_id);
    let mut quote = format!(
        "{FORWARD_MARK}\nVon: {}\nDatum: {}\nBetreff: {}\nAn: {}\n",
        m.summary.from.display(),
        util::datetime(m.summary.date),
        m.summary.subject,
        format_addresses(&m.summary.to)
    );
    if !m.cc.is_empty() {
        quote.push_str(&format!("Cc: {}\n", format_addresses(&m.cc)));
    }
    quote.push('\n');
    quote.push_str(m.text_body.trim_end());
    let body = body_with(&account_id, "", &quote);
    open(Draft {
        title: "Weiterleiten".into(),
        account_id,
        subject: prefixed(&m.summary.subject, "Fwd:", &["fwd:", "fw:", "wg:"]),
        body,
        forward_of: Some(m.summary.id.clone()),
        forward_attachments: m.attachments.iter().filter(|a| !a.inline).cloned().collect(),
        ..Default::default()
    });
}

/// Gespeicherten Entwurf bzw. geplante Nachricht weiter bearbeiten.
pub fn edit_outbox(item: &OutboxItem) {
    let c = &item.compose;
    open(Draft {
        title: match item.status {
            OutboxStatus::Scheduled => "Geplante Nachricht".into(),
            OutboxStatus::Failed => "Nicht gesendete Nachricht".into(),
            _ => "Entwurf".into(),
        },
        account_id: if c.account_id.is_empty() { item.account_id.clone() } else { c.account_id.clone() },
        to: format_addresses(&c.to),
        cc: format_addresses(&c.cc),
        bcc: format_addresses(&c.bcc),
        subject: c.subject.clone(),
        body: c.text.clone(),
        in_reply_to: c.in_reply_to.clone(),
        forward_of: c.forward_of.clone(),
        attachments: c.attachments.clone(),
        draft_id: Some(item.id.clone()),
        ..Default::default()
    });
}

/// Nachricht aus dem Entwürfe-Ordner des Servers als neue Nachricht öffnen.
pub fn edit_message(m: &MessageDetail) {
    open(Draft {
        title: "Entwurf".into(),
        account_id: sending_account(&m.summary.account_id),
        to: format_addresses(&m.summary.to),
        cc: format_addresses(&m.cc),
        bcc: format_addresses(&m.bcc),
        subject: m.summary.subject.clone(),
        body: m.text_body.clone(),
        forward_attachments: m.attachments.iter().filter(|a| !a.inline).cloned().collect(),
        ..Default::default()
    });
}

/// Einmal in der Shell einbinden.
#[component]
pub fn ComposeHost() -> Element {
    match COMPOSE() {
        Some(d) => rsx! { ComposeWindow { key: "{d.key}", draft: d } },
        None => rsx! {},
    }
}

fn close() {
    *UNSAVED.write() = false;
    *COMPOSE.write() = None;
}

/// Startzeiten für „Später senden“.
fn schedule_presets() -> Vec<(String, i64)> {
    let now = js_sys::Date::new_0();
    let at = |days: f64, hour: f64| {
        let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(now.get_time() + days * 86_400_000.0));
        d.set_hours(hour as u32);
        d.set_minutes(0);
        d.set_seconds(0);
        (d.get_time() / 1000.0) as i64
    };
    let mut out = vec![("Morgen früh (8:00)".to_string(), at(1.0, 8.0)), ("Morgen Nachmittag (13:00)".to_string(), at(1.0, 13.0))];
    // Nächster Montag
    let weekday = now.get_day() as f64; // 0 = Sonntag
    let to_monday = ((8.0 - weekday) % 7.0).max(1.0);
    out.push(("Montag früh (8:00)".to_string(), at(to_monday, 8.0)));
    out
}

#[component]
fn ComposeWindow(draft: Draft) -> Element {
    let ai_available = AI.read().available;
    let dieter = { let n = AI.read().assistant_name.trim().to_string(); if n.is_empty() { "Dieter".to_string() } else { n } };
    let dieter_initial = util::initial(&dieter);
    let accounts: Vec<_> = ACCOUNTS().into_iter().filter(|a| a.kind == AccountKind::Imap).collect();
    let mut account = use_signal(|| draft.account_id.clone());
    let to = use_signal(|| draft.to.clone());
    let cc = use_signal(|| draft.cc.clone());
    let bcc = use_signal(|| draft.bcc.clone());
    let mut show_cc = use_signal(|| !draft.cc.is_empty() || !draft.bcc.is_empty());
    let mut subject = use_signal(|| draft.subject.clone());
    let mut body = use_signal(|| draft.body.clone());
    let mut attachments = use_signal(|| draft.attachments.clone());
    let mut forwarded = use_signal(|| draft.forward_attachments.iter().map(|a| (a.clone(), true)).collect::<Vec<_>>());
    let mut draft_id = use_signal(|| draft.draft_id.clone());
    let mut dirty = use_signal(|| draft.dirty);
    // Für die Rückfrage beim Ersetzen (siehe `open`)
    use_effect(move || {
        let d = dirty();
        if *UNSAVED.peek() != d {
            *UNSAVED.write() = d;
        }
    });
    let mut busy = use_signal(|| false);
    let mut ai_busy = use_signal(|| false);
    let mut ai_bar = use_signal(|| false);
    let mut ai_instruction = use_signal(String::new);
    // Vorschlag der KI, der erst nach „Übernehmen“ in den Text kommt (Einstellung „KI-Texte erst freigeben“)
    let mut proposal = use_signal(|| None::<(AiDraft, String)>);
    let mut minimized = use_signal(|| false);
    let mut schedule_open = use_signal(|| false);
    let mut schedule_value = use_signal(|| util::to_local_input(util::now() + 3600));
    let mut reading = use_signal(|| false);
    let in_reply_to = draft.in_reply_to.clone();
    let forward_of = draft.forward_of.clone();
    let ai_reply_to = draft.in_reply_to.clone();
    let is_reply = draft.in_reply_to.is_some();

    // Wenn noch kein Absender feststeht (Postfächer erst jetzt geladen): Vorgabe nehmen
    use_effect(move || {
        if account.peek().is_empty()
            && let Some(a) = ACCOUNTS().iter().find(|a| a.kind == AccountKind::Imap) {
                account.set(a.id.clone());
            }
    });

    let build = use_callback(move |send_at: Option<i64>| -> Result<Compose, String> {
        let to_list = parse_addresses(&to());
        let cc_list = parse_addresses(&cc());
        let bcc_list = parse_addresses(&bcc());
        if let Some(bad) = to_list.iter().chain(&cc_list).chain(&bcc_list).find(|a| !a.is_valid()) {
            return Err(format!("Die Adresse „{}“ sieht nicht gültig aus.", bad.address));
        }
        Ok(Compose {
            account_id: account(),
            to: to_list,
            cc: cc_list,
            bcc: bcc_list,
            subject: subject().trim().to_string(),
            text: body(),
            html: None,
            in_reply_to: in_reply_to.clone(),
            forward_of: forward_of.clone(),
            attachments: attachments(),
            forward_attachments: forwarded().into_iter().filter(|(_, on)| *on).map(|(a, _)| a.id).collect(),
            send_at,
            draft_id: draft_id(),
        })
    });

    let send = use_callback(move |send_at: Option<i64>| {
        if busy() {
            return;
        }
        let compose = match build.call(send_at) {
            Ok(c) => c,
            Err(e) => return toast_error(e),
        };
        if compose.account_id.is_empty() {
            return toast_error("Zuerst ein Postfach zum Senden einrichten (Einstellungen → Postfächer).");
        }
        if compose.to.is_empty() && compose.cc.is_empty() && compose.bcc.is_empty() {
            return toast_error("Bitte mindestens einen Empfänger angeben.");
        }
        spawn(async move {
            if compose.subject.is_empty()
                && !confirm(Ask::new("Ohne Betreff senden?", "Die Nachricht hat keinen Betreff.").confirm("Trotzdem senden")).await
            {
                return;
            }
            busy.set(true);
            let res = api::post::<SendResult, _>("/send", &compose).await;
            busy.set(false);
            match res {
                Ok(r) => {
                    after_send(&r, compose.send_at);
                    close();
                    state::refresh_list();
                }
                Err(e) => toast_error(e),
            }
        });
    });

    let save_draft = use_callback(move |(quiet, then_close): (bool, bool)| {
        let compose = match build.call(None) {
            Ok(c) => c,
            Err(e) => return toast_error(e),
        };
        spawn(async move {
            busy.set(true);
            let res = match draft_id() {
                Some(id) => api::put::<OutboxItem, _>(&format!("/drafts/{}", api::enc(&id)), &compose).await,
                None => api::post::<OutboxItem, _>("/drafts", &compose).await,
            };
            busy.set(false);
            match res {
                Ok(item) => {
                    draft_id.set(Some(item.id));
                    dirty.set(false);
                    if !quiet {
                        toast("Entwurf gespeichert.");
                    } else if then_close {
                        toast("Als Entwurf gespeichert.");
                    }
                    if then_close {
                        close();
                    }
                    state::refresh_list();
                }
                Err(e) => toast_error(e),
            }
        });
    });

    // Schliessen: Änderungen landen als Entwurf, nichts geht verloren
    let request_close = move |_| {
        let has_content = !to().trim().is_empty() || !subject().trim().is_empty() || !split_tail(&body()).0.trim().is_empty();
        if !dirty() || !has_content {
            return close();
        }
        match build.call(None) {
            Ok(_) => save_draft.call((true, true)),
            // Nichts stillschweigend verwerfen: erst die Adresse korrigieren (oder „Verwerfen“)
            Err(e) => toast_error(format!("{e} Zum Schliessen korrigieren oder die Nachricht verwerfen.")),
        }
    };

    let discard = move |_| {
        spawn(async move {
            if dirty() || draft_id().is_some() {
                let ask = Ask::new("Nachricht verwerfen?", "Der Text und ein gespeicherter Entwurf werden gelöscht.").confirm("Verwerfen").destructive();
                if !confirm(ask).await {
                    return;
                }
            }
            if let Some(id) = draft_id() {
                if let Err(e) = api::delete(&format!("/outbox/{}", api::enc(&id))).await {
                    toast_error(e);
                }
                state::refresh_list();
            }
            close();
        });
    };

    let change_account = use_callback(move |id: String| {
        let old = signature_block(&account());
        let new = signature_block(&id);
        let mut b = body();
        if old != new {
            if !old.is_empty() && b.contains(&old) {
                b = b.replacen(&old, &new, 1);
            } else if old.is_empty() && !new.is_empty() {
                let (head, tail) = split_tail(&b);
                b = if tail.is_empty() { format!("{head}\n\n{new}") } else { format!("{head}\n\n{new}\n\n{tail}") };
            }
            body.set(b);
        }
        account.set(id);
        dirty.set(true);
    });

    // KI: neu schreiben oder den eigenen Teil überarbeiten
    let ai_write = use_callback(move |(instruction, rewrite): (String, bool)| {
        if ai_busy() {
            return;
        }
        let (head, tail) = split_tail(&body());
        let req = WriteRequest {
            instruction,
            text: head.clone(),
            subject: subject(),
            to: parse_addresses(&to()),
            account_id: Some(account()).filter(|a| !a.is_empty()),
            reply_to: ai_reply_to.clone(),
        };
        if rewrite && head.trim().is_empty() {
            return toast_error("Zuerst etwas schreiben – dann kann die KI es überarbeiten.");
        }
        spawn(async move {
            ai_busy.set(true);
            let res = api::post::<AiDraft, _>("/ai/write", &req).await;
            ai_busy.set(false);
            match res {
                Ok(d) => {
                    ai_bar.set(false);
                    ai_instruction.set(String::new());
                    if PREFS.peek().ai_require_approval {
                        proposal.set(Some((d, tail)));
                    } else {
                        apply_ai(d, &tail, body, subject, dirty);
                    }
                }
                Err(e) => toast_error(e),
            }
        });
    });

    let add_files = use_callback(move |files: Vec<web_sys::File>| {
        if files.is_empty() {
            return;
        }
        spawn(async move {
            reading.set(true);
            for f in files {
                match util::file_base64(&f).await {
                    Some(data) => {
                        let content_type = if f.type_().is_empty() { "application/octet-stream".to_string() } else { f.type_() };
                        attachments.write().push(OutgoingAttachment { filename: f.name(), content_type, data_base64: data });
                        dirty.set(true);
                    }
                    None => toast_error(format!("„{}“ liess sich nicht lesen.", f.name())),
                }
            }
            reading.set(false);
            let total: i64 = attachments.peek().iter().map(|a| util::base64_size(&a.data_base64)).sum();
            if total > 20 * 1_048_576 {
                toast_error(format!("Anhänge zusammen {} – viele Server nehmen höchstens 20–25 MB an.", util::size(total)));
            }
        });
    });

    let title = draft.title.clone();
    if minimized() {
        return rsx! {
            div { "data-compose": "true",
                class: "fixed bottom-0 right-4 z-[55] flex w-72 items-center gap-2 rounded-t-xl border border-b-0 bg-popover px-4 py-2.5 shadow-2xl",
                button { class: "min-w-0 flex-1 truncate text-left text-sm font-medium", onclick: move |_| minimized.set(false),
                    if subject().trim().is_empty() { "{title}" } else { "{subject}" }
                }
                button { class: "grid size-7 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground", title: "Wieder öffnen",
                    onclick: move |_| minimized.set(false), icons::Maximize2 { class: "size-3.5" } }
                button { class: "grid size-7 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground", title: "Schliessen",
                    onclick: request_close, icons::X { class: "size-4" } }
            }
        };
    }

    let field_row = "flex items-center gap-2 border-b px-4";
    let field_label = "w-14 shrink-0 text-sm text-muted-foreground";
    let bare = "h-10 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground max-md:text-base";

    rsx! {
        div { "data-compose": "true",
            class: "fixed inset-0 z-[55] flex flex-col bg-popover text-popover-foreground shadow-2xl animate-in max-sm:bottom-auto max-sm:top-[var(--vvt,0px)] max-sm:h-[var(--vvh,100dvh)] max-sm:pt-safe max-sm:animate-sheet-in sm:inset-auto sm:bottom-4 sm:right-4 sm:h-[min(46rem,calc(100dvh-2rem))] sm:w-[42rem] sm:rounded-2xl sm:border",
            role: "dialog",
            "aria-label": "{title}",
            ondragover: move |e| e.prevent_default(),
            ondrop: move |e| {
                e.prevent_default();
                add_files.call(util::dropped_files(&e));
            },
            onkeydown: move |e| {
                let m = e.modifiers();
                if e.key() == Key::Enter && (m.meta() || m.ctrl()) {
                    e.prevent_default();
                    send.call(None);
                }
            },
            // Kopf
            div { class: "flex items-center gap-2 border-b px-4 py-2.5",
                p { class: "min-w-0 flex-1 truncate font-semibold", "{title}" }
                if draft_id().is_some() && !dirty() {
                    span { class: "text-xs text-muted-foreground", "Gespeichert" }
                }
                button { class: "hidden size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground sm:grid", title: "Minimieren",
                    onclick: move |_| minimized.set(true), icons::Minimize2 { class: "size-4" } }
                button { class: "grid size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground", title: "Schliessen (Änderungen werden als Entwurf gespeichert)",
                    onclick: request_close, icons::X { class: "size-4" } }
            }
            // Felder
            div { class: field_row,
                label { class: field_label, r#for: "compose-from", "Von" }
                if accounts.is_empty() {
                    span { class: "h-10 flex-1 content-center text-sm text-muted-foreground", "Kein Postfach zum Senden eingerichtet" }
                } else {
                    select { id: "compose-from", class: "{bare} [&>option]:bg-popover", value: "{account}",
                        onchange: move |e| change_account.call(e.value()),
                        for a in accounts.iter() {
                            option { value: "{a.id}", selected: a.id == account(),
                                {if a.display_name.trim().is_empty() { a.email.clone() } else { format!("{} <{}>", a.display_name, a.email) }}
                            }
                        }
                    }
                }
            }
            div { class: field_row,
                AddressField { id: "compose-to", label: "An", value: to, dirty, autofocus: draft.to.is_empty() }
                if !show_cc() {
                    button { class: "text-xs text-muted-foreground hover:text-foreground", onclick: move |_| show_cc.set(true), "Cc/Bcc" }
                }
            }
            if show_cc() {
                div { class: field_row, AddressField { id: "compose-cc", label: "Cc", value: cc, dirty } }
                div { class: field_row, AddressField { id: "compose-bcc", label: "Bcc", value: bcc, dirty } }
            }
            div { class: field_row,
                label { class: field_label, r#for: "compose-subject", "Betreff" }
                input { id: "compose-subject", class: bare, value: "{subject}", autocomplete: "off",
                    oninput: move |e| { subject.set(e.value()); dirty.set(true); } }
            }
            // Dieter: Schreibassistent
            if ai_bar() {
                div { class: "flex flex-col gap-2 border-b bg-accent/30 px-4 py-2",
                    form { class: "flex items-center gap-2 max-md:flex-wrap",
                        onsubmit: move |e| { e.prevent_default(); ai_write.call((ai_instruction(), false)); },
                        span { class: "grid size-6 shrink-0 place-items-center rounded-full bg-primary text-[11px] font-semibold text-primary-foreground", title: "{dieter}", "{dieter_initial}" }
                        input { class: "{bare} max-md:basis-[calc(100%-2.5rem)]", placeholder: "{dieter}, was soll die Nachricht sagen? z. B. „Termin nächste Woche vorschlagen“",
                            onmounted: move |e| async move { let _ = e.set_focus(true).await; },
                            "aria-label": "Auftrag an {dieter}",
                            value: "{ai_instruction}", oninput: move |e| ai_instruction.set(e.value()),
                            onkeydown: move |e| if e.key() == Key::Escape { e.stop_propagation(); ai_bar.set(false); } }
                        button { class: "{btn(BTN_GHOST)} {BTN_SM}", r#type: "button", disabled: ai_busy() || ai_instruction().trim().is_empty(),
                            title: "Den eigenen Text nach dieser Anweisung überarbeiten",
                            onclick: move |_| ai_write.call((ai_instruction(), true)), "Überarbeiten" }
                        button { class: "{btn(BTN_PRIMARY)} {BTN_SM}", r#type: "submit", disabled: ai_busy() || ai_instruction().trim().is_empty(),
                            if ai_busy() { icons::LoaderCircle { class: "size-4 animate-spin" } }
                            "Schreiben"
                        }
                        button { class: "grid size-7 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground", r#type: "button", title: "Schliessen",
                            onclick: move |_| ai_bar.set(false), icons::X { class: "size-3.5" } }
                    }
                    div { class: "flex flex-wrap gap-1.5 pl-8 max-md:no-scrollbar max-md:flex-nowrap max-md:overflow-x-auto max-md:pl-0",
                        for (label , instruction) in [
                            ("Verbessern", "Verbessere Rechtschreibung, Grammatik und Stil. Inhalt und Sprache beibehalten."),
                            ("Kürzer", "Kürze den Text deutlich, ohne wichtige Informationen zu verlieren."),
                            ("Förmlicher", "Formuliere den Text förmlicher und höflicher."),
                            ("Freundlicher", "Formuliere den Text lockerer und freundlicher."),
                            ("Klarer", "Formuliere den Text klarer und strukturierter, mit kurzen Absätzen."),
                        ] {
                            button { class: "shrink-0 rounded-full border bg-background px-2.5 py-0.5 text-xs hover:bg-accent disabled:opacity-50 max-md:px-3 max-md:py-1.5 max-md:text-sm", r#type: "button",
                                disabled: ai_busy(), onclick: move |_| ai_write.call((instruction.to_string(), true)), "{label}" }
                        }
                    }
                }
            }
            // Vorschlag der KI zur Freigabe
            if let Some((d, tail)) = proposal() {
                div { class: "flex max-h-[55%] min-h-0 flex-col border-b bg-accent/30",
                    div { class: "flex items-center gap-2 px-4 pt-2.5 text-xs font-medium uppercase tracking-wider text-muted-foreground",
                        icons::Sparkles { class: "size-3.5" }
                        span { class: "flex-1", "Vorschlag von {dieter}" }
                        span { class: "normal-case tracking-normal", "{d.model}" }
                    }
                    if !d.subject.trim().is_empty() && subject().trim().is_empty() {
                        p { class: "px-4 pt-1 text-sm", span { class: "text-muted-foreground", "Betreff: " } "{d.subject}" }
                    }
                    div { class: "min-h-0 flex-1 overflow-y-auto whitespace-pre-wrap px-4 py-2 text-sm leading-relaxed", "{d.text}" }
                    div { class: "flex flex-wrap items-center gap-2 px-4 pb-2.5",
                        button { class: "{btn(BTN_PRIMARY)} {BTN_SM}",
                            onclick: { let d = d.clone(); let tail = tail.clone(); move |_| { apply_ai(d.clone(), &tail, body, subject, dirty); proposal.set(None); } },
                            icons::Check { class: "size-4" } "Übernehmen" }
                        button { class: "{btn(BTN_OUTLINE)} {BTN_SM}", onclick: move |_| { proposal.set(None); ai_bar.set(true); },
                            icons::RefreshCw { class: "size-4" } "Anders formulieren" }
                        button { class: "{btn(BTN_GHOST)} {BTN_SM}", onclick: move |_| proposal.set(None),
                            icons::X { class: "size-4" } "Verwerfen" }
                    }
                }
            }
            // Text
            div { class: "relative min-h-0 flex-1",
                textarea {
                    id: "compose-body",
                    class: "h-full w-full resize-none bg-transparent px-4 py-3 text-sm leading-relaxed outline-none placeholder:text-muted-foreground max-md:text-base",
                    "aria-label": "Text",
                    placeholder: "Nachricht schreiben …",
                    autofocus: !draft.to.is_empty(),
                    value: "{body}",
                    oninput: move |e| { body.set(e.value()); dirty.set(true); },
                    onmounted: move |_| cursor_to_start(),
                }
                if ai_busy() {
                    div { class: "absolute inset-0 grid place-items-center bg-popover/70 backdrop-blur-[1px]",
                        span { class: "flex items-center gap-2 text-sm text-muted-foreground",
                            icons::LoaderCircle { class: "size-4 animate-spin" }
                            "Die KI schreibt …"
                        }
                    }
                }
            }
            // Anhänge
            if !attachments().is_empty() || !forwarded().is_empty() || reading() {
                div { class: "flex max-h-28 flex-wrap gap-1.5 overflow-y-auto border-t px-4 py-2",
                    for (i , a) in attachments().into_iter().enumerate() {
                        span { key: "a{i}", class: "inline-flex max-w-full items-center gap-1.5 rounded-md border bg-card px-2 py-1 text-xs",
                            icons::Paperclip { class: "size-3.5 shrink-0 text-muted-foreground" }
                            span { class: "max-w-48 truncate", "{a.filename}" }
                            span { class: "text-muted-foreground", {util::size(util::base64_size(&a.data_base64))} }
                            button { class: "text-muted-foreground hover:text-foreground", title: "Entfernen", "aria-label": "Anhang {a.filename} entfernen",
                                onclick: move |_| { attachments.write().remove(i); dirty.set(true); },
                                icons::X { class: "size-3" }
                            }
                        }
                    }
                    for (i , (a , on)) in forwarded().into_iter().enumerate() {
                        button { key: "f{i}",
                            class: if on { "inline-flex items-center gap-1.5 rounded-md border bg-card px-2 py-1 text-xs" } else { "inline-flex items-center gap-1.5 rounded-md border border-dashed px-2 py-1 text-xs text-muted-foreground line-through" },
                            title: if on { "Nicht mitschicken" } else { "Mitschicken" },
                            onclick: move |_| { if let Some(x) = forwarded.write().get_mut(i) { x.1 = !x.1; } dirty.set(true); },
                            icons::Paperclip { class: "size-3.5 shrink-0" }
                            span { class: "max-w-48 truncate", "{a.filename}" }
                            span { class: "text-muted-foreground", {util::size(a.size)} }
                        }
                    }
                    if reading() {
                        span { class: "inline-flex items-center gap-1.5 px-2 py-1 text-xs text-muted-foreground",
                            icons::LoaderCircle { class: "size-3.5 animate-spin" } "Lese Datei …" }
                    }
                }
            }
            // Fuss
            div { class: "flex flex-wrap items-center gap-1.5 border-t px-3 py-2.5 max-sm:pb-safe-2",
                div { class: "flex items-center",
                    button { class: "{btn(BTN_PRIMARY)} rounded-r-none", disabled: busy() || accounts.is_empty(), title: "Senden (⌘/Ctrl+Enter)",
                        onclick: move |_| send.call(None),
                        if busy() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::Send { class: "size-4" } }
                        "Senden"
                    }
                    Dropdown { side: Side::Top, class: "w-64",
                        trigger_class: "inline-flex h-9 items-center rounded-r-md border-l border-primary-foreground/20 bg-primary px-2 text-primary-foreground hover:bg-primary/90 disabled:opacity-50",
                        trigger: rsx! { icons::ChevronDown { class: "size-4" } },
                        MenuLabel { "Später senden" }
                        for (label , at) in schedule_presets() {
                            MenuItem { onclick: move |_| send.call(Some(at)),
                                icons::Clock {}
                                span { class: "flex-1", "{label}" }
                            }
                        }
                        MenuSeparator {}
                        MenuItem { onclick: move |_| schedule_open.set(true), icons::CalendarClock {} "Datum und Uhrzeit wählen …" }
                    }
                }
                label { class: "{btn(BTN_GHOST)} {BTN_ICON} cursor-pointer", title: "Datei anhängen",
                    icons::Paperclip { class: "size-4" }
                    input { r#type: "file", multiple: true, class: "sr-only", "aria-label": "Datei anhängen",
                        onchange: move |e| add_files.call(util::input_files(&e)) }
                }
                if ai_available {
                    Dropdown { side: Side::Top, class: "w-60",
                        trigger_class: "{btn(BTN_GHOST)} h-9 px-3",
                        trigger: rsx! { icons::Sparkles { class: "size-4" } span { class: "max-[380px]:hidden", "{dieter}" } },
                        MenuItem { onclick: move |_| ai_bar.set(true), icons::WandSparkles {} "{dieter} hilft beim Schreiben …" }
                        if is_reply {
                            MenuItem { onclick: move |_| ai_write.call(("Schreibe eine passende Antwort auf die letzte Nachricht der Unterhaltung.".to_string(), false)),
                                icons::MessagesSquare {} "Antwort aus dem Verlauf schreiben" }
                        }
                        MenuSeparator {}
                        MenuLabel { "Eigenen Text überarbeiten" }
                        for (label , instruction) in [
                            ("Verbessern", "Verbessere Rechtschreibung, Grammatik und Stil. Inhalt und Sprache beibehalten."),
                            ("Kürzen", "Kürze den Text deutlich, ohne wichtige Informationen zu verlieren."),
                            ("Förmlicher", "Formuliere den Text förmlicher und höflicher."),
                            ("Lockerer", "Formuliere den Text lockerer und freundlicher."),
                            ("Auf Englisch übersetzen", "Übersetze den Text ins Englische."),
                            ("Auf Deutsch übersetzen", "Übersetze den Text ins Deutsche."),
                            ("Auf Französisch übersetzen", "Übersetze den Text ins Französische."),
                            ("Auf Italienisch übersetzen", "Übersetze den Text ins Italienische."),
                        ] {
                            MenuItem { onclick: move |_| ai_write.call((instruction.to_string(), true)), "{label}" }
                        }
                    }
                }
                div { class: "flex-1" }
                button { class: "{btn(BTN_GHOST)} {BTN_SM} text-muted-foreground", disabled: busy(), onclick: move |_| save_draft.call((false, false)),
                    icons::Save { class: "size-4" }
                    span { class: "hidden sm:inline", "Entwurf speichern" }
                }
                button { class: "{btn(BTN_GHOST)} {BTN_ICON} text-muted-foreground hover:text-destructive", title: "Verwerfen", "aria-label": "Verwerfen",
                    onclick: discard, icons::Trash2 { class: "size-4" } }
            }
        }
        Modal { open: schedule_open, title: "Später senden", description: "Die Nachricht bleibt bis dahin unter Entwürfe und wird zur gewählten Zeit gesendet.",
            div { class: "grid gap-4",
                Field { label: "Datum und Uhrzeit",
                    input { class: INPUT, r#type: "datetime-local", value: "{schedule_value}", oninput: move |e| schedule_value.set(e.value()) }
                }
                div { class: "flex justify-end gap-2",
                    button { class: btn(BTN_OUTLINE), onclick: move |_| schedule_open.set(false), "Abbrechen" }
                    button { class: btn(BTN_PRIMARY),
                        onclick: move |_| {
                            match util::parse_local_datetime(&schedule_value()) {
                                Some(at) if at > util::now() => {
                                    schedule_open.set(false);
                                    send.call(Some(at));
                                }
                                Some(_) => toast_error("Der Zeitpunkt liegt in der Vergangenheit."),
                                None => toast_error("Bitte Datum und Uhrzeit wählen."),
                            }
                        },
                        icons::CalendarClock { class: "size-4" }
                        "Planen"
                    }
                }
            }
        }
    }
}

/// Text der KI in die Nachricht übernehmen (vor Signatur und Zitat).
fn apply_ai(d: AiDraft, tail: &str, mut body: Signal<String>, mut subject: Signal<String>, mut dirty: Signal<bool>) {
    let text = d.text.trim_end().to_string();
    body.set(if tail.is_empty() { text } else { format!("{text}\n\n{tail}") });
    if subject.peek().trim().is_empty() && !d.subject.trim().is_empty() {
        subject.set(d.subject.trim().to_string());
    }
    dirty.set(true);
}

/// Cursor im Text an den Anfang (über Signatur und Zitat).
fn cursor_to_start() {
    document::eval(
        "requestAnimationFrame(() => { const t = document.getElementById('compose-body'); if (t && document.activeElement === t) t.setSelectionRange(0, 0); })",
    );
}

/// Teil nach dem letzten Trennzeichen (das gerade getippte Stück).
fn last_segment(v: &str) -> &str {
    let mut in_quotes = false;
    let mut start = 0;
    for (i, ch) in v.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' | ';' if !in_quotes => start = i + 1,
            _ => {}
        }
    }
    v[start..].trim_start()
}

/// Vorschlag übernehmen: das letzte Stück ersetzen.
fn apply_contact(v: &str, c: &Contact) -> String {
    let seg = last_segment(v);
    let prefix = v[..v.len() - seg.len()].trim_end();
    let addr = Address::new(c.name.clone(), c.address.clone()).display();
    if prefix.is_empty() { format!("{addr}, ") } else { format!("{prefix} {addr}, ") }
}

/// Empfänger-Feld mit Vorschlägen aus `GET /contacts?q=`.
#[component]
fn AddressField(#[props(into)] id: String, label: &'static str, value: Signal<String>, dirty: Signal<bool>, #[props(default)] autofocus: bool) -> Element {
    let mut value = value;
    let mut dirty = dirty;
    let mut suggestions = use_signal(Vec::<Contact>::new);
    let mut active = use_signal(|| 0usize);
    let mut generation = use_signal(|| 0u64);
    let mut focused = use_signal(|| false);
    let invalid: Vec<String> = if focused() {
        Vec::new()
    } else {
        parse_addresses(&value()).into_iter().filter(|a| !a.is_valid()).map(|a| a.address).collect()
    };
    let invalid_text = invalid.join(", ");
    let list_id = format!("{id}-list");
    let mut lookup = move |v: String| {
        let term = last_segment(&v).trim().to_string();
        *generation.write() += 1;
        let g = *generation.peek();
        if term.chars().count() < 2 {
            suggestions.set(Vec::new());
            return;
        }
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(150).await;
            if *generation.peek() != g {
                return;
            }
            if let Ok(list) = api::get::<Vec<Contact>>(&format!("/contacts?q={}&limit=8", api::enc(&term))).await
                && *generation.peek() == g {
                    active.set(0);
                    suggestions.set(list);
                }
        });
    };
    let mut choose = move |c: Contact| {
        value.set(apply_contact(&value(), &c));
        suggestions.set(Vec::new());
        dirty.set(true);
    };
    let list = suggestions();
    let n = list.len();
    rsx! {
        label { class: "w-14 shrink-0 text-sm text-muted-foreground", r#for: "{id}", "{label}" }
        div { class: "relative min-w-0 flex-1",
            input {
                id: "{id}",
                class: "h-10 w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground max-md:text-base",
                autocomplete: "off",
                autofocus,
                role: "combobox",
                "aria-expanded": "{n > 0}",
                "aria-controls": "{list_id}",
                "aria-invalid": "{!invalid.is_empty()}",
                value: "{value}",
                oninput: move |e| {
                    let v = e.value();
                    value.set(v.clone());
                    dirty.set(true);
                    lookup(v);
                },
                onfocus: move |_| focused.set(true),
                onblur: move |_| {
                    focused.set(false);
                    suggestions.set(Vec::new());
                },
                onkeydown: move |e| {
                    if n == 0 {
                        return;
                    }
                    match e.key() {
                        Key::ArrowDown => { e.prevent_default(); active.set((active() + 1) % n); }
                        Key::ArrowUp => { e.prevent_default(); active.set((active() + n - 1) % n); }
                        Key::Enter | Key::Tab => {
                            if let Some(c) = suggestions().get(active()).cloned() {
                                e.prevent_default();
                                choose(c);
                            }
                        }
                        Key::Escape => { e.stop_propagation(); suggestions.set(Vec::new()); }
                        _ => {}
                    }
                },
            }
            if n > 0 {
                div { id: "{list_id}", role: "listbox", class: "absolute left-0 right-0 top-full z-10 mt-1 overflow-hidden rounded-lg border bg-popover p-1 shadow-lg",
                    for (i , c) in list.into_iter().enumerate() {
                        button { key: "{c.address}",
                            role: "option",
                            "aria-selected": "{i == active()}",
                            class: if i == active() { "flex w-full items-center gap-2.5 rounded-md bg-accent px-2 py-1.5 text-left text-sm" } else { "flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm hover:bg-accent/60" },
                            // Mousedown statt Klick: sonst verliert das Feld zuerst den Fokus
                            onmousedown: move |e| { e.prevent_default(); choose(c.clone()); },
                            Avatar { name: if c.name.is_empty() { c.address.clone() } else { c.name.clone() }, class: "size-6 text-[11px]" }
                            span { class: "min-w-0 flex-1 truncate",
                                if !c.name.is_empty() { span { class: "font-medium", "{c.name} " } }
                                span { class: "text-muted-foreground", "{c.address}" }
                            }
                        }
                    }
                }
            }
            if !invalid.is_empty() {
                p { class: "pb-1.5 text-xs text-destructive", "Ungültig: {invalid_text}" }
            }
        }
    }
}
