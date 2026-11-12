#![allow(unknown_lints, clippy::assert_is_empty)]

//! Route helpers — the substrate half of peregrin's
//! `routes/shared.ts`: the `{error}` body shape, JSON-body reading
//! with per-route caps, and query-param decoding. Session-shaped wire
//! types and the `ControlError` → status mapping stay with the
//! integration that owns them.

pub mod entities;
pub mod events;
pub mod kv;
pub mod misc;
pub mod push;

use std::collections::HashMap;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

/// `{error: message}` JSON body at `status`.
pub fn json_response(body: Value, status: StatusCode) -> Response {
    (status, axum::Json(body)).into_response()
}

pub fn ok() -> Response {
    json_response(serde_json::json!({ "ok": true }), StatusCode::OK)
}

/// Terminal handler — unknown paths fall through to
/// `{error:"Not found"}` 404.
pub fn not_found() -> Response {
    json_response(
        serde_json::json!({ "error": "Not found" }),
        StatusCode::NOT_FOUND,
    )
}

/* ---- body caps --------------------------------------------------------*/

/// Small bodies — pair codes, login tokens, renames, op submissions,
/// push subscriptions. The biggest legit field anywhere on these
/// routes is small; 64KiB is generous.
pub const SMALL_JSON_BODY: usize = 64 * 1024;

/// Hard ceiling — `POST /api/entities/snapshot` carries a whole
/// exported snapshot; nothing else legitimately approaches this.
pub const MAX_JSON_BODY: usize = 32 * 1024 * 1024;

/// Read a JSON body with an explicit byte cap: empty body → `None`;
/// parse failure → the boxed 400 `Invalid JSON body` response; over
/// `limit` bytes → 413 (`Box` keeps the `Err` variant small for
/// `clippy::result_large_err`).
pub async fn read_json_body(
    req: Request<Body>,
    limit: usize,
) -> Result<Option<Value>, Box<Response>> {
    let bytes = axum::body::to_bytes(req.into_body(), limit)
        .await
        .map_err(|e| {
            if std::error::Error::source(&e)
                .is_some_and(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
            {
                Box::new(payload_too_large())
            } else {
                Box::new(invalid_json())
            }
        })?;
    read_json_bytes(&bytes)
}

fn payload_too_large() -> Response {
    json_response(
        serde_json::json!({ "error": "Request body too large" }),
        StatusCode::PAYLOAD_TOO_LARGE,
    )
}

fn invalid_json() -> Response {
    json_response(
        serde_json::json!({ "error": "Invalid JSON body" }),
        StatusCode::BAD_REQUEST,
    )
}

pub fn read_json_bytes(bytes: &Bytes) -> Result<Option<Value>, Box<Response>> {
    let text = String::from_utf8_lossy(bytes);
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<Value>(text.trim())
        .map(Some)
        .map_err(|_| Box::new(invalid_json()))
}

/// `url.searchParams` — a decoded name→value map.
pub fn query_params(query: Option<&str>) -> HashMap<String, String> {
    let Some(query) = query else {
        return HashMap::new();
    };
    form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

pub fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query.and_then(|q| {
        form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    })
}

/// `Date.now()` — epoch milliseconds.
pub fn now_ms_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod cov_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn respond_and_helper_arms() {
        // Query helpers.
        assert!(query_params(None).is_empty());
        assert_eq!(query_params(Some("a=1&b=2")).get("b").unwrap(), "2");
        assert_eq!(query_param(Some("a=1"), "a").as_deref(), Some("1"));
        assert_eq!(query_param(None, "a"), None);
        // Error-response shapes.
        let _ = invalid_json();
        let _ = payload_too_large();
        let _ = ok();
        let _ = not_found();
    }

    #[tokio::test]
    async fn read_json_body_stream_error_is_invalid() {
        // A body that errors mid-stream (not a length limit) → the
        // `invalid_json` arm of the `to_bytes` map_err.
        let body = Body::from_stream(futures::stream::once(async {
            Err::<axum::body::Bytes, std::io::Error>(std::io::Error::other("boom"))
        }));
        let req = Request::builder()
            .method("POST")
            .uri("/x")
            .body(body)
            .unwrap();
        let res = read_json_body(req, SMALL_JSON_BODY).await;
        assert!(res.is_err());
    }

    #[test]
    fn read_json_bytes_arms() {
        assert!(read_json_bytes(&Bytes::new()).unwrap().is_none());
        assert!(
            read_json_bytes(&Bytes::from_static(b"  "))
                .unwrap()
                .is_none()
        );
        assert!(read_json_bytes(&Bytes::from_static(b"{")).is_err());
        let v = read_json_bytes(&Bytes::from_static(br#"{"a":1}"#)).unwrap();
        assert_eq!(v.unwrap()["a"], 1);
    }
}
