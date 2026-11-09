//! `routes/misc.ts` + `routes/fs.ts` + `routes/node.ts` substrate
//! halves — health probe, directory listing for pickers, node
//! descriptor, user info. Session-shaped handlers (the session-list
//! health probe, meta-backed config/projects) stay with the
//! integration.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::Response;
use serde_json::json;

use crate::HyphaState;
use crate::node::PROTOCOL_VERSION;
use crate::routes::{SMALL_JSON_BODY, json_response, not_found, query_param, read_json_body};

/// `GET /api/health` — deliberately ahead of the auth gate (probes).
/// The substrate has no session store to probe; liveness is the
/// answer. Integrations with a deeper health check override by
/// nesting their own route ahead of this router.
pub async fn health(
    State(_state): State<HyphaState>,
    method: Method,
    _req: Request<Body>,
) -> Response {
    if method != Method::GET {
        return not_found();
    }
    json_response(json!({ "ok": true }), StatusCode::OK)
}

/// `GET /api/fs?path=/abs/dir` — direct children of a directory, for
/// pickers. Directories only, capped at 200.
pub async fn fs(State(_state): State<HyphaState>, method: Method, req: Request<Body>) -> Response {
    if method != Method::GET {
        return not_found();
    }
    let path = query_param(req.uri().query(), "path").unwrap_or_default();
    if !path.starts_with('/') {
        return json_response(
            json!({ "error": "path must be absolute" }),
            StatusCode::BAD_REQUEST,
        );
    }
    let read = std::fs::read_dir(&path);
    match read {
        Err(_) => json_response(
            json!({ "error": "Cannot read that directory" }),
            StatusCode::BAD_REQUEST,
        ),
        Ok(entries) => {
            let mut dirs: Vec<String> = entries
                .filter_map(std::result::Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path().to_string_lossy().into_owned())
                .collect();
            dirs.sort();
            dirs.truncate(200);
            json_response(json!({ "dirs": dirs }), StatusCode::OK)
        }
    }
}

/// `process.platform`/`process.arch` under Node's names.
fn node_platform() -> &'static str {
    node_platform_of(std::env::consts::OS)
}

#[cfg_attr(coverage, allow(dead_code))]
fn node_platform_of(os: &'static str) -> &'static str {
    match os {
        "macos" => "darwin",
        "windows" => "win32",
        other => match other {
            "linux" => "linux",
            "freebsd" => "freebsd",
            "openbsd" => "openbsd",
            "netbsd" => "netbsd",
            "android" => "android",
            o => o,
        },
    }
}

fn node_arch() -> &'static str {
    node_arch_of(std::env::consts::ARCH)
}

fn node_arch_of(arch: &'static str) -> &'static str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "arm" => "arm",
        "x86" => "ia32",
        other => match other {
            "riscv64" => "riscv64",
            "powerpc64" => "ppc64",
            "s390x" => "s390x",
            o => o,
        },
    }
}

fn hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

/// `GET /api/user` — the OS user the node runs as (`os.userInfo()`).
pub async fn user(
    State(_state): State<HyphaState>,
    method: Method,
    _req: Request<Body>,
) -> Response {
    if method != Method::GET {
        return not_found();
    }
    let info = nix::unistd::User::from_uid(nix::unistd::Uid::effective())
        .ok()
        .flatten();
    let (username, homedir, shell) = info.map_or_else(user_fallback, |u| {
        (
            u.name,
            u.dir.to_string_lossy().into_owned(),
            u.shell.to_string_lossy().into_owned(),
        )
    });
    json_response(
        json!({
            "user": {
                "username": username,
                "homedir": homedir,
                "shell": shell,
                "hostname": hostname(),
                "platform": node_platform(),
                "arch": node_arch(),
            }
        }),
        StatusCode::OK,
    )
}

/// `GET|PATCH /api/node` — the node descriptor every federated client
/// hits first; PATCH renames the node and persists the identity file.
/// `agents` is whatever the composing integration advertises
/// (`HyphaState::node_agents` — empty on a bare substrate); the
/// substrate's own capabilities are `entities`/`events`/`export`,
/// plus `push`/`pairing` when those backends are wired.
pub async fn node(State(state): State<HyphaState>, method: Method, req: Request<Body>) -> Response {
    match method {
        Method::GET => node_get(&state).await,
        Method::PATCH => node_patch(&state, req).await,
        _ => not_found(),
    }
}

/// The descriptor shape `GET` and a successful `PATCH` both return.
/// `agents` prefers the live [`crate::AgentLister`] provider over the
/// static `node_agents` list; `capabilities` honors the integration's
/// `node_capabilities` override before the substrate defaults.
async fn node_descriptor(state: &HyphaState) -> serde_json::Value {
    let agents = match &state.agent_lister {
        Some(lister) => lister().await,
        None => state.node_agents.clone(),
    };
    let capabilities = state.node_capabilities.clone().unwrap_or_else(|| {
        let mut capabilities: Vec<String> = ["entities", "events", "export"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        if state.push.is_some() {
            capabilities.push("push".into());
        }
        if state.pairing.is_some() {
            capabilities.push("pairing".into());
        }
        capabilities
    });
    let identity = state.node_identity();
    json!({
        "id": identity.id,
        "name": identity.name,
        "version": identity.version,
        "protocol": PROTOCOL_VERSION,
        "agents": agents,
        "capabilities": capabilities,
    })
}

async fn node_get(state: &HyphaState) -> Response {
    json_response(node_descriptor(state).await, StatusCode::OK)
}

/// `PATCH /api/node` — `{name}` renames the node, persists to
/// `HYPHA_NODE` (`node.json`) and returns the updated descriptor.
async fn node_patch(state: &HyphaState, req: Request<Body>) -> Response {
    let body = match read_json_body(req, SMALL_JSON_BODY).await {
        Ok(b) => b,
        Err(res) => return *res,
    };
    let Some(body) = body.and_then(|b| b.as_object().cloned()) else {
        return json_response(
            json!({ "error": "Expected a JSON object body" }),
            StatusCode::BAD_REQUEST,
        );
    };
    let Some(name) = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.len() <= 100)
    else {
        return json_response(
            json!({ "error": "name must be a non-empty string (max 100)" }),
            StatusCode::BAD_REQUEST,
        );
    };
    let mut updated = state.node_identity();
    updated.name = name.to_string();
    if let Some(path) = &state.node_path {
        if let Err(e) = crate::node::save_node_identity(path, &updated) {
            return json_response(
                json!({ "error": format!("failed to persist node identity: {e}"), "code": "internal" }),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }
    *state
        .node
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = updated;
    json_response(node_descriptor(state).await, StatusCode::OK)
}

/// env-var fallback when `nix` can't resolve the uid — extracted so
/// tests can drive the arm (the live lookup always succeeds).
fn user_fallback() -> (String, String, String) {
    (
        std::env::var("USER").unwrap_or_default(),
        std::env::var("HOME").unwrap_or_default(),
        std::env::var("SHELL").unwrap_or_default(),
    )
}

#[cfg(test)]
mod cov_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn platform_arch_tables() {
        for (input, want) in [
            ("linux", "linux"),
            ("freebsd", "freebsd"),
            ("openbsd", "openbsd"),
            ("netbsd", "netbsd"),
            ("android", "android"),
            ("plan9", "plan9"),
        ] {
            assert_eq!(node_platform_of(input), want);
        }
        for (input, want) in [
            ("x86_64", "x64"),
            ("aarch64", "arm64"),
            ("arm", "arm"),
            ("x86", "ia32"),
            ("riscv64", "riscv64"),
            ("powerpc64", "ppc64"),
            ("s390x", "s390x"),
            ("mips", "mips"),
        ] {
            assert_eq!(node_arch_of(input), want);
        }
    }

    #[test]
    fn user_fallback_reads_env() {
        let _ = user_fallback();
    }
}
