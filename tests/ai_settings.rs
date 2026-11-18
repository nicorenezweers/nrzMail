//! KI-Einstellungen und Anbieter: Rechte, Schlüssel-Hinweis statt Schlüssel, Umgebung,
//! OpenAI-kompatibel und Anthropic gegen gefälschte Server, verständliche Fehler.

mod ai_support;

use ai_support::*;
use rocket::http::Status;
use serde_json::{json, Value};
use std::sync::Arc;

#[rocket::async_test]
async fn rights_and_key_hint() {
    let (_d, c) = server().await;
    let (_, admin) = user(&c, "alice", true).await;
    let (_, bob) = user(&c, "bob", false).await;

    // Ohne Anmeldung nichts, Status und Anbieter für alle Angemeldeten
    assert_eq!(
        call(&c, "", "GET", "/api/v1/ai/status", None).await.0,
        Status::Unauthorized
    );
    let (s, v) = call(&c, &bob, "GET", "/api/v1/ai/status", None).await;
    assert_eq!(s, Status::Ok, "{v}");
    assert_eq!(v["assistantName"], "Dieter");
    let (s, v) = call(&c, &bob, "GET", "/api/v1/ai/providers", None).await;
    assert_eq!(s, Status::Ok);
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    for id in [
        "openrouter",
        "openai",
        "anthropic",
        "gemini",
        "mistral",
        "groq",
        "xai",
        "deepseek",
        "together",
        "fireworks",
        "nvidia",
        "huggingface",
        "azure",
        "ollama",
        "lmstudio",
        "vllm",
        "localai",
        "custom",
    ] {
        assert!(ids.contains(&id), "{id}");
    }
    let ollama = v
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "ollama")
        .unwrap();
    assert_eq!(
        (ollama["local"].clone(), ollama["needsKey"].clone()),
        (json!(true), json!(false))
    );

    // Einstellungen, Modelle, Test: nur Admins
    for (method, uri, body) in [
        ("GET", "/api/v1/ai/settings", None),
        (
            "PUT",
            "/api/v1/ai/settings",
            Some(json!({ "provider": "openai" })),
        ),
        ("GET", "/api/v1/ai/models", None),
        ("POST", "/api/v1/ai/test", Some(json!({}))),
    ] {
        assert_eq!(
            call(&c, &bob, method, uri, body).await.0,
            Status::Forbidden,
            "{method} {uri}"
        );
    }

    // Schlüssel speichern: nach aussen nur der Hinweis
    let key = "sk-test-0123456789abcdefXYZ";
    let (s, v) = call(
        &c,
        &admin,
        "PUT",
        "/api/v1/ai/settings",
        Some(json!({ "enabled": true, "provider": "openai", "apiKey": key, "model": "", "baseUrl": "", "autoCategorize": true, "assistantName": "Erika" })),
    )
    .await;
    assert_eq!(s, Status::Ok, "{v}");
    assert_eq!(v["provider"], "openai");
    assert_eq!(v["model"], "gpt-4o-mini");
    assert_eq!(v["baseUrl"], "");
    assert_eq!(v["keySource"], "settings");
    assert_eq!(v["keyHint"], "sk-test-01…fXYZ");
    assert_eq!(
        (v["autoCategorize"].clone(), v["assistantName"].clone()),
        (json!(true), json!("Erika"))
    );
    assert!(!v.to_string().contains(key), "Schlüssel bleibt geheim");
    let (_, v) = call(&c, &admin, "GET", "/api/v1/ai/settings", None).await;
    assert!(!v.to_string().contains(key));
    assert_eq!(v["keyHint"], "sk-test-01…fXYZ");
    let (_, v) = call(&c, &bob, "GET", "/api/v1/ai/status", None).await;
    assert_eq!(
        (v["available"].clone(), v["assistantName"].clone()),
        (json!(true), json!("Erika"))
    );
    assert_eq!(v["model"], "gpt-4o-mini");

    // Ohne apiKey bleibt der Schlüssel, mit "" verschwindet er
    let (_, v) = call(
        &c,
        &admin,
        "PUT",
        "/api/v1/ai/settings",
        Some(json!({ "enabled": true, "provider": "openai", "model": "gpt-4.1-mini" })),
    )
    .await;
    assert_eq!(
        (v["keySource"].clone(), v["model"].clone()),
        (json!("settings"), json!("gpt-4.1-mini"))
    );
    let (_, v) = call(
        &c,
        &admin,
        "PUT",
        "/api/v1/ai/settings",
        Some(json!({ "enabled": true, "provider": "openai", "apiKey": "" })),
    )
    .await;
    assert_eq!(
        (v["keySource"].clone(), v["keyHint"].clone()),
        (json!("none"), json!(""))
    );
    let (_, v) = call(&c, &bob, "GET", "/api/v1/ai/status", None).await;
    assert_eq!(v["available"], false);
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("Kein Schlüssel für OpenAI"),
        "{v}"
    );

    // Ungültiges
    for bad in [
        json!({ "provider": "gibts-nicht" }),
        json!({ "provider": "custom", "baseUrl": "ftp://x" }),
        json!({ "provider": "openrouter", "apiKey": "falsch-12345" }),
        json!({ "provider": "openai", "apiKey": "kurz" }),
    ] {
        assert_eq!(
            call(&c, &admin, "PUT", "/api/v1/ai/settings", Some(bad.clone()))
                .await
                .0,
            Status::BadRequest,
            "{bad}"
        );
    }

    // Ausgeschaltet: nicht verfügbar, KI-Aufrufe melden 503
    let (_, v) = call(
        &c,
        &admin,
        "PUT",
        "/api/v1/ai/settings",
        Some(json!({ "enabled": false, "provider": "ollama" })),
    )
    .await;
    assert_eq!(v["enabled"], false);
    let (_, v) = call(&c, &bob, "GET", "/api/v1/ai/status", None).await;
    assert_eq!(v["available"], false);
    assert!(v["message"].as_str().unwrap().contains("ausgeschaltet"));
    let (s, v) = call(
        &c,
        &bob,
        "POST",
        "/api/v1/ai/write",
        Some(json!({ "instruction": "Hallo sagen" })),
    )
    .await;
    assert_eq!(s, Status::ServiceUnavailable, "{v}");
    assert_eq!(v["error"]["code"], "ai_unavailable");
    c.terminate().await;
}

#[rocket::async_test]
async fn key_from_environment() {
    let (_d, c) = server_with(|f| {
        f.merge(("ai_provider", "mistral"))
            .merge(("ai_key", "env-key-0123456789abcdef"))
            .merge(("ai_model", "mistral-large-latest"))
    })
    .await;
    let (_, admin) = user(&c, "alice", true).await;
    let (_, v) = call(&c, &admin, "GET", "/api/v1/ai/settings", None).await;
    assert_eq!(v["provider"], "mistral");
    assert_eq!(v["keySource"], "environment");
    assert_eq!(v["model"], "mistral-large-latest");
    assert_eq!(v["enabled"], true, "Vorgabe: mit Schlüssel eingeschaltet");
    assert!(!v.to_string().contains("env-key-0123456789abcdef"));
    // Anderer Anbieter: der Schlüssel aus der Umgebung gilt dort nicht
    let (_, v) = call(
        &c,
        &admin,
        "PUT",
        "/api/v1/ai/settings",
        Some(json!({ "enabled": true, "provider": "groq" })),
    )
    .await;
    assert_eq!(v["keySource"], "none");
    // Zurück: Umgebung gilt wieder, Modell aus der Umgebung
    let (_, v) = call(
        &c,
        &admin,
        "PUT",
        "/api/v1/ai/settings",
        Some(json!({ "enabled": true, "provider": "mistral" })),
    )
    .await;
    assert_eq!(
        (v["keySource"].clone(), v["model"].clone()),
        (json!("environment"), json!("mistral-large-latest"))
    );
    c.terminate().await;
}

fn openai_like(seen: &Seen) -> (u16, Value) {
    if seen.path.ends_with("/models") {
        return (
            200,
            json!({ "object": "list", "data": [{ "id": "llama3.2" }, { "id": "qwen3" }] }),
        );
    }
    if seen.head.contains("authorization: bearer falsch") {
        return (401, json!({ "error": { "message": "Invalid API key" } }));
    }
    if seen.body["model"] == "gibts-nicht" {
        return (
            404,
            json!({ "error": { "message": "The model `gibts-nicht` does not exist" } }),
        );
    }
    if seen.body["model"] == "zu-viel" {
        return (429, json!({ "error": { "message": "Rate limit reached" } }));
    }
    (200, openai_text("llama3.2", "OK"))
}

fn anthropic_like(seen: &Seen) -> (u16, Value) {
    if seen.path.contains("/models") {
        return (
            200,
            json!({ "data": [{ "id": "claude-haiku-5-5", "display_name": "Claude Haiku 5.5" }] }),
        );
    }
    (200, anthropic_text("claude-haiku-5-5", "OK"))
}

#[rocket::async_test]
async fn providers_test_and_models() {
    let (_d, c) = server().await;
    let (_, admin) = user(&c, "alice", true).await;

    // Lokaler OpenAI-kompatibler Server ohne Schlüssel
    let fake = fake_ai(Arc::new(openai_like)).await;
    configure(
        &c,
        &admin,
        "ollama",
        &format!("{}/v1", fake.url),
        "llama3.2",
        None,
    )
    .await;
    let (s, v) = call(&c, &admin, "POST", "/api/v1/ai/test", None).await;
    assert_eq!(s, Status::Ok, "{v}");
    assert_eq!(
        (v["ok"].clone(), v["model"].clone()),
        (json!(true), json!("llama3.2")),
        "{v}"
    );
    let seen = fake.last();
    assert_eq!(
        (seen.method.as_str(), seen.path.as_str()),
        ("POST", "/v1/chat/completions")
    );
    assert!(
        !seen.head.contains("authorization:"),
        "ohne Schlüssel keine Kopfzeile"
    );
    assert_eq!(seen.body["messages"][0]["role"], "system");
    assert!(
        seen.body.get("reasoning").is_none(),
        "nur OpenRouter kennt reasoning"
    );
    let (s, v) = call(&c, &admin, "GET", "/api/v1/ai/models", None).await;
    assert_eq!(s, Status::Ok, "{v}");
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["id"], "llama3.2");

    // Test mit noch nicht gespeicherten Einstellungen: falscher Schlüssel, unbekanntes Modell
    let base = format!("{}/v1", fake.url);
    let (_, v) = call(&c, &admin, "POST", "/api/v1/ai/test", Some(json!({ "enabled": true, "provider": "custom", "baseUrl": base, "model": "m", "apiKey": "falsch-0123456789" }))).await;
    assert_eq!(v["ok"], false);
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("Schlüssel abgelehnt"),
        "{v}"
    );
    let (_, v) = call(&c, &admin, "POST", "/api/v1/ai/test", Some(json!({ "enabled": true, "provider": "custom", "baseUrl": base, "model": "gibts-nicht" }))).await;
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("kennt das Modell „gibts-nicht“ nicht"),
        "{v}"
    );
    let (_, v) = call(&c, &admin, "GET", "/api/v1/ai/settings", None).await;
    assert_eq!(v["provider"], "ollama", "Test speichert nichts");

    // Rate-Limit und unerreichbarer Anbieter bei echten Aufrufen
    configure(&c, &admin, "custom", &base, "zu-viel", None).await;
    let (s, v) = call(
        &c,
        &admin,
        "POST",
        "/api/v1/ai/write",
        Some(json!({ "instruction": "Hallo sagen" })),
    )
    .await;
    assert_eq!(s, Status::TooManyRequests, "{v}");
    assert_eq!(v["error"]["code"], "ai_rate_limited");
    configure(&c, &admin, "custom", "http://127.0.0.1:9/v1", "m", None).await;
    let (s, v) = call(
        &c,
        &admin,
        "POST",
        "/api/v1/ai/write",
        Some(json!({ "instruction": "Hallo sagen" })),
    )
    .await;
    assert_eq!(s, Status::BadGateway, "{v}");
    assert_eq!(v["error"]["code"], "ai_unreachable");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("nicht erreichbar"));

    // Anthropic: eigene Kopfzeilen, Messages-API, Modellliste
    let fake = fake_ai(Arc::new(anthropic_like)).await;
    let v = configure(
        &c,
        &admin,
        "anthropic",
        &format!("{}/v1", fake.url),
        "",
        Some("sk-ant-test-0123456789"),
    )
    .await;
    assert_eq!(v["model"], "claude-haiku-5-5");
    let (_, v) = call(&c, &admin, "POST", "/api/v1/ai/test", None).await;
    assert_eq!(
        (v["ok"].clone(), v["model"].clone()),
        (json!(true), json!("claude-haiku-5-5")),
        "{v}"
    );
    let seen = fake.last();
    assert_eq!(seen.path, "/v1/messages");
    assert!(seen.head.contains("x-api-key: sk-ant-test-0123456789"));
    assert!(seen.head.contains("anthropic-version: 2023-06-01"));
    assert!(!seen.head.contains("authorization:"));
    assert!(seen.body["system"].as_str().unwrap().contains("OK"));
    assert_eq!(seen.body["messages"][0]["role"], "user");
    let (_, v) = call(&c, &admin, "GET", "/api/v1/ai/models", None).await;
    assert_eq!(v[0]["name"], "Claude Haiku 5.5");
    assert!(fake.last().path.starts_with("/v1/models"));

    // Modelle eines anderen Anbieters (dessen Adresse)
    let (s, _) = call(
        &c,
        &admin,
        "GET",
        "/api/v1/ai/models?provider=gibts-nicht",
        None,
    )
    .await;
    assert_eq!(s, Status::BadRequest);
    c.terminate().await;
}

fn openrouter_like(seen: &Seen) -> (u16, Value) {
    match seen.body["model"].as_str().unwrap_or("") {
        // Freies Routing überlastet → Ersatzmodell
        "openrouter/free" => (
            503,
            json!({ "error": { "message": "No endpoints available", "code": 503 } }),
        ),
        // Neuere Modelle mögen keine eigene Temperatur
        "neu-1" if seen.body.get("temperature").is_some() => (
            400,
            json!({ "error": { "message": "Unsupported parameter: 'temperature' is not supported with this model." } }),
        ),
        m => (200, openai_text(m, "OK")),
    }
}

#[rocket::async_test]
async fn openrouter_fallbacks_and_compat_retry() {
    let (_d, c) = server().await;
    let (_, admin) = user(&c, "alice", true).await;
    let fake = fake_ai(Arc::new(openrouter_like)).await;
    let v = configure(
        &c,
        &admin,
        "openrouter",
        &format!("{}/api/v1", fake.url),
        "",
        Some("sk-or-v1-0123456789abcdef"),
    )
    .await;
    assert_eq!(
        (v["model"].clone(), v["keyHint"].clone()),
        (json!("openrouter/free"), json!("sk-or-v1-0…cdef"))
    );
    let (_, v) = call(&c, &admin, "POST", "/api/v1/ai/test", None).await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(
        v["model"], "nvidia/nemotron-3.5-super-vl:free",
        "erstes Ersatzmodell"
    );
    let log = fake.log.lock().unwrap().clone();
    let models: Vec<&str> = log
        .iter()
        .map(|s| s.body["model"].as_str().unwrap())
        .collect();
    assert_eq!(
        models,
        vec![
            "openrouter/free",
            "openrouter/free",
            "nvidia/nemotron-3.5-super-vl:free"
        ]
    );
    assert!(log[0]
        .head
        .contains("authorization: bearer sk-or-v1-0123456789abcdef"));
    assert!(log[0].head.contains("x-title: nrzmail"));
    assert_eq!(log[0].body["reasoning"]["effort"], "low");

    // Eigenes Modell: keine Ersatzmodelle; Parameter-Fehler → einmal ohne Temperatur
    configure(
        &c,
        &admin,
        "openrouter",
        &format!("{}/api/v1", fake.url),
        "neu-1",
        None,
    )
    .await;
    let before = fake.count();
    let (_, v) = call(&c, &admin, "POST", "/api/v1/ai/test", None).await;
    assert_eq!(
        (v["ok"].clone(), v["model"].clone()),
        (json!(true), json!("neu-1")),
        "{v}"
    );
    let seen = fake.last();
    assert_eq!(fake.count(), before + 2);
    assert!(
        seen.body.get("temperature").is_none() && seen.body["max_completion_tokens"].is_number()
    );
    c.terminate().await;
}
