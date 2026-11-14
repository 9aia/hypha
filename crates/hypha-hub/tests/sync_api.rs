//! `SyncEntityApi` — merged entity index, op submits + outbox, lock
//! arbitration, pairing, node-level verbs. Canned nodes record calls;
//! the projection is seeded in-memory.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use hypha_hub::credentials::{FileCredentials, NodeCredentials};
use hypha_hub::sync_api::{HubNode, SyncEntityApi, parse_nodes, resolve_node_tokens};
use hypha_node_api::api::{EntityApi, OpResult};
use hypha_node_api::dto::EntityOp;
use hypha_sync::{NodeTarget, SyncEngine, SyncHandle, SyncOptions};
use serde_json::{Value, json};

fn file_creds() -> (tempfile::TempDir, Arc<dyn NodeCredentials>) {
    let dir = tempfile::tempdir().unwrap();
    let creds = Arc::new(FileCredentials::new(dir.path().join("credentials.json")));
    (dir, creds)
}

fn engine_with(
    nodes: &[HubNode],
) -> (
    SyncHandle,
    hypha_sync::ProjectionStore,
    hypha_outbox::Outbox,
) {
    engine_with_options(nodes, SyncOptions::default())
}

fn engine_with_options(
    nodes: &[HubNode],
    options: SyncOptions,
) -> (
    SyncHandle,
    hypha_sync::ProjectionStore,
    hypha_outbox::Outbox,
) {
    let projection = hypha_sync::ProjectionStore::in_memory().unwrap();
    let outbox = hypha_outbox::Outbox::in_memory().unwrap();
    let targets = nodes.iter().map(|n| {
        NodeTarget::new(hypha_sync::NodeRef {
            id: n.id.clone(),
            url: n.url.clone(),
            label: n.id.clone(),
        })
    });
    (
        SyncEngine::spawn(projection.clone(), outbox.clone(), targets, options),
        projection,
        outbox,
    )
}

fn hub_node(id: &str, url: &str) -> HubNode {
    HubNode {
        id: id.into(),
        url: url.into(),
        token: None,
    }
}

/// A stand-in integration policy — the fixture kinds (`sepia-session`,
/// `campaign`) and `sepia.*` op rules mirror what an integration would
/// register in P4; the hub itself ships none.
struct TestPolicy;

impl hypha_sync::KindPolicy for TestPolicy {
    fn entity_kinds(&self) -> &'static [&'static str] {
        &["sepia-session", "campaign"]
    }
    fn owns_op(&self, op: &str) -> bool {
        op.starts_with("sepia.")
    }
    fn op_gated(&self, op: &str) -> bool {
        op.starts_with("sepia.") && !op.starts_with("sepia.meta")
    }
    fn op_kind(&self, op: &str) -> hypha_outbox::OpKind {
        if matches!(op, "sepia.meta" | "sepia.meta.patch") {
            hypha_outbox::OpKind::Metadata
        } else {
            hypha_outbox::OpKind::Turn
        }
    }
    fn external_actor_id(&self, row: &hypha_sync::IndexedEntity) -> Option<String> {
        let doc = row.raw.get("indexDoc").unwrap_or(&row.raw);
        doc.get("lockHolderPid")
            .and_then(Value::as_f64)
            .map(|pid| format!("pid:{pid}"))
    }
    fn create_kind(&self, requested: &str) -> Option<&'static str> {
        match requested {
            "sepia-session" | "session" => Some("sepia-session"),
            "campaign" => Some("campaign"),
            _ => None,
        }
    }
}

fn test_policies() -> hypha_sync::KindPolicies {
    let mut policies = hypha_sync::KindPolicies::new();
    policies.register(std::sync::Arc::new(TestPolicy));
    policies
}

/// One `sepia-session` entity doc in the projection.
fn seed_entity(projection: &hypha_sync::ProjectionStore, node: &str, id: &str, title: &str) {
    projection
        .upsert_entity(
            node,
            &json!({
                "id": id, "kind": "sepia-session",
                "indexDoc": {"id": id, "title": title, "cwd": "/w", "agent": "devin"},
                "updatedAt": "2024-01-01T00:00:00Z",
            }),
        )
        .unwrap();
}

/* ---- full-surface mock node ------------------------------------------*/

/// A canned-response node: records `"METHOD path"` + bodies; `fail`
/// maps path prefixes to a 500, `blank` maps them to `200 {}`.
#[derive(Clone, Default)]
struct Canned {
    calls: Arc<std::sync::Mutex<Vec<String>>>,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
    fail: Arc<std::sync::Mutex<Vec<String>>>,
    blank: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Canned {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }
}

async fn canned(
    axum::extract::State(m): axum::extract::State<Canned>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    m.calls.lock().unwrap().push(format!("{method} {path}"));
    let bytes = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let body_text = String::from_utf8_lossy(&bytes).into_owned();
    if !body_text.is_empty() {
        m.bodies
            .lock()
            .unwrap()
            .push(format!("{method} {path} {body_text}"));
    }
    let key = format!("{method} {path}");
    if m.fail
        .lock()
        .unwrap()
        .iter()
        .any(|p| key.starts_with(p.as_str()) || path.starts_with(p.as_str()))
    {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"error":"node blew up"})),
        )
            .into_response();
    }
    if m.blank
        .lock()
        .unwrap()
        .iter()
        .any(|p| path.starts_with(p.as_str()))
    {
        return axum::Json(json!({})).into_response();
    }
    let body = match (method.as_str(), path.as_str()) {
        ("GET", "/api/node") => json!({
            "id":"n1","name":"node-one","version":"0.1","protocol":1,
            "capabilities":["import"],
        }),
        ("PATCH", "/api/node") => json!({
            "id":"n1","name":"renamed","version":"0.1","protocol":1,
        }),
        ("GET", "/api/config") => json!({"config":{"theme":"dark","k":1}}),
        ("GET", "/api/fs") => json!({"dirs":["/a","/b"]}),
        ("POST", "/api/entities") => json!({"id":"new-1","capabilities":{}}),
        ("GET", "/api/push/vapid") => json!({"publicKey":"pk-1"}),
        ("POST", "/api/pair") => json!({"token":"sepia_minted"}),
        ("DELETE", _) => json!({}),
        (_, "/api/sessions/import") => {
            json!({"id":"i1","kind":"sepia-session","agent":"devin"})
        }
        // Op responses are integration-shaped — `ci.claim`-style
        // bodies must reach `submit_op_result` verbatim.
        ("POST", _) if path.ends_with("/ops") => {
            json!({"claimed":true,"fence":7,"job":{"id":"j1"}})
        }
        _ => json!({}),
    };
    axum::Json(body).into_response()
}

/// A node that stays up: `/api/events` is an open (never-ending) SSE
/// stream and `GET /api/entities` lists `s1`, so the sync loop's
/// connect cycle completes and marks the node `Up`. Everything else
/// falls through to the canned responder. Returns the recorder so
/// callers can assert on the wire calls.
async fn serve_live_node() -> (Canned, String) {
    serve_live_canned(Canned::default()).await
}

async fn serve_live_canned(m: Canned) -> (Canned, String) {
    async fn events() -> axum::response::Response {
        use axum::response::IntoResponse;
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            axum::body::Body::from_stream(futures::stream::pending::<
                Result<axum::body::Bytes, std::io::Error>,
            >()),
        )
            .into_response()
    }
    async fn entities() -> axum::Json<Value> {
        axum::Json(json!({
            "entities": [{
                "id": "s1", "kind": "sepia-session",
                "indexDoc": {"id": "s1", "title": "Alpha", "cwd": "/w", "agent": "devin"},
                "updatedAt": "2024-01-01T00:00:00Z",
            }],
        }))
    }
    // GET lists the fixture entity (keeps the sync loop's connect
    // cycle alive → node Up); POST falls through to the canned
    // responder so create assertions still record the wire call.
    let app = axum::Router::new()
        .route("/api/events", axum::routing::get(events))
        .route("/api/entities", axum::routing::get(entities).post(canned))
        .route("/api/{*rest}", axum::routing::any(canned))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (m, format!("http://{addr}"))
}

async fn serve_canned(m: Canned) -> (Canned, String) {
    let app = axum::Router::new()
        .route("/api/{*rest}", axum::routing::any(canned))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (m, format!("http://{addr}"))
}

fn api_against(nodes: Vec<HubNode>) -> (SyncEntityApi, hypha_sync::ProjectionStore) {
    let (engine, projection, _outbox) = engine_with(&nodes);
    let (_dir, creds) = file_creds();
    (
        SyncEntityApi::new(engine, nodes, creds).with_policies(test_policies()),
        projection,
    )
}

/* ---- registration / tokens -------------------------------------------*/

#[test]
fn parses_node_lists() {
    let nodes = parse_nodes("tower=http://a:1@tok; edge = http://b:2/ ");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].id, "tower");
    assert_eq!(nodes[0].url, "http://a:1");
    assert_eq!(nodes[0].token.as_deref(), Some("tok"));
    assert_eq!(nodes[1].id, "edge");
    assert_eq!(nodes[1].url, "http://b:2");
    assert!(nodes[1].token.is_none());
    assert!(parse_nodes("").is_empty());
    assert!(parse_nodes("garbage").is_empty());
}

#[test]
fn resolve_node_tokens_resolves_seeds_and_never_clobbers() {
    struct FakeCreds {
        map: std::sync::Mutex<BTreeMap<String, String>>,
    }
    impl NodeCredentials for FakeCreds {
        fn get(&self, id: &str) -> Result<Option<String>, String> {
            Ok(self.map.lock().unwrap().get(id).cloned())
        }
        fn set(&self, id: &str, t: &str) -> Result<(), String> {
            self.map.lock().unwrap().insert(id.into(), t.into());
            Ok(())
        }
        fn delete(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
        fn backend(&self) -> &'static str {
            "fake"
        }
    }
    let creds = FakeCreds {
        map: std::sync::Mutex::new(BTreeMap::from([("stored".into(), "cred-tok".into())])),
    };
    let mut nodes = parse_nodes("stored=http://a; env=http://b@env-tok; none=http://c");
    let (resolved, seeded) = resolve_node_tokens(&mut nodes, &creds);
    assert_eq!((resolved, seeded), (1, 1));
    assert_eq!(nodes[0].token.as_deref(), Some("cred-tok"));
    assert_eq!(nodes[1].token.as_deref(), Some("env-tok"));
    assert!(nodes[2].token.is_none());
    assert_eq!(creds.get("env").unwrap(), Some("env-tok".into()));
}

/* ---- the entity surface ----------------------------------------------*/

#[tokio::test]
async fn list_reads_the_projection_across_nodes() {
    let (api, projection) = api_against(vec![
        hub_node("n1", "http://127.0.0.1:1"),
        hub_node("n2", "http://127.0.0.1:2"),
    ]);
    seed_entity(&projection, "n1", "s1", "Alpha");
    seed_entity(&projection, "n2", "s2", "Beta");
    projection
        .upsert_entity(
            "n2",
            &json!({"id":"c1","kind":"campaign","indexDoc":{"title":"sweep"},"updatedAt":"2024-01-01T00:00:00Z"}),
        )
        .unwrap();
    let all = api.list_entities(None).await.unwrap();
    assert_eq!(all.len(), 3);
    let sessions = api.list_entities(Some("sepia-session")).await.unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|e| e.kind == "sepia-session"));
    let titles: Vec<&str> = sessions
        .iter()
        .map(|e| e.index_doc["title"].as_str().unwrap_or_default())
        .collect();
    assert!(
        titles.contains(&"Alpha") && titles.contains(&"Beta"),
        "{titles:?}"
    );
    let campaigns = api.list_entities(Some("campaign")).await.unwrap();
    assert_eq!(campaigns.len(), 1);
}

#[tokio::test]
async fn entity_resolves_across_nodes() {
    let (api, projection) = api_against(vec![
        hub_node("n1", "http://127.0.0.1:1"),
        hub_node("n2", "http://127.0.0.1:2"),
    ]);
    seed_entity(&projection, "n2", "s1", "Beta");
    let e = api.entity("s1").await.unwrap();
    assert_eq!(e.node.as_deref(), Some("n2"));
    assert_eq!(e.index_doc["title"], "Beta");
    assert!(
        api.entity("ghost")
            .await
            .unwrap_err()
            .contains("Entity not found")
    );
}

#[tokio::test]
async fn entity_index_groups_by_node() {
    let (api, projection) = api_against(vec![hub_node("n1", "http://127.0.0.1:1")]);
    projection
        .upsert_node(&hypha_sync::NodeRef {
            id: "n1".into(),
            url: "http://127.0.0.1:1".into(),
            label: "n1".into(),
        })
        .unwrap();
    seed_entity(&projection, "n1", "s1", "Alpha");
    let index = api.entity_index().await.unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].node, "n1");
    assert_eq!(index[0].entities.len(), 1);
}

#[tokio::test]
async fn submit_op_goes_direct_when_up_queue_when_down() {
    let (mock, url) = serve_canned(Canned::default()).await;
    let (api, projection) = api_against(vec![hub_node("n1", &url)]);
    seed_entity(&projection, "n1", "s1", "Alpha");
    // Node is registered but has never synced → status "unknown"/down →
    // a `sepia.prompt` op queues instead of posting.
    let result = api
        .submit_op_result(
            "s1",
            &EntityOp {
                kind: "sepia.prompt".into(),
                payload: json!({"text":"hi"}),
            },
        )
        .await
        .unwrap();
    // Queued writes carry no node response yet.
    assert_eq!(result, OpResult::Queued);
    let pending = api.pending_ops().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, "sepia.prompt");
    assert_eq!(pending[0].entity_id, "s1");
    assert_eq!(pending[0].status, "queued");
    assert!(
        mock.calls().iter().all(|c| !c.contains("prompt")),
        "queued op must not post — calls: {:?}",
        mock.calls()
    );
}

#[tokio::test]
async fn submit_op_result_returns_the_node_body_when_up() {
    let (_mock, url) = serve_live_node().await;
    let (api, projection) = api_against(vec![hub_node("n1", &url)]);
    seed_entity(&projection, "n1", "s1", "Alpha");
    // Wait for the sync loop's connect cycle to mark the node up.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let up = projection
            .node("n1")
            .unwrap()
            .is_some_and(|row| row.status == hypha_sync::NodeStatus::Up);
        if up {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "node never came up");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let result = api
        .submit_op_result(
            "s1",
            &EntityOp {
                kind: "sepia.prompt".into(),
                payload: json!({"text":"hi"}),
            },
        )
        .await
        .unwrap();
    // The node's op response body reaches the caller verbatim.
    assert_eq!(
        result,
        OpResult::Applied(Some(json!({
            "claimed": true, "fence": 7, "job": {"id": "j1"}
        })))
    );
}

#[tokio::test]
async fn gated_op_rejects_a_foreign_hold() {
    let (api, projection) = api_against(vec![hub_node("n1", "http://127.0.0.1:1")]);
    // A locked entity — held by someone else.
    projection
        .upsert_entity(
            "n1",
            &json!({
                "id":"s1","kind":"sepia-session","locked":true,
                "indexDoc":{"id":"s1","title":"Held","locked":true,"lockHolderPid":4321},
                "updatedAt":"2024-01-01T00:00:00Z",
            }),
        )
        .unwrap();
    let err = api
        .submit_op(
            "s1",
            &EntityOp {
                kind: "sepia.prompt".into(),
                payload: json!({"text":"hi"}),
            },
        )
        .await
        .unwrap_err();
    assert!(err.contains("held by pid:4321"), "{err}");
    // Meta ops aren't gated — the overlay is last-writer-wins anyway.
    api.submit_op(
        "s1",
        &EntityOp {
            kind: "sepia.meta.patch".into(),
            payload: json!({"pinned":true}),
        },
    )
    .await
    .unwrap();
    let pending = api.pending_ops().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, "sepia.meta.patch");
}

#[tokio::test]
async fn delete_entity_submits_the_delete_op() {
    let (api, projection) = api_against(vec![hub_node("n1", "http://127.0.0.1:1")]);
    seed_entity(&projection, "n1", "s1", "Alpha");
    api.delete_entity("s1").await.unwrap();
    let pending = api.pending_ops().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, "entity.delete");
}

/// Poll the projection until the sync loop marks `n1` up — the
/// create fast path only posts direct once reachability is proven.
async fn wait_node_up(projection: &hypha_sync::ProjectionStore) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let up = projection
            .node("n1")
            .unwrap()
            .is_some_and(|row| row.status == hypha_sync::NodeStatus::Up);
        if up {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "node never came up");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn create_entity_posts_to_the_scoped_node() {
    let (mock, url) = serve_live_node().await;
    let (api, projection) = api_against(vec![hub_node("n1", &url)]);
    wait_node_up(&projection).await;
    let res = api
        .create_entity("sepia-session", &json!({"cwd":"/work"}), Some("n1"))
        .await
        .unwrap();
    assert_eq!(res.id, "new-1");
    assert!(!res.queued);
    assert!(
        mock.bodies()
            .iter()
            .any(|b| b.starts_with("POST /api/entities") && b.contains("/work")),
        "bodies: {:?}",
        mock.bodies()
    );
    // An unregistered kind errors without posting.
    assert!(
        api.create_entity("k8s-pod", &json!({}), Some("n1"))
            .await
            .unwrap_err()
            .contains("not registered")
    );
}

#[tokio::test]
async fn create_entity_queues_durably_when_the_node_is_down() {
    // A canned node that never completes a sync cycle stays
    // unregistered-down — the create must land in the outbox instead
    // of erroring (the T-054 durable-enqueue path: webhook pushes
    // survive node downtime and replay as `entity.create` ops).
    let (mock, url) = serve_canned(Canned::default()).await;
    let (api, _projection) = api_against(vec![hub_node("n1", &url)]);
    let res = api
        .create_entity("sepia-session", &json!({"cwd":"/work"}), Some("n1"))
        .await
        .unwrap();
    assert!(res.queued);
    assert_eq!(res.id, "");
    // Nothing posted to the node — the write is parked, not dropped.
    assert!(
        mock.calls().iter().all(|c| !c.starts_with("POST ")),
        "calls: {:?}",
        mock.calls()
    );
    let pending = api.pending_ops().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, "entity.create");
    assert_eq!(pending[0].entity_id, "create:sepia-session");
    assert_eq!(pending[0].status, "queued");
    // A retried identical create dedups on the content-keyed
    // idempotency key — no double enqueue.
    let again = api
        .create_entity("sepia-session", &json!({"cwd":"/work"}), Some("n1"))
        .await
        .unwrap();
    assert!(again.queued);
    assert_eq!(api.pending_ops().await.unwrap().len(), 1);
    // A different init is a different write.
    api.create_entity("sepia-session", &json!({"cwd":"/other"}), Some("n1"))
        .await
        .unwrap();
    assert_eq!(api.pending_ops().await.unwrap().len(), 2);
}

/// T-060 — a node that stops answering while the projection still
/// reads `up` (the sync loop hasn't observed the failure yet) must not
/// hold the create for the full REST budget: the direct fast path is
/// bounded by the engine's `submit_timeout`, then the create lands in
/// the durable outbox as `queued` — the ci-ingress webhook path.
#[tokio::test]
async fn create_entity_fails_fast_into_outbox_when_node_hangs() {
    // Blackhole endpoint: accepts TCP, never responds.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok((conn, _)) = listener.accept() {
            std::mem::forget(conn);
        }
    });
    let url = format!("http://127.0.0.1:{port}");
    let nodes = vec![hub_node("n1", &url)];
    // `submit_timeout` bounds the direct-delivery probe; the REST
    // budget stays comfortably past the test's own runtime so the
    // node loop's probes can't mark the node down first — the window
    // under test is "status up, node dead".
    let options = SyncOptions {
        submit_timeout: std::time::Duration::from_millis(200),
        request_timeout: std::time::Duration::from_secs(5),
        ..SyncOptions::default()
    };
    let (engine, projection, _outbox) = engine_with_options(&nodes, options);
    let (_dir, creds) = file_creds();
    let api = SyncEntityApi::new(engine, nodes, creds).with_policies(test_policies());
    // The last connect succeeded; the node has since stopped
    // answering but nothing has marked it down yet. The row is
    // seeded directly — the supervisor's `upsert_node` is async and
    // may not have landed yet.
    projection
        .upsert_node(&hypha_sync::NodeRef {
            id: "n1".into(),
            url: url.clone(),
            label: "n1".into(),
        })
        .unwrap();
    projection
        .set_node_status("n1", hypha_sync::NodeStatus::Up)
        .unwrap();

    let start = std::time::Instant::now();
    let res = api
        .create_entity("sepia-session", &json!({"cwd":"/work"}), Some("n1"))
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert!(res.queued, "hung node must queue the create");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "create blocked {elapsed:?} — the submit_timeout bound didn't hold"
    );
    assert_eq!(
        projection.node("n1").unwrap().unwrap().status,
        hypha_sync::NodeStatus::Down,
        "the lapsed probe must mark the node down"
    );
    let pending = api.pending_ops().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, "entity.create");

    // Known-down now: the next create skips the probe outright.
    let start = std::time::Instant::now();
    let again = api
        .create_entity("sepia-session", &json!({"cwd":"/other"}), Some("n1"))
        .await
        .unwrap();
    assert!(again.queued);
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
}

/* ---- node-level verbs -------------------------------------------------*/

#[tokio::test]
async fn node_verbs_sweep() {
    let (_mock, url) = serve_canned(Canned::default()).await;
    let (api, _projection) = api_against(vec![hub_node("n1", &url)]);
    let info = api.node_info().await.unwrap();
    assert_eq!(info.id, "n1");
    let renamed = api.rename_node("renamed", Some("n1")).await.unwrap();
    assert_eq!(renamed.name, "renamed");
    let config = api.get_config().await.unwrap();
    assert_eq!(config["theme"], "dark");
    api.set_config("theme", &json!("light")).await.unwrap();
    let dirs = api.fs_dirs("/", Some("n1".into())).await.unwrap();
    assert_eq!(dirs, vec!["/a", "/b"]);
    assert_eq!(api.push_vapid().await.unwrap(), "pk-1");
    let status = api.node_status().await.unwrap();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].id, "n1");
    // Unconfigured: the row exists with status unknown — the node was
    // registered but the (faked) sync loop hasn't marked it.
    let _ = api
        .push_subscribe(&hypha_node_api::dto::PushSubscriptionDto {
            endpoint: "https://push/e".into(),
            keys: hypha_node_api::dto::PushKeysDto {
                auth: "a".into(),
                p256dh: "p".into(),
            },
            prefs: None,
        })
        .await;
    api.push_unsubscribe("https://push/e").await.unwrap();
}

#[tokio::test]
async fn pair_persists_the_minted_token() {
    let (_mock, url) = serve_canned(Canned::default()).await;
    let (api, _projection) = api_against(vec![hub_node("n1", &url)]);
    let token = api.pair("code-1", Some("n1")).await.unwrap();
    assert_eq!(token, "sepia_minted");
}

#[tokio::test]
async fn api_error_arms() {
    // The create arm needs the node marked up — a down node queues
    // the create instead of erroring (the durable path).
    let canned_mock = Canned::default();
    canned_mock
        .blank
        .lock()
        .unwrap()
        .push("/api/entities".into());
    let (_mock, url) = serve_live_canned(canned_mock).await;
    let (api, projection) = api_against(vec![hub_node("n1", &url)]);
    wait_node_up(&projection).await;
    // `create` returns `{}` → no `id` → the malformed arm.
    assert!(
        api.create_entity("sepia-session", &json!({}), Some("n1"))
            .await
            .unwrap_err()
            .contains("no entity id")
    );
    // `entity` on a missing row → the "not found" arm.
    assert!(
        api.entity("ghost")
            .await
            .unwrap_err()
            .contains("Entity not found")
    );
    // `snapshot_import` on a missing id → the same arm.
    assert!(
        api.snapshot_import(&json!({}), Some("ghost"))
            .await
            .unwrap_err()
            .contains("not registered")
    );
}

/* ---- boot / state -----------------------------------------------------*/

#[tokio::test]
async fn hub_state_from_boot_arms() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    // A read-only projection.db → `purge_unregistered` fails → warn.
    let hub_home = home.join("hub");
    std::fs::create_dir_all(&hub_home).unwrap();
    let db = hub_home.join("projection.db");
    let _ = hypha_sync::ProjectionStore::open(&db).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let home_s = home.to_string_lossy().into_owned();
    let cfg = hypha_hub::HubConfig::default();
    let _ = hypha_hub::hub_state_from(
        |k| (k == "HYPHA_HOME").then(|| std::ffi::OsString::from(home_s.clone())),
        &cfg,
    );
}

/* ---- node lifecycle ----------------------------------------------------*/

#[tokio::test]
async fn node_status_and_entity_rows_carry_liveness() {
    let (api, projection) = api_against(vec![hub_node("n1", "http://127.0.0.1:1")]);
    projection
        .upsert_node(&hypha_sync::NodeRef {
            id: "n1".into(),
            url: "http://127.0.0.1:1".into(),
            label: "n1".into(),
        })
        .unwrap();
    projection
        .set_node_status("n1", hypha_sync::NodeStatus::Down)
        .unwrap();
    projection.set_node_ephemeral("n1", true).unwrap();
    seed_entity(&projection, "n1", "s1", "Alpha");

    let status = api.node_status().await.unwrap();
    let row = status.iter().find(|n| n.id == "n1").unwrap();
    assert_eq!(row.status, "down");
    assert!(row.ephemeral);
    assert!(row.down_since.is_some());
    assert!(row.last_seen_at.is_none());

    // The merged entity list marks the dead node's entries inline.
    let entities = api.list_entities(None).await.unwrap();
    let e = entities.iter().find(|e| e.id == "s1").unwrap();
    assert_eq!(e.node.as_deref(), Some("n1"));
    assert_eq!(e.node_status.as_deref(), Some("down"));
    let single = api.entity("s1").await.unwrap();
    assert_eq!(single.node_status.as_deref(), Some("down"));
}
