//! Einrichtung, Registrierung, Anmeldung und Abmeldung, die Bremse gegen Durchprobieren und
//! das eigene Konto (Profil, Passwort, Einstellungen).

mod core_support;

use core_support::*;
use nrzmail::services::users;
use rocket::http::{ContentType, Status};
use serde_json::json;

#[rocket::async_test]
async fn setup_works_only_once_and_creates_an_admin() {
    let (_d, c) = server().await;
    {
        let (_, v) = call(&c, "GET", "/api/v1/auth/status", None).await;
        assert_eq!(v["setupRequired"], true);
        assert_eq!(v["user"], json!(null));
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));

        // Ohne Einrichtung gibt es keine Registrierung (sonst gäbe es nie einen Admin)
        let (s, _) = call(
            &c,
            "POST",
            "/api/v1/auth/register",
            Some(json!({ "username": "bob", "password": "geheim-456" })),
        )
        .await;
        assert_eq!(s, Status::Conflict);

        let res = c
            .post("/api/v1/auth/setup")
            .header(ContentType::JSON)
            .body(
                json!({ "username": "alice", "password": "geheim-123", "displayName": " Alice ", "email": "alice@example.org" })
                    .to_string(),
            )
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
        let cookie = res.headers().get_one("Set-Cookie").unwrap().to_string();
        assert!(
            cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"),
            "Cookie-Flags fehlen: {cookie}"
        );
        assert!(!cookie.contains("Secure"), "Secure nur mit cookie_secure");
        let v: serde_json::Value = res.into_json().await.unwrap();
        assert_eq!(v["isAdmin"], true);
        assert_eq!(v["displayName"], "Alice");
        assert!(v["lastLoginAt"].is_i64());

        let (_, v) = call(&c, "GET", "/api/v1/auth/status", None).await;
        assert_eq!(v["setupRequired"], false);
        assert_eq!(v["user"]["username"], "alice");

        // Ein zweites Mal geht nicht – auch nicht nach dem Abmelden
        logout(&c).await;
        let (s, v) = call(
            &c,
            "POST",
            "/api/v1/auth/setup",
            Some(json!({ "username": "mallory", "password": "geheim-789" })),
        )
        .await;
        assert_eq!(s, Status::Conflict, "{v}");
        assert_eq!(users::count(pool(&c)).await.unwrap(), 1);
        let user = users::by_username(pool(&c), "ALICE")
            .await
            .unwrap()
            .unwrap();
        assert!(user.password_hash.starts_with("$argon2id$"));
        assert!(actions(&c).await.contains(&"setup".to_string()));
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn simultaneous_setups_create_only_one_account() {
    let (_d, c) = server().await;
    {
        let a = call(
            &c,
            "POST",
            "/api/v1/auth/setup",
            Some(json!({ "username": "erste", "password": "geheim-123" })),
        );
        let b = call(
            &c,
            "POST",
            "/api/v1/auth/setup",
            Some(json!({ "username": "zweite", "password": "geheim-123" })),
        );
        let ((sa, _), (sb, _)) = futures::join!(a, b);
        let mut statuses = [sa.code, sb.code];
        statuses.sort();
        assert_eq!(statuses, [200, 409]);
        assert_eq!(users::count(pool(&c)).await.unwrap(), 1);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn invalid_setup_is_rejected() {
    let (_d, c) = server().await;
    {
        for body in [
            json!({ "username": "a", "password": "geheim-123" }),
            json!({ "username": "mit leerzeichen", "password": "geheim-123" }),
            json!({ "username": "alice", "password": "kurz" }),
            json!({ "username": "alice", "password": "geheim-123", "email": "keine-adresse" }),
        ] {
            let (s, v) = call(&c, "POST", "/api/v1/auth/setup", Some(body.clone())).await;
            assert_eq!(s, Status::BadRequest, "{body}: {v}");
            assert_eq!(v["error"]["code"], "bad_request");
        }
        assert_eq!(users::count(pool(&c)).await.unwrap(), 0);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn login_logout_and_wrong_passwords() {
    let (_d, c) = with_admin().await;
    {
        assert_eq!(call(&c, "GET", "/api/v1/me", None).await.0, Status::Ok);
        logout(&c).await;
        let (s, v) = call(&c, "GET", "/api/v1/me", None).await;
        assert_eq!(s, Status::Unauthorized);
        assert_eq!(v["error"]["code"], "unauthorized");
        // Abmelden ohne Sitzung schadet nicht
        logout(&c).await;

        // Falsches Passwort und unbekannter Name: gleiche Antwort
        let (s1, v1) = login(&c, "alice", "falsch-falsch").await;
        let (s2, v2) = login(&c, "niemand", "falsch-falsch").await;
        assert_eq!((s1, s2), (Status::Unauthorized, Status::Unauthorized));
        assert_eq!(v1, v2);
        assert_eq!(
            v1["error"]["message"],
            "Benutzername oder Passwort ist falsch."
        );
        assert_eq!(
            call(&c, "GET", "/api/v1/me", None).await.0,
            Status::Unauthorized
        );

        // Gross-/Kleinschreibung des Namens egal
        let (s, v) = login(&c, "ALICE", ADMIN_PASSWORD).await;
        assert_eq!(s, Status::Ok, "{v}");
        assert_eq!(v["username"], "alice");
        let (s, v) = call(&c, "GET", "/api/v1/me", None).await;
        assert_eq!(s, Status::Ok);
        assert_eq!(v["email"], "alice@example.org");

        let log = actions(&c).await;
        for a in ["login", "login_failed", "logout"] {
            assert!(
                log.contains(&a.to_string()),
                "{a} fehlt im Protokoll: {log:?}"
            );
        }
        // Fehlversuch mit bekanntem Namen gehört zum Benutzer, mit unbekanntem zu niemandem
        let failed: Vec<(Option<String>, String)> = sqlx::query_as(
            "SELECT user_id, detail FROM activity_log WHERE action = 'login_failed' ORDER BY id",
        )
        .fetch_all(pool(&c))
        .await
        .unwrap();
        assert!(failed[0].0.is_some() && failed[0].1.starts_with("alice"));
        assert!(failed[1].0.is_none() && failed[1].1.starts_with("niemand"));
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn disabled_accounts_cannot_log_in() {
    let (_d, c) = with_admin().await;
    {
        let bob = create_user(&c, "bob", "geheim-456", false).await;
        let (s, _) = call(
            &c,
            "PUT",
            &format!("/api/v1/admin/users/{bob}"),
            Some(json!({ "disabled": true })),
        )
        .await;
        assert_eq!(s, Status::Ok);
        logout(&c).await;
        let (s, v) = login(&c, "bob", "geheim-456").await;
        assert_eq!(s, Status::Unauthorized);
        assert_eq!(
            v["error"]["message"],
            "Benutzername oder Passwort ist falsch."
        );
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn too_many_failures_are_throttled() {
    let (_d, c) = server_with(|f| f.merge(("login_max_failures", 3))).await;
    {
        setup(&c, ADMIN, ADMIN_PASSWORD).await;
        logout(&c).await;
        let body = |pw: &str| Some(json!({ "username": "alice", "password": pw }));
        for _ in 0..3 {
            let (s, _) = call_from(
                &c,
                "203.0.113.7",
                "POST",
                "/api/v1/auth/login",
                body("falsch-123"),
            )
            .await;
            assert_eq!(s, Status::Unauthorized);
        }
        // Jetzt hilft auch das richtige Passwort nicht – von dieser Adresse
        let res = request(&c, "POST", "/api/v1/auth/login")
            .header(rocket::http::Header::new("X-Real-IP", "203.0.113.7"))
            .header(ContentType::JSON)
            .body(body(ADMIN_PASSWORD).unwrap().to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::TooManyRequests);
        let wait: u64 = res
            .headers()
            .get_one("Retry-After")
            .unwrap()
            .parse()
            .unwrap();
        assert!(wait > 0 && wait <= 15 * 60, "{wait}");
        let v: serde_json::Value = res.into_json().await.unwrap();
        assert_eq!(v["error"]["code"], "too_many_requests");
        assert!(v["error"]["message"].as_str().unwrap().contains("Minuten"));

        // Andere Adresse bzw. anderer Name sind nicht betroffen
        let (s, _) = call_from(
            &c,
            "198.51.100.1",
            "POST",
            "/api/v1/auth/login",
            body(ADMIN_PASSWORD),
        )
        .await;
        assert_eq!(s, Status::Ok);
        assert!(actions(&c).await.contains(&"login_blocked".to_string()));
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn a_successful_login_resets_the_counter() {
    let (_d, c) = server_with(|f| f.merge(("login_max_failures", 3))).await;
    {
        setup(&c, ADMIN, ADMIN_PASSWORD).await;
        for _ in 0..2 {
            assert_eq!(
                login(&c, "alice", "falsch-123").await.0,
                Status::Unauthorized
            );
        }
        assert_eq!(login(&c, "alice", ADMIN_PASSWORD).await.0, Status::Ok);
        for _ in 0..2 {
            assert_eq!(
                login(&c, "alice", "falsch-123").await.0,
                Status::Unauthorized
            );
        }
        assert_eq!(login(&c, "alice", ADMIN_PASSWORD).await.0, Status::Ok);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn registration_follows_the_instance_setting() {
    let (_d, c) = with_admin().await;
    {
        logout(&c).await;
        let register = |name: &'static str| {
            call(
                &c,
                "POST",
                "/api/v1/auth/register",
                Some(json!({ "username": name, "password": "geheim-456", "displayName": "Bob" })),
            )
        };
        let (_, v) = call(&c, "GET", "/api/v1/auth/status", None).await;
        assert_eq!(v["registrationOpen"], false);
        let (s, v) = register("bob").await;
        assert_eq!(s, Status::Forbidden, "{v}");

        login(&c, ADMIN, ADMIN_PASSWORD).await;
        let (s, v) = call(
            &c,
            "PUT",
            "/api/v1/admin/settings",
            Some(json!({ "registrationOpen": true })),
        )
        .await;
        assert_eq!(
            (s, v["registrationOpen"].clone()),
            (Status::Ok, json!(true))
        );
        logout(&c).await;

        let (s, v) = register("bob").await;
        assert_eq!(s, Status::Ok, "{v}");
        assert_eq!(v["isAdmin"], false);
        let (_, v) = call(&c, "GET", "/api/v1/me", None).await;
        assert_eq!(v["username"], "bob");
        // Gleicher Name, andere Schreibweise: vergeben
        logout(&c).await;
        assert_eq!(register("BOB").await.0, Status::Conflict);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn configuration_opens_registration_unless_an_admin_closes_it() {
    let (_d, c) = server_with(|f| f.merge(("registration_open", true))).await;
    {
        setup(&c, ADMIN, ADMIN_PASSWORD).await;
        let (_, v) = call(&c, "GET", "/api/v1/admin/settings", None).await;
        assert_eq!(v["registrationOpen"], true);
        call(
            &c,
            "PUT",
            "/api/v1/admin/settings",
            Some(json!({ "registrationOpen": false })),
        )
        .await;
        logout(&c).await;
        let (s, _) = call(
            &c,
            "POST",
            "/api/v1/auth/register",
            Some(json!({ "username": "bob", "password": "geheim-456" })),
        )
        .await;
        assert_eq!(s, Status::Forbidden);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn cookie_is_secure_when_configured() {
    let (_d, c) = server_with(|f| f.merge(("cookie_secure", true))).await;
    {
        let res = c
            .post("/api/v1/auth/setup")
            .header(ContentType::JSON)
            .body(json!({ "username": "alice", "password": "geheim-123" }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
        let cookie = res.headers().get_one("Set-Cookie").unwrap();
        assert!(cookie.contains("Secure"), "{cookie}");
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn changing_the_password_ends_other_sessions() {
    let (_d, c) = with_admin().await;
    {
        let first = session_cookie(&c);
        assert_eq!(login(&c, ADMIN, ADMIN_PASSWORD).await.0, Status::Ok);
        let second = session_cookie(&c);
        assert_ne!(first, second);

        let change = |current: &str, new: &str| {
            let body = json!({ "current": current, "new": new });
            call_as(&c, &first, "PUT", "/api/v1/me/password", Some(body))
        };
        let (s, v) = change("falsch-falsch", "neu-geheim-1").await;
        assert_eq!(s, Status::BadRequest);
        assert_eq!(v["error"]["message"], "Das aktuelle Passwort stimmt nicht.");
        assert_eq!(change(ADMIN_PASSWORD, "kurz").await.0, Status::BadRequest);
        assert_eq!(
            change(ADMIN_PASSWORD, ADMIN_PASSWORD).await.0,
            Status::BadRequest
        );
        // Bis hier gelten beide Sitzungen
        assert_eq!(
            call_as(&c, &second, "GET", "/api/v1/me", None).await.0,
            Status::Ok
        );

        assert_eq!(
            change(ADMIN_PASSWORD, "neu-geheim-1").await.0,
            Status::NoContent
        );
        assert_eq!(
            call_as(&c, &first, "GET", "/api/v1/me", None).await.0,
            Status::Ok
        );
        assert_eq!(
            call_as(&c, &second, "GET", "/api/v1/me", None).await.0,
            Status::Unauthorized
        );

        assert_eq!(
            login(&c, ADMIN, ADMIN_PASSWORD).await.0,
            Status::Unauthorized
        );
        assert_eq!(login(&c, ADMIN, "neu-geheim-1").await.0, Status::Ok);
        assert!(actions(&c).await.contains(&"password_changed".to_string()));
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn profile_can_be_changed() {
    let (_d, c) = with_admin().await;
    {
        let (s, v) = call(
            &c,
            "PUT",
            "/api/v1/me",
            Some(json!({ "displayName": "Alice Muster", "email": "  a@example.ch ", "language": "en" })),
        )
        .await;
        assert_eq!(s, Status::Ok, "{v}");
        assert_eq!(v["displayName"], "Alice Muster");
        assert_eq!(v["email"], "a@example.ch");
        assert_eq!(v["language"], "en");

        // Leere Sprache bleibt, leere E-Mail löscht
        let (_, v) = call(
            &c,
            "PUT",
            "/api/v1/me",
            Some(json!({ "displayName": "Alice" })),
        )
        .await;
        assert_eq!(
            (v["language"].as_str(), v["email"].as_str()),
            (Some("en"), Some(""))
        );

        for bad in [
            json!({ "email": "kaputt@" }),
            json!({ "language": "Deutsch" }),
            json!({ "displayName": "x".repeat(101) }),
        ] {
            let (s, v) = call(&c, "PUT", "/api/v1/me", Some(bad.clone())).await;
            assert_eq!(s, Status::BadRequest, "{bad}: {v}");
        }
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn preferences_are_stored_per_user() {
    let (_d, c) = with_admin().await;
    {
        let (s, v) = call(&c, "GET", "/api/v1/me/preferences", None).await;
        assert_eq!(s, Status::Ok);
        assert_eq!(v["showSnippets"], true);
        assert_eq!(v["loadRemoteImages"], false);

        let (s, v) = call(
            &c,
            "PUT",
            "/api/v1/me/preferences",
            Some(json!({ "compact": true, "threads": false, "defaultAccount": "gibt-es-nicht" })),
        )
        .await;
        assert_eq!(s, Status::Ok, "{v}");
        assert_eq!(v["compact"], true);
        // Fehlende Felder bekommen ihre Vorgabe, unbekannte Postfächer werden verworfen
        assert_eq!(v["showSnippets"], true);
        assert_eq!(v["defaultAccount"], json!(null));

        let (_, v) = call(&c, "GET", "/api/v1/me/preferences", None).await;
        assert_eq!(
            (v["compact"].clone(), v["threads"].clone()),
            (json!(true), json!(false))
        );

        // Andere Benutzer haben ihre eigenen
        create_user(&c, "bob", "geheim-456", false).await;
        login(&c, "bob", "geheim-456").await;
        let (_, v) = call(&c, "GET", "/api/v1/me/preferences", None).await;
        assert_eq!(v["compact"], false);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn without_ip_header_invented_addresses_do_not_help() {
    let (_d, c) = server_with(|f| {
        f.merge(("login_max_failures", 2))
            .merge(("ip_header", false))
    })
    .await;
    {
        setup(&c, ADMIN, ADMIN_PASSWORD).await;
        logout(&c).await;
        let body = |pw: &str| Some(json!({ "username": "alice", "password": pw }));
        for ip in ["192.0.2.1", "192.0.2.2"] {
            let (s, _) = call_from(&c, ip, "POST", "/api/v1/auth/login", body("falsch-123")).await;
            assert_eq!(s, Status::Unauthorized);
        }
        let (s, _) = call_from(
            &c,
            "192.0.2.3",
            "POST",
            "/api/v1/auth/login",
            body(ADMIN_PASSWORD),
        )
        .await;
        assert_eq!(s, Status::TooManyRequests);
    }
    c.terminate().await;
}
