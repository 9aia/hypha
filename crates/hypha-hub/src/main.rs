//! hypha-hub — the node-API gateway binary. Multi-node sync engine
//! plus HTTP/SSE bridges to the node daemon (`HYPHA_NODE_URL`).
//!
//! Env: `HYPHA_HUB_HOST`/`HYPHA_HUB_PORT` (or `HYPHA_PORT`),
//! `HYPHA_HUB_TOKEN` (required on non-loopback binds),
//! `HYPHA_NODE_URL`, `HYPHA_NODE_TOKEN`, `HYPHA_NODES`
//! (`id=url[@token];…` for multi-node), `HYPHA_HOME`,
//! `HYPHA_CREDENTIAL_STORE` (`auto|keyring|file`).

use hypha_hub::{HubConfig, hub_state, router};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // fmt + RUST_LOG as always; OTLP span export layered on when
    // HYPHA_OTEL_ENDPOINT / OTEL_EXPORTER_OTLP_* is set. The guard's
    // Drop flushes the provider on exit.
    let _telemetry = hypha_telemetry::init();

    let config = HubConfig::from_env().map_err(std::io::Error::other)?;
    let addr = config.socket_addr().map_err(std::io::Error::other)?;
    let state = hub_state(&config).map_err(std::io::Error::other)?;
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, node = %config.node_url, "hypha-hub listening");
    // Graceful SIGINT/SIGTERM — mirrors the node's shutdown so an
    // operator's ^C (or a service stop) closes listeners cleanly.
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let Ok(mut term) = signal(SignalKind::terminate()) else {
                    let _ = tokio::signal::ctrl_c().await;
                    tracing::info!("hypha-hub shutting down");
                    return;
                };
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            tracing::info!("hypha-hub shutting down");
        })
        .await
}
