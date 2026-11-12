//! `routes/kv.ts` generalized — the substrate KV surface (P-006):
//! `/api/kv` namespace list, `/api/kv/{ns}` entry map, and
//! `/api/kv/{ns}/{key}` single-entry CRUD over [`hypha_kv::KvStore`],
//! plus `POST /api/client/keypair` (the stateless Ed25519 mint moved
//! verbatim from peregrin — a client on a non-secure context has no
//! `crypto.subtle`, so the node mints its identity server-side).
//!
//! Visibility is the wire gate: `internal` namespaces answer reads
//! with the same 404 as a missing key and reject writes with 400 —
//! the generic replacement for peregrin's `INTERNAL_CONFIG`
//! denylist. The store itself stays agnostic (store-API callers read
//! internal namespaces freely).
//!
//! Writes report through [`crate::KvHook`]: default is a `"kv"` feed
//! event (`{ns, key, value}` / `{ns, key, deleted}`); integrations
//! install a hook to emit their own kinds (peregrin's `"project"`
//! events) or suppress entirely.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hypha_kv::{KvOp, KvStore, Visibility};
use serde_json::{Value, json};

use crate::routes::{SMALL_JSON_BODY, json_response, not_found, ok, read_json_body};
use crate::{HyphaState, KvWrite};

fn kv_missing() -> Response {
    json_response(
        json!({ "error": "KV store is not configured on this server" }),
        StatusCode::NOT_IMPLEMENTED,
    )
}

fn store_of(state: &HyphaState) -> Option<Arc<dyn KvStore>> {
    state.kv.clone()
}

/// Internal namespaces are invisible on the wire — reads 404 like a
/// missing key.
fn hidden(state: &HyphaState, ns: &str) -> bool {
    store_of(state).is_some_and(|s| s.visibility(ns) == Visibility::Internal)
}

/// Report one committed write — the integration hook when installed,
/// else the default `"kv"` feed event.
fn emit_write(state: &HyphaState, ns: &str, key: &str, op: KvOp) {
    let write = KvWrite {
        ns: ns.to_string(),
        key: key.to_string(),
        op,
    };
    if let Some(hook) = &state.kv_hook {
        hook(&write, &state.feed);
        return;
    }
    match &write.op {
        KvOp::Set { value } => state.feed.emit(
            "kv",
            json!({ "ns": write.ns, "key": write.key, "value": value }),
        ),
        KvOp::Delete => state.feed.emit(
            "kv",
            json!({ "ns": write.ns, "key": write.key, "deleted": true }),
        ),
    }
}

/// `GET /api/kv` — the public namespace directory. Internal
/// namespaces aren't named at all.
pub async fn namespaces(
    State(state): State<HyphaState>,
    method: Method,
    _req: Request<Body>,
) -> Response {
    if method != Method::GET {
        return not_found();
    }
    let Some(store) = store_of(&state) else {
        return kv_missing();
    };
    let public: Vec<Value> = store
        .namespaces()
        .into_iter()
        .filter(|n| n.visibility == Visibility::Public)
        .map(|n| json!({ "name": n.name, "visibility": "public" }))
        .collect();
    json_response(json!({ "namespaces": public }), StatusCode::OK)
}

/// `GET /api/kv/{ns}` — the namespace's whole entry map.
pub async fn namespace(
    State(state): State<HyphaState>,
    method: Method,
    Path(ns): Path<String>,
    _req: Request<Body>,
) -> Response {
    if method != Method::GET {
        return not_found();
    }
    let Some(store) = store_of(&state) else {
        return kv_missing();
    };
    if store.visibility(&ns) == Visibility::Internal {
        return not_found();
    }
    json_response(
        json!({ "ns": ns, "entries": store.list(&ns) }),
        StatusCode::OK,
    )
}

/// `GET|PUT|PATCH|DELETE /api/kv/{ns}/{key}` — one entry. Writes take
/// a `{"value": <any json>}` envelope (the `/api/config/{key}` wire
/// shape); `value` absent stores `null`.
pub async fn entry(
    State(state): State<HyphaState>,
    method: Method,
    Path((ns, key)): Path<(String, String)>,
    req: Request<Body>,
) -> Response {
    let Some(store) = store_of(&state) else {
        return kv_missing();
    };
    match method {
        Method::GET => {
            if hidden(&state, &ns) {
                return not_found();
            }
            match store.get(&ns, &key) {
                Some(value) => json_response(
                    json!({ "ns": ns, "key": key, "value": value }),
                    StatusCode::OK,
                ),
                None => json_response(json!({ "error": "Not found" }), StatusCode::NOT_FOUND),
            }
        }
        Method::PUT | Method::PATCH => {
            if hidden(&state, &ns) {
                return json_response(
                    json!({ "error": "Namespace is internal" }),
                    StatusCode::BAD_REQUEST,
                );
            }
            let body = match read_json_body(req, SMALL_JSON_BODY).await {
                Ok(b) => b,
                Err(res) => return *res,
            };
            let value = match body {
                Some(Value::Object(map)) => map.get("value").cloned().unwrap_or(Value::Null),
                Some(_) => {
                    return json_response(
                        json!({ "error": "Expected a JSON object body {\"value\": ...}" }),
                        StatusCode::BAD_REQUEST,
                    );
                }
                None => Value::Null,
            };
            store.set(&ns, &key, value.clone());
            emit_write(
                &state,
                &ns,
                &key,
                KvOp::Set {
                    value: value.clone(),
                },
            );
            json_response(
                json!({ "ns": ns, "key": key, "value": value }),
                StatusCode::OK,
            )
        }
        Method::DELETE => {
            if hidden(&state, &ns) {
                return json_response(
                    json!({ "error": "Namespace is internal" }),
                    StatusCode::BAD_REQUEST,
                );
            }
            store.delete(&ns, &key);
            emit_write(&state, &ns, &key, KvOp::Delete);
            ok()
        }
        _ => not_found(),
    }
}

/// Mint an Ed25519 keypair; returns `{algorithm, publicKey, secretKey}`
/// — the JWK `d` is the 32-byte private seed, base64url (no pad).
/// Verbatim port of peregrin's `routes/client.rs`: TS tried Ed25519
/// then ECDSA P-256 — in Rust Ed25519 always works, so the fallback
/// branch is unreachable.
fn mint_ed25519() -> Response {
    let mut seed = [0u8; 32];
    // uuid v4 is OS-CSPRNG backed — two draws fill the 32-byte seed.
    seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    let public = signing.verifying_key().to_bytes();
    json_response(
        json!({
            "algorithm": "Ed25519",
            "publicKey": URL_SAFE_NO_PAD.encode(public),
            "secretKey": URL_SAFE_NO_PAD.encode(seed),
        }),
        StatusCode::OK,
    )
}

/// `POST /api/client/keypair` — authenticated like every other
/// `/api` route (the auth gate wraps this router); the secret
/// transits the wire, so on plaintext LAN this is only as private as
/// the transport.
pub async fn keypair(
    State(_state): State<HyphaState>,
    method: Method,
    _req: Request<Body>,
) -> Response {
    if method != Method::POST {
        return not_found();
    }
    mint_ed25519()
}
