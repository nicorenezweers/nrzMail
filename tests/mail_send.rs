//! Senden gegen einen Schein-SMTP-Server (und Schein-IMAP für die Gesendet-Kopie):
//! Antwort mit Cc/Bcc, HTML und Anhang, als beantwortet markieren, geplantes Senden,
//! Entwürfe, Fehler und Grössenlimit.

mod mail_common;

use base64::Engine;
use mail_common::{
    call, client, config, fake_imap, fake_smtp, mail, pool, user, FakeFolder, ImapState, Smtp,
    SmtpState,
};
use rocket::http::Status;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn send_reply_schedule_drafts_and_errors() {
    let (_d, c) = client().await;
    let u = user(&c, "nico").await;
    // Ohne Sendeverzögerung (Vorgabe 1 Minute) – die wird unten eigens geprüft
    let (s, _) = call(
        &c,
        &u,
        "PUT",
        "/me/preferences",
        Some(json!({ "undoSendSeconds": 0 })),
    )
    .await;
    assert_eq!(s, Status::Ok);

    let mut inbox = FakeFolder::new("INBOX", "\\HasNoChildren");
    inbox.add(
        &mail(
            "Anna Müller <anna@example.ch>",
            "Frage",
            "q1@example.ch",
            "Kommst du morgen?",
        ),
        &[],
    );
    let imap = Arc::new(Mutex::new(ImapState {
        password: "imap-geheim".into(),
        folders: vec![inbox, FakeFolder::new("Sent", "\\HasNoChildren \\Sent")],
        log: Vec::new(),
    }));
    let imap_port = fake_imap(imap.clone()).await;
    let smtp: Smtp = Arc::new(Mutex::new(SmtpState {
        password: "smtp-geheim".into(),
        mails: Vec::new(),
    }));
    let smtp_port = fake_smtp(smtp.clone()).await;

    let input = json!({
        "kind": "imap", "name": "Privat", "email": "nico@example.ch", "displayName": "Nico Renezweers",
        "imapHost": "127.0.0.1", "imapPort": imap_port, "imapSecurity": "none", "imapPassword": "imap-geheim",
        "smtpHost": "127.0.0.1", "smtpPort": smtp_port, "smtpSecurity": "none",
        "smtpUsername": "nico@example.ch", "smtpPassword": "smtp-geheim",
        "syncEnabled": true, "syncIntervalMinutes": 5,
    });
    let (s, v) = call(&c, &u, "POST", "/accounts", Some(input.clone())).await;
    assert_eq!(s, Status::Created, "{v}");
    let acc = v["id"].as_str().unwrap().to_string();
    let (_, t) = call(
        &c,
        &u,
        "POST",
        format!("/accounts/test?account={acc}"),
        Some(input.clone()),
    )
    .await;
    assert_eq!(
        (t["imapOk"].as_bool(), t["smtpOk"].as_bool()),
        (Some(true), Some(true)),
        "{t}"
    );
    nrzmail::mail::sync::run(pool(&c), config(&c), &acc)
        .await
        .unwrap();
    let (_, list) = call(&c, &u, "GET", "/messages?view=inbox", None).await;
    let question = list["items"][0]["id"].as_str().unwrap().to_string();

    // Antwort mit Cc, Bcc, HTML und Anhang
    let compose = json!({
        "accountId": acc,
        "to": [{ "name": "Anna Müller", "address": "anna@example.ch" }],
        "cc": [{ "name": "", "address": "bruno@example.com" }],
        "bcc": [{ "name": "", "address": "geheim@example.com" }],
        "subject": "Re: Frage",
        "text": "Ja, gerne – bis morgen!",
        "html": "<p>Ja, gerne – <b>bis morgen</b>!</p>",
        "inReplyTo": question,
        "attachments": [{ "filename": "Plan.txt", "contentType": "text/plain",
                          "dataBase64": base64::engine::general_purpose::STANDARD.encode("Treffpunkt: Bahnhof") }],
    });
    let (s, r) = call(&c, &u, "POST", "/send", Some(compose.clone())).await;
    assert_eq!(s, Status::Ok, "{r}");
    assert_eq!(r["status"], "sent");
    let local = r["messageId"].as_str().unwrap().to_string();

    let sent = smtp.lock().unwrap().mails.clone();
    assert_eq!(sent.len(), 1);
    let m = &sent[0];
    assert!(m.from.contains("nico@example.ch"));
    let mut rcpt = m.rcpt.join(" ");
    rcpt.make_ascii_lowercase();
    for a in ["anna@example.ch", "bruno@example.com", "geheim@example.com"] {
        assert!(rcpt.contains(a), "{rcpt}");
    }
    assert!(
        m.data.contains("In-Reply-To: <q1@example.ch>"),
        "{}",
        m.data
    );
    assert!(m.data.contains("References: <q1@example.ch>"));
    assert!(m.data.contains("Subject: Re: Frage"));
    assert!(m.data.contains("multipart/alternative") && m.data.contains("Plan.txt"));
    assert!(
        !m.data.contains("geheim@example.com"),
        "kein Bcc im Kopf: {}",
        m.data
    );
    assert!(
        m.data
            .contains("From: \"Nico Renezweers\" <nico@example.ch>"),
        "{}",
        m.data
    );

    // Kopie im Gesendet-Ordner (mit Bcc), Original beantwortet
    {
        let mut s = imap.lock().unwrap();
        let copy = s.folder("Sent").messages.first().cloned().expect("APPEND");
        assert!(copy.flags.contains(&"\\Seen".to_string()));
        assert!(String::from_utf8_lossy(&copy.raw).contains("Bcc: geheim@example.com"));
        assert!(s.folder("INBOX").messages[0]
            .flags
            .contains(&"\\Answered".to_string()));
    }
    let (_, d) = call(&c, &u, "GET", format!("/messages/{local}"), None).await;
    assert_eq!(d["isSent"], true);
    assert_eq!(d["folderName"], "Sent");
    assert_eq!(d["attachments"][0]["filename"], "Plan.txt");
    assert_eq!(d["bcc"][0]["address"], "geheim@example.com");
    assert_eq!(
        d["threadId"], list["items"][0]["threadId"],
        "gleiche Unterhaltung"
    );
    let (_, q) = call(&c, &u, "GET", format!("/messages/{question}"), None).await;
    assert_eq!(q["answered"], true);
    // Der nächste Abgleich ordnet die Kopie zu statt sie doppelt zu speichern
    let r = nrzmail::mail::sync::run(pool(&c), config(&c), &acc)
        .await
        .unwrap();
    assert_eq!(r.new, 0, "{r:?}");
    let (_, sent_view) = call(&c, &u, "GET", "/messages?view=sent", None).await;
    assert_eq!(sent_view["total"], 1);
    assert_eq!(sent_view["items"][0]["threadCount"], 2);

    // Geplant: liegt im Postausgang, bis es fällig ist
    let mut later = compose.clone();
    later["inReplyTo"] = Value::Null;
    later["subject"] = json!("Später");
    later["sendAt"] = json!(time::OffsetDateTime::now_utc().unix_timestamp() + 3600);
    let (_, r) = call(&c, &u, "POST", "/send", Some(later)).await;
    assert_eq!(r["status"], "scheduled", "{r}");
    let (_, out) = call(&c, &u, "GET", "/outbox", None).await;
    assert_eq!(out.as_array().unwrap().len(), 1);
    assert_eq!(out[0]["status"], "scheduled");
    assert_eq!(out[0]["compose"]["subject"], "Später");
    assert_eq!(
        nrzmail::mail::send::send_due(pool(&c), config(&c))
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE outbox SET send_at = 1")
        .execute(pool(&c))
        .await
        .unwrap();
    assert_eq!(
        nrzmail::mail::send::send_due(pool(&c), config(&c))
            .await
            .unwrap(),
        1
    );
    assert_eq!(smtp.lock().unwrap().mails.len(), 2);
    let (_, out) = call(&c, &u, "GET", "/outbox", None).await;
    assert_eq!(out, json!([]));

    // Entwurf anlegen, ändern, damit senden
    let draft = json!({ "accountId": acc, "to": [{ "address": "anna@example.ch" }], "subject": "Entwurf", "text": "erst" });
    let (s, d) = call(&c, &u, "POST", "/drafts", Some(draft.clone())).await;
    assert_eq!(s, Status::Created, "{d}");
    assert_eq!(d["status"], "draft");
    let draft_id = d["id"].as_str().unwrap().to_string();
    let mut changed = draft.clone();
    changed["text"] = json!("fertig");
    let (s, d) = call(
        &c,
        &u,
        "PUT",
        format!("/drafts/{draft_id}"),
        Some(changed.clone()),
    )
    .await;
    assert_eq!(s, Status::Ok);
    assert_eq!(d["compose"]["text"], "fertig");
    let (_, out) = call(&c, &u, "GET", "/outbox?status=draft", None).await;
    assert_eq!(out.as_array().unwrap().len(), 1);
    let other = user(&c, "fremd").await;
    assert_eq!(
        call(&c, &other, "DELETE", format!("/outbox/{draft_id}"), None)
            .await
            .0,
        Status::NotFound
    );
    assert_eq!(
        call(
            &c,
            &other,
            "PUT",
            format!("/drafts/{draft_id}"),
            Some(changed.clone())
        )
        .await
        .0,
        Status::NotFound
    );
    assert_eq!(
        call(&c, &other, "POST", "/send", Some(changed.clone()))
            .await
            .0,
        Status::NotFound,
        "fremdes Postfach"
    );
    changed["draftId"] = json!(draft_id);
    let (s, r) = call(&c, &u, "POST", "/send", Some(changed)).await;
    assert_eq!((s, r["status"].as_str()), (Status::Ok, Some("sent")), "{r}");
    let (_, out) = call(&c, &u, "GET", "/outbox", None).await;
    assert_eq!(out, json!([]), "Entwurf erledigt");

    // Weiterleiten mit dem Anhang der gesendeten Nachricht
    let (_, copy) = call(&c, &u, "GET", format!("/messages/{local}"), None).await;
    let plan = copy["attachments"][0]["id"].as_str().unwrap().to_string();
    let fwd = json!({ "accountId": acc, "to": [{ "address": "carla@example.com" }], "subject": "WG: Re: Frage",
                      "text": "Zur Info", "forwardOf": local, "forwardAttachments": [plan] });
    let (s, r) = call(&c, &u, "POST", "/send", Some(fwd)).await;
    assert_eq!((s, r["status"].as_str()), (Status::Ok, Some("sent")), "{r}");
    let data = smtp.lock().unwrap().mails.last().unwrap().data.clone();
    assert!(
        data.contains("Plan.txt") && data.contains("Treffpunkt: Bahnhof"),
        "{data}"
    );
    let (s, _) = call(
        &c,
        &u,
        "POST",
        "/send",
        Some(json!({ "accountId": acc, "to": [{ "address": "x@y.ch" }],
        "forwardAttachments": ["gibtsnicht"] })),
    )
    .await;
    assert_eq!(s, Status::NotFound);

    // Ungültige Eingaben
    let mut bad = draft.clone();
    bad["to"] = json!([]);
    let (s, v) = call(&c, &u, "POST", "/send", Some(bad)).await;
    assert_eq!(s, Status::BadRequest);
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Empfänger"));
    let mut bad = draft.clone();
    bad["cc"] = json!([{ "address": "kaputt" }]);
    let (s, v) = call(&c, &u, "POST", "/send", Some(bad)).await;
    assert_eq!(s, Status::BadRequest);
    assert!(v["error"]["message"].as_str().unwrap().contains("kaputt"));
    let mut big = draft.clone();
    big["attachments"] = json!([{ "filename": "gross.bin", "contentType": "application/octet-stream",
                                   "dataBase64": base64::engine::general_purpose::STANDARD.encode(vec![0u8; 26 * 1024 * 1024]) }]);
    let (s, v) = call(&c, &u, "POST", "/send", Some(big)).await;
    assert_eq!(s, Status::BadRequest);
    assert!(
        v["error"]["message"].as_str().unwrap().contains("zu gross"),
        "{v}"
    );

    // Sendeverzögerung: wartet intern, lässt sich zurückziehen, sonst geht sie später raus
    let (_, p) = call(
        &c,
        &u,
        "PUT",
        "/me/preferences",
        Some(json!({ "undoSendSeconds": 5000 })),
    )
    .await;
    assert_eq!(p["undoSendSeconds"], 1800, "höchstens 30 Minuten");
    call(
        &c,
        &u,
        "PUT",
        "/me/preferences",
        Some(json!({ "undoSendSeconds": 60 })),
    )
    .await;
    let mut held = draft.clone();
    held["subject"] = json!("Verzögert");
    let before = time::OffsetDateTime::now_utc().unix_timestamp();
    let (s, r) = call(&c, &u, "POST", "/send", Some(held.clone())).await;
    assert_eq!(
        (s, r["status"].as_str()),
        (Status::Ok, Some("scheduled")),
        "{r}"
    );
    let until = r["undoUntil"].as_i64().unwrap();
    assert!((before + 59..=before + 62).contains(&until), "{until}");
    let held_id = r["outboxId"].as_str().unwrap().to_string();
    assert_eq!(
        nrzmail::mail::send::send_due(pool(&c), config(&c))
            .await
            .unwrap(),
        0,
        "noch nicht fällig"
    );
    assert_eq!(
        call(&c, &other, "POST", format!("/outbox/{held_id}/undo"), None)
            .await
            .0,
        Status::Conflict,
        "fremde Nachricht"
    );
    let (s, d) = call(&c, &u, "POST", format!("/outbox/{held_id}/undo"), None).await;
    assert_eq!(
        (s, d["status"].as_str()),
        (Status::Ok, Some("draft")),
        "{d}"
    );
    assert_eq!(d["compose"]["subject"], "Verzögert");
    sqlx::query("UPDATE outbox SET send_at = 1")
        .execute(pool(&c))
        .await
        .unwrap();
    assert_eq!(
        nrzmail::mail::send::send_due(pool(&c), config(&c))
            .await
            .unwrap(),
        0,
        "zurückgezogen = nicht gesendet"
    );
    assert_eq!(
        call(&c, &u, "DELETE", format!("/outbox/{held_id}"), None)
            .await
            .0,
        Status::NoContent
    );
    let (_, r) = call(&c, &u, "POST", "/send", Some(held)).await;
    let held_id = r["outboxId"].as_str().unwrap().to_string();
    sqlx::query("UPDATE outbox SET send_at = 1")
        .execute(pool(&c))
        .await
        .unwrap();
    assert_eq!(
        nrzmail::mail::send::send_due(pool(&c), config(&c))
            .await
            .unwrap(),
        1
    );
    assert_eq!(smtp.lock().unwrap().mails.len(), 5);
    assert_eq!(
        call(&c, &u, "POST", format!("/outbox/{held_id}/undo"), None)
            .await
            .0,
        Status::Conflict,
        "schon gesendet"
    );
    call(
        &c,
        &u,
        "PUT",
        "/me/preferences",
        Some(json!({ "undoSendSeconds": 0 })),
    )
    .await;

    // SMTP lehnt die Anmeldung ab: Fehler mit Grund, Nachricht im Postausgang
    smtp.lock().unwrap().password = "anderes".into();
    let (s, v) = call(&c, &u, "POST", "/send", Some(draft.clone())).await;
    assert_eq!(s, Status::BadRequest);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("SMTP-Anmeldung abgelehnt") && msg.contains("Postausgang"),
        "{msg}"
    );
    let (_, out) = call(&c, &u, "GET", "/outbox?status=failed", None).await;
    assert_eq!(out.as_array().unwrap().len(), 1);
    assert!(out[0]["error"].as_str().unwrap().contains("abgelehnt"));
    let failed_id = out[0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        call(&c, &u, "DELETE", format!("/outbox/{failed_id}"), None)
            .await
            .0,
        Status::NoContent
    );
    assert_eq!(
        smtp.lock().unwrap().mails.len(),
        5,
        "nichts zusätzlich gesendet"
    );
    c.terminate().await;
}
