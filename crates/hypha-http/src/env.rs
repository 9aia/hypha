//! Boot-time configuration — the substrate half of peregrin's
//! `env.ts`: bind, auth token, CORS, SSE keep-alive, pairing, node
//! identity, and the push document. Session-plane tunables and
//! integration-specific knobs deliberately don't ride `Env` — each
//! integration parses its own. `Env::parse` fails fast on bad values.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 18787;
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_SSE_KEEPALIVE_MS: u64 = 15_000;
/// `POST /api/pair` redeem attempts per client IP: ~40-bit codes make
/// online guessing infeasible anyway, so these only need to throttle
/// abuse on nodes exposed off-LAN — not gate legit pairing.
pub const DEFAULT_PAIR_LIMIT_PER_MIN: u32 = 5;
pub const DEFAULT_PAIR_LIMIT_PER_DAY: u32 = 20;

const DEFAULT_ORIGINS: [&str; 2] = ["http://localhost:13000", "http://127.0.0.1:13000"];

/// A boot-time configuration error.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct EnvError(pub String);

/// Parsed `HYPHA_*` environment — every field the substrate routes read.
#[derive(Clone, Debug)]
pub struct Env {
    /// `HYPHA_PORT` (falls back to the generic `PORT`).
    pub port: u16,
    /// `HYPHA_HOST`.
    pub host: String,
    /// `HYPHA_TOKEN` — bearer auth; required on non-loopback binds.
    pub token: Option<String>,
    /// `HYPHA_HOME` — hypha-owned data dir (node.json, pair-code, tokens, push).
    pub home: PathBuf,
    /// `HYPHA_NODE` — stable node identity file.
    pub node_path: PathBuf,
    /// `HYPHA_NAME` — display name reported by `GET /api/node`.
    pub node_name: String,
    /// `HYPHA_ORIGINS` — CORS allowlist (`*` echoes any origin).
    pub origins: Vec<String>,
    /// `HYPHA_SSE_KEEPALIVE_MS` — `/api/events` keep-alive cadence; `0` disables.
    pub sse_keep_alive: Duration,
    /// `HYPHA_PAIR_LIMIT_PER_MIN` — `/api/pair` attempts/min per client
    /// IP (sliding window); `0` disables this window.
    pub pair_limit_per_min: u32,
    /// `HYPHA_PAIR_LIMIT_PER_DAY` — `/api/pair` attempts/24h per client
    /// IP; `0` disables. Both at `0` turns the throttle off entirely.
    pub pair_limit_per_day: u32,
    /// `<home>/pair-code` — written by the pairing CLI, consumed on read.
    pub pair_code_path: PathBuf,
    /// `<home>/tokens.json` — sha256 hashes of issued pair credentials.
    pub tokens_path: PathBuf,
    /// `HYPHA_PUSH`/`HYPHA_PUSH_PATH` — the push config document
    /// (VAPID keys + subscriptions); `<home>/push.json` by default.
    pub push_path: PathBuf,
    /// `HYPHA_KV`/`HYPHA_KV_PATH` — the node-local KV document
    /// (`/api/kv*`); `<home>/kv.json` by default.
    pub kv_path: PathBuf,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

fn hostname_or_default() -> String {
    nix::unistd::gethostname()
        .ok()
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

/// `serve`'s bind guard — loopback binds may go tokenless.
pub fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "::1" || host == "[::1]" || host.starts_with("127.")
}

/// `HYPHA_SSE_KEEPALIVE_MS` style parse: absent → default; a non-finite
/// or negative value falls back to the default; `0` disables.
fn ms_or_default(raw: Option<&String>, default: u64) -> u64 {
    let Some(raw) = raw else { return default };
    if raw.is_empty() {
        return default;
    }
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => v.floor() as u64,
        _ => default,
    }
}

/// Count-valued knob: absent/unparseable → default (never silently 0 —
/// a typo must not disable the knob), `0` is the explicit off switch.
fn count_or_default(raw: Option<&String>, default: u32) -> u32 {
    raw.filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

impl Env {
    /// Parse from the process environment; fails fast on bad values.
    ///
    /// # Errors
    /// `EnvError` on a bad port or a non-loopback bind without
    /// `HYPHA_TOKEN`.
    pub fn parse() -> Result<Self, EnvError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Test seam — parse from a key/value map instead of `std::env`.
    pub fn from_map(vars: &HashMap<String, String>) -> Result<Self, EnvError> {
        Self::from_lookup(|key| vars.get(key).cloned())
    }

    /// Shared parser behind [`Env::parse`]/[`Env::from_map`].
    ///
    /// # Errors
    /// See [`Env::parse`].
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, EnvError> {
        let home_dir = home_dir();

        // The namespaced var is preferred; the generic `PORT` is the
        // fallback for bare launches.
        let raw_port = get("HYPHA_PORT").or_else(|| get("PORT"));
        let port: u16 = match raw_port.as_deref() {
            None | Some("") => DEFAULT_PORT,
            Some(raw) => match raw.parse::<f64>() {
                Ok(v) if v.fract() == 0.0 && (1.0..=65535.0).contains(&v) => v as u16,
                _ => {
                    return Err(EnvError(format!(
                        "PORT must be an integer between 1 and 65535, got \"{raw}\""
                    )));
                }
            },
        };

        let origins: Vec<String> = get("HYPHA_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let origins = if origins.is_empty() {
            DEFAULT_ORIGINS.iter().map(|s| (*s).to_string()).collect()
        } else {
            origins
        };

        let home =
            get("HYPHA_HOME").map_or_else(|| home_dir.join(".local/share/hypha"), PathBuf::from);

        let host = get("HYPHA_HOST").unwrap_or_else(|| DEFAULT_HOST.to_string());
        let token = get("HYPHA_TOKEN");

        // The bind guard, enforced at parse so a bad deployment fails
        // fast at construction rather than mid-request.
        if !is_loopback_host(&host) && token.as_deref().unwrap_or("").is_empty() {
            return Err(EnvError(format!(
                "hypha refuses to bind {host} without HYPHA_TOKEN. Set HYPHA_TOKEN or bind a loopback address (HYPHA_HOST=127.0.0.1)."
            )));
        }

        let node_name = get("HYPHA_NAME")
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(hostname_or_default);

        Ok(Self {
            port,
            host,
            token,
            home: home.clone(),
            node_path: get("HYPHA_NODE").map_or_else(|| home.join("node.json"), PathBuf::from),
            node_name,
            origins,
            sse_keep_alive: Duration::from_millis(ms_or_default(
                get("HYPHA_SSE_KEEPALIVE_MS").as_ref(),
                DEFAULT_SSE_KEEPALIVE_MS,
            )),
            pair_limit_per_min: count_or_default(
                get("HYPHA_PAIR_LIMIT_PER_MIN").as_ref(),
                DEFAULT_PAIR_LIMIT_PER_MIN,
            ),
            pair_limit_per_day: count_or_default(
                get("HYPHA_PAIR_LIMIT_PER_DAY").as_ref(),
                DEFAULT_PAIR_LIMIT_PER_DAY,
            ),
            pair_code_path: home.join("pair-code"),
            tokens_path: home.join("tokens.json"),
            push_path: get("HYPHA_PUSH")
                .or_else(|| get("HYPHA_PUSH_PATH"))
                .map_or_else(|| home.join("push.json"), PathBuf::from),
            kv_path: get("HYPHA_KV")
                .or_else(|| get("HYPHA_KV_PATH"))
                .map_or_else(|| home.join("kv.json"), PathBuf::from),
        })
    }
}
