#![allow(clippy::unwrap_used)]
//! Router-level tests — a stub [`EntityPlane`] behind the real
//! middleware stack (auth gate, CORS, log, trace) via `oneshot`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hypha_http::entity_plane::{EntityContext, EntityPlane};
use hypha_http::{HyphaState, app};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Widget;

#[async_trait::async_trait]
impl EntityPlane for Widget {
    fn kind(&self) -> &'static str {
        "test-widget"
    }
    fn op_namespace(&self) -> &'static str {
        "widget"
    }
    async fn list(&self, _: &EntityContext, _: Option<&str>) -> Vec<Value> {
        vec![json!({ "id": "w1", "kind": "test-widget" })]
    }
    async fn get(&self, _: &EntityContext, id: &str, _: Option<&str>) -> Response {
        if id == "w1" {
            axum::Json(json!({ "id": id, "kind": "test-widget" })).into_response()
        } else {
            axum::http::StatusCode::NOT_FOUND.into_response()
        }
    }
    async fn create(&self, _: &EntityContext, init: Value) -> Response {
        axum::Json(json!({ "created": init })).into_response()
    }
    async fn delete(&self, _: &EntityContext, id: &str, _: Option<&str>) -> Response {
        axum::Json(json!({ "deleted": id })).into_response()
    }
    async fn op(
        &self,
        _: &EntityContext,
        id: &str,
        op: &str,
        payload: Value,
        _: Option<&str>,
    ) -> Response {
        axum::Json(json!({ "id": id, "op": op, "payload": payload })).into_response()
    }
    async fn export(&self, _: &EntityContext, id: &str, _: Option<&str>) -> Response {
        axum::Json(json!({ "snapshot": id })).into_response()
    }
    async fn import(&self, _: &EntityContext, body: Value) -> Response {
        axum::Json(json!({ "imported": body })).into_response()
    }
}

fn state() -> HyphaState {
    HyphaState::new().with_kind(Arc::new(Widget))
}

async fn body_json(res: Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn health_is_open_and_entities_list() {
    let router = app(state());
    let res = router
        .clone()
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await, json!({ "ok": true }));

    let res = router
        .oneshot(Request::get("/api/entities").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        body_json(res).await,
        json!({ "entities": [{ "id": "w1", "kind": "test-widget" }] })
    );
}

#[tokio::test]
async fn entity_get_create_op_delete_flow() {
    let router = app(state());

    // GET by id — the plane claims w1, 404s the rest.
    let res = router
        .clone()
        .oneshot(
            Request::get("/api/entities/w1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = router
        .clone()
        .oneshot(
            Request::get("/api/entities/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // POST create — kind-less lands on the default (first) plane.
    let res = router
        .clone()
        .oneshot(
            Request::post("/api/entities")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"indexDoc":{"x":1}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await, json!({ "created": { "x": 1 } }));

    // Unknown kind → 501.
    let res = router
        .clone()
        .oneshot(
            Request::post("/api/entities")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"kind":"ghost"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);

    // Namespaced op dispatch.
    let res = router
        .clone()
        .oneshot(
            Request::post("/api/entities/w1/ops")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"kind":"widget.poke","payload":{"n":2}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        body_json(res).await,
        json!({ "id": "w1", "op": "widget.poke", "payload": { "n": 2 } })
    );

    // entity.delete — owning_plane probes get(); w1 is claimed.
    let res = router
        .clone()
        .oneshot(
            Request::post("/api/entities/w1/ops")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"kind":"entity.delete"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(res).await, json!({ "deleted": "w1" }));

    // Snapshot export + import.
    let res = router
        .clone()
        .oneshot(
            Request::get("/api/entities/w1/snapshot")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(res).await, json!({ "snapshot": "w1" }));
    let res = router
        .oneshot(
            Request::post("/api/entities/snapshot")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"kind":"test-widget","blob":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        body_json(res).await,
        json!({ "imported": { "kind": "test-widget", "blob": 1 } })
    );
}

#[tokio::test]
async fn auth_gate_blocks_api_without_token_when_configured() {
    let router = app(state().with_token(Some("sekret".into())));
    let res = router
        .clone()
        .oneshot(Request::get("/api/entities").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Bearer passes; health stays open either way.
    let res = router
        .clone()
        .oneshot(
            Request::get("/api/entities")
                .header(header::AUTHORIZATION, "Bearer sekret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = router
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn node_and_push_descriptors() {
    let router = app(state());
    let res = router
        .clone()
        .oneshot(Request::get("/api/node").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let node = body_json(res).await;
    assert_eq!(node["protocol"], 1);
    assert!(
        node["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("entities"))
    );

    // No push store wired → 501.
    let res = router
        .oneshot(Request::get("/api/push/vapid").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn unknown_paths_and_methods_404() {
    let router = app(state());
    let res = router
        .clone()
        .oneshot(Request::get("/api/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    // Wrong method falls through to the TS-style 404, not a 405.
    let res = router
        .oneshot(Request::delete("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
