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

use std::any::Any;
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

/// Async agent-inventory provider behind `GET /api/node`'s `agents`
/// field — integrations whose roster is a live query (e.g. a session
/// plane's `list_agents`) install one via
/// [`HyphaState::with_agent_lister`]. When unset the static
/// [`HyphaState::node_agents`] list is reported.
pub type AgentLister =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, Vec<String>> + Send + Sync>;

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
    /// from its session plane). Static fallback: a live
    /// [`Self::agent_lister`] wins when installed.
    pub node_agents: Vec<String>,
    /// Live agent-inventory provider for `GET /api/node` — see
    /// [`AgentLister`]. `None` → `node_agents` is reported verbatim.
    pub agent_lister: Option<AgentLister>,
    /// Full `capabilities` override for `GET /api/node` — integrations
    /// whose wire contract names its own capability tags (peregrin's
    /// `sessions`/`projects`/`import`) install the exact list; `None`
    /// → the substrate computes `entities`/`events`/`export` plus
    /// `push`/`pairing` when those backends are wired.
    pub node_capabilities: Option<Vec<String>>,
    /// Opaque payload the [`EntityContext`] handed to every
    /// [`EntityPlane`] carries as `host` — integrations whose planes
    /// wrap host handlers (peregrin's session plane downcasts it back
    /// to `AppState`) install their own state via
    /// [`Self::with_entity_host`]. `None` → the `HyphaState` itself.
    pub entity_host: Option<Arc<dyn Any + Send + Sync>>,
    /// Auth-cookie name the login/logout exchange sets and the gate
    /// reads — `hypha_token` on a bare substrate; integrations keep
    /// their historical name (peregrin serves `sepia_token`).
    pub auth_cookie: Arc<str>,
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
    /// Deep health probe for `GET /api/health` — integrations with a
    /// real liveness check (peregrin's session-plane probe) install a
    /// handler returning the full response; `None` → the static
    /// `{"ok":true}` liveness answer.
    pub health_probe: Option<HealthProbe>,
}

/// A `GET /api/health` override — returns the complete response
/// (status + body) the integration wants probes to see.
pub type HealthProbe = Arc<dyn Fn() -> futures::future::BoxFuture<'static, Response> + Send + Sync>;

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
            agent_lister: None,
            node_capabilities: None,
            entity_host: None,
            auth_cookie: Arc::from(auth::AUTH_COOKIE),
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
            health_probe: None,
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
            agent_lister: None,
            node_capabilities: None,
            entity_host: None,
            auth_cookie: Arc::from(auth::AUTH_COOKIE),
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
            health_probe: None,
        }
    }

    /// The [`EntityContext`] the `/api/entities*` surface hands to
    /// every [`EntityPlane`] — the kind registry plus the host payload:
    /// [`Self::entity_host`] when an integration installed one (planes
    /// downcast it back to their own state), else this `HyphaState`.
    pub fn entity_context(&self) -> EntityContext {
        let host = self
            .entity_host
            .clone()
            .unwrap_or_else(|| Arc::new(self.clone()));
        EntityContext::new(self.kinds.clone(), host)
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

    /// Install a live agent-inventory provider — wins over the static
    /// [`Self::node_agents`] list on `GET /api/node`.
    #[must_use]
    pub fn with_agent_lister(mut self, lister: AgentLister) -> Self {
        self.agent_lister = Some(lister);
        self
    }

    /// Override the `capabilities` list `GET /api/node` reports —
    /// integrations whose wire contract names its own tags install the
    /// exact list (computed per-request state like `push`/`pairing`
    /// is the caller's job).
    #[must_use]
    pub fn with_node_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.node_capabilities = Some(capabilities);
        self
    }

    /// Install the opaque [`EntityContext::host`] payload planes
    /// downcast — e.g. an integration's own app state.
    #[must_use]
    pub fn with_entity_host(mut self, host: Arc<dyn Any + Send + Sync>) -> Self {
        self.entity_host = Some(host);
        self
    }

    /// Rename the auth cookie the login/logout exchange sets and the
    /// gate reads (default `hypha_token`).
    #[must_use]
    pub fn with_auth_cookie(mut self, name: impl Into<Arc<str>>) -> Self {
        self.auth_cookie = name.into();
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

    /// Install a deep `GET /api/health` probe — see
    /// [`Self::health_probe`].
    #[must_use]
    pub fn with_health_probe(mut self, probe: HealthProbe) -> Self {
        self.health_probe = Some(probe);
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

/// Build the substrate route table + middleware WITHOUT the terminal
/// fallback — returns `Router<HyphaState>` so an integration can
/// `.with_state(...)` it and `merge` it with its own router under one
/// listener. axum panics merging two routers that both carry a custom
/// fallback, so the composer installs exactly one (see
/// [`app_routes`] vs [`app`]).
///
/// Only the kind-agnostic surface: health, pairing + auth bootstrap,
/// node, fs, user, the event feed, `/api/entities*` plus the
/// `/api/entity-index`/`/api/pending-ops` reads, and `/api/push/*`.
/// Every route is `any()` + an internal method guard so
/// wrong-method requests fall through to the 404 (rather than axum's
/// bare 405). Integrations nest their own routes around or inside
/// this router and register entity planes on the shared
/// [`HyphaState`].
pub fn app_routes(state: &HyphaState) -> Router<HyphaState> {
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
        .route("/api/entity-index", any(routes::entities::entity_index))
        .route("/api/pending-ops", any(routes::entities::pending_ops))
        .route("/api/push/vapid", any(routes::push::vapid))
        .route("/api/push/subscribe", any(routes::push::subscribe))
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
}

/// The standalone substrate router — [`app_routes`] plus the 404
/// fallback and the state applied. Integrations composing several
/// routers under one listener use [`app_routes`] instead and install
/// their own single fallback.
pub fn app(state: HyphaState) -> Router {
    app_routes(&state)
        .fallback(async || routes::not_found())
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
