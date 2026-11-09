//! hypha-http — the axum driving adapter for the hypha entity
//! substrate. Serves the kind-agnostic `/api/*` surface: entity
//! planes (`/api/entities*`), the node event feed (`/api/events`),
//! node identity, pairing + bearer auth, directory/user info, and web
//! push subscriptions. Integrations (peregrin's session plane, future
//! campaign planes) compose [`app`] with their own routes and register
//! [`EntityPlane`]s through [`HyphaState::with_kind`]; the
//! `EntityContext::host` downcast is how planes reach host state.

pub mod auth;
pub mod entity_plane;
pub mod env;
pub mod feed;
pub mod node;
pub mod pair;
pub mod ratelimit;
pub mod routes;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Router, serve as axum_serve};
use tracing::Instrument;

pub use entity_plane::{EntityContext, EntityPlane, KindRegistry};
pub use env::{Env, EnvError};
pub use feed::{EventFeed, FeedEvent, MAX_QUEUED};
pub use node::{
    HYPHA_VERSION, NodeIdentity, PROTOCOL_VERSION, load_node_identity, save_node_identity,
};
pub use pair::{Pairing, PairingStore};
pub use ratelimit::{Quota, RateLimiter};

/// Shared state threaded through every substrate handler — the
/// `createApp` options object minus everything an integration owns
/// (session planes, meta stores, campaign state). `Clone` is cheap
/// (Arc/Mutex/registry clones) because axum clones state per request.
#[derive(Clone)]
pub struct HyphaState {
    /// `kind` → [`EntityPlane`] registry behind `/api/entities*`.
    /// Empty on a bare substrate — integrations register their kinds
    /// at composition ([`Self::with_kind`]).
    pub kinds: KindRegistry,
    /// Process-wide event feed behind `GET /api/events`.
    pub feed: EventFeed,
    /// Identity reported by `GET /api/node`; `PATCH /api/node` renames
    /// it in place (a mutex because `HyphaState` clones per request).
    pub node: Arc<Mutex<NodeIdentity>>,
    /// Where the identity persists (`HYPHA_NODE`); `None` keeps a
    /// `PATCH /api/node` rename in-memory only.
    pub node_path: Option<PathBuf>,
    /// Agent ids advertised by `GET /api/node` — the integration's
    /// runtime inventory (empty on a bare substrate; peregrin fills it
    /// from its session plane).
    pub node_agents: Vec<String>,
    /// Pairing backend for `POST /api/pair` + issued-credential auth.
    pub pairing: Option<Pairing>,
    /// Per-client-IP throttle on `POST /api/pair` — the one route that
    /// mints a bearer credential with no prior auth, so it's the brute-
    /// force surface. Keyed by `ConnectInfo` socket IP (never
    /// `X-Forwarded-For` — the node doesn't run behind a trusted proxy).
    pub pair_limit: RateLimiter,
    /// `HYPHA_TOKEN` — `None`/empty means bearer auth is off entirely.
    pub token: Option<String>,
    /// CORS allowlist — `*` echoes any Origin back (bearer auth, not
    /// CORS, is the gate).
    pub origins: Vec<String>,
    /// SSE keep-alive cadence; `0` disables.
    pub keep_alive: Duration,
    /// Web Push service — `None` → `/api/push/*` answers 501.
    pub push: Option<Arc<hypha_push::PushStore>>,
}

impl HyphaState {
    /// Minimal state — tokenless, no pairing, no push, ephemeral node
    /// id, empty kind registry.
    pub fn new() -> Self {
        Self {
            kinds: KindRegistry::new(),
            feed: EventFeed::new(),
            node: Arc::new(Mutex::new(NodeIdentity {
                id: format!("node_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]),
                name: hostname_or_unknown(),
                version: HYPHA_VERSION.to_string(),
            })),
            node_path: None,
            node_agents: Vec::new(),
            pairing: None,
            pair_limit: RateLimiter::pair(
                env::DEFAULT_PAIR_LIMIT_PER_MIN,
                env::DEFAULT_PAIR_LIMIT_PER_DAY,
            ),
            token: None,
            origins: vec![
                "http://localhost:13000".to_string(),
                "http://127.0.0.1:13000".to_string(),
            ],
            keep_alive: Duration::from_millis(env::DEFAULT_SSE_KEEPALIVE_MS),
            push: None,
        }
    }

    /// Assemble state from a parsed [`Env`] — node identity at
    /// `HYPHA_NODE`, pairing at `<home>/pair-code` +
    /// `<home>/tokens.json`, push over the `HYPHA_PUSH` document.
    pub fn from_env(env: &Env) -> Self {
        Self {
            kinds: KindRegistry::new(),
            feed: EventFeed::new(),
            node: Arc::new(Mutex::new(load_node_identity(
                &env.node_path,
                &env.node_name,
            ))),
            node_path: Some(env.node_path.clone()),
            node_agents: Vec::new(),
            pairing: Some(Pairing::open(
                env.pair_code_path.clone(),
                env.tokens_path.clone(),
            )),
            pair_limit: RateLimiter::pair(env.pair_limit_per_min, env.pair_limit_per_day),
            token: env.token.clone(),
            origins: env.origins.clone(),
            keep_alive: env.sse_keep_alive,
            push: Some(Arc::new(hypha_push::PushStore::open(
                hypha_push::JsonDocStore::open(&env.push_path),
            ))),
        }
    }

    /// The [`EntityContext`] the `/api/entities*` surface hands to
    /// every [`EntityPlane`] — the kind registry plus this
    /// `HyphaState` as the opaque host payload (integrations downcast
    /// it back to reach their own state).
    pub fn entity_context(&self) -> EntityContext {
        EntityContext::new(self.kinds.clone(), Arc::new(self.clone()))
    }

    /// Register an entity kind plane behind `/api/entities*` — the
    /// composition hook for every kind.
    #[must_use]
    pub fn with_kind(mut self, plane: Arc<dyn EntityPlane>) -> Self {
        self.kinds.register(plane);
        self
    }

    /// Advertise agent ids on `GET /api/node` (integration runtime
    /// inventory).
    #[must_use]
    pub fn with_agents(mut self, agents: Vec<String>) -> Self {
        self.node_agents = agents;
        self
    }

    #[must_use]
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
    }

    #[must_use]
    pub fn with_pairing(mut self, pairing: Pairing) -> Self {
        self.pairing = Some(pairing);
        self
    }

    /// Override the `/api/pair` throttle (tests inject tiny quotas).
    #[must_use]
    pub fn with_pair_limit(mut self, pair_limit: RateLimiter) -> Self {
        self.pair_limit = pair_limit;
        self
    }

    #[must_use]
    pub fn with_node(mut self, node: NodeIdentity) -> Self {
        self.node = Arc::new(Mutex::new(node));
        self
    }

    /// Persist `PATCH /api/node` renames to `path` (`HYPHA_NODE`).
    #[must_use]
    pub fn with_node_path(mut self, path: PathBuf) -> Self {
        self.node_path = Some(path);
        self
    }

    /// Snapshot of the node identity — one lock, one clone.
    pub fn node_identity(&self) -> NodeIdentity {
        self.node
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn with_origins(mut self, origins: Vec<String>) -> Self {
        self.origins = origins;
        self
    }

    #[must_use]
    pub fn with_keep_alive(mut self, keep_alive: Duration) -> Self {
        self.keep_alive = keep_alive;
        self
    }

    /// Override the push service (tests wire a stub store, or `None`
    /// to answer 501).
    #[must_use]
    pub fn with_push(mut self, push: Option<Arc<hypha_push::PushStore>>) -> Self {
        self.push = push;
        self
    }
}

impl Default for HyphaState {
    fn default() -> Self {
        Self::new()
    }
}

fn hostname_or_unknown() -> String {
    nix::unistd::gethostname()
        .ok()
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// CORS — port of `corsHeaders` + the OPTIONS preflight: `vary: Origin`
/// on every response, and the allow-* triple only when the request's
/// Origin is in the allowlist (or the allowlist carries `*`).
async fn cors_layer(State(state): State<HyphaState>, req: Request<Body>, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let allowed: HashSet<&str> = state.origins.iter().map(String::as_str).collect();
    let mut res = if req.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    let headers = res.headers_mut();
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    if let Some(origin) = origin {
        if allowed.contains(origin.as_str()) || allowed.contains("*") {
            if let Ok(v) = HeaderValue::from_str(&origin) {
                headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            }
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET,POST,PATCH,DELETE,OPTIONS"),
            );
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("content-type,authorization"),
            );
        }
    }
    res
}

/// OTEL server span — the outermost layer. Extracts the W3C remote
/// parent (`traceparent`/`tracestate`) so proxied hub→node calls join
/// the hub's trace; requests without context get a local-root
/// `http_request` span. Inert (plain span) when export is off.
async fn trace_layer(req: Request<Body>, next: Next) -> Response {
    let span = hypha_telemetry::server_span(req.method(), req.uri().path(), req.headers());
    let for_status = span.clone();
    async move {
        let res = next.run(req).await;
        hypha_telemetry::record_response(&for_status, res.status().as_u16());
        res
    }
    .instrument(span)
    .await
}

/// One line per request — `/api/health` excluded, like the TS logger.
async fn log_layer(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let started = Instant::now();
    let res = next.run(req).await;
    if path != "/api/health" {
        // Hoisted — `tracing` fields evaluate only when a subscriber
        // is attached, which coverage runs don't guarantee.
        let status = res.status().as_u16();
        let elapsed_ms = started.elapsed().as_millis();
        tracing::info!("{method} {path} {status} {elapsed_ms}ms");
    }
    res
}

/// Build the substrate axum router — only the kind-agnostic surface:
/// health, pairing + auth bootstrap, node, fs, user, the event feed,
/// `/api/entities*`, and `/api/push/*`. Every route is `any()` + an
/// internal method guard so wrong-method requests fall through to the
/// 404 (rather than axum's bare 405). Integrations nest their own
/// routes around or inside this router and register entity planes on
/// the shared [`HyphaState`].
pub fn app(state: HyphaState) -> Router {
    Router::new()
        // Pre-auth routes — the auth gate itself passes these through
        // (health for probes; pair/login are the credential bootstrap).
        .route("/api/health", any(routes::misc::health))
        .route("/api/pair", any(auth::pair))
        .route("/api/auth/login", any(auth::handle))
        .route("/api/auth/logout", any(auth::handle))
        .route("/api/user", any(routes::misc::user))
        .route("/api/node", any(routes::misc::node))
        .route("/api/fs", any(routes::misc::fs))
        .route("/api/events", any(routes::events::events))
        .route("/api/entities", any(routes::entities::collection))
        .route(
            "/api/entities/snapshot",
            any(routes::entities::snapshot_collection),
        )
        .route("/api/entities/{id}", any(routes::entities::item))
        .route("/api/entities/{id}/ops", any(routes::entities::ops))
        .route(
            "/api/entities/{id}/snapshot",
            any(routes::entities::snapshot_item),
        )
        .route("/api/push/vapid", any(routes::push::vapid))
        .route("/api/push/subscribe", any(routes::push::subscribe))
        .fallback(async || routes::not_found())
        // Every body read goes through `read_json_body` with an
        // explicit per-route cap (SMALL/MAX_JSON_BODY); axum's
        // 2MiB `DefaultBodyLimit` stays on as the backstop for any
        // future extractor-based read.
        // Innermost → outermost: auth gate, request log, CORS, trace.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_gate,
        ))
        .layer(middleware::from_fn(log_layer))
        .layer(middleware::from_fn_with_state(state.clone(), cors_layer))
        .layer(middleware::from_fn(trace_layer))
        .with_state(state)
}

/// Bind `env.host:env.port` and serve the app. The bind guard already
/// ran in [`Env::parse`]; callers wanting graceful shutdown own the
/// listener themselves.
///
/// # Errors
/// `std::io::Error` on bind/serve failure.
pub async fn serve(env: &Env, state: HyphaState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind((env.host.as_str(), env.port)).await?;
    tracing::info!("hypha-http listening on {}:{}", env.host, env.port);
    // `ConnectInfo` feeds the /api/pair limiter its client-IP key.
    let serving = axum_serve(
        listener,
        app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    #[cfg(not(coverage))]
    {
        serving.await
    }
    #[cfg(coverage)]
    {
        // The accept loop never returns; under coverage a ready arm
        // lets `serve` complete so the function tail counts.
        tokio::select! {
            r = serving => r,
            () = std::future::ready(()) => Ok(()),
        }
    }
}
