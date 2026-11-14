#![allow(clippy::unwrap_used, clippy::pedantic)]

//! Pure projection-store tests — no node harness required. The
//! node-backed engine tests live in peregrin until the P4 re-home.

use hypha_sync::kind::{DiffTombstone, KindPolicy};
use hypha_sync::{KindPolicies, NodeRef, NodeStatus, OpKind, PatchStatus, ProjectionStore};
use serde_json::{Value, json};

/// The `sepia-session` kind policy, replicated for tests — the real
/// registration lives at the hub/node composition roots. Mirrors the
/// wire contract the tests exercise: `session`/`meta` diffs, the
/// `?agent=` scope param, the session stream path, and `sepia.*` op
/// gating.
struct TestSepiaPolicy;

impl KindPolicy for TestSepiaPolicy {
    fn entity_kinds(&self) -> &'static [&'static str] {
        &["sepia-session"]
    }
    fn diff_events(&self) -> &'static [&'static str] {
        &["session", "meta"]
    }
    fn tombstone(&self, event: &str) -> DiffTombstone {
        match event {
            // `meta` tombstone = overlay removed — the entity survives.
            "meta" => DiffTombstone::Refetch,
            _ => DiffTombstone::DeleteEntity,
        }
    }
    fn owns_op(&self, op: &str) -> bool {
        op.starts_with("sepia.")
    }
    fn op_gated(&self, op: &str) -> bool {
        op.starts_with("sepia.") && !op.starts_with("sepia.meta")
    }
    fn op_kind(&self, op: &str) -> OpKind {
        if op.starts_with("sepia.meta") {
            OpKind::Metadata
        } else {
            OpKind::Turn
        }
    }
    fn call_params(&self, scope: Option<&str>) -> Vec<(String, String)> {
        // The interim disambiguator — the caller's kind hint as `agent`.
        scope
            .map(|k| vec![("agent".to_string(), k.to_string())])
            .unwrap_or_default()
    }
    fn stream_path(&self, id: &str) -> Option<String> {
        Some(format!("/api/sessions/{id}/stream"))
    }
    fn columns(&self, raw: &Value) -> Option<hypha_sync::IndexColumns> {
        // `busy` reads as `live`, mirroring the session wire.
        let doc = raw.get("indexDoc").filter(|d| d.is_object()).unwrap_or(raw);
        let flag = |k: &str| doc.get(k).and_then(Value::as_bool).unwrap_or(false);
        Some(hypha_sync::IndexColumns {
            live: Some(flag("live") || flag("busy")),
            ..hypha_sync::IndexColumns::default()
        })
    }
}

fn test_policies() -> KindPolicies {
    let mut policies = KindPolicies::new();
    policies.register(std::sync::Arc::new(TestSepiaPolicy));
    policies
}

/// Wrap a session wire summary as an entity doc — mirrors
/// `NodeClient`'s `entity_doc_of` (INTERIM until `/api/entities`).
fn ent(summary: &Value) -> Value {
    json!({
        "id": summary["id"],
        "kind": "sepia-session",
        "indexDoc": summary,
        "updatedAt": summary["updatedAt"],
        "locked": summary["locked"],
        "live": summary["busy"],
    })
}

/* ---- projection store (pure) -----------------------------------------*/

#[test]
fn projection_upserts_patches_and_reconciles() {
    let store = ProjectionStore::in_memory()
        .unwrap()
        .with_kind_policies(test_policies());
    store
        .upsert_node(&NodeRef {
            id: "n1".into(),
            url: "http://x".into(),
            label: "node one".into(),
        })
        .unwrap();
    let row = store.node("n1").unwrap().unwrap();
    assert_eq!(row.status, NodeStatus::Unknown);

    let summary = json!({
        "id": "s1", "title": "T1", "cwd": "/a", "agent": "devin",
        "source": "devin", "updatedAt": "2024-01-01T00:00:01.000Z",
        "locked": false, "busy": true, "pinned": true,
    });
    store.upsert_entity("n1", &ent(&summary)).unwrap();
    let got = store.entity("n1", "s1").unwrap().unwrap();
    assert_eq!(got.raw["indexDoc"]["title"], "T1");
    assert!(got.live);
    assert_eq!(got.raw["indexDoc"]["pinned"], true);

    // Diff merge — un-patched fields survive; columns re-derive.
    store
        .apply_patch("n1", "s1", &json!({"title": "T2", "busy": false}))
        .unwrap();
    let got = store.entity("n1", "s1").unwrap().unwrap();
    assert_eq!(got.raw["indexDoc"]["title"], "T2");
    assert!(!got.live);
    assert_eq!(got.raw["indexDoc"]["cwd"], "/a");
    assert_eq!(got.raw["indexDoc"]["pinned"], true);

    // Reconcile: a listing without s1 drops it; s2 keeps it.
    store
        .upsert_entity(
            "n1",
            &ent(&json!({"id":"s2","title":"S2","updatedAt":"2024-01-02T00:00:00.000Z"})),
        )
        .unwrap();
    store
        .apply_listing(
            "n1",
            &[ent(
                &json!({"id":"s2","title":"S2","updatedAt":"2024-01-02T00:00:00.000Z"}),
            )],
        )
        .unwrap();
    assert!(store.entity("n1", "s1").unwrap().is_none());
    assert!(store.entity("n1", "s2").unwrap().is_some());
    assert_eq!(
        store.cursor("n1").unwrap().as_deref(),
        Some("2024-01-02T00:00:00.000Z")
    );

    store.set_node_status("n1", NodeStatus::Up).unwrap();
    let row = store.node("n1").unwrap().unwrap();
    assert_eq!(row.status, NodeStatus::Up);
    assert!(row.last_seen_at.is_some());
}

#[test]
fn purge_unregistered_drops_stale_node_rows() {
    let store = ProjectionStore::in_memory()
        .unwrap()
        .with_kind_policies(test_policies());
    for id in ["main", "node"] {
        store
            .upsert_node(&NodeRef {
                id: id.into(),
                url: "http://x".into(),
                label: id.into(),
            })
            .unwrap();
        store
            .upsert_entity(
                id,
                &ent(&json!({"id":"s1","title":"T","updatedAt":"2024-01-01T00:00:00.000Z"})),
            )
            .unwrap();
    }
    // The legacy `node` id is gone from config — its rows go with it.
    store.purge_unregistered(&["main".to_string()]).unwrap();
    assert!(store.entity("main", "s1").unwrap().is_some());
    assert!(store.entity("node", "s1").unwrap().is_none());
    assert!(store.node("main").unwrap().is_some());
    assert!(store.node("node").unwrap().is_none());
    // Empty keep-list is a no-op guard, not a wipe.
    store.purge_unregistered(&[]).unwrap();
    assert!(store.entity("main", "s1").unwrap().is_some());
}

#[test]
fn projection_remove_node_and_cursor_roundtrip() {
    let store = ProjectionStore::in_memory()
        .unwrap()
        .with_kind_policies(test_policies());
    store
        .upsert_node(&NodeRef {
            id: "n1".into(),
            url: "http://x".into(),
            label: "l".into(),
        })
        .unwrap();
    store
        .upsert_entity(
            "n1",
            &ent(&json!({"id":"s1","title":"T","updatedAt":"2024-01-01T00:00:00.000Z"})),
        )
        .unwrap();
    store.set_cursor("n1", "cur-1").unwrap();
    assert_eq!(store.cursor("n1").unwrap().as_deref(), Some("cur-1"));
    store.set_cursor("n1", "cur-2").unwrap();
    assert_eq!(store.cursor("n1").unwrap().as_deref(), Some("cur-2"));

    store.remove_node("n1").unwrap();
    assert!(store.node("n1").unwrap().is_none());
    assert!(store.entity("n1", "s1").unwrap().is_none());
    assert_eq!(store.cursor("n1").unwrap(), None);
}

#[test]
fn projection_surfaces_corrupt_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proj.db");
    let store = ProjectionStore::open(&path)
        .unwrap()
        .with_kind_policies(test_policies());
    store
        .upsert_node(&NodeRef {
            id: "n1".into(),
            url: "http://x".into(),
            label: "l".into(),
        })
        .unwrap();
    // A raw_json that isn't JSON — the row_to_indexed / apply_patch
    // decode error arms.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO entities (node_id, entity_id, raw_json)
             VALUES ('n1', 'bad', 'not json')",
            [],
        )
        .unwrap();
    }
    assert!(store.entity("n1", "bad").is_err());
    assert!(store.apply_patch("n1", "bad", &json!({"x": 1})).is_err());
}

#[test]
fn envelope_columns_come_from_the_wrap_not_indexdoc() {
    // `kind="ci-job"`/`"campaign"` docs carry `updatedAt`/`locked`/`live`
    // only on the `{id,kind,indexDoc,…}` wrap — the indexDoc is the
    // kind's own spec and doesn't repeat them. The projection must
    // read the envelope, not dig into indexDoc (T-061).
    let store = ProjectionStore::in_memory().unwrap();
    let ci_job = json!({
        "id": "repo:main:abc123",
        "kind": "ci-job",
        "indexDoc": {"key": "repo", "event": "push", "state": "running", "attempt": 1},
        "updatedAt": "2024-01-03T00:00:00.000Z",
        "locked": true,
        "live": true,
    });
    store.apply_listing("n1", &[ci_job]).unwrap();
    let got = store.entity("n1", "repo:main:abc123").unwrap().unwrap();
    assert_eq!(got.kind, "ci-job");
    assert_eq!(got.updated_at, "2024-01-03T00:00:00.000Z");
    assert!(got.locked);
    assert!(got.live);
    // The cursor tracks the wrap's updatedAt too.
    assert_eq!(
        store.cursor("n1").unwrap().as_deref(),
        Some("2024-01-03T00:00:00.000Z")
    );

    // Wrap fields win over same-named indexDoc fields; a bare doc
    // (no indexDoc) still reads its own fields.
    let wrapped = json!({
        "id": "e2", "kind": "k",
        "indexDoc": {"id": "inner", "updatedAt": "inner-ts", "live": true},
        "updatedAt": "outer-ts", "live": false,
    });
    store.upsert_entity("n1", &wrapped).unwrap();
    let got = store.entity("n1", "e2").unwrap().unwrap();
    assert_eq!(got.entity_id, "e2");
    assert_eq!(got.updated_at, "outer-ts");
    assert!(!got.live);
    store
        .upsert_entity(
            "n1",
            &json!({"id":"e3","kind":"k","updatedAt":"bare-ts","locked":true}),
        )
        .unwrap();
    let got = store.entity("n1", "e3").unwrap().unwrap();
    assert_eq!(got.updated_at, "bare-ts");
    assert!(got.locked);
}

#[test]
fn apply_listing_summary_without_updated_at() {
    // Missing `updatedAt` → the `if let Some(u)` else arm; the cursor
    // stays unset.
    let store = ProjectionStore::in_memory()
        .unwrap()
        .with_kind_policies(test_policies());
    store
        .apply_listing("n1", &[json!({"id":"s1","title":"T"})])
        .unwrap();
    assert!(store.entity("n1", "s1").unwrap().is_some());
    assert_eq!(store.cursor("n1").unwrap(), None);
}

#[test]
fn apply_patch_write_failure_surfaces() {
    // `apply_patch`'s trailing `write_row?` — swap the table under the
    // store so the re-INSERT fails after the patch merges.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proj.db");
    let store = ProjectionStore::open(&path)
        .unwrap()
        .with_kind_policies(test_policies());
    store
        .upsert_entity("n1", &json!({"id":"s1","title":"T"}))
        .unwrap();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE entities;
             CREATE TABLE entities (node_id TEXT, entity_id TEXT, raw_json TEXT);
             INSERT INTO entities VALUES ('n1','s1','{}');",
        )
        .unwrap();
    }
    assert!(
        store
            .apply_patch("n1", "s1", &json!({"title": "T2"}))
            .is_err()
    );
}

#[test]
fn apply_patch_non_object_skips_merge() {
    // A non-object patch → the `if let (Some, Some)` else arm: the row
    // is rewritten unchanged.
    let store = ProjectionStore::in_memory()
        .unwrap()
        .with_kind_policies(test_policies());
    store
        .upsert_entity("n1", &json!({"id":"s1","title":"T"}))
        .unwrap();
    assert_eq!(
        store.apply_patch("n1", "s1", &json!([1, 2])).unwrap(),
        PatchStatus::Applied
    );
    assert_eq!(store.entity("n1", "s1").unwrap().unwrap().raw["title"], "T");
}

/* ---- node lifecycle (ephemeral awareness) ----------------------------*/

fn node_ref(id: &str) -> NodeRef {
    NodeRef {
        id: id.into(),
        url: format!("http://{id}"),
        label: id.into(),
    }
}

#[test]
fn node_row_tracks_status_down_since_and_ephemeral() {
    let store = ProjectionStore::in_memory().unwrap();
    store.upsert_node(&node_ref("n1")).unwrap();
    let row = store.node("n1").unwrap().unwrap();
    assert_eq!(row.status, NodeStatus::Unknown);
    assert!(row.down_since.is_none());
    assert!(!row.ephemeral);

    store.set_node_status("n1", NodeStatus::Up).unwrap();
    store.set_node_status("n1", NodeStatus::Down).unwrap();
    let first = store.node("n1").unwrap().unwrap().down_since.unwrap();
    assert_eq!(store.node("n1").unwrap().unwrap().status, NodeStatus::Down);

    // Repeated downs keep the first stamp — "down since", not "last
    // retry".
    std::thread::sleep(std::time::Duration::from_millis(2));
    store.set_node_status("n1", NodeStatus::Down).unwrap();
    assert_eq!(
        store.node("n1").unwrap().unwrap().down_since.unwrap(),
        first
    );

    // Up clears it; a later down restamps.
    store.set_node_status("n1", NodeStatus::Up).unwrap();
    assert!(store.node("n1").unwrap().unwrap().down_since.is_none());
    store.set_node_status("n1", NodeStatus::Down).unwrap();
    assert_ne!(
        store.node("n1").unwrap().unwrap().down_since.unwrap(),
        first
    );

    store.set_node_ephemeral("n1", true).unwrap();
    assert!(store.node("n1").unwrap().unwrap().ephemeral);
    assert!(store.nodes().unwrap()[0].ephemeral);
}

#[test]
fn drop_node_entities_keeps_the_node_row() {
    let store = ProjectionStore::in_memory().unwrap();
    store.upsert_node(&node_ref("n1")).unwrap();
    store
        .apply_listing(
            "n1",
            &[json!({"id":"e1","kind":"k","updatedAt":"2024-01-01T00:00:00Z"})],
        )
        .unwrap();
    assert!(store.entity("n1", "e1").unwrap().is_some());
    assert!(store.cursor("n1").unwrap().is_some());

    store.drop_node_entities("n1").unwrap();
    assert_eq!(store.entities(Some("n1")).unwrap().len(), 0);
    assert!(store.cursor("n1").unwrap().is_none());
    // The node row survives — status stays reportable.
    assert!(store.node("n1").unwrap().is_some());
}

#[test]
fn legacy_nodes_schema_gains_lifecycle_columns() {
    // A pre-ephemeral projection.db: `nodes` without the new columns.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proj.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                label TEXT NOT NULL,
                last_seen_at TEXT,
                status TEXT NOT NULL DEFAULT 'unknown'
            );
            INSERT INTO nodes (id,url,label,status) VALUES ('old','http://old','old','up');",
        )
        .unwrap();
    }
    let store = ProjectionStore::open(&path).unwrap();
    let row = store.node("old").unwrap().unwrap();
    assert_eq!(row.status, NodeStatus::Up);
    assert!(!row.ephemeral);
    assert!(row.down_since.is_none());
    // The migrated columns are writable.
    store.set_node_ephemeral("old", true).unwrap();
    store.set_node_status("old", NodeStatus::Down).unwrap();
    let row = store.node("old").unwrap().unwrap();
    assert!(row.ephemeral);
    assert!(row.down_since.is_some());
}
