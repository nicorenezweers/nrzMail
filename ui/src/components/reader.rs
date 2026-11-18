//! Lesebereich: eine Unterhaltung (`GET /threads/<id>`) bzw. eine Nachricht mit Kopf
//! (Von/An/Cc/Datum/Postfach), Aktionen, Inhalt, Anhängen, KI-Zusammenfassung und
//! „Antwort mit KI“. Aktionen meldet er an die Seite, die sie ausführt und die Liste nachführt.

use crate::api;
use crate::components::common::*;
use crate::components::compose;
use crate::components::mail_body::MailBody;
use crate::components::overlay::*;
use crate::pages::layout::folder_label;
use crate::state::{self, AI, FOLDERS, PREFS};
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{
    AiDraft, AiText, Attachment, Category, ConversationView, DraftReplyRequest, SendResult, FolderRole, MailView, MessageAction, MessageDetail,
    MessageSummary, SummarizeRequest, Thread, format_addresses,
};
use std::collections::HashSet;

/// Geladene Unterhaltung (älteste zuerst) oder Fehlermeldung.
pub type Loaded = Option<Result<Vec<MessageDetail>, String>>;

/// Nachricht laden und – wenn Unterhaltungen gebündelt werden – die ganze Unterhaltung.
pub async fn load(id: &str) -> Result<Vec<MessageDetail>, String> {
    let detail = api::get::<MessageDetail>(&format!("/messages/{}", api::enc(id))).await.map_err(|e| e.message)?;
    if !PREFS.peek().threads || detail.summary.thread_count == 1 {
        return Ok(vec![detail]);
    }
    match api::get::<Thread>(&format!("/threads/{}", api::enc(&detail.summary.thread_id))).await {
        Ok(t) if !t.messages.is_empty() => Ok(t.messages),
        _ => Ok(vec![detail]),
    }
}

/// Die geöffnete Nachricht in der Unterhaltung (sonst die neueste).
pub fn representative<'a>(msgs: &'a [MessageDetail], id: &str) -> Option<&'a MessageDetail> {
    msgs.iter().find(|m| m.summary.id == id).or_else(|| msgs.last())
}

/// Die neueste Nachricht, auf die man antworten würde (nicht die eigene, wenn es andere gibt).
pub fn reply_target(msgs: &[MessageDetail]) -> Option<&MessageDetail> {
    msgs.iter().rev().find(|m| !m.summary.is_sent).or_else(|| msgs.last())
}

/// IDs, die eine Aktion in dieser Unterhaltung betrifft.
pub fn thread_ids(msgs: &[MessageDetail], id: &str, action: &MessageAction) -> Vec<String> {
    let list: Vec<MessageSummary> = msgs.iter().map(|m| m.summary.clone()).collect();
    match representative(msgs, id) {
        Some(rep) => crate::actions::ids_for(&rep.summary, &list, action),
        None => Vec::new(),
    }
}

#[component]
pub fn Reader(
    /// Geöffnete Nachricht
    id: String,
    /// Inhalt, den der Lesebereich lädt (die Seite liest ihn für Tastenkürzel mit)
    thread: Signal<Loaded>,
    view: Option<MailView>,
    on_act: EventHandler<(Vec<String>, MessageAction)>,
    on_close: EventHandler<()>,
    /// Handy: Beschriftung des Zurück-Knopfs (z. B. „Posteingang“)
    #[props(into, default)]
    back_label: String,
    /// Handy: vorherige/nächste Nachricht der Liste (Pfeile oben)
    #[props(default)]
    on_prev: Option<EventHandler<()>>,
    #[props(default)]
    on_next: Option<EventHandler<()>>,
) -> Element {
    let mut thread = thread;
    let phone = state::PHONE();
    // Handy als Web-App: vom linken Rand nach rechts wischen = zurück
    let mut edge = use_signal(|| None::<(f64, f64)>);
    let mut edge_dx = use_signal(|| 0.0f64);
    let mut expanded = use_signal(HashSet::<String>::new);
    let mut summary = use_signal(|| None::<String>);
    let mut summarizing = use_signal(|| false);
    let mut reply_open = use_signal(|| false);

    // Laden, wenn sich die Nachricht ändert
    let load_id = id.clone();
    use_effect(use_reactive!(|load_id| {
        thread.set(None);
        summary.set(None);
        let id = load_id.clone();
        spawn(async move {
            let res = load(&id).await;
            thread.set(Some(res.clone()));
            if let Ok(msgs) = &res {
                // Aufgeklappt: die geöffnete, die neueste und alle ungelesenen
                let mut open: HashSet<String> = msgs.iter().filter(|m| !m.summary.seen).map(|m| m.summary.id.clone()).collect();
                open.insert(id.clone());
                if let Some(last) = msgs.last() {
                    open.insert(last.summary.id.clone());
                }
                expanded.set(open);
                summary.set(msgs.iter().rev().find_map(|m| m.ai_summary.clone()).filter(|s| !s.trim().is_empty()));
                let unseen: Vec<String> = msgs.iter().filter(|m| !m.summary.seen && !m.summary.is_sent).map(|m| m.summary.id.clone()).collect();
                if PREFS.peek().mark_read_on_open && !unseen.is_empty() {
                    on_act.call((unseen, MessageAction::MarkRead));
                }
            }
        });
    }));

    let loaded = thread();
    let msgs = match loaded {
        None => return rsx! { ReaderSkeleton {} },
        Some(Err(e)) => {
            return rsx! {
                div { class: "p-6",
                    BackButton { on_close }
                    EmptyState { title: "Nachricht nicht verfügbar", description: e, icon: rsx! { icons::MailX {} } }
                }
            };
        }
        Some(Ok(m)) if m.is_empty() => return rsx! { div { class: "p-6", BackButton { on_close } } },
        Some(Ok(m)) => m,
    };
    let Some(rep) = representative(&msgs, &id).cloned() else { return rsx! {} };
    let s = rep.summary.clone();
    let ai = AI.read().clone();
    let account = state::account(&s.account_id);
    let folder = s.folder_id.as_ref().and_then(|fid| FOLDERS.read().iter().find(|f| &f.id == fid).cloned());
    let role = folder.as_ref().map(|f| f.role);
    let in_trash = role == Some(FolderRole::Trash) || view == Some(MailView::Trash);
    let in_junk = role == Some(FolderRole::Junk) || view == Some(MailView::Junk);
    let in_archive = role == Some(FolderRole::Archive);
    let move_targets: Vec<_> = FOLDERS.read().iter().filter(|f| f.account_id == s.account_id && Some(&f.id) != s.folder_id.as_ref()).cloned().collect();
    let subject = if s.subject.trim().is_empty() { "(kein Betreff)".to_string() } else { s.subject.clone() };
    let flagged = msgs.iter().any(|m| m.summary.flagged);
    let in_drafts = role == Some(FolderRole::Drafts);

    let act = {
        let msgs = msgs.clone();
        let id = id.clone();
        move |action: MessageAction| {
            let ids = thread_ids(&msgs, &id, &action);
            on_act.call((ids, action));
        }
    };
    let a1 = act.clone();
    let a2 = act.clone();
    let a3 = act.clone();
    let a4 = act.clone();
    let a5 = act.clone();
    let a6 = act.clone();
    let a7 = act.clone();
    let a8 = act.clone();
    let (b1, b4, b5) = (act.clone(), act.clone(), act.clone());

    let target = reply_target(&msgs).cloned().unwrap_or_else(|| rep.clone());
    let t_reply = target.clone();
    let t_all = target.clone();
    let t_fwd = target.clone();
    let t_ai = target.clone();
    let (t_reply2, t_all2, t_fwd2) = (target.clone(), target.clone(), target.clone());
    let dieter = if ai.assistant_name.trim().is_empty() { "Dieter".to_string() } else { ai.assistant_name.trim().to_string() };
    let move_targets2 = move_targets.clone();
    let thread_id = s.thread_id.clone();
    let multi = msgs.len() > 1;
    let rep_id = s.id.clone();

    let summarize = use_callback(move |_: ()| {
        let req = if multi {
            SummarizeRequest { message_id: None, thread_id: Some(thread_id.clone()) }
        } else {
            SummarizeRequest { message_id: Some(rep_id.clone()), thread_id: None }
        };
        spawn(async move {
            summarizing.set(true);
            match api::post::<AiText, _>("/ai/summarize", &req).await {
                Ok(t) => summary.set(Some(t.text)),
                Err(e) => toast_error(e),
            }
            summarizing.set(false);
        });
    });

    let tool = "grid size-9 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/60 [&_svg]:size-4";
    let tool_lg = tool.replace("size-9", "size-10");
    let raw_url = api::url(&format!("/messages/{}/raw", api::enc(&s.id)));
    let eml_name = format!("{}.eml", sanitize_filename(&subject));
    let unsubscribe = rep.list_unsubscribe.clone().filter(|u| !u.trim().is_empty());
    let last_id = msgs.last().map(|m| m.summary.id.clone()).unwrap_or_default();
    let detail_for_edit = rep.clone();
    // Chat-Ansicht (Einstellung) für Unterhaltungen mit mehreren Nachrichten
    let chat_view = multi && PREFS.read().conversation_view == ConversationView::Chat;

    let edge_offset = edge_dx();
    rsx! {
        div { class: "flex h-full min-h-0 flex-col bg-background",
            class: if edge().is_none() { "transition-transform duration-200" } else { "" },
            style: if edge_offset > 0.0 { "transform: translateX({edge_offset}px)" } else { "transform: none" },
            ontouchstart: move |e| {
                if !phone || !util::standalone() {
                    return;
                }
                if let Some(t) = e.touches().into_iter().next() {
                    let p = t.client_coordinates();
                    if p.x < 24.0 {
                        edge.set(Some((p.x, p.y)));
                    }
                }
            },
            ontouchmove: move |e| {
                let Some((sx, sy)) = edge() else { return };
                let touches = e.touches();
                let Some(t) = touches.first() else { return };
                let p = t.client_coordinates();
                if (p.y - sy).abs() > 60.0 && edge_dx() < 30.0 {
                    edge.set(None);
                    edge_dx.set(0.0);
                    return;
                }
                edge_dx.set((p.x - sx).max(0.0));
            },
            ontouchend: move |_| {
                let go = edge().is_some() && edge_dx() > 90.0;
                edge.set(None);
                edge_dx.set(0.0);
                if go {
                    on_close.call(());
                }
            },
            // Aktionen
            div { class: "flex h-14 shrink-0 items-center gap-0.5 border-b px-2 md:px-3 max-md:h-12 max-md:px-1",
                if phone {
                    button { class: "flex h-10 min-w-0 max-w-[55%] items-center gap-0.5 rounded-md pr-2 text-[15px] font-medium active:bg-accent",
                        "aria-label": "Zurück zur Liste", onclick: move |_| on_close.call(()),
                        icons::ChevronLeft { class: "size-6 shrink-0" }
                        span { class: "truncate", if back_label.is_empty() { "Zurück" } else { "{back_label}" } }
                    }
                } else {
                    button { class: tool, title: "Zurück zur Liste (Esc)", "aria-label": "Zurück zur Liste", onclick: move |_| on_close.call(()),
                        icons::ArrowLeft {}
                    }
                }
                if phone {
                    div { class: "flex-1" }
                    button { class: "{tool_lg} disabled:opacity-30", title: "Vorherige Nachricht", "aria-label": "Vorherige Nachricht", disabled: on_prev.is_none(),
                        onclick: move |_| if let Some(h) = on_prev { h.call(()) },
                        icons::ChevronUp { class: "size-5" }
                    }
                    button { class: "{tool_lg} disabled:opacity-30", title: "Nächste Nachricht", "aria-label": "Nächste Nachricht", disabled: on_next.is_none(),
                        onclick: move |_| if let Some(h) = on_next { h.call(()) },
                        icons::ChevronDown { class: "size-5" }
                    }
                }
                if !phone {
                div { class: "mx-1 h-5 w-px bg-border" }
                if in_trash || in_junk || in_archive {
                    button { class: tool, title: "In den Posteingang", "aria-label": "In den Posteingang", onclick: move |_| a1(MessageAction::Restore), icons::ArchiveRestore {} }
                } else {
                    button { class: tool, title: "Archivieren (e)", "aria-label": "Archivieren", onclick: move |_| a1(MessageAction::Archive), icons::Archive {} }
                }
                if in_trash {
                    button { class: "{tool} hover:text-destructive", title: "Endgültig löschen (#)", "aria-label": "Endgültig löschen", onclick: move |_| a2(MessageAction::Delete), icons::Trash2 {} }
                } else {
                    button { class: tool, title: "Löschen (#)", "aria-label": "Löschen", onclick: move |_| a2(MessageAction::Trash), icons::Trash2 {} }
                }
                if in_junk {
                    button { class: tool, title: "Kein Spam", "aria-label": "Kein Spam", onclick: move |_| a3(MessageAction::NotSpam), icons::ShieldCheck {} }
                } else {
                    button { class: tool, title: "Spam", "aria-label": "Als Spam markieren", onclick: move |_| a3(MessageAction::Spam), icons::ShieldAlert {} }
                }
                Dropdown { class: "max-h-80 w-64 overflow-y-auto",
                    trigger_class: tool,
                    trigger: rsx! { span { title: "Verschieben", "aria-label": "Verschieben", icons::FolderInput {} } },
                    MenuLabel { "Verschieben nach" }
                    if move_targets.is_empty() {
                        div { class: "px-2 py-1.5 text-sm text-muted-foreground", "Keine anderen Ordner" }
                    }
                    for f in move_targets.iter().cloned() {
                        MenuItem { onclick: { let a = a4.clone(); let fid = f.id.clone(); move |_| a(MessageAction::Move { folder_id: fid.clone() }) },
                            {crate::pages::layout::role_icon(f.role, false)}
                            span { class: "truncate", style: "padding-left: {f.depth * 10}px", {folder_label(&f)} }
                        }
                    }
                }
                div { class: "mx-1 h-5 w-px bg-border" }
                button { class: tool, title: "Als ungelesen markieren (u)", "aria-label": "Als ungelesen markieren", onclick: move |_| a5(MessageAction::MarkUnread), icons::Mail {} }
                }
                button { class: tool,
                    title: if flagged { "Stern entfernen (s)" } else { "Mit Stern markieren (s)" },
                    "aria-pressed": "{flagged}",
                    "aria-label": "Stern",
                    onclick: move |_| a6(if flagged { MessageAction::Unflag } else { MessageAction::Flag }),
                    if flagged { icons::Star { class: "fill-current text-foreground" } } else { icons::Star {} }
                }
                Dropdown { align: Align::End, class: "w-64",
                    trigger_class: tool,
                    trigger: rsx! { span { title: "Mehr", "aria-label": "Mehr Aktionen", icons::EllipsisVertical {} } },
                    if in_drafts {
                        MenuItem { onclick: move |_| compose::edit_message(&detail_for_edit), icons::Pencil {} "Entwurf bearbeiten" }
                    }
                    if phone {
                        MenuItem { onclick: move |_| b4(MessageAction::MarkUnread), icons::Mail {} "Als ungelesen markieren" }
                        if in_junk {
                            MenuItem { onclick: move |_| b5(MessageAction::NotSpam), icons::ShieldCheck {} "Kein Spam" }
                        } else {
                            MenuItem { onclick: move |_| b5(MessageAction::Spam), icons::ShieldAlert {} "Als Spam markieren" }
                        }
                        MenuSeparator {}
                    }
                    MenuItem { onclick: { let url = raw_url.clone(); let name = eml_name.clone(); move |_| download(&url, &name) },
                        icons::Download {} "Als .eml herunterladen"
                    }
                    MenuItem { onclick: { let id = s.id.clone(); let name = sanitize_filename(&subject); move |_| download(&api::url(&format!("/messages/{}/pdf?tz={}", api::enc(&id), util::tz_offset_minutes())), &format!("{name}.pdf")) },
                        icons::FileText {} "Als PDF herunterladen"
                    }
                    if multi {
                        MenuItem { onclick: { let tid = s.thread_id.clone(); let name = sanitize_filename(&subject); move |_| download(&api::url(&format!("/threads/{}/pdf?tz={}", api::enc(&tid), util::tz_offset_minutes())), &format!("{name} (Unterhaltung).pdf")) },
                            icons::FileText {} "Ganze Unterhaltung als PDF"
                        }
                    }
                    if unsubscribe.is_some() || s.category == Category::Newsletter {
                        MenuItem { onclick: { let id = s.id.clone(); let u = unsubscribe.clone(); move |_| unsubscribe_from(id.clone(), u.clone()) }, icons::BellOff {} "Vom Newsletter abmelden" }
                    }
                    MenuSeparator {}
                    MenuLabel { "Einteilen als" }
                    for c in Category::ALL {
                        MenuItem { active: s.category == c, onclick: { let a = a7.clone(); move |_| a(MessageAction::SetCategory { category: c }) }, "{c.label()}" }
                    }
                    if !in_trash {
                        MenuSeparator {}
                        MenuItem { destructive: true, onclick: move |_| a8(MessageAction::Delete), icons::Trash2 {} "Endgültig löschen" }
                    }
                }
                if !phone { div { class: "flex-1" } }
                if ai.available && !phone {
                    button { class: "{btn(BTN_GHOST)} {BTN_SM} max-sm:hidden", disabled: summarizing(), onclick: move |_| summarize.call(()),
                        if summarizing() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::Sparkles { class: "size-4" } }
                        "Zusammenfassen"
                    }
                }
            }
            // Inhalt
            div { class: "min-h-0 flex-1 overflow-y-auto", id: "reader-scroll",
                div { class: "mx-auto max-w-4xl px-3 py-5 md:px-6 max-md:pt-4",
                    div { class: "mb-4 grid grid-cols-[minmax(0,1fr)] gap-2 max-md:px-1",
                        h1 { class: "text-xl font-semibold leading-snug tracking-tight break-words max-md:text-[22px]", "{subject}" }
                        div { class: "flex flex-wrap items-center gap-1.5",
                            if let Some(a) = account.as_ref() {
                                Pill { Dot { color: a.color.clone(), class: "size-1.5" } "{a.label()}" }
                            }
                            if let Some(f) = folder.as_ref() {
                                Pill { {folder_label(f)} }
                            } else if let Some(n) = rep.folder_name.clone() {
                                Pill { "{n}" }
                            }
                            if s.category != Category::Primary && !s.is_sent {
                                Pill { "{s.category.label()}" }
                            }
                            if !s.on_server {
                                Pill { icons::Database {} "Nur im Archiv" }
                            }
                            if multi {
                                Pill { "{msgs.len()} Nachrichten" }
                            }
                        }
                    }
                    if ai.available {
                        if let Some(text) = summary() {
                            div { class: "mb-4 rounded-xl border bg-card p-4 text-sm",
                                div { class: "mb-2 flex items-center gap-2 text-xs font-medium uppercase tracking-wider text-muted-foreground",
                                    icons::Sparkles { class: "size-3.5" }
                                    "Zusammenfassung"
                                    span { class: "flex-1" }
                                    button { class: "rounded p-0.5 hover:text-foreground", title: "Ausblenden", onclick: move |_| summary.set(None), icons::X { class: "size-3.5" } }
                                }
                                div { class: "leading-relaxed", {crate::components::assistant::markdown(&text)} }
                            }
                        } else if summarizing() {
                            div { class: "mb-4 grid gap-2 rounded-xl border bg-card p-4",
                                crate::ui::skeleton::Skeleton { class: "h-3 w-3/4" }
                                crate::ui::skeleton::Skeleton { class: "h-3 w-2/3" }
                                crate::ui::skeleton::Skeleton { class: "h-3 w-1/2" }
                            }
                        }
                    }
                    if chat_view {
                        ChatThread { messages: msgs.clone() }
                        if PREFS.read().chat_quick_reply {
                            QuickReply { key: "{last_id}", target: target.clone(), ai: ai.available }
                        }
                    } else {
                    div { class: "grid grid-cols-[minmax(0,1fr)] gap-3",
                        for m in msgs.iter().cloned() {
                            MessageCard {
                                key: "{m.summary.id}",
                                open: expanded.read().contains(&m.summary.id),
                                last: m.summary.id == last_id,
                                on_toggle: move |mid: String| {
                                    let mut e = expanded.write();
                                    if !e.remove(&mid) { e.insert(mid); }
                                },
                                message: m,
                            }
                        }
                    }
                    }
                    // Antworten (Handy: in der Leiste unten)
                    div { class: "mt-5 flex flex-wrap gap-2 max-md:hidden",
                        button { class: btn(BTN_OUTLINE), onclick: move |_| compose::reply(&t_reply, false), icons::Reply { class: "size-4" } "Antworten" }
                        button { class: btn(BTN_OUTLINE), onclick: move |_| compose::reply(&t_all, true), icons::ReplyAll { class: "size-4" } "Allen antworten" }
                        button { class: btn(BTN_OUTLINE), onclick: move |_| compose::forward(&t_fwd), icons::Forward { class: "size-4" } "Weiterleiten" }
                        if ai.available {
                            button { class: btn(BTN_OUTLINE), onclick: move |_| reply_open.set(true), icons::WandSparkles { class: "size-4" } "Antwort mit KI" }
                            button { class: "{btn(BTN_OUTLINE)} sm:hidden", disabled: summarizing(), onclick: move |_| summarize.call(()),
                                icons::Sparkles { class: "size-4" } "Zusammenfassen" }
                        }
                    }
                }
            }
            if phone {
                PhoneReaderBar {
                    restore: in_trash || in_junk || in_archive,
                    in_trash,
                    move_targets: move_targets2,
                    ai: ai.available,
                    dieter: dieter.clone(),
                    summarizing: summarizing(),
                    on_act: move |a: MessageAction| b1(a),
                    on_summarize: move |_| {
                        summarize.call(());
                        document::eval("document.getElementById('reader-scroll')?.scrollTo({ top: 0, behavior: 'smooth' })");
                    },
                    on_ai_reply: move |_| reply_open.set(true),
                    on_reply: move |k: u8| match k {
                        0 => compose::reply(&t_reply2, false),
                        1 => compose::reply(&t_all2, true),
                        _ => compose::forward(&t_fwd2),
                    },
                }
            }
        }
        AiReplyDialog { open: reply_open, message: t_ai }
    }
}

/// Handy: Werkzeugleiste unten im Lesebereich – Archivieren, Löschen, Verschieben, KI und
/// Antworten in Daumenreichweite. Menüs öffnen als Blatt von unten.
#[component]
fn PhoneReaderBar(
    restore: bool,
    in_trash: bool,
    move_targets: Vec<nrzmail_api::Folder>,
    ai: bool,
    dieter: String,
    summarizing: bool,
    on_act: EventHandler<MessageAction>,
    on_summarize: EventHandler<()>,
    on_ai_reply: EventHandler<()>,
    on_reply: EventHandler<u8>,
) -> Element {
    let tool = "grid size-11 place-items-center rounded-xl text-foreground active:bg-accent [&_svg]:size-[22px]";
    rsx! {
        div { class: "box-content flex h-[3.25rem] shrink-0 items-center justify-around border-t bg-background px-1 pb-safe",
            if restore {
                button { class: tool, title: "In den Posteingang", "aria-label": "In den Posteingang", onclick: move |_| on_act.call(MessageAction::Restore), icons::ArchiveRestore {} }
            } else {
                button { class: tool, title: "Archivieren", "aria-label": "Archivieren", onclick: move |_| on_act.call(MessageAction::Archive), icons::Archive {} }
            }
            if in_trash {
                button { class: tool.replace("text-foreground", "text-destructive"), title: "Endgültig löschen", "aria-label": "Endgültig löschen", onclick: move |_| on_act.call(MessageAction::Delete), icons::Trash2 {} }
            } else {
                button { class: tool, title: "Löschen", "aria-label": "Löschen", onclick: move |_| on_act.call(MessageAction::Trash), icons::Trash2 {} }
            }
            Dropdown { side: Side::Top, trigger_class: tool,
                trigger: rsx! { span { title: "Verschieben", "aria-label": "Verschieben", icons::FolderInput {} } },
                MenuLabel { "Verschieben nach" }
                if move_targets.is_empty() {
                    div { class: "px-3 py-2 text-sm text-muted-foreground", "Keine anderen Ordner" }
                }
                for f in move_targets.iter().cloned() {
                    MenuItem { onclick: { let fid = f.id.clone(); move |_| on_act.call(MessageAction::Move { folder_id: fid.clone() }) },
                        {crate::pages::layout::role_icon(f.role, false)}
                        span { class: "truncate", style: "padding-left: {f.depth * 12}px", {folder_label(&f)} }
                    }
                }
            }
            if ai {
                Dropdown { side: Side::Top, align: Align::End, trigger_class: tool,
                    trigger: rsx! {
                        span { title: "{dieter}", "aria-label": "KI: {dieter}",
                            if summarizing { icons::LoaderCircle { class: "animate-spin" } } else { icons::Sparkles {} }
                        }
                    },
                    MenuLabel { "{dieter} zu dieser Nachricht" }
                    MenuItem { onclick: move |_| on_summarize.call(()), icons::Sparkles {} "Zusammenfassen" }
                    MenuItem { onclick: move |_| on_ai_reply.call(()), icons::WandSparkles {} "Antwort schreiben lassen …" }
                    MenuSeparator {}
                    MenuLabel { "Fragen" }
                    MenuItem { onclick: move |_| crate::components::assistant::ask("Was wird in dieser Nachricht von mir erwartet? Gibt es Fristen oder Termine?"),
                        icons::ListTodo {} "Was wird von mir erwartet?" }
                    MenuItem { onclick: move |_| crate::components::assistant::ask("Gibt es frühere Mails zu diesem Thema? Fasse zusammen, was bisher geschah."),
                        icons::History {} "Frühere Mails zum Thema" }
                    MenuItem { onclick: move |_| crate::components::assistant::open(), icons::MessageCircle {} "{dieter} etwas fragen …" }
                }
            }
            Dropdown { side: Side::Top, align: Align::End, trigger_class: tool,
                trigger: rsx! { span { title: "Antworten", "aria-label": "Antworten", icons::Reply {} } },
                MenuItem { onclick: move |_| on_reply.call(0), icons::Reply {} "Antworten" }
                MenuItem { onclick: move |_| on_reply.call(1), icons::ReplyAll {} "Allen antworten" }
                MenuItem { onclick: move |_| on_reply.call(2), icons::Forward {} "Weiterleiten" }
                if ai {
                    MenuSeparator {}
                    MenuItem { onclick: move |_| on_ai_reply.call(()), icons::WandSparkles {} "Antwort mit KI …" }
                }
            }
        }
    }
}

#[component]
fn BackButton(on_close: EventHandler<()>) -> Element {
    rsx! {
        button { class: "{btn(BTN_GHOST)} {BTN_SM} mb-4 md:hidden", onclick: move |_| on_close.call(()),
            icons::ArrowLeft { class: "size-4" } "Zurück"
        }
    }
}

#[component]
fn ReaderSkeleton() -> Element {
    use crate::ui::skeleton::Skeleton;
    rsx! {
        div { class: "flex h-full flex-col", "aria-busy": "true",
            div { class: "flex h-14 items-center gap-2 border-b px-3",
                for i in 0..6 { Skeleton { key: "{i}", class: "size-8" } }
            }
            div { class: "mx-auto grid w-full max-w-4xl gap-4 px-6 py-6",
                Skeleton { class: "h-6 w-2/3" }
                div { class: "flex items-center gap-3",
                    Skeleton { class: "size-9 rounded-full" }
                    div { class: "grid flex-1 gap-2", Skeleton { class: "h-3 w-1/3" } Skeleton { class: "h-3 w-1/4" } }
                }
                Skeleton { class: "h-64 w-full" }
            }
        }
    }
}

/// Eine Nachricht der Unterhaltung – eingeklappt nur Absender, Vorschau und Datum.
#[component]
fn MessageCard(message: MessageDetail, open: bool, last: bool, on_toggle: EventHandler<String>) -> Element {
    let m = message;
    let s = m.summary.clone();
    let mut details = use_signal(|| false);
    let mut images = use_signal(|| PREFS.peek().load_remote_images);
    let from_name = s.from.label().to_string();
    let mid = s.id.clone();
    let mid2 = s.id.clone();
    if !open {
        return rsx! {
            button { class: "flex w-full items-center gap-3 rounded-xl border bg-card/60 px-4 py-3 text-left hover:bg-accent/40",
                onclick: move |_| on_toggle.call(mid.clone()),
                Avatar { name: from_name.clone(), class: "size-8" }
                span { class: if s.seen { "w-40 shrink-0 truncate text-sm" } else { "w-40 shrink-0 truncate text-sm font-semibold" }, "{from_name}" }
                span { class: "min-w-0 flex-1 truncate text-sm text-muted-foreground", "{s.snippet}" }
                if s.has_attachments { icons::Paperclip { class: "size-3.5 shrink-0 text-muted-foreground" } }
                span { class: "shrink-0 text-xs text-muted-foreground", {util::relative(s.date)} }
            }
        };
    }
    let to_names: Vec<String> = s.to.iter().map(|a| a.label().to_string()).collect();
    let to_short = if to_names.is_empty() { "(keine Empfänger)".to_string() } else { to_names.join(", ") };
    let attachments: Vec<Attachment> = m.attachments.iter().filter(|a| !a.inline).cloned().collect();
    let (r1, r2, r3) = (m.clone(), m.clone(), m.clone());
    let show_images_banner = m.has_remote_images && !images() && m.html_body.is_some();
    rsx! {
        article { class: "overflow-hidden rounded-xl border bg-card",
            header { class: "flex items-start gap-3 px-4 py-3",
                Avatar { name: from_name.clone() }
                div { class: "min-w-0 flex-1",
                    button { class: "flex w-full min-w-0 flex-wrap items-baseline gap-x-2 text-left", onclick: move |_| if !last { on_toggle.call(mid2.clone()) },
                        span { class: "truncate font-semibold", "{from_name}" }
                        if !s.from.name.trim().is_empty() {
                            span { class: "truncate text-xs text-muted-foreground", "{s.from.address}" }
                        }
                    }
                    button { class: "mt-0.5 flex max-w-full items-center gap-1 text-left text-xs text-muted-foreground hover:text-foreground",
                        "aria-expanded": "{details()}",
                        onclick: move |_| details.toggle(),
                        span { class: "truncate", "an {to_short}" }
                        if details() { icons::ChevronDown { class: "size-3 shrink-0 rotate-180" } } else { icons::ChevronDown { class: "size-3 shrink-0" } }
                    }
                    if details() {
                        dl { class: "mt-2 grid grid-cols-[4.5rem_minmax(0,1fr)] gap-x-2 gap-y-1 rounded-lg border bg-background/40 p-3 text-xs",
                            dt { class: "text-muted-foreground", "Von" }
                            dd { class: "break-all", {s.from.display()} }
                            dt { class: "text-muted-foreground", "An" }
                            dd { class: "break-all", {format_addresses(&s.to)} }
                            if !m.cc.is_empty() {
                                dt { class: "text-muted-foreground", "Cc" }
                                dd { class: "break-all", {format_addresses(&m.cc)} }
                            }
                            if !m.bcc.is_empty() {
                                dt { class: "text-muted-foreground", "Bcc" }
                                dd { class: "break-all", {format_addresses(&m.bcc)} }
                            }
                            if !m.reply_to.is_empty() {
                                dt { class: "text-muted-foreground", "Antwort an" }
                                dd { class: "break-all", {format_addresses(&m.reply_to)} }
                            }
                            dt { class: "text-muted-foreground", "Datum" }
                            dd { {util::datetime_long(s.date)} }
                            dt { class: "text-muted-foreground", "Postfach" }
                            dd { class: "break-all", "{m.account_email}" }
                            if let Some(f) = m.folder_name.clone() {
                                dt { class: "text-muted-foreground", "Ordner" }
                                dd { "{f}" }
                            }
                            dt { class: "text-muted-foreground", "Grösse" }
                            dd { {util::size(s.size)} }
                        }
                    }
                }
                div { class: "flex shrink-0 items-center gap-0.5",
                    span { class: "mr-1 whitespace-nowrap text-xs text-muted-foreground", title: "{util::datetime_long(s.date)}", {util::datetime(s.date)} }
                    button { class: "grid size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground", title: "Antworten", "aria-label": "Antworten",
                        onclick: move |_| compose::reply(&r1, false), icons::Reply { class: "size-4" } }
                    Dropdown { align: Align::End, class: "w-52",
                        trigger_class: "grid size-8 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground",
                        trigger: rsx! { span { "aria-label": "Mehr", icons::Ellipsis { class: "size-4" } } },
                        MenuItem { onclick: move |_| compose::reply(&r2, true), icons::ReplyAll {} "Allen antworten" }
                        MenuItem { onclick: move |_| compose::forward(&r3), icons::Forward {} "Weiterleiten" }
                        MenuItem { onclick: { let u = api::url(&format!("/messages/{}/raw", api::enc(&s.id))); let n = format!("{}.eml", sanitize_filename(&s.subject)); move |_| download(&u, &n) },
                            icons::Download {} "Als .eml herunterladen"
                        }
                    }
                }
            }
            if show_images_banner {
                div { class: "mx-4 mb-3 flex flex-wrap items-center gap-2 rounded-lg border bg-muted/40 px-3 py-2 text-xs text-muted-foreground",
                    icons::Image { class: "size-4 shrink-0" }
                    span { class: "min-w-0 flex-1 basis-56", "Externe Bilder sind blockiert – so erfährt der Absender nicht, wann du die Nachricht öffnest." }
                    button { class: "{btn(BTN_OUTLINE)} h-7 px-2.5 text-xs", onclick: move |_| images.set(true), "Bilder laden" }
                    button { class: "h-7 px-1.5 text-xs underline-offset-2 hover:text-foreground hover:underline",
                        onclick: move |_| {
                            images.set(true);
                            let mut p = PREFS.peek().clone();
                            p.load_remote_images = true;
                            spawn(async move {
                                match state::save_prefs(p).await {
                                    Ok(()) => toast("Externe Bilder werden ab jetzt immer geladen."),
                                    Err(e) => toast_error(e),
                                }
                            });
                        },
                        "Immer laden"
                    }
                }
            }
            div { class: "px-4 pb-4",
                MailBody { id: s.id.clone(), html: m.html_body.clone(), text: m.text_body.clone(), load_images: images() }
            }
            if !attachments.is_empty() {
                div { class: "border-t px-4 py-3",
                    p { class: "mb-2 text-xs font-medium text-muted-foreground",
                        if attachments.len() == 1 { "1 Anhang" } else { "{attachments.len()} Anhänge" }
                    }
                    div { class: "flex flex-wrap gap-2",
                        for a in attachments {
                            AttachmentChip { key: "{a.id}", attachment: a }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn AttachmentChip(attachment: Attachment) -> Element {
    let a = attachment;
    let url = api::url(&format!("/attachments/{}", api::enc(&a.id)));
    let is_image = a.content_type.starts_with("image/");
    let preview = format!("{url}?inline=true");
    rsx! {
        a { href: "{url}", download: "{a.filename}",
            class: "group flex max-w-xs items-center gap-2.5 rounded-lg border bg-background/40 p-2 pr-3 text-sm hover:bg-accent/50",
            title: "{a.filename} herunterladen",
            if is_image {
                img { src: "{preview}", alt: "", loading: "lazy", class: "size-10 shrink-0 rounded object-cover" }
            } else {
                span { class: "grid size-10 shrink-0 place-items-center rounded bg-muted text-muted-foreground", icons::File { class: "size-5" } }
            }
            span { class: "min-w-0",
                span { class: "block truncate font-medium", "{a.filename}" }
                span { class: "block text-xs text-muted-foreground", {util::size(a.size)} }
            }
            icons::Download { class: "size-4 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100" }
        }
    }
}

/// Antwort von der KI entwerfen lassen und im Editor öffnen.
#[component]
fn AiReplyDialog(open: Signal<bool>, message: MessageDetail) -> Element {
    let mut open = open;
    let mut instruction = use_signal(String::new);
    let mut tone = use_signal(|| "freundlich".to_string());
    let mut all = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let m = message.clone();
    let go = move |_| {
        let m = m.clone();
        spawn(async move {
            busy.set(true);
            let req = DraftReplyRequest { message_id: m.summary.id.clone(), instruction: instruction().trim().to_string(), tone: tone() };
            let res = api::post::<AiDraft, _>("/ai/reply", &req).await;
            busy.set(false);
            match res {
                Ok(d) => {
                    open.set(false);
                    instruction.set(String::new());
                    compose::reply_with(&m, all(), d.text.trim_end(), Some(d.subject).filter(|s| !s.trim().is_empty()));
                }
                Err(e) => toast_error(e),
            }
        });
    };
    rsx! {
        Modal { open, title: "Antwort mit KI", description: "Die KI entwirft eine Antwort – du kannst sie vor dem Senden bearbeiten.",
            div { class: "grid gap-4",
                Field { label: "Was soll die Antwort sagen?", hint: "Leer lassen, dann entscheidet die KI.",
                    textarea { class: TEXTAREA, rows: 3, placeholder: "z. B. zusagen, aber erst ab Dienstag", value: "{instruction}",
                        oninput: move |e| instruction.set(e.value()) }
                }
                Field { label: "Ton",
                    select { class: SELECT, value: "{tone}", onchange: move |e| tone.set(e.value()),
                        for (v , l) in [("freundlich", "Freundlich"), ("förmlich", "Förmlich"), ("kurz", "Kurz und knapp"), ("locker", "Locker"), ("bestimmt", "Bestimmt")] {
                            option { value: v, selected: tone() == v, "{l}" }
                        }
                    }
                }
                Checkbox { checked: all(), onchange: move |v| all.set(v), label: "Allen antworten" }
                div { class: "flex justify-end gap-2",
                    button { class: btn(BTN_OUTLINE), onclick: move |_| open.set(false), "Abbrechen" }
                    button { class: btn(BTN_PRIMARY), disabled: busy(), onclick: go,
                        if busy() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::WandSparkles { class: "size-4" } }
                        "Entwerfen"
                    }
                }
            }
        }
    }
}

/// Eigener Teil einer Nachricht: ohne zitierte frühere Nachrichten („Am … schrieb …:“,
/// „On … wrote:“, „-----Original Message-----“, Zeilen mit „>“) und ohne Signatur nach „-- “.
pub fn own_part(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let l = line.trim();
        let lower = l.to_lowercase();
        if line.trim_end() == "--" || line == &"-- " {
            break;
        }
        if l.starts_with('>') {
            continue;
        }
        let header_start = (lower.starts_with("am ") && lower.contains("schrieb"))
            || (lower.starts_with("on ") && lower.contains("wrote"))
            || (lower.starts_with("le ") && lower.contains("a écrit"))
            || (lower.starts_with("il ") && lower.contains("ha scritto"));
        // „Am … schrieb“ kann über zwei Zeilen laufen
        let two_line = (lower.starts_with("am ") || lower.starts_with("on "))
            && lines.get(i + 1).is_some_and(|n| { let n = n.to_lowercase(); n.contains("schrieb") || n.contains("wrote") });
        if header_start || two_line
            || (l.starts_with("-----") && (lower.contains("original") || lower.contains("ursprüngliche")))
            || ((lower.starts_with("von:") || lower.starts_with("from:")) && lines.get(i + 1).is_some_and(|n| { let n = n.to_lowercase(); n.starts_with("gesendet:") || n.starts_with("sent:") || n.starts_with("datum:") || n.starts_with("date:") }))
        {
            break;
        }
        out.push(line);
    }
    let joined = out.join("\n");
    let trimmed = joined.trim();
    if trimmed.is_empty() { text.trim().to_string() } else { trimmed.to_string() }
}

/// Unterhaltung als Chat: Sprechblasen, eigene Nachrichten rechts, Zitate ausgeblendet
/// (Einstellung), „Ganze Nachricht“ zeigt die Mail vollständig mit HTML.
#[component]
fn ChatThread(messages: Vec<MessageDetail>) -> Element {
    let hide_quotes = PREFS.read().chat_hide_quotes;
    let own: Vec<String> = state::ACCOUNTS.read().iter().map(|a| a.email.to_lowercase()).collect();
    let mut full = use_signal(HashSet::<String>::new);
    let mut last_day = String::new();
    let mut rows: Vec<(Option<String>, MessageDetail, bool)> = Vec::new();
    for m in messages {
        let day = util::date(m.summary.date);
        let sep = (day != last_day).then(|| day.clone());
        last_day = day;
        let mine = m.summary.is_sent || own.contains(&m.summary.from.address.to_lowercase());
        rows.push((sep, m, mine));
    }
    rsx! {
        div { class: "flex flex-col gap-2", "aria-label": "Unterhaltung als Chat",
            for (sep , m , mine) in rows {
                if let Some(d) = sep {
                    div { key: "d{m.summary.id}", class: "my-2 flex items-center gap-3 text-[11px] uppercase tracking-wider text-muted-foreground",
                        span { class: "h-px flex-1 bg-border" } "{d}" span { class: "h-px flex-1 bg-border" }
                    }
                }
                {
                    let id = m.summary.id.clone();
                    let show_full = full.read().contains(&id);
                    let text = if hide_quotes { own_part(&m.text_body) } else { m.text_body.trim().to_string() };
                    let name = m.summary.from.label().to_string();
                    let atts: Vec<Attachment> = m.attachments.iter().filter(|a| !a.inline).cloned().collect();
                    let toggle_id = id.clone();
                    let r = m.clone();
                    rsx! {
                        div { key: "{id}", class: if mine { "flex items-end justify-end gap-2" } else { "flex items-end gap-2" },
                            if !mine { Avatar { name: name.clone(), class: "size-7 shrink-0" } }
                            div { class: if mine { "flex max-w-[85%] flex-col items-end" } else { "flex max-w-[85%] flex-col items-start" },
                                if !mine {
                                    span { class: "mb-0.5 px-1 text-xs text-muted-foreground", "{name}" }
                                }
                                div { class: if mine { "rounded-2xl rounded-br-md bg-primary px-3.5 py-2 text-sm leading-relaxed text-primary-foreground" } else { "rounded-2xl rounded-bl-md border bg-card px-3.5 py-2 text-sm leading-relaxed" },
                                    if show_full {
                                        div { class: "min-w-[min(36rem,70vw)] rounded-lg bg-background p-2 text-foreground",
                                            MailBody { id: id.clone(), html: m.html_body.clone(), text: m.text_body.clone(), load_images: PREFS.read().load_remote_images }
                                        }
                                    } else {
                                        p { class: "whitespace-pre-wrap break-words", "{text}" }
                                    }
                                    if !atts.is_empty() {
                                        div { class: "mt-2 flex flex-wrap gap-1.5",
                                            for a in atts { AttachmentChip { key: "{a.id}", attachment: a } }
                                        }
                                    }
                                }
                                div { class: "mt-0.5 flex items-center gap-2 px-1 text-[11px] text-muted-foreground",
                                    span { title: "{util::datetime_long(m.summary.date)}", {util::time(m.summary.date)} }
                                    button { class: "hover:text-foreground hover:underline",
                                        onclick: move |_| { let mut f = full.write(); if !f.remove(&toggle_id) { f.insert(toggle_id.clone()); } },
                                        if show_full { "Kurz" } else { "Ganze Nachricht" } }
                                    button { class: "hover:text-foreground hover:underline", onclick: move |_| compose::reply(&r, false), "Antworten" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Antwortfeld unter dem Chat. Geht als normale E-Mail (Signatur, Zitat, Sendeverzögerung).
/// Die KI schlägt auf Wunsch eine Antwort aus dem ganzen Verlauf vor – freigeben muss man selbst.
#[component]
fn QuickReply(target: MessageDetail, ai: bool) -> Element {
    let mut text = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut ai_busy = use_signal(|| false);
    let mut proposal = use_signal(|| None::<String>);
    let t_send = target.clone();
    let t_ai = target.clone();
    let t_full = target.clone();
    let send = move |_| {
        let body = text();
        if body.trim().is_empty() || busy() {
            return;
        }
        let c = compose::quick_reply(&t_send, false, &body);
        spawn(async move {
            busy.set(true);
            let res = api::post::<SendResult, _>("/send", &c).await;
            busy.set(false);
            match res {
                Ok(r) => {
                    compose::after_send(&r, None);
                    text.set(String::new());
                    state::refresh_list();
                }
                Err(e) => toast_error(e),
            }
        });
    };
    let suggest = move |_| {
        let req = DraftReplyRequest { message_id: t_ai.summary.id.clone(), instruction: text().trim().to_string(), tone: String::new() };
        spawn(async move {
            ai_busy.set(true);
            let res = api::post::<AiDraft, _>("/ai/reply", &req).await;
            ai_busy.set(false);
            match res {
                Ok(d) if PREFS.peek().ai_require_approval => proposal.set(Some(d.text.trim_end().to_string())),
                Ok(d) => text.set(d.text.trim_end().to_string()),
                Err(e) => toast_error(e),
            }
        });
    };
    rsx! {
        div { class: "mt-4 rounded-2xl border bg-card p-2",
            if let Some(p) = proposal() {
                div { class: "mb-2 rounded-xl border bg-accent/30 p-3",
                    p { class: "mb-1.5 flex items-center gap-1.5 text-xs font-medium uppercase tracking-wider text-muted-foreground",
                        icons::Sparkles { class: "size-3.5" } "Vorschlag der KI – erst nach Freigabe übernommen" }
                    p { class: "max-h-60 overflow-y-auto whitespace-pre-wrap text-sm leading-relaxed", "{p}" }
                    div { class: "mt-2 flex flex-wrap gap-2",
                        button { class: "{btn(BTN_PRIMARY)} {BTN_SM}", onclick: { let p = p.clone(); move |_| { text.set(p.clone()); proposal.set(None); } },
                            icons::Check { class: "size-4" } "Übernehmen" }
                        button { class: "{btn(BTN_GHOST)} {BTN_SM}", onclick: move |_| proposal.set(None), icons::X { class: "size-4" } "Verwerfen" }
                    }
                }
            }
            textarea { class: "max-h-60 min-h-[3rem] w-full resize-y bg-transparent px-2 py-1.5 text-sm outline-none max-md:text-base placeholder:text-muted-foreground",
                rows: 2,
                placeholder: if ai { "Antworten … (leer lassen und „KI-Vorschlag“ drücken, oder eine Anweisung schreiben)" } else { "Antworten …" },
                "aria-label": "Antwort",
                value: "{text}",
                oninput: move |e| text.set(e.value()),
                onkeydown: move |e| { let m = e.modifiers(); if e.key() == Key::Enter && (m.meta() || m.ctrl()) { e.prevent_default(); e.stop_propagation(); let body = text(); if !body.trim().is_empty() { document::eval("document.getElementById('quick-send')?.click()"); } } },
            }
            div { class: "flex flex-wrap items-center gap-1.5 px-1",
                if ai {
                    button { class: "{btn(BTN_GHOST)} {BTN_SM}", disabled: ai_busy(), onclick: suggest, title: "Antwort aus dem ganzen Verlauf vorschlagen",
                        if ai_busy() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::Sparkles { class: "size-4" } }
                        "KI-Vorschlag"
                    }
                }
                button { class: "{btn(BTN_GHOST)} {BTN_SM} text-muted-foreground", title: "Im Editor öffnen (Cc, Anhänge, Betreff)",
                    onclick: move |_| compose::reply_with(&t_full, false, &text(), None), icons::Maximize2 { class: "size-4" } "Editor" }
                div { class: "flex-1" }
                button { id: "quick-send", class: "{btn(BTN_PRIMARY)} {BTN_SM}", disabled: busy() || text().trim().is_empty(), onclick: send, title: "Senden (⌘/Ctrl+Enter)",
                    if busy() { icons::LoaderCircle { class: "size-4 animate-spin" } } else { icons::Send { class: "size-4" } }
                    "Senden"
                }
            }
        }
    }
}

/// Datei über einen unsichtbaren Link herunterladen.
pub fn download(url: &str, filename: &str) {
    document::eval(&format!(
        "const a = document.createElement('a'); a.href = {url:?}; a.download = {filename:?}; document.body.appendChild(a); a.click(); a.remove();"
    ));
}

fn sanitize_filename(s: &str) -> String {
    let clean: String = s
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let clean = clean.trim().chars().take(80).collect::<String>();
    if clean.is_empty() { "nachricht".into() } else { clean }
}

/// Abmelden: der Server meldet automatisch ab (One-Click, Abmelde-Mail oder Formular der
/// Abmeldeseite); geht das nicht, öffnet sich die Seite nach Rückfrage im neuen Tab.
fn unsubscribe_from(id: String, link: Option<String>) {
    spawn(async move {
        let ask = Ask::new("Vom Newsletter abmelden?", "nrzMail meldet dich automatisch beim Absender ab.").confirm("Abmelden");
        if !confirm(ask).await {
            return;
        }
        match api::post::<nrzmail_api::UnsubscribeResult, _>(&format!("/messages/{}/unsubscribe", api::enc(&id)), &()).await {
            Ok(r) if r.done => {
                toast(r.message);
                state::refresh_list();
            }
            Ok(r) => {
                let url = r.open_url.clone().or(link.filter(|l| !l.starts_with("mailto:")));
                match url {
                    Some(url) => {
                        if confirm(Ask::new("Bitte selbst abmelden", format!("{}\n{url}", r.message)).confirm("Seite öffnen")).await {
                            util::open_external(&url);
                        }
                    }
                    None => toast_error(r.message),
                }
            }
            Err(e) => toast_error(e),
        }
    });
}
