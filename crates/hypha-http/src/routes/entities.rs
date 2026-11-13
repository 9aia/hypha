//! The entity surface — `/api/entities*` is the substrate-facing view
//! of everything a node hosts. Every handler here is kind-agnostic:
//! it parses just enough of the request to pick a plane out of the
//! [`EntityContext::kinds`] registry (`body.kind`, the `<ns>.<verb>`
//! op namespace, or owner-resolution across planes for id-scoped
//! calls) and hands over verbatim — payload translation, status
//! codes, and feed emission stay inside each kind's [`EntityPlane`]
//! impl.
//!
//! Integrations register planes at composition
//! ([`crate::HyphaState::with_kind`]). `entity.*` is the substrate's
//! own op namespace: `entity.delete` resolves the owning plane and
//! deletes through it.
//!
//! Handlers extract [`HyphaState`] only to build the
//! [`EntityContext`] — the entity-plane contract itself sees nothing
//! else.

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::HyphaState;
use crate::entity_plane::{EntityContext, EntityPlane};

use super::{
    MAX_JSON_BODY, SMALL_JSON_BODY, json_response, not_found, ok, query_param, read_json_body,
};

fn bad_request(message: &str) -> Response {
    json_response(json!({ "error": message }), StatusCode::BAD_REQUEST)
}

/// Unknown kind on create/import — kept at 501 with the long-standing
/// `not registered` wording the hub's `api_err` maps back to 501.
fn kind_not_registered(kind: &str) -> Response {
    json_response(
        json!({ "error": format!("entity kind {kind:?} not registered") }),
        StatusCode::NOT_IMPLEMENTED,
    )
}

fn unknown_op(kind: &str) -> Response {
    json_response(
        json!({ "error": format!("unknown op {kind:?}") }),
        StatusCode::NOT_IMPLEMENTED,
    )
}

/// First plane whose `get` claims `id` — any non-404 answer counts
/// (a 500 means "mine, but broken": more informative than probing on).
async fn owning_plane(
    ctx: &EntityContext,
    id: &str,
    query: Option<&str>,
) -> Option<Arc<dyn EntityPlane>> {
    for plane in ctx.kinds.planes() {
        if plane.get(ctx, id, query).await.status() != StatusCode::NOT_FOUND {
            return Some(Arc::clone(plane));
        }
    }
    None
}

/// `entity.delete` — the substrate verb: find the owning plane and
/// delete through it. Nobody owning the id reads as already-gone —
/// delete stays idempotent (replayed outbox deletes must not
/// dead-letter on a node that already applied them).
async fn delete_entity(ctx: &EntityContext, id: &str, query: Option<&str>) -> Response {
    match owning_plane(ctx, id, query).await {
        Some(plane) => plane.delete(ctx, id, query).await,
        None => ok(),
    }
}

/// `GET /api/entities[?kind=][&withLocks=1]` — every registered
/// plane's entity docs (or just one kind's when `?kind=` filters);
/// `POST /api/entities` — `{kind?, indexDoc}` → the kind's create.
pub async fn collection(
    State(state): State<HyphaState>,
    method: Method,
    req: Request<Body>,
) -> Response {
    match method {
        Method::GET => {
            let ctx = state.entity_context();
            let query = req.uri().query();
            let kind = query_param(query, "kind");
            let mut entities = Vec::new();
            for plane in ctx.kinds.planes() {
                if kind.as_deref().is_some_and(|k| k != plane.kind()) {
                    continue;
                }
                entities.extend(plane.list(&ctx, query).await);
            }
            json_response(json!({ "entities": entities }), StatusCode::OK)
        }
        Method::POST => create(state, req).await,
        _ => not_found(),
    }
}

/// `POST /api/entities` — `{kind?, indexDoc}`; a missing `kind`
/// targets the default (first-registered) plane, an unregistered one
/// 501s.
async fn create(state: HyphaState, req: Request<Body>) -> Response {
    let body = match read_json_body(req, SMALL_JSON_BODY).await {
        Ok(b) => b,
        Err(res) => return *res,
    };
    let Some(body) = body else {
        return bad_request("Expected a JSON object body");
    };
    let ctx = state.entity_context();
    let plane = match body.get("kind").and_then(Value::as_str) {
        Some(kind) => match ctx.kinds.plane(kind) {
            Some(plane) => plane,
            None => return kind_not_registered(kind),
        },
        None => match ctx.kinds.default_plane() {
            Some(plane) => plane,
            None => return kind_not_registered(""),
        },
    };
    let init = body.get("indexDoc").cloned().unwrap_or(body);
    plane.create(&ctx, init).await
}

/// `entity.create` — dispatch a create op to the plane named by
/// `payload.kind` (the create body verbatim: `{kind, indexDoc?}`).
/// Same plane resolution + init extraction as [`create`]; the op's
/// path id is ignored — creates are idempotent on the kind-derived
/// entity id, so replays dedup for free.
async fn entity_create(ctx: &EntityContext, payload: Value) -> Response {
    let plane = match payload.get("kind").and_then(Value::as_str) {
        Some(kind) => match ctx.kinds.plane(kind) {
            Some(plane) => plane,
            None => return kind_not_registered(kind),
        },
        None => match ctx.kinds.default_plane() {
            Some(plane) => plane,
            None => return kind_not_registered(""),
        },
    };
    let init = payload.get("indexDoc").cloned().unwrap_or(payload);
    plane.create(ctx, init).await
}

/// `GET /api/entities/{id}` — the owning plane's entity doc (first
/// non-404 wins; all-404 answers the last one). `DELETE` is
/// `entity.delete`.
pub async fn item(
    State(state): State<HyphaState>,
    method: Method,
    Path(id): Path<String>,
    req: Request<Body>,
) -> Response {
    let ctx = state.entity_context();
    match method {
        Method::GET => {
            let query = req.uri().query();
            let mut last = None;
            for plane in ctx.kinds.planes() {
                let res = plane.get(&ctx, &id, query).await;
                if res.status() != StatusCode::NOT_FOUND {
                    return res;
                }
                last = Some(res);
            }
            last.unwrap_or_else(not_found)
        }
        Method::DELETE => delete_entity(&ctx, &id, req.uri().query()).await,
        _ => not_found(),
    }
}

/// `POST /api/entities/{id}/ops` — submit `{kind, payload}`.
/// `entity.delete` (and the bare `delete` alias) is substrate-generic;
/// `<ns>.<verb>` dispatches to the plane owning `ns`; bare verbs fall
/// back to the default plane — legacy un-namespaced outbox rows.
pub async fn ops(
    State(state): State<HyphaState>,
    method: Method,
    Path(id): Path<String>,
    req: Request<Body>,
) -> Response {
    if method != Method::POST {
        return not_found();
    }
    let ctx = state.entity_context();
    let query = req.uri().query().map(str::to_string);
    let body = match read_json_body(req, SMALL_JSON_BODY).await {
        Ok(b) => b,
        Err(res) => return *res,
    };
    let Some(body) = body else {
        return bad_request("Expected a JSON object body");
    };
    let kind = body.get("kind").and_then(Value::as_str).unwrap_or_default();
    let payload = body.get("payload").cloned().unwrap_or_else(|| json!({}));

    if kind == "entity.delete" || kind == "delete" {
        return delete_entity(&ctx, &id, query.as_deref()).await;
    }
    // `entity.create` — the replayable-create verb: the hub's durable
    // outbox carries creates as ops when the owning node is down, and
    // the drain replays them through this route. The `{id}` path
    // segment is the outbox's FIFO bucket, not the entity id — the
    // kind derives the real id inside `create` exactly like
    // `POST /api/entities` does. Payload shape mirrors the create
    // body: `{kind, indexDoc}` (`indexDoc` optional — the whole
    // payload is the init then, `kind` included, same as the
    // collection route).
    if kind == "entity.create" {
        return entity_create(&ctx, payload).await;
    }
    // `entity.*` past `delete`/`create` is the substrate's own
    // (reserved) namespace — nothing else lives there yet.
    if kind.starts_with("entity.") {
        return unknown_op(kind);
    }
    match ctx.kinds.plane_for_op(kind) {
        Some(plane) => plane.op(&ctx, &id, kind, payload, query.as_deref()).await,
        None => unknown_op(kind),
    }
}

/// `GET /api/entities/{id}/snapshot` — kind-shaped snapshot bytes the
/// substrate moves opaquely; the owning plane produces them.
pub async fn snapshot_item(
    State(state): State<HyphaState>,
    method: Method,
    Path(id): Path<String>,
    req: Request<Body>,
) -> Response {
    if method != Method::GET {
        return not_found();
    }
    let ctx = state.entity_context();
    let query = req.uri().query();
    let mut last = None;
    for plane in ctx.kinds.planes() {
        let res = plane.export(&ctx, &id, query).await;
        if res.status() != StatusCode::NOT_FOUND {
            return res;
        }
        last = Some(res);
    }
    last.unwrap_or_else(not_found)
}

/// `GET /api/entity-index` — the single-node entity index: this
/// node's id plus every registered plane's entity docs (`?kind=`
/// filters, same as `GET /api/entities`). The hub's merged
/// multi-node version is `hypha-hub`'s `entity_routes::entity_index`;
/// both answer `{"index": [{node, entities}]}`.
pub async fn entity_index(
    State(state): State<HyphaState>,
    method: Method,
    req: Request<Body>,
) -> Response {
    if method != Method::GET {
        return not_found();
    }
    let ctx = state.entity_context();
    let query = req.uri().query();
    let kind = query_param(query, "kind");
    let mut entities = Vec::new();
    for plane in ctx.kinds.planes() {
        if kind.as_deref().is_some_and(|k| k != plane.kind()) {
            continue;
        }
        entities.extend(plane.list(&ctx, query).await);
    }
    let node = state.node_identity().id;
    json_response(
        json!({ "index": [{ "node": node, "entities": entities }] }),
        StatusCode::OK,
    )
}

/// `GET /api/pending-ops` — the node's outbox view. A bare node owns
/// no outbox (the durable queue lives hub-side in `hypha-hub`), so the
/// honest answer is an empty list — same `{"ops": [...]}` shape the
/// hub serves.
pub async fn pending_ops(State(_state): State<HyphaState>, method: Method) -> Response {
    if method != Method::GET {
        return not_found();
    }
    json_response(json!({ "ops": [] }), StatusCode::OK)
}

/// `POST /api/entities/snapshot` — import a snapshot on this node;
/// `body.kind` picks the plane (absent → the default plane).
pub async fn snapshot_collection(
    State(state): State<HyphaState>,
    method: Method,
    req: Request<Body>,
) -> Response {
    if method != Method::POST {
        return not_found();
    }
    let body = match read_json_body(req, MAX_JSON_BODY).await {
        Ok(b) => b,
        Err(res) => return *res,
    };
    let Some(body) = body else {
        return bad_request("Expected a JSON object body");
    };
    let ctx = state.entity_context();
    let kind_opt = body.get("kind").and_then(Value::as_str).or_else(|| {
        body.get("entity")
            .and_then(|e| e.get("kind"))
            .and_then(Value::as_str)
    });
    let plane = match kind_opt {
        Some(kind) => match ctx.kinds.plane(kind) {
            Some(plane) => plane,
            None => return kind_not_registered(kind),
        },
        None => match ctx.kinds.default_plane() {
            Some(plane) => plane,
            None => return kind_not_registered(""),
        },
    };
    plane.import(&ctx, body).await
}
