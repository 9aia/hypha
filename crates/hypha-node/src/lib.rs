//! hypha-node — the node host library: the thin composition layer
//! between [`hypha_http::Env`] and the substrate axum app.
//!
//! A bare `hypha serve` mounts zero entity kinds — the substrate
//! surface (`/api/health`, `/api/node`, `/api/events`, `/api/pair`,
//! `/api/auth/*`, `/api/user`, `/api/fs`, `/api/push/*`,
//! `/api/entity-index`, `/api/pending-ops`, and the `entity.*` ops
//! namespace) still works end-to-end. Integrations
//! (peregrin's session plane, campaign planes, …) compose
//! [`state`] themselves: build a [`HyphaState`], register their
//! [`EntityPlane`]s via [`HyphaState::with_kind`], and hand it to
//! [`serve_state`].

use std::net::SocketAddr;

use hypha_http::{Env, HyphaState, app};

/// The `HyphaState` a bare node serves — [`HyphaState::from_env`]
/// with no kind planes registered. Integrations wanting planes call
/// `HyphaState::from_env(env)` directly and `.with_kind(...)`.
pub fn state(env: &Env) -> HyphaState {
    HyphaState::from_env(env)
}

/// Build state from env and serve until shutdown — the whole
/// `hypha serve` boot path. Call on a tokio runtime; returns when the
/// listener fails or after graceful shutdown (SIGINT/SIGTERM).
///
/// # Errors
/// `std::io::Error` on bind or serve failure.
pub async fn serve(env: &Env) -> std::io::Result<()> {
    serve_state(env, state(env)).await
}

/// Serve a pre-composed [`HyphaState`] — the integration entry point.
/// Same bind + graceful-shutdown contract as [`serve`].
///
/// # Errors
/// `std::io::Error` on bind or serve failure.
pub async fn serve_state(env: &Env, state: HyphaState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind((env.host.as_str(), env.port)).await?;
    tracing::info!("hypha-node listening on {}:{}", env.host, env.port);
    // `ConnectInfo` feeds the /api/pair limiter its client-IP key.
    let serving = axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    );
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! {
            r = serving => r,
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("hypha-node shutting down");
                Ok(())
            }
            _ = term.recv() => {
                tracing::info!("hypha-node shutting down");
                Ok(())
            }
        }
    }
    #[cfg(not(unix))]
    {
        serving
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("hypha-node shutting down");
            })
            .await
    }
}
