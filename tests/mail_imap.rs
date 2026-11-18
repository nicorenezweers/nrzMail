//! IMAP-Abgleich und Aktionen gegen einen Schein-IMAP-Server: Ordner und Rollen, neue
//! Nachrichten, Flags, auf dem Server gelöschte Nachrichten bleiben im Archiv, UIDVALIDITY,
//! erster Abgleich mit Limit, Aktionen auf dem Server, Aufbewahrung und Wiederherstellen.

mod mail_common;

use mail_common::{
    call, client_with, config, fake_imap, mail, pool, post_bytes, user, FakeFolder, Imap,
    ImapState, User,
};
use rocket::http::Status;
use rocket::local::asynchronous::Client;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const PASSWORD: &str = "geheim-imap";

fn server() -> Imap {
    let mut inbox = FakeFolder::new("INBOX", "\\HasNoChildren");
    inbox.validity = 7;
    inbox.add(
        &mail(
            "Anna <anna@example.ch>",
            "Hallo 1",
            "i1@x",
            "Erste Nachricht",
        ),
        &[],
    );
    inbox.add(
        &mail(
            "Bruno <bruno@example.com>",
            "Hallo 2",
            "i2@x",
            "Zweite Nachricht",
        ),
        &["\\Seen"],
    );
    inbox.add(
        &mail(
            "Carla <carla@example.com>",
            "Hallo 3",
            "i3@x",
            "Dritte Nachricht",
        ),
        &[],
    );
    let mut sent = FakeFolder::new("Sent", "\\HasNoChildren \\Sent");
    sent.add(
        &mail(
            "Nico <nico@example.ch>",
            "Gesendete Nachricht",
            "s1@x",
            "Von mir",
        ),
        &["\\Seen"],
    );
    let mut all = FakeFolder::new("[Gmail]/Alle Nachrichten", "\\HasNoChildren \\All");
    all.add(
        &mail(
            "Anna <anna@example.ch>",
            "Hallo 1",
            "i1@x",
            "Erste Nachricht",
        ),
        &[],
    );
    Arc::new(Mutex::new(ImapState {
        password: PASSWORD.into(),
        folders: vec![
            inbox,
            sent,
            FakeFolder::new("Entw&APw-rfe", "\\HasNoChildren"),
            FakeFolder::new("Archiv", "\\HasNoChildren"),
            FakeFolder::new("Papierkorb", "\\HasNoChildren \\Trash"),
            FakeFolder::new("[Gmail]", "\\HasChildren \\Noselect"),
            all,
        ],
        log: Vec::new(),
    }))
}

fn account(port: u16, password: &str) -> Value {
    json!({
        "kind": "imap", "name": "Privat", "email": "nico@example.ch", "displayName": "Nico",
        "imapHost": "127.0.0.1", "imapPort": port, "imapSecurity": "none",
        "imapUsername": "nico@example.ch", "imapPassword": password,
        "syncEnabled": true, "syncIntervalMinutes": 5,
    })
}

struct Env {
    _dir: tempfile::TempDir,
    c: Client,
    u: User,
    imap: Imap,
    id: String,
}

async fn setup(limit: u32) -> Env {
    let (dir, c) = client_with(|f| f.merge(("initial_sync_limit", limit))).await;
    let u = user(&c, "nico").await;
    let imap = server();
    let port = fake_imap(imap.clone()).await;
    let (s, v) = call(&c, &u, "POST", "/accounts", Some(account(port, PASSWORD))).await;
    assert_eq!(s, Status::Created, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    Env {
        _dir: dir,
        c,
        u,
        imap,
        id,
    }
}

impl Env {
    async fn sync(&self) -> nrzmail::mail::sync::Report {
        nrzmail::mail::sync::run(pool(&self.c), config(&self.c), &self.id)
            .await
            .unwrap_or_else(|e| panic!("Abgleich: {e}"))
    }

    async fn list(&self, query: &str) -> Value {
        let (s, v) = call(&self.c, &self.u, "GET", format!("/messages?{query}"), None).await;
        assert_eq!(s, Status::Ok, "{v}");
        v
    }

    /// ID über den Betreff (auch in Papierkorb und Spam).
    async fn id_of(&self, subject: &str) -> String {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM messages WHERE subject = ?")
            .bind(subject)
            .fetch_all(pool(&self.c))
            .await
            .unwrap();
        assert_eq!(ids.len(), 1, "{subject}: {ids:?}");
        ids[0].clone()
    }

    async fn act(&self, subject: &str, action: Value) -> Value {
        let id = self.id_of(subject).await;
        let (s, v) = call(
            &self.c,
            &self.u,
            "POST",
            "/messages/actions",
            Some(json!({ "ids": [id], "action": action })),
        )
        .await;
        assert_eq!(s, Status::Ok, "{v}");
        v
    }

    fn server_subjects(&self, folder: &str) -> Vec<String> {
        let mut s = self.imap.lock().unwrap();
        let mut out: Vec<String> = s
            .folder(folder)
            .messages
            .iter()
            .map(|m| {
                let text = String::from_utf8_lossy(&m.raw).to_string();
                text.lines()
                    .find_map(|l| l.strip_prefix("Subject: "))
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        out.sort();
        out
    }

    fn server_flags(&self, folder: &str, subject: &str) -> Vec<String> {
        let mut s = self.imap.lock().unwrap();
        s.folder(folder)
            .messages
            .iter()
            .find(|m| String::from_utf8_lossy(&m.raw).contains(&format!("Subject: {subject}\r\n")))
            .map(|m| m.flags.clone())
            .unwrap_or_default()
    }
}

fn subjects(list: &Value) -> Vec<String> {
    let mut s: Vec<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap().to_string())
        .collect();
    s.sort();
    s
}

#[tokio::test]
async fn connection_test_and_sync() {
    let e = setup(0).await;
    let port = {
        let (_, v) = call(&e.c, &e.u, "GET", format!("/accounts/{}", e.id), None).await;
        v["imapPort"].as_u64().unwrap() as u16
    };

    // Verbindung testen: falsches Passwort, dann das gespeicherte
    let (s, v) = call(
        &e.c,
        &e.u,
        "POST",
        "/accounts/test",
        Some(account(port, "falsch")),
    )
    .await;
    assert_eq!(s, Status::Ok);
    assert_eq!(v["imapOk"], false);
    assert!(
        v["imapMessage"]
            .as_str()
            .unwrap()
            .contains("Anmeldung abgelehnt"),
        "{v}"
    );
    let (_, v) = call(
        &e.c,
        &e.u,
        "POST",
        format!("/accounts/test?account={}", e.id),
        Some(account(port, "")),
    )
    .await;
    assert_eq!(v["imapOk"], true, "{v}");
    let names: Vec<&str> = v["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Entwürfe") && names.contains(&"INBOX") && !names.contains(&"[Gmail]"),
        "{names:?}"
    );
    assert_eq!(v["smtpOk"], false);
    assert!(v["smtpMessage"]
        .as_str()
        .unwrap()
        .contains("Kein SMTP-Server"));

    // Erster Abgleich: INBOX und Gesendet, nicht der Sammelordner
    let r = e.sync().await;
    assert_eq!(r.new, 4, "{r:?}");
    let (_, folders) = call(
        &e.c,
        &e.u,
        "GET",
        format!("/folders?account={}", e.id),
        None,
    )
    .await;
    let roles = mail_common::roles(&folders);
    assert_eq!(roles["INBOX"], "inbox");
    assert_eq!(roles["Sent"], "sent");
    assert_eq!(roles["Entwürfe"], "drafts");
    assert_eq!(roles["Archiv"], "archive");
    assert_eq!(roles["Papierkorb"], "trash");
    assert_eq!(roles["Alle Nachrichten"], "other");
    let all_mail = folders
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Alle Nachrichten")
        .unwrap();
    assert_eq!(all_mail["syncEnabled"], false);
    assert_eq!(all_mail["parentPath"], "[Gmail]");
    assert!(!roles.contains_key("[Gmail]"), "nicht auswählbar");

    let inbox = e.list("view=inbox").await;
    assert_eq!(subjects(&inbox), ["Hallo 1", "Hallo 2", "Hallo 3"]);
    for m in inbox["items"].as_array().unwrap() {
        assert_eq!(m["onServer"], true);
        assert_eq!(m["seen"], m["subject"] == "Hallo 2", "{m}");
    }
    assert_eq!(
        subjects(&e.list("view=sent").await),
        ["Gesendete Nachricht"]
    );
    let (_, a) = call(&e.c, &e.u, "GET", format!("/accounts/{}", e.id), None).await;
    assert!(a["lastSyncAt"].as_i64().is_some());
    assert!(a["lastError"].is_null(), "{a}");
    assert_eq!(a["unreadCount"], 2);
    {
        let s = e.imap.lock().unwrap();
        assert!(
            s.log
                .iter()
                .any(|c| c == "UID FETCH 1:* (UID FLAGS RFC822.SIZE)"),
            "{:?}",
            s.log
        );
        assert!(s.log.iter().any(|c| c.contains("BODY.PEEK[]")));
        assert!(
            !s.log.iter().any(|c| c.contains("STORE")),
            "der Abgleich ändert nichts"
        );
        assert!(!s
            .log
            .iter()
            .any(|c| c.contains("Alle Nachrichten") && c.starts_with("SELECT")));
    }

    // Auf dem Server: markiert, gelöscht, neu
    {
        let mut s = e.imap.lock().unwrap();
        let inbox = s.folder("INBOX");
        inbox.messages[0].flags.push("\\Flagged".into());
        inbox.messages.retain(|m| m.uid != 3);
        inbox.add(
            &mail("Dora <dora@example.ch>", "Hallo 4", "i4@x", "Vierte"),
            &[],
        );
    }
    let r = e.sync().await;
    assert_eq!((r.new, r.updated, r.gone), (1, 1, 1), "{r:?}");
    let inbox = e.list("view=inbox").await;
    assert_eq!(subjects(&inbox), ["Hallo 1", "Hallo 2", "Hallo 4"]);
    assert_eq!(e.list("view=starred").await["total"], 1);
    // Gelöscht auf dem Server: bleibt im Archiv
    let all = e.list("view=all").await;
    let gone = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"] == "Hallo 3")
        .unwrap();
    assert_eq!(gone["onServer"], false);
    let uid: Option<i64> = sqlx::query_scalar("SELECT uid FROM messages WHERE id = ?")
        .bind(gone["id"].as_str().unwrap())
        .fetch_one(pool(&e.c))
        .await
        .unwrap();
    assert_eq!(uid, None);
    let (_, d) = call(
        &e.c,
        &e.u,
        "GET",
        format!("/messages/{}", gone["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(d["textBody"].as_str().unwrap().trim(), "Dritte Nachricht");

    // Nochmals: nichts Neues, nichts doppelt
    let r = e.sync().await;
    assert_eq!((r.new, r.updated, r.gone), (0, 0, 0));
    assert_eq!(e.list("view=all").await["total"], 5);

    // Verschwundener Ordner: Nachrichten bleiben lokal
    e.imap.lock().unwrap().folders.retain(|f| f.name != "Sent");
    e.sync().await;
    let sent = e.list("view=all&q=gesendete").await;
    assert_eq!(sent["items"][0]["onServer"], false, "{sent}");
    e.c.terminate().await;
}

#[tokio::test]
async fn actions_reach_the_server() {
    let e = setup(0).await;
    e.sync().await;

    e.act("Hallo 1", json!({ "type": "markRead" })).await;
    assert!(e
        .server_flags("INBOX", "Hallo 1")
        .contains(&"\\Seen".to_string()));
    e.act("Hallo 3", json!({ "type": "flag" })).await;
    assert!(e
        .server_flags("INBOX", "Hallo 3")
        .contains(&"\\Flagged".to_string()));
    e.act("Hallo 3", json!({ "type": "unflag" })).await;
    assert!(!e
        .server_flags("INBOX", "Hallo 3")
        .contains(&"\\Flagged".to_string()));

    // Archivieren: UID MOVE; der nächste Abgleich findet die Nachricht im Archiv wieder
    let v = e.act("Hallo 1", json!({ "type": "archive" })).await;
    assert!(v["message"].is_null(), "{v}");
    assert_eq!(e.server_subjects("INBOX"), ["Hallo 2", "Hallo 3"]);
    assert_eq!(e.server_subjects("Archiv"), ["Hallo 1"]);
    assert_eq!(subjects(&e.list("view=archive").await), ["Hallo 1"]);
    let r = e.sync().await;
    assert_eq!(r.new, 0, "nicht doppelt: {r:?}");
    let id = e.id_of("Hallo 1").await;
    let (uid, on_server): (Option<i64>, bool) =
        sqlx::query_as("SELECT uid, on_server FROM messages WHERE id = ?")
            .bind(&id)
            .fetch_one(pool(&e.c))
            .await
            .unwrap();
    assert!(uid.is_some() && on_server, "wieder zugeordnet");
    assert!(e
        .imap
        .lock()
        .unwrap()
        .log
        .iter()
        .any(|c| c.contains("HEADER.FIELDS (MESSAGE-ID)")));

    // Papierkorb, Spam (Ordner wird angelegt), endgültig löschen
    e.act("Hallo 2", json!({ "type": "trash" })).await;
    assert_eq!(e.server_subjects("Papierkorb"), ["Hallo 2"]);
    e.act("Hallo 3", json!({ "type": "spam" })).await;
    assert_eq!(e.server_subjects("Spam"), ["Hallo 3"]);
    assert!(e
        .imap
        .lock()
        .unwrap()
        .log
        .iter()
        .any(|c| c == "CREATE \"Spam\""));
    let (_, folders) = call(
        &e.c,
        &e.u,
        "GET",
        format!("/folders?account={}", e.id),
        None,
    )
    .await;
    assert_eq!(mail_common::roles(&folders)["Spam"], "junk");
    e.sync().await;
    e.act("Hallo 3", json!({ "type": "notSpam" })).await;
    assert_eq!(e.server_subjects("INBOX"), ["Hallo 3"]);
    e.sync().await;
    e.act("Hallo 3", json!({ "type": "delete" })).await;
    assert!(e.server_subjects("INBOX").is_empty());
    assert_eq!(
        e.list("view=all&q=hallo").await["total"],
        1,
        "Hallo 1 (Hallo 2 liegt im Papierkorb)"
    );

    // Papierkorb-Frist: endgültig gelöscht, auch auf dem Server
    e.sync().await;
    sqlx::query("UPDATE messages SET trashed_at = 1000 WHERE subject = 'Hallo 2'")
        .execute(pool(&e.c))
        .await
        .unwrap();
    let mut cfg = config(&e.c).clone();
    cfg.trash_retention_days = 30;
    let n = nrzmail::mail::retention::run_local(pool(&e.c), &cfg)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert!(e.server_subjects("Papierkorb").is_empty());
    assert_eq!(e.list("view=trash").await["total"], 0);

    // Server nicht erreichbar: nur lokal, mit Hinweis
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    sqlx::query("UPDATE mail_accounts SET imap_port = ?")
        .bind(closed as i64)
        .execute(pool(&e.c))
        .await
        .unwrap();
    let v = e.act("Hallo 1", json!({ "type": "markUnread" })).await;
    assert_eq!(v["updated"], 1);
    assert!(v["message"].as_str().unwrap().contains("nur lokal"), "{v}");
    assert_eq!(e.list("view=unread").await["total"], 1);
    // Abgleich schlägt fehl und hält den Fehler fest
    assert!(nrzmail::mail::sync::run(pool(&e.c), config(&e.c), &e.id)
        .await
        .is_err());
    let (_, a) = call(&e.c, &e.u, "GET", format!("/accounts/{}", e.id), None).await;
    assert!(
        a["lastError"]
            .as_str()
            .unwrap()
            .contains("lehnt die Verbindung ab"),
        "{a}"
    );
    e.c.terminate().await;
}

#[tokio::test]
async fn uidvalidity_change_relinks_without_duplicates() {
    let e = setup(0).await;
    e.sync().await;
    {
        let mut s = e.imap.lock().unwrap();
        let inbox = s.folder("INBOX");
        inbox.validity = 8;
        for (i, m) in inbox.messages.iter_mut().enumerate() {
            m.uid = 100 + i as u32;
        }
        inbox.next_uid = 103;
    }
    let r = e.sync().await;
    assert_eq!(r.new, 0, "{r:?}");
    assert_eq!(e.list("view=inbox").await["total"], 3);
    let uids: Vec<i64> = sqlx::query_scalar(
        "SELECT m.uid FROM messages m JOIN folders f ON f.id = m.folder_id WHERE f.path = 'INBOX' ORDER BY m.uid",
    )
    .fetch_all(pool(&e.c))
    .await
    .unwrap();
    assert_eq!(uids, [100, 101, 102]);
    let v: i64 = sqlx::query_scalar("SELECT uid_validity FROM folders WHERE path = 'INBOX'")
        .fetch_one(pool(&e.c))
        .await
        .unwrap();
    assert_eq!(v, 8);
    e.c.terminate().await;
}

#[tokio::test]
async fn initial_sync_limit_takes_newest() {
    let e = setup(2).await;
    e.sync().await;
    assert_eq!(
        subjects(&e.list("view=inbox").await),
        ["Hallo 2", "Hallo 3"]
    );
    e.imap
        .lock()
        .unwrap()
        .folder("INBOX")
        .add(&mail("x@y.ch", "Hallo 4", "i4@x", "neu"), &[]);
    let r = e.sync().await;
    assert_eq!(r.new, 1);
    assert_eq!(
        subjects(&e.list("view=inbox").await),
        ["Hallo 2", "Hallo 3", "Hallo 4"],
        "ältere bleiben weg"
    );
    e.c.terminate().await;
}

#[tokio::test]
async fn retention_and_restore() {
    let e = setup(0).await;
    // Nach 30 Tagen vom Server löschen (die Testnachrichten sind vom Oktober 2025)
    let (_, a) = call(&e.c, &e.u, "GET", format!("/accounts/{}", e.id), None).await;
    let mut input = account(a["imapPort"].as_u64().unwrap() as u16, "");
    input["deleteFromServerAfterDays"] = json!(30);
    let (s, v) = call(
        &e.c,
        &e.u,
        "PUT",
        format!("/accounts/{}", e.id),
        Some(input.clone()),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    e.sync().await;
    assert!(
        e.server_subjects("INBOX").is_empty(),
        "auf dem Server gelöscht"
    );
    assert!(e.server_subjects("Sent").is_empty());
    let all = e.list("view=all").await;
    assert_eq!(all["total"], 4);
    assert!(all["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["onServer"] == false));
    assert_eq!(e.list("view=inbox").await["total"], 0, "nur noch im Archiv");
    let (_, arch) = call(&e.c, &e.u, "GET", "/archive", None).await;
    assert_eq!(arch["accounts"][0]["localOnlyCount"], 4);

    // Ohne Frist wiederherstellen: APPEND in den Posteingang, der Abgleich ordnet zu
    input["deleteFromServerAfterDays"] = Value::Null;
    call(
        &e.c,
        &e.u,
        "PUT",
        format!("/accounts/{}", e.id),
        Some(input),
    )
    .await;
    let (_, folders) = call(
        &e.c,
        &e.u,
        "GET",
        format!("/folders?account={}", e.id),
        None,
    )
    .await;
    let inbox_id = folders
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["role"] == "inbox")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Dazu eine importierte Nachricht (nur lokal)
    let (s, v) = post_bytes(
        &e.c,
        &e.u,
        &format!("/import?account={}&folder=Altes%20Archiv", e.id),
        mail("alt@example.ch", "Alte Nachricht", "alt1@x", "von früher"),
    )
    .await;
    assert_eq!((s, v["imported"].as_u64()), (Status::Ok, Some(1)), "{v}");
    let ids = vec![e.id_of("Hallo 1").await, e.id_of("Alte Nachricht").await];
    let (s, v) = call(
        &e.c,
        &e.u,
        "POST",
        "/archive/restore",
        Some(json!({ "ids": ids, "folderId": inbox_id })),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    assert_eq!(v["updated"], 2, "{v}");
    assert_eq!(e.server_subjects("INBOX"), ["Alte Nachricht", "Hallo 1"]);
    assert_eq!(
        subjects(&e.list("view=inbox").await),
        ["Alte Nachricht", "Hallo 1"]
    );
    let r = e.sync().await;
    assert_eq!(r.new, 0, "{r:?}");
    let linked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM messages WHERE uid IS NOT NULL AND on_server = 1")
            .fetch_one(pool(&e.c))
            .await
            .unwrap();
    assert_eq!(linked, 2);
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM activity_log WHERE action = 'archive_restore'")
            .fetch_one(pool(&e.c))
            .await
            .unwrap();
    assert_eq!(n, 1);
    e.c.terminate().await;
}
