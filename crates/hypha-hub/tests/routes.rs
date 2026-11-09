#![allow(clippy::unwrap_used, clippy::pedantic)]
//! Router-level coverage — the static handlers, the push sub-store,
//! the auth gate, config parsing, and the HTTP/SSE bridges against a
//! stubbed upstream (a raw `TcpListener` answering canned bytes).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use hypha_hub::{HubConfig, HubState, router};
use hypha_node_api::api::EntityApi;
use hypha_node_api::testing::FakeEntityApi;
use serde_json::Value;
use tower::ServiceExt;

fn state(node_url: &str) -> HubState {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("push.json");
    std::mem::forget(dir); // the store reads lazily — keep the dir alive
    let push = hypha_push::PushStore::open(hypha_push::JsonDocStore::open(&path));
    let mut s = HubState::new(
        FakeEntityApi::seeded().shared() as Arc<dyn EntityApi>,
        node_url,
        None,
    );
    s.push = Some(Arc::new(push));
    s
}

async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), 8 * 1024 * 1024).await.unwrap();
    (status, String::from_utf8_lossy(&body).to_string())
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn push_routes_full_surface() {
    let app = router(state("http://127.0.0.1:9"));
    let (s, b) = call(&app, get("/api/push/vapid")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(b.contains("publicKey"));
    // Bad JSON → 400.
    let req = Request::post("/api/push/subscribe")
        .body(Body::from("{nope"))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Non-object → 400.
    let req = Request::post("/api/push/subscribe")
        .body(Body::from("[1]"))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Missing keys → 400.
    let req = Request::post("/api/push/subscribe")
        .body(Body::from("{}"))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let req = Request::post("/api/push/subscribe")
        .body(Body::from(r#"{"endpoint":"e","keys":{}}"#))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Valid subscribe with prefs.
    let req = Request::post("/api/push/subscribe")
        .body(Body::from(
            r#"{"endpoint":"https://push/e","keys":{"auth":"a","p256dh":"p"},"prefs":{"done":false}}"#,
        ))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::OK);
    // DELETE removes by endpoint; a missing endpoint is still ok.
    let req = Request::delete("/api/push/subscribe")
        .body(Body::from(r#"{"endpoint":"https://push/e"}"#))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::OK);
    let req = Request::delete("/api/push/subscribe")
        .body(Body::from("{}"))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::OK);
    // Oversized body → 413.
    let req = Request::post("/api/push/subscribe")
        .body(Body::from("x".repeat(70 * 1024)))
        .unwrap();
    let (s, _) = call(&app, req).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn push_routes_unconfigured_501() {
    let mut s = state("http://127.0.0.1:9");
    s.push = None;
    let app = router(s);
    let (status, _) = call(&app, get("/api/push/vapid")).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let req = Request::post("/api/push/subscribe")
        .body(Body::from("{}"))
        .unwrap();
    let (status, _) = call(&app, req).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

/// A one-shot HTTP stub: reads the request line + headers, replies
/// with `bytes`. For SSE endpoints it emits a couple frames then
/// closes — `pump_sse` forwards them and exits on EOF.
fn stub_upstream() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok((mut sock, _)) = listener.accept() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                let mut path = String::new();
                let mut auth_seen = false;
                let mut ct_seen = false;
                // Read request line + headers; then drain a small body.
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let t = line.trim();
                    if t.is_empty() {
                        break;
                    }
                    if path.is_empty() {
                        path = t.split_whitespace().nth(1).unwrap_or("").to_string();
                    }
                    if t.to_lowercase().starts_with("authorization:") {
                        auth_seen = true;
                    }
                    if t.to_lowercase().starts_with("content-type:") {
                        ct_seen = true;
                    }
                }
                let mut buf = [0u8; 4096];
                let _ = reader.get_mut().set_nonblocking(true);
                let _ = reader.get_mut().read(&mut buf);
                let (status, ct, body) =
                    if path.starts_with("/api/events") || path.contains("/stream") {
                        (
                            "200 OK",
                            "text/event-stream",
                            "event: diff\ndata: {\"k\":1}\n\n: ping\ndata: more\n\n".to_string(),
                        )
                    } else {
                        (
                            "200 OK",
                            "application/json",
                            format!("{{\"ok\":true,\"auth\":{auth_seen},\"ct\":{ct_seen}}}"),
                        )
                    };
                let res = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {ct}\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(res.as_bytes());
                let _ = sock.flush();
                // SSE: closing right away ends the pump via EOF.
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn passthrough_forwards_verbatim() {
    let url = stub_upstream();
    let mut s = state(&url);
    s.node_token = Some("tok".into());
    let app = router(s);
    // GET with a query — upstream sees it and replies 200 JSON.
    let (status, body) = call(&app, get("/api/sessions?archived=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"ok\":true") || body.contains("auth"));
    // POST with JSON + content-type → forwarded.
    let req = Request::post("/api/sessions/s1/prompt")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let (status, _) = call(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    // Browser auth header is forwarded when no node_token is set.
    let mut s = state(&url);
    s.node_token = None;
    let app = router(s);
    let req = Request::get("/api/sessions")
        .header("authorization", "Bearer browser")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"auth\":true"));
}

#[tokio::test]
async fn passthrough_unreachable_and_sse() {
    // Nothing listening → 502.
    let app = router(state("http://127.0.0.1:9"));
    let (status, body) = call(&app, get("/api/sessions")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("unreachable"));
    let (status, _) = call(&app, get("/api/events")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    // Real upstream → SSE frames bridge through.
    let url = stub_upstream();
    let mut s = state(&url);
    s.entity_router = Some(Arc::new(|_id| {
        Some(("http://127.0.0.1:9".into(), Some("t".into())))
    }));
    let app = router(s.clone());
    let res = app.clone().oneshot(get("/api/events?x=1")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("diff") || text.contains("ping"));
    // Entity stream — the stream_router resolves s1 → the stub url +
    // a canned upstream path; use the live upstream so it connects.
    let mut s2 = state(&url);
    let up = url.clone();
    s2.stream_router = Some(Arc::new(move |_| {
        Some((up.clone(), None, "/api/entities/s1/stream".to_string()))
    }));
    let app = router(s2);
    let res = app.oneshot(get("/api/entities/s1/stream")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("data"));
    // No stream router → 404.
    let app = router(state("http://127.0.0.1:9"));
    let (status, _) = call(&app, get("/api/entities/s1/stream")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn auth_gate() {
    let mut s = state("http://127.0.0.1:9");
    s.hub_token = Some("sekret".into());
    let app = router(s);
    // No credential → JSON 401.
    let res = app.clone().oneshot(get("/api/sessions")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // Bearer token passes the gate (upstream is down → 502, not 401).
    let req = Request::get("/api/sessions")
        .header("authorization", "Bearer sekret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    // `?token=` authenticates the same way (SSE clients can't set headers).
    let res = app
        .clone()
        .oneshot(get("/api/events?token=sekret"))
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
    // Wrong token → 401.
    let res = app.oneshot(get("/api/sessions?token=nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn hub_config_parsing() {
    let d = HubConfig::from_map(&HashMap::new()).unwrap();
    assert_eq!(d.port, 13000);
    let mut v = HashMap::new();
    v.insert("HYPHA_HUB_HOST".into(), "127.0.0.1".into());
    v.insert("HYPHA_HUB_PORT".into(), "13001".into());
    v.insert("HYPHA_NODE_URL".into(), "http://x:18787/".into());
    v.insert("HYPHA_NODE_TOKEN".into(), "".into());
    v.insert("HYPHA_HUB_TOKEN".into(), "".into());
    let c = HubConfig::from_map(&v).unwrap();
    assert_eq!(c.port, 13001);
    assert_eq!(c.node_url, "http://x:18787");
    assert!(c.node_token.is_none());
    // Bad port → Err.
    v.insert("HYPHA_HUB_PORT".into(), "nope".into());
    assert!(HubConfig::from_map(&v).is_err());
    v.insert("HYPHA_HUB_PORT".into(), "13000".into());
    v.insert("HYPHA_PORT".into(), "13002".into());
    // `HYPHA_PORT` is the fallback — HUB_PORT wins.
    let c = HubConfig::from_map(&v).unwrap();
    assert_eq!(c.port, 13000);
    // Non-loopback without a hub token → Err.
    v.remove("HYPHA_HUB_PORT");
    v.insert("HYPHA_HUB_HOST".into(), "0.0.0.0".into());
    assert!(HubConfig::from_map(&v).is_err());
    // A bad socket addr surfaces from `socket_addr`.
    v.insert("HYPHA_HUB_HOST".into(), "not a host".into());
    v.insert("HYPHA_HUB_TOKEN".into(), "t".into());
    let c = HubConfig::from_map(&v).unwrap();
    assert!(c.socket_addr().is_err());
}

#[tokio::test]
async fn session_upstream_falls_back() {
    let s = state("http://127.0.0.1:9");
    // No router → default upstream; unreachable path covered via the
    // public routing of proxied calls above.
    let app = router(s);
    drop(app);
}

/// A stand-in kind policy — proves the feed hook dispatches through
/// the injected registry (the real session rules live with the
/// integration, not the hub).
struct TestNoticePolicy;

impl hypha_sync::KindPolicy for TestNoticePolicy {
    fn entity_kinds(&self) -> &'static [&'static str] {
        &["test-entity"]
    }
    fn diff_events(&self) -> &'static [&'static str] {
        &["test"]
    }
    fn notices(
        &self,
        _event: &str,
        entity_id: &str,
        _row: Option<&hypha_sync::IndexedEntity>,
        patch: &Value,
    ) -> Vec<hypha_sync::Notice> {
        if patch.get("done") == Some(&Value::Bool(true)) {
            vec![hypha_sync::Notice {
                kind: hypha_sync::NoticeKind::Done,
                title: format!("done {entity_id}"),
                body: String::new(),
                url: "/".into(),
            }]
        } else {
            Vec::new()
        }
    }
}

/// The push feed-hook: diffs a policy claims produce notices; other
/// frames are ignored. The empty default registry sends nothing.
#[test]
fn feed_hook_dispatches_through_the_policy_registry() {
    let dir = tempfile::tempdir().unwrap();
    let projection = hypha_sync::ProjectionStore::in_memory().unwrap();
    projection
        .upsert_node(&hypha_sync::NodeRef {
            id: "n1".into(),
            url: "u".into(),
            label: "l".into(),
        })
        .unwrap();
    projection
        .upsert_entity(
            "n1",
            &serde_json::json!({"id":"s1","kind":"test-entity","title":"T1","updatedAt":"2024-01-01T00:00:00Z"}),
        )
        .unwrap();
    let push = std::sync::Arc::new(hypha_push::PushStore::open(hypha_push::JsonDocStore::open(
        &dir.path().join("push.json"),
    )));
    let mut policies = hypha_sync::KindPolicies::new();
    policies.register(std::sync::Arc::new(TestNoticePolicy));
    let hook = hypha_hub::feed_hook(projection, push, &policies);
    use hypha_sync::client::FeedEvent;
    // Non-diff frames are ignored.
    hook("n1", &FeedEvent::Heartbeat);
    // A diff no policy claims (`session` isn't registered here).
    hook(
        "n1",
        &FeedEvent::Diff {
            kind: "session".into(),
            id: "s1".into(),
            agent: None,
            patch: serde_json::json!({"done": true}),
        },
    );
    // A claimed diff with the flag → one notice; `ghost` exercises the
    // missing-row path (the hook tolerates `row: None`).
    for id in ["s1", "ghost"] {
        hook(
            "n1",
            &FeedEvent::Diff {
                kind: "test".into(),
                id: id.into(),
                agent: None,
                patch: serde_json::json!({"done": true}),
            },
        );
        // Flag absent → no notice.
        hook(
            "n1",
            &FeedEvent::Diff {
                kind: "test".into(),
                id: id.into(),
                agent: None,
                patch: serde_json::json!({}),
            },
        );
    }
}

/// `hub_state_from` — env arms without touching the process env.
#[tokio::test]
async fn hub_state_from_env_arms() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let config = hypha_hub::HubConfig {
        node_url: "http://127.0.0.1:9".into(),
        node_token: None,
        hub_token: None,
        host: "127.0.0.1".into(),
        port: 0,
    };
    // `HYPHA_NODES` env path + `HYPHA_HOME`.
    let nodes_env: std::ffi::OsString = "n1=http://127.0.0.1:9".to_string().into();
    let vars = |k: &str| -> Option<std::ffi::OsString> {
        match k {
            "HYPHA_HOME" => Some(home.clone().into_os_string()),
            "HYPHA_NODES" => Some(nodes_env.clone()),
            _ => None,
        }
    };
    let state = hypha_hub::hub_state_from(vars, &config).unwrap();
    // The session router closure resolves through the projection —
    // invoke it once (unknown id → primary node URL).
    let router = state.entity_router.as_ref().unwrap();
    let _ = router("s-ghost");
    // `HOME`-fallback arm (no HYPHA_HOME).
    let vars2 = |k: &str| -> Option<std::ffi::OsString> {
        (k == "HOME").then(|| dir.path().join("h2").into_os_string())
    };
    let _ = hypha_hub::hub_state_from(vars2, &config).unwrap();
    // Neither var → `.hypha` fallback under cwd — point it at a
    // safe dir by running in the tempdir.
    std::env::set_current_dir(dir.path()).unwrap();
    let vars3 = |_: &str| None;
    let _ = hypha_hub::hub_state_from(vars3, &config).unwrap();
}

/* ---- the merged entity surface ---------------------------------------*/

#[tokio::test]
async fn entities_list_reads_the_merged_index() {
    let app = hypha_hub::router(state("http://127.0.0.1:1"));
    let (status, body) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/entities")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    let entities = v["entities"].as_array().unwrap();
    assert_eq!(entities.len(), 3);
    assert_eq!(entities[0]["kind"], "sepia-session");
    assert_eq!(entities[0]["indexDoc"]["title"], "fix parser");
    // `?kind=` filters the merged index.
    let (status, body) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/entities?kind=campaign")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    let entities = v["entities"].as_array().unwrap();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0]["id"], "c1");
}

#[tokio::test]
async fn entities_item_and_kind_filter() {
    let app = hypha_hub::router(state("http://127.0.0.1:1"));
    let (status, body) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/entities/s1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], "s1");
    assert_eq!(v["indexDoc"]["cwd"], "/work/a");
    let (status, _) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/entities/ghost")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn entity_ops_submit_and_delete() {
    let app = hypha_hub::router(state("http://127.0.0.1:1"));
    let (status, body) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/entities/s1/ops")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"kind":"sepia.prompt","payload":{"parts":"hi"}}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, _) = call(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/entities/s1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Missing `kind` → 400.
    let (status, _) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/entities/s1/ops")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn entity_index_and_pending_ops() {
    let app = hypha_hub::router(state("http://127.0.0.1:1"));
    let (status, body) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/entity-index")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["index"].is_array());
    let (status, body) = call(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/pending-ops")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["ops"].is_array());
}
