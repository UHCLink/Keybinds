//! Tests du vrai routeur Axum et du stockage local, sans ouvrir de port réseau.
//! Les requêtes passent par oneshot et utilisent un dossier temporaire par test.
//! Ces scénarios complètent les transitions pures testées dans src/lib.rs, ils
//! ne prétendent pas exercer l'API Raw Input Windows ou un microphone réel.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uhc_link_companion::{
    api::{router, LocalState},
    Action, ShortcutConfig,
};
const ORIGIN: &str = "https://uhc-link.fr";
async fn call(
    state: Arc<LocalState>,
    path: &str,
    method: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    // Même contrat Origin/Host/Bearer que le site. Cette construction explicite
    // permet aussi aux tests de retirer ou altérer chaque garde indépendamment.
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:32145")
        .header("origin", ORIGIN);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let response = router(state)
        .oneshot(
            request
                .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn rejects_foreign_origins_dns_rebinding_and_unknown_client_tokens() {
    // Une origine autorisée seule ne suffit pas: Host et jeton restent vérifiés.
    let dir = tempfile::tempdir().unwrap();
    let state = LocalState::new(
        dir.path().join("settings.json"),
        vec![ORIGIN.into()],
        |_, _| {},
    )
    .unwrap();
    for (origin, host) in [
        ("https://evil.example", "localhost:32145"),
        (ORIGIN, "evil.example:32145"),
        ("null", "localhost:32145"),
    ] {
        let response = router(state.clone())
            .oneshot(
                Request::post("/v1/connect")
                    .header("host", host)
                    .header("origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response
            .headers()
            .contains_key("access-control-allow-origin"));
    }
    assert_eq!(
        call(
            state.clone(),
            "/v1/state",
            "POST",
            Some("invalid"),
            Some(json!({"active":true}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let response = router(state)
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/config")
                .header("host", "localhost:32145")
                .header("origin", ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["access-control-allow-origin"], ORIGIN);
}
#[tokio::test]
async fn mouse_bindings_persist_and_the_api_rejects_primary_clicks() {
    // La restriction souris doit être côté API également, une UI modifiée ne
    // peut pas faire accepter MouseLeft/MouseRight au stockage natif.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let state = LocalState::new(path.clone(), vec![ORIGIN.into()], |_, _| {}).unwrap();
    let first = call(state.clone(), "/v1/connect", "POST", None, None)
        .await
        .1;
    assert_eq!(first["mouseButtons"], true);
    let token = first["token"].as_str().unwrap();
    let mut config = json!({"microphone":{"code":"MouseMiddle","modifiers":0,"label":"Clic molette"},"headphones":{"code":"MouseBack","modifiers":0,"label":"Souris 4"},"pushToTalk":{"code":"MouseForward","modifiers":1,"label":"Souris 5"},"pushToTalkEnabled":true});
    for code in ["MouseLeft", "MouseRight", "Mouse6"] {
        config["microphone"]["code"] = json!(code);
        let result = call(
            state.clone(),
            "/v1/config",
            "PUT",
            Some(token),
            Some(json!({"revision":0,"config":config})),
        )
        .await;
        assert!(!result.0.is_success());
        assert!(!path.exists());
    }
    config["microphone"]["code"] = json!("MouseMiddle");
    assert_eq!(
        call(
            state,
            "/v1/config",
            "PUT",
            Some(token),
            Some(json!({"revision":0,"config":config}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let restored = LocalState::new(path, vec![ORIGIN.into()], |_, _| {}).unwrap();
    assert_eq!(
        call(restored, "/v1/connect", "POST", None, None).await.1["config"],
        config
    );
}
#[tokio::test]
async fn configuration_survives_restart_without_an_account_and_rejects_stale_updates() {
    // Recréer LocalState depuis le disque simule un nouveau processus: les
    // préférences/revisions survivent, le jeton et l'état PTT ne survivent pas.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let state = LocalState::new(path.clone(), vec![ORIGIN.into()], |_, _| {}).unwrap();
    let first = call(state.clone(), "/v1/connect", "POST", None, None)
        .await
        .1;
    let token = first["token"].as_str().unwrap();
    let config = json!({"microphone":{"code":"F8","modifiers":0,"label":"F8"},"headphones":null,"pushToTalk":{"code":"KeyV","modifiers":1,"label":"V"},"pushToTalkEnabled":true});
    let updated = call(
        state.clone(),
        "/v1/config",
        "PUT",
        Some(token),
        Some(json!({"revision":0,"config":config})),
    )
    .await;
    assert_eq!(updated.0, StatusCode::OK);
    assert_eq!(updated.1["config"], config);
    assert_eq!(
        call(
            state,
            "/v1/config",
            "PUT",
            Some(token),
            Some(json!({"revision":0,"config":config}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let restored = LocalState::new(path, vec![ORIGIN.into()], |_, _| {}).unwrap();
    let snapshot = call(restored.clone(), "/v1/connect", "POST", None, None)
        .await
        .1;
    assert_eq!(snapshot["config"], config);
    assert_eq!(snapshot["revision"], 1);
    assert_eq!(snapshot["held"], false);
    assert_ne!(snapshot["token"], token);
    assert_eq!(
        call(
            restored,
            "/v1/state",
            "POST",
            Some(token),
            Some(json!({"active":true}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn routes_to_each_active_page_and_releases_on_disconnect_or_config_change() {
    // Le callback remplace l'adaptateur Win32 et enregistre les changements de
    // configuration et d’activation pour vérifier l’indépendance des deux pages.
    let dir = tempfile::tempdir().unwrap();
    let applied = Arc::new(Mutex::new(Vec::new()));
    let copy = applied.clone();
    let state = LocalState::new(
        dir.path().join("settings.json"),
        vec![ORIGIN.into()],
        move |config, active| copy.lock().unwrap().push((config, active)),
    )
    .unwrap();
    let a = call(state.clone(), "/v1/connect", "POST", None, None)
        .await
        .1;
    let a = a["token"].as_str().unwrap().to_string();
    let b = call(state.clone(), "/v1/connect", "POST", None, None)
        .await
        .1;
    let b = b["token"].as_str().unwrap().to_string();
    assert_eq!(
        call(
            state.clone(),
            "/v1/state",
            "POST",
            Some(&a),
            Some(json!({"active":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            state.clone(),
            "/v1/state",
            "POST",
            Some(&b),
            Some(json!({"active":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    state.action(Action::Microphone);
    state.action(Action::PushToTalk(true));
    let snapshot = call(state.clone(), "/v1/events?after=0", "GET", Some(&a), None)
        .await
        .1;
    assert_eq!(snapshot["events"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["held"], true);
    let other = call(state.clone(), "/v1/events?after=0", "GET", Some(&b), None)
        .await
        .1;
    assert_eq!(other["events"], snapshot["events"]);
    assert_eq!(other["held"], true);
    let config = serde_json::to_value(ShortcutConfig::default()).unwrap();
    call(
        state.clone(),
        "/v1/config",
        "PUT",
        Some(&a),
        Some(json!({"revision":0,"config":config})),
    )
    .await;
    assert_eq!(
        call(state.clone(), "/v1/events?after=0", "GET", Some(&a), None)
            .await
            .1["held"],
        false
    );
    call(
        state.clone(),
        "/v1/state",
        "POST",
        Some(&a),
        Some(json!({"active":false})),
    )
    .await;
    assert_eq!(
        call(
            state.clone(),
            "/v1/state",
            "POST",
            Some(&b),
            Some(json!({"active":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(applied.lock().unwrap().iter().any(|(_, active)| *active));
    assert_eq!(
        call(state.clone(), "/v1/client", "DELETE", Some(&b), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(state.clone(), "/v1/events?after=0", "GET", Some(&b), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            state,
            "/v1/state",
            "POST",
            Some(&a),
            Some(json!({"active":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
}
