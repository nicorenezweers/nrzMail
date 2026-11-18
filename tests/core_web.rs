//! Web-Grundlagen: Lebenszeichen, Fehler als JSON, Rückfall auf die Oberfläche, Sicherheits-
//! Header, Icons und der Schutz gegen Anfragen von fremden Seiten.

mod core_support;

use core_support::*;
use rocket::http::{Accept, ContentType, Header, Status};
use serde_json::json;

#[rocket::async_test]
async fn health_answers_without_login() {
    let (_d, c) = server().await;
    {
        let (s, v) = call(&c, "GET", "/api/v1/health", None).await;
        assert_eq!(s, Status::Ok);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["name"], "nrzMail");
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn unknown_api_paths_are_json_404() {
    let (_d, c) = with_admin().await;
    {
        for (method, uri) in [
            ("GET", "/api/v1/gibt-es-nicht"),
            ("POST", "/api/v1/gibt-es-nicht"),
            ("GET", "/api/v2/health"),
            ("GET", "/api"),
        ] {
            let res = request(&c, method, uri).dispatch().await;
            assert_eq!(res.status(), Status::NotFound, "{method} {uri}");
            assert_eq!(
                res.content_type(),
                Some(ContentType::JSON),
                "{method} {uri}"
            );
            let v: serde_json::Value = res.into_json().await.unwrap();
            assert_eq!(v["error"]["code"], "not_found");
        }
        // Kaputtes JSON: verständlicher Fehler statt HTML
        let res = c
            .put("/api/v1/me")
            .header(ContentType::JSON)
            .body("{kein json")
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::BadRequest);
        assert_eq!(res.content_type(), Some(ContentType::JSON));
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn pages_of_the_app_fall_back_to_the_interface() {
    let (_d, c) = server().await;
    {
        // Jede Seite der App bekommt index.html (bzw. den Hinweis, solange die Oberfläche
        // nicht eingebaut ist) – auch ohne Anmeldung; die App leitet selbst zur Anmeldung.
        for uri in ["/", "/inbox", "/settings/users", "/messages/0190-abc"] {
            let res = c.get(uri).header(Accept::HTML).dispatch().await;
            assert!(
                res.status() == Status::Ok || res.status() == Status::ServiceUnavailable,
                "{uri}: {}",
                res.status()
            );
            assert_eq!(res.content_type(), Some(ContentType::HTML), "{uri}");
            assert_eq!(
                res.headers().get_one("Cache-Control"),
                Some("no-cache"),
                "{uri}"
            );
        }
        // Fehlende Dateien der App sind 404, nicht die Seite
        for uri in ["/assets/alt-1234.wasm", "/static/gibt-es-nicht.png"] {
            assert_eq!(
                c.get(uri).dispatch().await.status(),
                Status::NotFound,
                "{uri}"
            );
        }
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn security_headers_are_set() {
    let (_d, c) = server().await;
    {
        for uri in ["/", "/api/v1/health", "/api/v1/gibt-es-nicht"] {
            let res = c.get(uri).dispatch().await;
            let csp = res
                .headers()
                .get_one("Content-Security-Policy")
                .unwrap_or_default()
                .to_string();
            assert!(csp.contains("frame-ancestors 'none'"), "{uri}: {csp}");
            assert!(
                csp.contains("img-src 'self' data: blob: https: http:"),
                "{uri}: {csp}"
            );
            assert!(
                csp.contains("script-src 'self' 'wasm-unsafe-eval'"),
                "{uri}: {csp}"
            );
            assert!(
                !csp.contains("script-src 'self' 'unsafe-inline'"),
                "{uri}: {csp}"
            );
            for (h, v) in [
                ("X-Content-Type-Options", "nosniff"),
                ("X-Frame-Options", "DENY"),
                ("Referrer-Policy", "same-origin"),
            ] {
                assert_eq!(res.headers().get_one(h), Some(v), "{uri}: {h}");
            }
        }
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn icons_are_served() {
    let (_d, c) = server().await;
    {
        for (uri, size) in [
            ("/static/icon.png", 512),
            ("/static/favicon-32.png", 32),
            ("/favicon.ico", 32),
        ] {
            let res = c.get(uri).dispatch().await;
            assert_eq!(res.status(), Status::Ok, "{uri}");
            assert_eq!(res.content_type(), Some(ContentType::PNG), "{uri}");
            let png = res.into_bytes().await.unwrap();
            assert_eq!(&png[1..4], b"PNG", "{uri}");
            // IHDR: Breite und Höhe als u32 (big-endian) ab Byte 16
            let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
            assert_eq!((w, h), (size, size), "{uri}");
        }
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn changes_from_foreign_sites_are_rejected() {
    let (_d, c) = with_admin().await;
    {
        let send = |method: &'static str,
                    uri: &'static str,
                    origin: &'static str,
                    body: serde_json::Value| {
            request(&c, method, uri)
                .header(Header::new("Host", "mail.example.org"))
                .header(Header::new("Origin", origin))
                .header(ContentType::JSON)
                .body(body.to_string())
                .dispatch()
        };
        let profile = json!({ "displayName": "Böse" });
        let creds = json!({ "username": ADMIN, "password": ADMIN_PASSWORD });

        // Angemeldet: ändernde Anfragen
        for (method, uri, body) in [
            ("PUT", "/api/v1/me", profile.clone()),
            (
                "PUT",
                "/api/v1/admin/settings",
                json!({ "registrationOpen": true }),
            ),
            (
                "POST",
                "/api/v1/admin/users",
                json!({ "username": "eve", "password": "geheim-789" }),
            ),
        ] {
            let res = send(method, uri, "https://evil.example", body).await;
            assert_eq!(res.status(), Status::Forbidden, "{method} {uri}");
            let v: serde_json::Value = res.into_json().await.unwrap();
            assert_eq!(v["error"]["code"], "forbidden");
        }
        // Ohne Anmeldung: Anmelden, Abmelden, Einrichten, Registrieren
        for uri in [
            "/api/v1/auth/login",
            "/api/v1/auth/logout",
            "/api/v1/auth/setup",
            "/api/v1/auth/register",
        ] {
            let res = send("POST", uri, "https://evil.example", creds.clone()).await;
            assert_eq!(res.status(), Status::Forbidden, "{uri}");
        }
        // Abmelden von der fremden Seite hat die Sitzung nicht beendet
        assert_eq!(call(&c, "GET", "/api/v1/me", None).await.0, Status::Ok);
        let (_, v) = call(&c, "GET", "/api/v1/me", None).await;
        assert_eq!(v["displayName"], "Alice");

        // Gleiche Herkunft (auch mit anderem Schema) und Anfragen ohne Origin gehen
        assert_eq!(
            send(
                "PUT",
                "/api/v1/me",
                "http://mail.example.org",
                profile.clone()
            )
            .await
            .status(),
            Status::Ok
        );
        assert_eq!(
            send(
                "PUT",
                "/api/v1/me",
                "https://mail.example.org",
                profile.clone()
            )
            .await
            .status(),
            Status::Ok
        );
        let (s, _) = call(&c, "PUT", "/api/v1/me", Some(profile)).await;
        assert_eq!(s, Status::Ok);
        // Lesen ist auch von fremden Seiten erlaubt (das Cookie schützt SameSite=Lax)
        let res = request(&c, "GET", "/api/v1/me")
            .header(Header::new("Origin", "https://evil.example"))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
    }
    c.terminate().await;
}

#[rocket::async_test]
async fn a_reverse_proxy_host_is_accepted() {
    let (_d, c) = with_admin().await;
    {
        let res = request(&c, "PUT", "/api/v1/me")
            .header(Header::new("Host", "nrzmail_app_1:8330"))
            .header(Header::new("X-Forwarded-Host", "umbrel.local:8330"))
            .header(Header::new("Origin", "http://umbrel.local:8330"))
            .header(ContentType::JSON)
            .body(json!({ "displayName": "Alice" }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);
    }
    c.terminate().await;
}
