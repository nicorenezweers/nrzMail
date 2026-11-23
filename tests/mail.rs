//! Mail-API mit einem Archiv-Postfach (ohne Server): Postfächer, Import einer mbox, Liste,
//! Suche, Unterhaltungen, Detail mit bereinigtem HTML, Anhänge, Export, Statistik,
//! Aktionen – und dass ein anderer Benutzer nichts davon sieht.

mod mail_common;

use mail_common::{call, call_anon, client, get_raw, pool, post_bytes, user, User};
use rocket::http::Status;
use rocket::local::asynchronous::Client;
use serde_json::{json, Value};

const MBOX: &[u8] = include_bytes!("fixtures/mail/archiv.mbox");

async fn import_account(c: &Client, u: &User, name: &str) -> String {
    let (s, v) = call(
        c,
        u,
        "POST",
        "/accounts",
        Some(json!({ "kind": "import", "name": name, "email": "nico@example.ch", "displayName": "Nico Renezweers" })),
    )
    .await;
    assert_eq!(s, Status::Created, "{v}");
    v["id"].as_str().unwrap().to_string()
}

async fn messages(c: &Client, u: &User, query: &str) -> Value {
    let (s, v) = call(c, u, "GET", format!("/messages?{query}"), None).await;
    assert_eq!(s, Status::Ok, "{query}: {v}");
    v
}

async fn act(c: &Client, u: &User, ids: &[&String], action: Value) -> Value {
    let (s, v) = call(
        c,
        u,
        "POST",
        "/messages/actions",
        Some(json!({ "ids": ids, "action": action })),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    v
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

fn find<'a>(list: &'a Value, subject: &str) -> &'a Value {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"] == subject)
        .unwrap_or_else(|| panic!("{subject} fehlt: {list}"))
}

#[tokio::test]
async fn accounts_validate_and_hide_passwords() {
    let (_d, c) = client().await;
    let u = user(&c, "nico").await;
    assert_eq!(
        call_anon(&c, "GET", "/accounts").await,
        Status::Unauthorized
    );

    let base = json!({
        "kind": "imap", "name": "Privat", "email": "nico@example.ch",
        "imapHost": "imap.example.ch", "imapPort": 993, "imapSecurity": "tls",
        "imapPassword": "sehr-geheimes-passwort", "smtpHost": "smtp.example.ch", "smtpPort": 465,
        "smtpSecurity": "tls", "syncEnabled": false, "syncIntervalMinutes": 5,
    });
    let with = |k: &str, v: Value| {
        let mut x = base.clone();
        x[k] = v;
        x
    };
    for bad in [
        with("email", json!("kaputt")),
        with("imapHost", json!("")),
        with("imapHost", json!("https://imap.example.ch")),
        with("imapPassword", json!(null)),
        with("syncIntervalMinutes", json!(0)),
        with("archiveRetentionDays", json!(0)),
    ] {
        let (s, v) = call(&c, &u, "POST", "/accounts", Some(bad.clone())).await;
        assert_eq!(s, Status::BadRequest, "{bad} → {v}");
        assert!(v["error"]["message"].as_str().unwrap().len() > 5);
    }
    let (s, v) = call(&c, &u, "POST", "/accounts", Some(base.clone())).await;
    assert_eq!(s, Status::Created, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        v["imapUsername"], "nico@example.ch",
        "Benutzername = Adresse"
    );
    assert_eq!(v["imapPasswordHint"], "se…rt");
    assert_eq!(v["smtpPasswordHint"], "");
    assert!(!v.to_string().contains("sehr-geheimes"));

    // Ändern ohne Passwort: bleibt; Art lässt sich nicht ändern
    let (s, v) = call(
        &c,
        &u,
        "PUT",
        format!("/accounts/{id}"),
        Some(with("imapPassword", json!(""))),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    let pw: String = sqlx::query_scalar("SELECT imap_password FROM mail_accounts WHERE id = ?")
        .bind(&id)
        .fetch_one(pool(&c))
        .await
        .unwrap();
    assert_eq!(pw, "sehr-geheimes-passwort");
    let (s, _) = call(
        &c,
        &u,
        "PUT",
        format!("/accounts/{id}"),
        Some(with("kind", json!("import"))),
    )
    .await;
    assert_eq!(s, Status::BadRequest);

    // Verbindungstest gegen einen geschlossenen Port: klare Meldung, kein Fehlerstatus
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let test = json!({
        "kind": "imap", "email": "nico@example.ch", "imapHost": "127.0.0.1", "imapPort": closed,
        "imapSecurity": "none", "smtpHost": "127.0.0.1", "smtpPort": closed, "smtpSecurity": "none",
        "syncIntervalMinutes": 5,
    });
    let (s, v) = call(
        &c,
        &u,
        "POST",
        format!("/accounts/test?account={id}"),
        Some(test),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    assert_eq!(v["imapOk"], false);
    assert!(
        v["imapMessage"]
            .as_str()
            .unwrap()
            .contains("lehnt die Verbindung ab"),
        "{v}"
    );
    assert_eq!(v["smtpOk"], false);

    let (_, list) = call(&c, &u, "GET", "/accounts", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        call(&c, &u, "POST", format!("/accounts/{id}/sync"), None)
            .await
            .0,
        Status::Accepted
    );

    // Fremde sehen nichts
    let other = user(&c, "zweite").await;
    for (m, uri) in [
        ("GET", format!("/accounts/{id}")),
        ("PUT", format!("/accounts/{id}")),
        ("DELETE", format!("/accounts/{id}")),
        ("POST", format!("/accounts/{id}/sync")),
    ] {
        let body = (m == "PUT").then(|| base.clone());
        assert_eq!(
            call(&c, &other, m, &uri, body).await.0,
            Status::NotFound,
            "{m} {uri}"
        );
    }
    let (_, list) = call(&c, &other, "GET", "/accounts", None).await;
    assert_eq!(list, json!([]));

    assert_eq!(
        call(&c, &u, "DELETE", format!("/accounts/{id}"), None)
            .await
            .0,
        Status::NoContent
    );
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM activity_log WHERE action LIKE 'account_%'")
            .fetch_one(pool(&c))
            .await
            .unwrap();
    assert_eq!(n, 3, "angelegt, geändert, gelöscht");
    c.terminate().await;
}

#[tokio::test]
async fn import_list_search_detail_export_stats() {
    let (_d, c) = client().await;
    let u = user(&c, "nico").await;
    let acc = import_account(&c, &u, "Archiv 2024").await;

    // Import der mbox: 8 Nachrichten, beim zweiten Mal alles Duplikate
    let (s, v) = post_bytes(
        &c,
        &u,
        &format!("/import?account={acc}&folder=INBOX"),
        MBOX.to_vec(),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    assert_eq!(
        (
            v["imported"].as_u64(),
            v["duplicates"].as_u64(),
            v["failed"].as_u64()
        ),
        (Some(8), Some(0), Some(0)),
        "{v}"
    );
    let (_, v) = post_bytes(
        &c,
        &u,
        &format!("/import?account={acc}&folder=INBOX"),
        MBOX.to_vec(),
    )
    .await;
    assert_eq!(v["duplicates"], 8, "{v}");
    assert_eq!(v["imported"], 0);

    // Ordner: INBOX mit Rolle Posteingang
    let (_, folders) = call(&c, &u, "GET", format!("/folders?account={acc}"), None).await;
    assert_eq!(folders[0]["role"], "inbox", "{folders}");
    assert_eq!(folders[0]["total"], 8);

    // Liste: Posteingang, neueste zuerst; Gesendete (Absender = Postfach) sind gelesen
    let inbox = messages(&c, &u, "view=inbox").await;
    assert_eq!(inbox["total"], 8);
    let items = inbox["items"].as_array().unwrap();
    let dates: Vec<i64> = items.iter().map(|m| m["date"].as_i64().unwrap()).collect();
    assert!(dates.windows(2).all(|w| w[0] >= w[1]), "{dates:?}");
    let gruss = find(&inbox, "Grüsse aus Zürich");
    assert_eq!(
        gruss["from"],
        json!({ "name": "Anna Müller", "address": "anna@example.ch" })
    );
    assert_eq!(gruss["onServer"], false);
    assert_eq!(gruss["threadCount"], 3);
    assert!(
        gruss["snippet"]
            .as_str()
            .unwrap()
            .starts_with("Hallo Nico, liebe Grüsse aus Zürich!"),
        "{gruss}"
    );
    let reply = find(&inbox, "Re: Grüsse aus Zürich");
    assert_eq!(reply["isSent"], true);
    assert!(
        !reply["snippet"].as_str().unwrap().contains("schrieb"),
        "ohne Zitat: {reply}"
    );
    assert_eq!(
        find(&inbox, "Herbst-Aktion: 20% auf alle Velohelme")["category"],
        "newsletter"
    );
    assert_eq!(
        find(&inbox, "Sie haben 3 neue Kontaktanfragen")["category"],
        "social"
    );
    assert_eq!(
        find(&inbox, "[nrzMail] Neuer Pull Request #7")["category"],
        "notification"
    );
    assert_eq!(
        find(&inbox, "Ihre Rechnung Oktober 2024")["hasAttachments"],
        true
    );
    assert_eq!(
        find(&inbox, "Ihre Rechnung Oktober 2024")["category"],
        "primary"
    );
    let broken = find(&inbox, "Notiz ohne Absender");
    assert_eq!(broken["from"]["address"], "");

    // Seiten
    let page = messages(&c, &u, "view=inbox&limit=3&offset=3").await;
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    assert_eq!(page["total"], 8);
    assert_eq!(page["offset"], 3);
    assert_eq!(
        messages(&c, &u, "view=inbox&category=newsletter").await["total"],
        1
    );
    assert_eq!(messages(&c, &u, "view=sent").await["total"], 1);
    assert_eq!(
        messages(&c, &u, "view=unread").await["total"],
        0,
        "importiert = gelesen"
    );

    // Suche
    let q = |s: &str| format!("view=all&q={}", nrzmail::api::encode(s));
    for (search, expected) in [
        ("zürich", 3),
        ("zurich", 3),
        ("Zür", 3),
        ("from:anna", 2),
        ("von:anna -dienstag", 0),
        ("to:bruno", 1),
        ("subject:rechnung", 1),
        ("\"20% Rabatt\"", 1),
        ("velohelme", 1),
        ("has:attachment", 1),
        ("filename:Rechnung", 1),
        ("is:sent", 1),
        ("category:sozial", 1),
        ("before:2025-01-01", 2),
        ("after:2025-03-01 before:2025-04-01", 3),
        ("folder:posteingang zeichensatz", 1),
        ("account:archiv rechnung", 1),
        ("-zürich", 5),
        ("gibtsnicht", 0),
    ] {
        let v = messages(&c, &u, &q(search)).await;
        assert_eq!(
            v["total"],
            expected,
            "{search}: {}",
            subjects(&v).join(" | ")
        );
    }

    // FTS-Syntax in der Eingabe führt nicht zu Fehlern
    messages(&c, &u, &q("a* OR \"x NEAR( ^-")).await;

    // Unterhaltungen: drei Nachrichten, eine Zeile
    let threads = messages(&c, &u, "view=all&threads=true").await;
    assert_eq!(threads["total"], 6, "{}", subjects(&threads).join(" | "));
    let t = find(&threads, "Re: Re: Grüsse aus Zürich");
    assert_eq!(t["threadCount"], 3);
    let (s, thread) = call(
        &c,
        &u,
        "GET",
        format!("/threads/{}", t["threadId"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(s, Status::Ok);
    let subs: Vec<&str> = thread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap())
        .collect();
    assert_eq!(
        subs,
        [
            "Grüsse aus Zürich",
            "Re: Grüsse aus Zürich",
            "Re: Re: Grüsse aus Zürich"
        ]
    );
    assert_eq!(thread["subject"], "Grüsse aus Zürich");

    // Detail: HTML bereinigt, eingebettetes Bild über die API, externe Bilder blockiert
    let nl_id = find(&inbox, "Herbst-Aktion: 20% auf alle Velohelme")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, nl) = call(&c, &u, "GET", format!("/messages/{nl_id}"), None).await;
    assert_eq!(s, Status::Ok);
    let html = nl["htmlBody"].as_str().unwrap();
    for bad in [
        "<script",
        "onclick",
        "javascript:",
        "<form",
        "<input",
        "bg.png",
        " src=\"https://",
    ] {
        assert!(!html.contains(bad), "{bad}: {html}");
    }
    assert!(
        html.contains("data-src=\"https://track.shop.example/pixel.gif?u=42\""),
        "{html}"
    );
    assert_eq!(nl["hasRemoteImages"], true);
    assert!(html.contains("target=\"_blank\""));
    assert_eq!(nl["listUnsubscribe"], "https://shop.example/abmelden?id=42");
    assert!(
        nl["textBody"].as_str().unwrap().contains("20% Rabatt"),
        "Text aus HTML"
    );
    let logo = &nl["attachments"][0];
    assert_eq!(logo["inline"], true);
    assert!(
        html.contains(&format!(
            "/api/v1/attachments/{}?inline=true",
            logo["id"].as_str().unwrap()
        )),
        "{html}"
    );
    let res = get_raw(
        &c,
        &u,
        &format!("/attachments/{}?inline=true", logo["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(res.content_type().unwrap().to_string(), "image/png");
    assert!(res
        .headers()
        .get_one("Content-Disposition")
        .unwrap()
        .starts_with("inline"));
    assert!(res.into_bytes().await.unwrap().starts_with(b"\x89PNG"));

    // PDF-Anhang: Download (auch mit inline=true kein Anzeigen im Browser)
    let rechnung_id = find(&inbox, "Ihre Rechnung Oktober 2024")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, r) = call(&c, &u, "GET", format!("/messages/{rechnung_id}"), None).await;
    assert_eq!(r["htmlBody"], Value::Null);
    let pdf = &r["attachments"][0];
    assert_eq!(pdf["filename"], "Rechnung-2024-10.pdf");
    let res = get_raw(
        &c,
        &u,
        &format!("/attachments/{}?inline=true", pdf["id"].as_str().unwrap()),
    )
    .await;
    let cd = res
        .headers()
        .get_one("Content-Disposition")
        .unwrap()
        .to_string();
    assert!(
        cd.starts_with("attachment; filename=\"Rechnung-2024-10.pdf\""),
        "{cd}"
    );
    assert!(res
        .headers()
        .get_one("Content-Security-Policy")
        .unwrap()
        .contains("sandbox"));
    assert!(res.into_bytes().await.unwrap().starts_with(b"%PDF-1.4"));

    // Rohdaten als .eml
    let res = get_raw(&c, &u, &format!("/messages/{rechnung_id}/raw")).await;
    assert_eq!(res.status(), Status::Ok);
    assert!(res
        .headers()
        .get_one("Content-Disposition")
        .unwrap()
        .contains("Ihre Rechnung Oktober 2024.eml"));
    let eml = res.into_bytes().await.unwrap();
    assert!(eml.starts_with(b"From: Swisscom <rechnung@swisscom.example>"));

    // Kontakte
    let (_, contacts) = call(&c, &u, "GET", "/contacts?q=ann", None).await;
    assert_eq!(contacts[0]["address"], "anna@example.ch", "{contacts}");
    assert_eq!(contacts[0]["name"], "Anna Müller");
    assert_eq!(
        contacts[0]["count"], 3,
        "zwei empfangen, einmal angeschrieben"
    );
    let (_, all) = call(&c, &u, "GET", "/contacts", None).await;
    assert!(
        !all.to_string().contains("nico@example.ch"),
        "eigene Adresse nicht: {all}"
    );

    // Export als mbox und Rundlauf in ein neues Postfach
    let res = get_raw(&c, &u, "/export?format=mbox&view=all").await;
    assert_eq!(res.status(), Status::Ok);
    let cd = res
        .headers()
        .get_one("Content-Disposition")
        .unwrap()
        .to_string();
    assert!(cd.contains("nrzmail-") && cd.contains(".mbox"), "{cd}");
    let mbox = res.into_bytes().await.unwrap();
    assert_eq!(
        mbox.windows(7).filter(|w| *w == b"\n\nFrom ").count() + 1,
        8
    );
    assert!(
        mbox.windows(17).any(|w| w == b"\n>From the start:"),
        "From-Zeilen maskiert"
    );
    let acc2 = import_account(&c, &u, "Kopie").await;
    let (_, v) = post_bytes(&c, &u, &format!("/import?account={acc2}"), mbox.clone()).await;
    assert_eq!(v["imported"], 8, "{v}");
    let (_, v) = post_bytes(&c, &u, &format!("/import?account={acc}&folder=INBOX"), mbox).await;
    assert_eq!(v["duplicates"], 8, "Rohdaten unverändert: {v}");
    assert!(
        !std::fs::read_dir(_d.path().join("tmp"))
            .unwrap()
            .any(|_| true),
        "keine Reste in tmp"
    );

    // Export ausgewählter Nachrichten als zip mit .eml – und wieder importieren
    let res = get_raw(
        &c,
        &u,
        &format!("/export?format=eml&ids={nl_id},{rechnung_id}"),
    )
    .await;
    assert_eq!(res.status(), Status::Ok);
    let zip_bytes = res.into_bytes().await.unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes.clone())).unwrap();
    assert_eq!(zip.len(), 2);
    let names: Vec<String> = (0..2)
        .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .any(|n| n == "2024-10-15_Ihre Rechnung Oktober 2024.eml"),
        "{names:?}"
    );
    let acc3 = import_account(&c, &u, "Zip").await;
    let (_, v) = post_bytes(
        &c,
        &u,
        &format!("/import?account={acc3}&folder=Rechnungen"),
        zip_bytes,
    )
    .await;
    assert_eq!(v["imported"], 2, "{v}");
    let (_, v) = post_bytes(
        &c,
        &u,
        &format!("/import?account={acc3}&folder=Einzeln"),
        eml.to_vec(),
    )
    .await;
    assert_eq!(v["duplicates"], 1, "einzelne .eml: {v}");
    let (s, _) = post_bytes(&c, &u, "/import?account=gibtsnicht", b"x".to_vec()).await;
    assert_eq!(s, Status::NotFound);

    // Archiv-Übersicht und Zeitleiste
    let (_, arch) = call(&c, &u, "GET", "/archive", None).await;
    let a1 = arch["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["account"]["id"] == acc.as_str())
        .unwrap();
    assert_eq!(a1["messageCount"], 8);
    assert_eq!(a1["localOnlyCount"], 8);
    assert_eq!(a1["attachmentCount"], 1);
    assert_eq!(
        a1["years"]
            .as_array()
            .unwrap()
            .iter()
            .map(|y| y["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["2024", "2025"]
    );
    assert_eq!(arch["totalMessages"], 18);
    assert!(arch["databaseBytes"].as_i64().unwrap() > 0);
    let (_, tl) = call(
        &c,
        &u,
        "GET",
        format!("/archive/timeline?account={acc}&year=2025"),
        None,
    )
    .await;
    let months: Vec<(&str, i64)> = tl
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["label"].as_str().unwrap(), p["count"].as_i64().unwrap()))
        .collect();
    // Die Nachricht ohne Date-Header hat das Datum ihrer From_-Zeile (1. Februar)
    assert_eq!(
        months,
        [
            ("2025-01", 1),
            ("2025-02", 1),
            ("2025-03", 3),
            ("2025-06", 1)
        ]
    );

    // Statistik über alles, in Schweizer Winterzeit
    let (s, st) = call(
        &c,
        &u,
        "GET",
        format!("/stats?account={acc}&days=0&tz=60"),
        None,
    )
    .await;
    assert_eq!(s, Status::Ok, "{st}");
    assert_eq!(st["totals"]["messages"], 8);
    assert_eq!(st["totals"]["sent"], 1);
    assert_eq!(st["totals"]["received"], 7);
    assert_eq!(st["totals"]["withAttachments"], 1);
    assert_eq!(st["totals"]["threads"], 6);
    assert_eq!(st["heatmap"].as_array().unwrap().len(), 7);
    assert!(st["heatmap"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r.as_array().unwrap().len() == 24));
    assert_eq!(
        st["perHour"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .sum::<i64>(),
        7
    );
    // Montag, 3. März 2025, 09:15 und 14:40 Ortszeit (Anna)
    assert_eq!(st["heatmap"][0][9], 1);
    assert_eq!(st["heatmap"][0][14], 1);
    // LinkedIn: Mi 8. Jan 22:30 Ortszeit
    assert_eq!(st["perWeekday"][2], 1);
    assert_eq!(st["topSenders"][0]["address"], "anna@example.ch");
    assert_eq!(st["topSenders"][0]["count"], 2);
    assert_eq!(st["topRecipients"][0]["address"], "anna@example.ch");
    assert!(st["topDomains"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["domain"] == "example.ch"));
    assert_eq!(st["busiestDay"]["date"], "2025-03-03");
    assert_eq!(st["busiestDay"]["received"], 2);
    let per_day = st["perDay"].as_array().unwrap();
    assert!(per_day.len() <= 365 && !per_day.is_empty());
    let months: Vec<&str> = st["perMonth"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["month"].as_str().unwrap())
        .collect();
    assert_eq!(months[0], "2024-10");
    assert!(
        months.windows(2).all(|w| w[0] < w[1]),
        "jeder Monat genau einmal"
    );
    assert!(st["categories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["category"] == "newsletter"));
    assert_eq!(st["accounts"][0]["count"], 8);
    // Zeitzone: LinkedIn 21:30 UTC am 8. Januar ist in UTC+3 schon der 9.
    let (_, st3) = call(
        &c,
        &u,
        "GET",
        format!("/stats?account={acc}&days=0&tz=180"),
        None,
    )
    .await;
    assert_eq!(st3["perWeekday"][3], 2, "Donnerstag: LinkedIn und GitHub");
    assert_eq!(st3["perWeekday"][2], 0);
    // Letzte 7 Tage: jeder Tag, auch ohne Nachrichten
    let (_, st7) = call(&c, &u, "GET", "/stats?days=7&tz=0", None).await;
    assert_eq!(st7["perDay"].as_array().unwrap().len(), 7);
    assert_eq!(st7["totals"]["messages"], 0);
    assert!(st7["busiestDay"].is_null());

    // Protokoll: Import und Export
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM activity_log WHERE action IN ('import', 'export')",
    )
    .fetch_one(pool(&c))
    .await
    .unwrap();
    assert!(n >= 6, "{n}");
    c.terminate().await;
}

#[tokio::test]
async fn actions_and_isolation() {
    let (_d, c) = client().await;
    let u = user(&c, "nico").await;
    let acc = import_account(&c, &u, "Archiv").await;
    let (_, v) = post_bytes(
        &c,
        &u,
        &format!("/import?account={acc}&folder=INBOX"),
        MBOX.to_vec(),
    )
    .await;
    assert_eq!(v["imported"], 8);
    let inbox = messages(&c, &u, "view=inbox").await;
    let id = |s: &str| find(&inbox, s)["id"].as_str().unwrap().to_string();
    let rechnung = id("Ihre Rechnung Oktober 2024");
    let nl = id("Herbst-Aktion: 20% auf alle Velohelme");
    let li = id("Sie haben 3 neue Kontaktanfragen");
    let v = act(&c, &u, &[&rechnung, &nl], json!({ "type": "markUnread" })).await;
    assert_eq!(
        (v["updated"].as_u64(), v["failed"].as_u64()),
        (Some(2), Some(0))
    );
    assert!(v["message"].is_null(), "ohne Server keine Meldung: {v}");
    assert_eq!(messages(&c, &u, "view=unread").await["total"], 2);
    let (_, accs) = call(&c, &u, "GET", "/accounts", None).await;
    assert_eq!(accs[0]["unreadCount"], 2);
    assert_eq!(accs[0]["messageCount"], 8);
    act(&c, &u, &[&rechnung], json!({ "type": "markRead" })).await;
    act(&c, &u, &[&rechnung], json!({ "type": "flag" })).await;
    assert_eq!(messages(&c, &u, "view=starred").await["total"], 1);
    assert_eq!(
        messages(&c, &u, "view=all&q=is%3Astarred").await["total"],
        1
    );

    // Archivieren legt den Archiv-Ordner an
    act(&c, &u, &[&rechnung], json!({ "type": "archive" })).await;
    let archive = messages(&c, &u, "view=archive").await;
    assert_eq!(subjects(&archive), ["Ihre Rechnung Oktober 2024"]);
    assert_eq!(messages(&c, &u, "view=inbox").await["total"], 7);
    let (_, folders) = call(&c, &u, "GET", format!("/folders?account={acc}"), None).await;
    assert_eq!(
        mail_common::roles(&folders)
            .get("Archiv")
            .map(String::as_str),
        Some("archive"),
        "{folders}"
    );

    // Papierkorb und zurück; Spam; Kategorie
    act(&c, &u, &[&nl], json!({ "type": "trash" })).await;
    assert_eq!(
        subjects(&messages(&c, &u, "view=trash").await),
        ["Herbst-Aktion: 20% auf alle Velohelme"]
    );
    assert_eq!(
        messages(&c, &u, "view=unread").await["total"],
        0,
        "Papierkorb zählt nicht"
    );
    act(&c, &u, &[&nl], json!({ "type": "restore" })).await;
    assert_eq!(messages(&c, &u, "view=trash").await["total"], 0);
    act(&c, &u, &[&li], json!({ "type": "spam" })).await;
    assert_eq!(messages(&c, &u, "view=junk").await["total"], 1);
    act(&c, &u, &[&li], json!({ "type": "notSpam" })).await;
    act(
        &c,
        &u,
        &[&li],
        json!({ "type": "setCategory", "category": "primary" }),
    )
    .await;
    let (_, d) = call(&c, &u, "GET", format!("/messages/{li}"), None).await;
    assert_eq!(d["category"], "primary");
    assert_eq!(d["folderName"], "INBOX");

    // Verschieben in einen Ordner eines anderen Postfachs geht nicht
    let acc2 = import_account(&c, &u, "Zweites").await;
    post_bytes(
        &c,
        &u,
        &format!("/import?account={acc2}&folder=Ablage"),
        mail_common::mail("x@y.ch", "Anderes", "o1@y.ch", "Text"),
    )
    .await;
    let (_, f2) = call(&c, &u, "GET", format!("/folders?account={acc2}"), None).await;
    let foreign_folder = f2[0]["id"].as_str().unwrap().to_string();
    let v = act(
        &c,
        &u,
        &[&li],
        json!({ "type": "move", "folderId": foreign_folder }),
    )
    .await;
    assert_eq!(v["failed"], 1, "{v}");

    // Ordner: Rolle setzen (gibt es danach nur einmal) und Abgleich ausschalten
    let (_, folders) = call(&c, &u, "GET", format!("/folders?account={acc}"), None).await;
    let archiv = folders
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Archiv")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let inbox_folder = folders
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "INBOX")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, f) = call(
        &c,
        &u,
        "PUT",
        format!("/folders/{inbox_folder}"),
        Some(json!({ "role": "archive", "syncEnabled": false })),
    )
    .await;
    assert_eq!(s, Status::Ok, "{f}");
    assert_eq!(
        (f["role"].as_str(), f["syncEnabled"].as_bool()),
        (Some("archive"), Some(false))
    );
    let (_, folders) = call(&c, &u, "GET", format!("/folders?account={acc}"), None).await;
    let roles = mail_common::roles(&folders);
    assert_eq!(roles["Archiv"], "other");
    call(
        &c,
        &u,
        "PUT",
        format!("/folders/{inbox_folder}"),
        Some(json!({ "role": "inbox" })),
    )
    .await;
    call(
        &c,
        &u,
        "PUT",
        format!("/folders/{archiv}"),
        Some(json!({ "role": "archive" })),
    )
    .await;

    // Fremder Benutzer: sieht und ändert nichts
    let other = user(&c, "fremd").await;
    let thread_id = find(&inbox, "Grüsse aus Zürich")["threadId"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, r) = call(&c, &u, "GET", format!("/messages/{rechnung}"), None).await;
    let att = r["attachments"][0]["id"].as_str().unwrap().to_string();
    for uri in [
        format!("/messages/{rechnung}"),
        format!("/messages/{rechnung}/raw"),
        format!("/threads/{thread_id}"),
        format!("/attachments/{att}"),
        format!("/folders?account={acc}"),
        format!("/stats?account={acc}"),
        format!("/archive/timeline?account={acc}&year=2025"),
    ] {
        let (s, v) = call(&c, &other, "GET", &uri, None).await;
        if uri.starts_with("/archive/timeline") {
            assert_eq!(v, json!([]), "{uri}");
        } else {
            assert_eq!(s, Status::NotFound, "{uri}: {v}");
        }
    }
    let (_, v) = call(&c, &other, "GET", "/messages?view=all", None).await;
    assert_eq!(v["total"], 0);
    let (_, v) = call(&c, &other, "GET", "/messages?view=all&q=z%C3%BCrich", None).await;
    assert_eq!(v["total"], 0);
    let (_, v) = call(&c, &other, "GET", "/contacts", None).await;
    assert_eq!(v, json!([]));
    let (_, v) = call(&c, &other, "GET", "/archive", None).await;
    assert_eq!(v["accounts"], json!([]));
    let (_, v) = call(&c, &other, "GET", "/stats?days=0", None).await;
    assert_eq!(v["totals"]["messages"], 0);
    let (_, v) = call(
        &c,
        &other,
        "POST",
        "/messages/actions",
        Some(json!({ "ids": [rechnung], "action": { "type": "delete" } })),
    )
    .await;
    assert_eq!(
        (v["updated"].as_u64(), v["failed"].as_u64()),
        (Some(0), Some(1))
    );
    let (s, _) = call(
        &c,
        &other,
        "PUT",
        format!("/folders/{archiv}"),
        Some(json!({ "role": "trash" })),
    )
    .await;
    assert_eq!(s, Status::NotFound);
    let (s, _) = post_bytes(&c, &other, &format!("/import?account={acc}"), MBOX.to_vec()).await;
    assert_eq!(s, Status::NotFound);
    let res = get_raw(&c, &other, &format!("/export?format=mbox&ids={rechnung}")).await;
    assert_eq!(
        res.into_bytes().await.unwrap_or_default().len(),
        0,
        "leerer Export"
    );
    let (s, v) = call(
        &c,
        &other,
        "POST",
        "/archive/restore",
        Some(json!({ "ids": [rechnung], "folderId": archiv })),
    )
    .await;
    assert_eq!(s, Status::NotFound, "{v}");

    // Endgültig löschen: auch aus dem Suchindex
    let fts_before: i64 = sqlx::query_scalar("SELECT count(*) FROM messages_fts")
        .fetch_one(pool(&c))
        .await
        .unwrap();
    let v = act(&c, &u, &[&rechnung], json!({ "type": "delete" })).await;
    assert_eq!(v["updated"], 1);
    let fts_after: i64 = sqlx::query_scalar("SELECT count(*) FROM messages_fts")
        .fetch_one(pool(&c))
        .await
        .unwrap();
    assert_eq!(fts_after, fts_before - 1);
    assert_eq!(
        call(&c, &u, "GET", format!("/messages/{rechnung}"), None)
            .await
            .0,
        Status::NotFound
    );
    assert_eq!(
        call(&c, &u, "GET", format!("/attachments/{att}"), None)
            .await
            .0,
        Status::NotFound
    );

    // Restore in ein Archiv-Postfach geht nicht (kein Server)
    let (s, v) = call(
        &c,
        &u,
        "POST",
        "/archive/restore",
        Some(json!({ "ids": [nl], "folderId": archiv })),
    )
    .await;
    assert_eq!(s, Status::BadRequest, "{v}");

    // Postfach löschen: alles weg, auch der Suchindex
    assert_eq!(
        call(&c, &u, "DELETE", format!("/accounts/{acc}"), None)
            .await
            .0,
        Status::NoContent
    );
    assert_eq!(
        call(&c, &u, "DELETE", format!("/accounts/{acc2}"), None)
            .await
            .0,
        Status::NoContent
    );
    for table in [
        "messages",
        "message_raw",
        "attachments",
        "messages_fts",
        "folders",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pool(&c))
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    c.terminate().await;
}

#[tokio::test]
async fn threads_link_in_any_order() {
    let (_d, c) = client().await;
    let u = user(&c, "nico").await;
    let acc = import_account(&c, &u, "Reihenfolge").await;
    let reply = "From: Bruno <bruno@example.com>\r\nTo: nico@example.ch\r\nSubject: Re: Plan\r\nDate: Tue, 7 Oct 2025 10:00:00 +0200\r\nMessage-ID: <b@x>\r\nIn-Reply-To: <a@x>\r\n\r\nAntwort\r\n";
    let root = "From: Anna <anna@example.ch>\r\nTo: nico@example.ch\r\nSubject: Plan\r\nDate: Mon, 6 Oct 2025 10:00:00 +0200\r\nMessage-ID: <a@x>\r\n\r\nFrage\r\n";
    let other = "From: Carla <carla@example.com>\r\nTo: nico@example.ch\r\nSubject: Plan\r\nDate: Mon, 6 Oct 2025 11:00:00 +0200\r\nMessage-ID: <c@x>\r\n\r\nAnderes\r\n";
    // Antwort zuerst, dann der Anfang, dann eine fremde Nachricht mit gleichem Betreff
    for raw in [reply, root, other] {
        let (s, v) = post_bytes(
            &c,
            &u,
            &format!("/import?account={acc}"),
            raw.as_bytes().to_vec(),
        )
        .await;
        assert_eq!((s, v["imported"].as_u64()), (Status::Ok, Some(1)), "{v}");
    }
    let threads: Vec<(String, String)> =
        sqlx::query_as("SELECT message_id, thread_id FROM messages ORDER BY message_id")
            .fetch_all(pool(&c))
            .await
            .unwrap();
    assert_eq!(
        threads[0].1, threads[1].1,
        "a und b gehören zusammen: {threads:?}"
    );
    assert_ne!(threads[0].1, threads[2].1, "c ist eine eigene Unterhaltung");
    let v = messages(&c, &u, "view=all&threads=true").await;
    assert_eq!(v["total"], 2);
    c.terminate().await;
}
