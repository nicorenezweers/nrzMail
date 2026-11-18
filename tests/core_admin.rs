//! Verwaltung: nur für Admins, Benutzer anlegen/ändern/sperren/löschen, die Regeln für den
//! letzten Administrator, Instanz-Einstellungen und Zugriffsprotokoll.

mod core_support;

use core_support::*;
use nrzmail::api;
use nrzmail::error::AppError;
use nrzmail::services::users;
use rocket::http::Status;
use serde_json::json;

#[rocket::async_test]
async fn only_admins_may_administrate() {
    let (_d, c) = with_admin().await;
    {
        let bob = create_user(&c, "bob", "geheim-456", false).await;
        let endpoints = [
            ("GET", "/api/v1/admin/users".to_string(), None),
            (
                "POST",
                "/api/v1/admin/users".to_string(),
                Some(json!({ "username": "eve", "password": "geheim-789" })),
            ),
            (
                "PUT",
                format!("/api/v1/admin/users/{bob}"),
                Some(json!({ "isAdmin": true })),
            ),
            ("DELETE", format!("/api/v1/admin/users/{bob}"), None),
            ("GET", "/api/v1/admin/settings".to_string(), None),
            (
                "PUT",
                "/api/v1/admin/settings".to_string(),
                Some(json!({ "registrationOpen": true })),
            ),
            ("GET", "/api/v1/admin/activity".to_string(), None),
        ];

        logout(&c).await;
        for (method, uri, body) in &endpoints {
            let (s, v) = call(&c, method, uri, body.clone()).await;
            assert_eq!(s, Status::Unauthorized, "{method} {uri}: {v}");
        }

        login(&c, "bob", "geheim-456").await;
        for (method, uri, body) in &endpoints {
            let (s, v) = call(&c, method, uri, body.clone()).await;
            assert_eq!(s, Status::Forbidden, "{method} {uri}: {v}");
            assert_eq!(v["error"]["code"], "forbidden");
        }
        // Nichts davon ist passiert
        let bob_row = users::by_id(pool(&c), &bob).await.unwrap().unwrap();
        assert!(!bob_row.is_admin);
        assert!(users::by_username(pool(&c), "eve").await.unwrap().is_none());
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn admins_manage_users() {
    let (_d, c) = with_admin().await;
    {
        let (s, v) = call(
            &c,
            "POST",
            "/api/v1/admin/users",
            Some(json!({ "username": "bob", "password": "geheim-456", "displayName": "Bob", "email": "bob@example.org" })),
        )
        .await;
        assert_eq!(s, Status::Created, "{v}");
        let bob = v["id"].as_str().unwrap().to_string();
        assert_eq!(v["isAdmin"], false);
        assert_eq!(v["lastLoginAt"], json!(null));

        let (s, _) = call(
            &c,
            "POST",
            "/api/v1/admin/users",
            Some(json!({ "username": "Bob", "password": "geheim-456" })),
        )
        .await;
        assert_eq!(s, Status::Conflict);

        let (s, v) = call(&c, "GET", "/api/v1/admin/users", None).await;
        assert_eq!(s, Status::Ok);
        let names: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["username"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["alice", "bob"]);
        assert!(v[0].get("passwordHash").is_none(), "kein Hash nach aussen");

        // Ändern: nur gesetzte Felder
        let (s, v) = call(
            &c,
            "PUT",
            &format!("/api/v1/admin/users/{bob}"),
            Some(json!({ "isAdmin": true, "displayName": "Robert" })),
        )
        .await;
        assert_eq!(s, Status::Ok, "{v}");
        assert_eq!(
            (
                v["isAdmin"].clone(),
                v["displayName"].clone(),
                v["email"].clone()
            ),
            (json!(true), json!("Robert"), json!("bob@example.org"))
        );

        let (s, _) = call(
            &c,
            "PUT",
            "/api/v1/admin/users/gibt-es-nicht",
            Some(json!({ "disabled": true })),
        )
        .await;
        assert_eq!(s, Status::NotFound);
        let (s, _) = call(
            &c,
            "PUT",
            &format!("/api/v1/admin/users/{bob}"),
            Some(json!({ "password": "kurz" })),
        )
        .await;
        assert_eq!(s, Status::BadRequest);

        // Löschen
        let (s, _) = call(&c, "DELETE", &format!("/api/v1/admin/users/{bob}"), None).await;
        assert_eq!(s, Status::NoContent);
        let (s, _) = call(&c, "DELETE", &format!("/api/v1/admin/users/{bob}"), None).await;
        assert_eq!(s, Status::NotFound);
        assert!(users::by_id(pool(&c), &bob).await.unwrap().is_none());

        let log = actions(&c).await;
        for a in ["user_created", "user_updated", "user_deleted"] {
            assert!(log.contains(&a.to_string()), "{a} fehlt: {log:?}");
        }
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn blocking_and_new_passwords_end_sessions() {
    let (_d, c) = with_admin().await;
    {
        let admin = session_cookie(&c);
        let bob = create_user(&c, "bob", "geheim-456", false).await;
        login(&c, "bob", "geheim-456").await;
        let bob_session = session_cookie(&c);
        assert_eq!(
            call_as(&c, &bob_session, "GET", "/api/v1/me", None).await.0,
            Status::Ok
        );

        // Neues Passwort: Bob ist abgemeldet, das neue gilt
        let uri = format!("/api/v1/admin/users/{bob}");
        let (s, _) = call_as(
            &c,
            &admin,
            "PUT",
            &uri,
            Some(json!({ "password": "neu-geheim-2" })),
        )
        .await;
        assert_eq!(s, Status::Ok);
        assert_eq!(
            call_as(&c, &bob_session, "GET", "/api/v1/me", None).await.0,
            Status::Unauthorized
        );
        assert_eq!(login(&c, "bob", "geheim-456").await.0, Status::Unauthorized);
        assert_eq!(login(&c, "bob", "neu-geheim-2").await.0, Status::Ok);
        let bob_session = session_cookie(&c);

        // Sperren: ebenso
        let (s, v) = call_as(&c, &admin, "PUT", &uri, Some(json!({ "disabled": true }))).await;
        assert_eq!((s, v["disabled"].clone()), (Status::Ok, json!(true)));
        assert_eq!(
            call_as(&c, &bob_session, "GET", "/api/v1/me", None).await.0,
            Status::Unauthorized
        );
        assert_eq!(
            login(&c, "bob", "neu-geheim-2").await.0,
            Status::Unauthorized
        );

        // Entsperren
        call_as(&c, &admin, "PUT", &uri, Some(json!({ "disabled": false }))).await;
        assert_eq!(login(&c, "bob", "neu-geheim-2").await.0, Status::Ok);

        // Das eigene Passwort über die Verwaltung: die laufende Sitzung bleibt
        let alice = users::by_username(pool(&c), "alice")
            .await
            .unwrap()
            .unwrap()
            .id;
        let (s, _) = call_as(
            &c,
            &admin,
            "PUT",
            &format!("/api/v1/admin/users/{alice}"),
            Some(json!({ "password": "neu-geheim-3" })),
        )
        .await;
        assert_eq!(s, Status::Ok);
        assert_eq!(
            call_as(&c, &admin, "GET", "/api/v1/me", None).await.0,
            Status::Ok
        );
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn admins_cannot_lock_themselves_out() {
    let (_d, c) = with_admin().await;
    {
        let me = users::by_username(pool(&c), "alice")
            .await
            .unwrap()
            .unwrap()
            .id;
        let uri = format!("/api/v1/admin/users/{me}");
        for body in [json!({ "isAdmin": false }), json!({ "disabled": true })] {
            let (s, v) = call(&c, "PUT", &uri, Some(body.clone())).await;
            assert_eq!(s, Status::Conflict, "{body}: {v}");
        }
        let (s, v) = call(&c, "DELETE", &uri, None).await;
        assert_eq!(s, Status::Conflict, "{v}");
        // Harmlose Änderungen am eigenen Konto gehen
        let (s, _) = call(
            &c,
            "PUT",
            &uri,
            Some(json!({ "isAdmin": true, "displayName": "Chefin" })),
        )
        .await;
        assert_eq!(s, Status::Ok);

        // Ein zweiter Admin darf den ersten herabstufen – dann bleibt er selbst der letzte
        let bob = create_user(&c, "bob", "geheim-456", true).await;
        login(&c, "bob", "geheim-456").await;
        let (s, v) = call(&c, "PUT", &uri, Some(json!({ "isAdmin": false }))).await;
        assert_eq!((s, v["isAdmin"].clone()), (Status::Ok, json!(false)));
        let (s, _) = call(&c, "DELETE", &format!("/api/v1/admin/users/{bob}"), None).await;
        assert_eq!(s, Status::Conflict);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn the_last_active_admin_always_remains() {
    let (_d, c) = with_admin().await;
    {
        let pool = pool(&c);
        let alice = users::by_username(pool, "alice").await.unwrap().unwrap();
        let carol = create_user(&c, "carol", "geheim-789", true).await;
        create_user(&c, "bob", "geheim-456", false).await;
        let bob = users::by_username(pool, "bob").await.unwrap().unwrap();

        // Ein gesperrter Admin zählt nicht
        let block = api::UpdateUser {
            disabled: Some(true),
            ..Default::default()
        };
        users::admin_update(pool, &alice, &carol, &block, None)
            .await
            .unwrap();

        // Auch wenn die Prüfung „nicht sich selbst“ nicht greift (z. B. ein Admin, der gerade
        // gleichzeitig herabgestuft wurde), verweigert die Datenbank-Anweisung selbst.
        let last = |r: Result<(), AppError>| matches!(r, Err(AppError::Conflict(m)) if m.contains("letzte"));
        assert!(last(users::delete(pool, &bob, &alice.id).await.map(|_| ())));
        assert!(last(
            users::admin_update(pool, &bob, &alice.id, &block, None)
                .await
                .map(|_| ())
        ));
        let demote = api::UpdateUser {
            is_admin: Some(false),
            ..Default::default()
        };
        assert!(last(
            users::admin_update(pool, &bob, &alice.id, &demote, None)
                .await
                .map(|_| ())
        ));
        let alice_now = users::by_id(pool, &alice.id).await.unwrap().unwrap();
        assert!(alice_now.is_admin && !alice_now.disabled);

        // Mit einem zweiten aktiven Admin geht es
        let unblock = api::UpdateUser {
            disabled: Some(false),
            ..Default::default()
        };
        users::admin_update(pool, &alice, &carol, &unblock, None)
            .await
            .unwrap();
        users::admin_update(pool, &bob, &alice.id, &demote, None)
            .await
            .unwrap();
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn deleting_a_user_removes_sessions_and_keeps_the_log() {
    let (_d, c) = with_admin().await;
    {
        let admin = session_cookie(&c);
        let bob = create_user(&c, "bob", "geheim-456", false).await;
        login(&c, "bob", "geheim-456").await;
        let bob_session = session_cookie(&c);

        let (s, _) = call_as(
            &c,
            &admin,
            "DELETE",
            &format!("/api/v1/admin/users/{bob}"),
            None,
        )
        .await;
        assert_eq!(s, Status::NoContent);
        assert_eq!(
            call_as(&c, &bob_session, "GET", "/api/v1/me", None).await.0,
            Status::Unauthorized
        );
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = ?")
            .bind(&bob)
            .fetch_one(pool(&c))
            .await
            .unwrap();
        assert_eq!(sessions, 0);

        // Bobs Anmeldung bleibt im Protokoll, nur ohne Benutzer
        let (s, v) = call_as(&c, &admin, "GET", "/api/v1/admin/activity?limit=50", None).await;
        assert_eq!(s, Status::Ok);
        let entries = v.as_array().unwrap();
        let bob_login = entries
            .iter()
            .find(|e| e["action"] == "login" && e["userId"].is_null())
            .expect("Anmeldung von Bob");
        assert_eq!(bob_login["username"], json!(null));
        let deleted = entries
            .iter()
            .find(|e| e["action"] == "user_deleted")
            .unwrap();
        assert_eq!(
            (deleted["username"].clone(), deleted["detail"].clone()),
            (json!("alice"), json!("bob"))
        );
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn activity_is_newest_first_and_limited() {
    let (_d, c) = with_admin().await;
    {
        for i in 0..5 {
            create_user(&c, &format!("user{i}"), "geheim-456", false).await;
        }
        let (s, v) = call(&c, "GET", "/api/v1/admin/activity?limit=3", None).await;
        assert_eq!(s, Status::Ok);
        let entries = v.as_array().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["detail"], "user4");
        assert_eq!(entries[0]["username"], "alice");
        assert!(entries[0]["id"].as_i64() > entries[1]["id"].as_i64());

        let (_, v) = call(&c, "GET", "/api/v1/admin/activity", None).await;
        assert_eq!(
            v.as_array().unwrap().len(),
            6,
            "Einrichtung + 5 neue Benutzer"
        );
        let (_, v) = call(&c, "GET", "/api/v1/admin/activity?limit=0", None).await;
        assert_eq!(v.as_array().unwrap().len(), 1);
    }
    c.terminate().await;
}
