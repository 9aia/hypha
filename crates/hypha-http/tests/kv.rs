#![allow(clippy::unwrap_used)]
//! `/api/kv*` + `/api/client/keypair` route tests — CRUD, namespace
//! visibility, feed emission, and the kv hook (P-006).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use hypha_http::{HyphaState, KvWrite, app};
use hypha_kv::{KvOp, KvStore, Visibility};
use hypha_kv_file::JsonKvStore;
use serde_json::{Value, json};
use tower::ServiceExt;

fn kv_state() -> (HyphaState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonKvStore::open(&dir.path().join("kv.json"));
    let state = HyphaState::new().with_kv(Some(Arc::new(store)));
    (state, dir)
}

fn req(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder().method(method).uri(uri);
    match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn call(state: &HyphaState, req: Request<Body>) -> (StatusCode, Value) {
    let res = app(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn kv_routes_501_without_store() {
    let state = HyphaState::new();
    let (status, _) = call(&state, req(Method::GET, "/api/kv", None)).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let (status, _) = call(
        &state,
        req(
            Method::PUT,
            "/api/kv/config/theme",
            Some(json!({"value": "dark"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn kv_crud_roundtrip() {
    let (state, _dir) = kv_state();

    // Write + read one entry.
    let (status, body) = call(
        &state,
        req(
            Method::PUT,
            "/api/kv/config/theme",
            Some(json!({ "value": "dark" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], "dark");

    let (status, body) = call(&state, req(Method::GET, "/api/kv/config/theme", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], "dark");

    // PATCH is the same write path.
    let (status, _) = call(
        &state,
        req(
            Method::PATCH,
            "/api/kv/config/scale",
            Some(json!({ "value": 2 })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Namespace list + missing-key 404.
    let (status, body) = call(&state, req(Method::GET, "/api/kv/config", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entries"]["theme"], "dark");
    assert_eq!(body["entries"]["scale"], 2);
    let (status, _) = call(&state, req(Method::GET, "/api/kv/config/nope", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Namespace directory lists only public namespaces.
    let (status, body) = call(&state, req(Method::GET, "/api/kv", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["namespaces"][0]["name"], "config");

    // Delete is idempotent-ok.
    let (status, _) = call(&state, req(Method::DELETE, "/api/kv/config/theme", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&state, req(Method::GET, "/api/kv/config/theme", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Non-object write bodies are rejected; empty body stores null.
    let (status, _) = call(
        &state,
        req(Method::PUT, "/api/kv/config/bad", Some(json!("raw"))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = call(&state, req(Method::PUT, "/api/kv/config/empty", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], Value::Null);

    // Wrong methods 404 (the `any()` + guard convention).
    let (status, _) = call(&state, req(Method::POST, "/api/kv", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&state, req(Method::DELETE, "/api/kv/config", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn kv_internal_namespaces_are_wire_hidden() {
    let (state, _dir) = kv_state();
    let store = state.kv.clone().unwrap();
    store.declare("push", Visibility::Internal);

    // Reads hide the namespace entirely.
    let (status, _) = call(&state, req(Method::GET, "/api/kv/push", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&state, req(Method::GET, "/api/kv/push/vapid", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Writes are rejected loudly.
    let (status, body) = call(
        &state,
        req(
            Method::PUT,
            "/api/kv/push/vapid",
            Some(json!({ "value": {} })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "Namespace is internal");
    let (status, _) = call(&state, req(Method::DELETE, "/api/kv/push/vapid", None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The namespace directory doesn't name it.
    store.set("config", "k", Value::from(1));
    let (status, body) = call(&state, req(Method::GET, "/api/kv", None)).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert_eq!(names, vec!["config"]);

    // ...but the store API still reads it (visibility is a wire gate).
    store.set("push", "vapid", json!({ "pub": "x" }));
    assert!(store.get("push", "vapid").is_some());
}

#[tokio::test]
async fn kv_writes_emit_kv_feed_events_by_default() {
    let (state, _dir) = kv_state();
    let mut rx = state.feed.subscribe();

    let _ = call(
        &state,
        req(
            Method::PUT,
            "/api/kv/project/p1",
            Some(json!({ "value": { "name": "Web" } })),
        ),
    )
    .await;
    let event = rx.try_recv().unwrap();
    assert_eq!(event.kind, "kv");
    assert_eq!(event.payload["ns"], "project");
    assert_eq!(event.payload["key"], "p1");
    assert_eq!(event.payload["value"]["name"], "Web");

    let _ = call(&state, req(Method::DELETE, "/api/kv/project/p1", None)).await;
    let event = rx.try_recv().unwrap();
    assert_eq!(event.kind, "kv");
    assert_eq!(event.payload["deleted"], true);
}

#[tokio::test]
async fn kv_hook_overrides_default_emission() {
    let (state, _dir) = kv_state();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let seen_hook = Arc::clone(&seen);
    let state = state.with_kv_hook(Some(Arc::new(
        move |write: &KvWrite, feed: &hypha_http::EventFeed| {
            // Integration-style mapping: project ns → "project" kind,
            // everything else suppressed (config writes emit nothing).
            seen_hook
                .lock()
                .unwrap()
                .push((write.ns.clone(), write.key.clone()));
            if write.ns == "project" {
                let patch = match &write.op {
                    KvOp::Set { value } => json!({ "value": value }),
                    KvOp::Delete => json!({ "deleted": true }),
                };
                feed.emit("project", json!({ "id": write.key, "patch": patch }));
            }
        },
    )));
    let mut rx = state.feed.subscribe();

    let _ = call(
        &state,
        req(
            Method::PUT,
            "/api/kv/project/p1",
            Some(json!({ "value": { "name": "Web" } })),
        ),
    )
    .await;
    let _ = call(
        &state,
        req(
            Method::PUT,
            "/api/kv/config/theme",
            Some(json!({ "value": "dark" })),
        ),
    )
    .await;

    let event = rx.try_recv().unwrap();
    assert_eq!(event.kind, "project");
    assert_eq!(event.payload["id"], "p1");
    // Config write produced no second event.
    assert!(rx.try_recv().is_err());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("project".to_string(), "p1".to_string()),
            ("config".to_string(), "theme".to_string())
        ]
    );
}

#[tokio::test]
async fn client_keypair_mints_ed25519() {
    let (state, _dir) = kv_state();
    // GET → 404 (POST only).
    let (status, _) = call(&state, req(Method::GET, "/api/client/keypair", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = call(&state, req(Method::POST, "/api/client/keypair", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["algorithm"], "Ed25519");
    // base64url-no-pad of 32 bytes → 43 chars.
    assert_eq!(body["publicKey"].as_str().unwrap().len(), 43);
    assert_eq!(body["secretKey"].as_str().unwrap().len(), 43);

    // Two mints differ (OS-CSPRNG seed).
    let (_, body2) = call(&state, req(Method::POST, "/api/client/keypair", None)).await;
    assert_ne!(body["publicKey"], body2["publicKey"]);
}

#[tokio::test]
async fn kv_persists_across_store_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kv.json");
    {
        let store = JsonKvStore::open(&path);
        store.set("config", "theme", json!("dark"));
        store.declare("push", Visibility::Internal);
    }
    // A fresh state over the same file sees the writes.
    let state = HyphaState::new().with_kv(Some(Arc::new(JsonKvStore::open(&path))));
    let (status, body) = call(&state, req(Method::GET, "/api/kv/config/theme", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["value"], "dark");
    let (status, _) = call(&state, req(Method::GET, "/api/kv/push/vapid", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn kv_op_debug_shape() {
    // Keeps the KvOp import honest in non-route tests.
    let op = KvOp::Delete;
    assert!(matches!(op, KvOp::Delete));
}
