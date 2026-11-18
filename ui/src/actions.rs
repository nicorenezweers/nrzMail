//! Aktionen auf Nachrichten (gelesen, Stern, archivieren, löschen, verschieben …) – gemeinsam
//! für Liste, Lesebereich, Tastatur und Befehlspalette. Wirkt über `POST /messages/actions`
//! lokal und per IMAP auf dem Server.

use crate::api;
use crate::components::overlay::{toast, toast_error};
use crate::state;
use dioxus::prelude::*;
use nrzmail_api::{BulkAction, BulkResult, MailView, MessageAction, MessageSummary, Thread};

/// Kurze Bestätigung nach einer Aktion.
pub fn done_text(action: &MessageAction, n: usize) -> Option<String> {
    let what = if n == 1 { "Nachricht".to_string() } else { format!("{n} Nachrichten") };
    Some(match action {
        MessageAction::MarkRead | MessageAction::MarkUnread | MessageAction::Flag | MessageAction::Unflag => {
            return None;
        }
        MessageAction::Archive => format!("{what} archiviert."),
        MessageAction::Trash => format!("{what} in den Papierkorb gelegt."),
        MessageAction::Delete => format!("{what} endgültig gelöscht."),
        MessageAction::Restore => format!("{what} in den Posteingang zurückgelegt."),
        MessageAction::Spam => format!("{what} als Spam markiert."),
        MessageAction::NotSpam => format!("{what} als „kein Spam“ markiert."),
        MessageAction::Move { .. } => format!("{what} verschoben."),
        MessageAction::SetCategory { category } => format!("{what} nach „{}“ eingeteilt.", category.label()),
    })
}

/// Aktion ausführen; meldet Erfolg/Fehler selbst und lädt danach die Zähler neu.
pub async fn apply(ids: Vec<String>, action: MessageAction) -> bool {
    apply_with(ids, action, true).await
}

/// Wie `apply`; `confirm = false` meldet nur Fehler (die Seite zeigt selbst „Rückgängig“).
pub async fn apply_with(ids: Vec<String>, action: MessageAction, confirm: bool) -> bool {
    if ids.is_empty() {
        return false;
    }
    let n = ids.len();
    let body = BulkAction { ids, action: action.clone() };
    match api::post::<BulkResult, _>("/messages/actions", &body).await {
        Ok(r) => {
            if let Some(m) = r.message.filter(|m| !m.trim().is_empty()) {
                if r.failed > 0 { toast_error(m) } else { toast(m) }
            } else if r.failed > 0 && r.updated == 0 {
                toast_error("Die Aktion hat nicht geklappt.");
            } else if let Some(t) = done_text(&action, n).filter(|_| confirm) {
                toast(t);
            }
            spawn(async {
                state::load_accounts().await;
            });
            r.updated > 0 || r.failed == 0
        }
        Err(e) => {
            toast_error(e);
            false
        }
    }
}

/// Verschwindet die Nachricht durch die Aktion aus der aktuellen Liste?
pub fn removes(action: &MessageAction, view: Option<MailView>, in_folder: bool) -> bool {
    use MessageAction as A;
    if in_folder {
        return !matches!(action, A::MarkRead | A::MarkUnread | A::Flag | A::Unflag | A::SetCategory { .. });
    }
    let view = view.unwrap_or_default();
    match action {
        A::MarkRead | A::MarkUnread | A::Flag | A::Unflag => false,
        A::Delete => true,
        A::Trash | A::Spam => view != MailView::All,
        A::Archive | A::Move { .. } => matches!(view, MailView::Inbox | MailView::Junk | MailView::Trash | MailView::Drafts),
        A::Restore | A::NotSpam => matches!(view, MailView::Trash | MailView::Junk | MailView::Archive),
        A::SetCategory { .. } => view == MailView::Inbox,
    }
}

/// Welche Nachrichten einer Unterhaltung eine Aktion betrifft: Stern nur die gewählte,
/// gelesen/ungelesen alle empfangenen, Verschieben & Co. alle im selben Ordner wie die gewählte.
pub fn ids_for(rep: &MessageSummary, thread: &[MessageSummary], action: &MessageAction) -> Vec<String> {
    use MessageAction as A;
    let mut ids: Vec<String> = match action {
        A::Flag | A::Unflag => vec![rep.id.clone()],
        A::MarkRead | A::MarkUnread | A::SetCategory { .. } => {
            thread.iter().filter(|m| !m.is_sent).map(|m| m.id.clone()).collect()
        }
        _ => thread
            .iter()
            .filter(|m| rep.folder_id.is_none() || m.folder_id == rep.folder_id)
            .map(|m| m.id.clone())
            .collect(),
    };
    if !ids.contains(&rep.id) {
        ids.push(rep.id.clone());
    }
    ids
}

/// IDs für eine Aktion auf Einträge der Liste: bei gebündelten Unterhaltungen die passenden
/// Nachrichten der ganzen Unterhaltung (lädt sie dazu), sonst die Einträge selbst.
pub async fn expand(items: Vec<MessageSummary>, action: &MessageAction) -> Vec<String> {
    let threads = state::PREFS.peek().threads;
    let mut out: Vec<String> = Vec::new();
    let needs_thread = |m: &MessageSummary| threads && m.thread_count > 1 && !matches!(action, MessageAction::Flag | MessageAction::Unflag);
    let loads = futures_util::future::join_all(items.iter().filter(|m| needs_thread(m)).map(|m| {
        let tid = m.thread_id.clone();
        async move { api::get::<Thread>(&format!("/threads/{}", api::enc(&tid))).await.ok() }
    }))
    .await;
    let mut loaded = loads.into_iter();
    for m in &items {
        let ids = if needs_thread(m) {
            match loaded.next().flatten() {
                Some(t) => {
                    let list: Vec<MessageSummary> = t.messages.into_iter().map(|d| d.summary).collect();
                    ids_for(m, &list, action)
                }
                None => vec![m.id.clone()],
            }
        } else {
            vec![m.id.clone()]
        };
        for id in ids {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}
