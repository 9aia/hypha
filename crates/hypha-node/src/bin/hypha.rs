//! `hypha` — the node host binary. `hypha serve` runs the substrate
//! daemon with zero integration kinds; integrations ship their own
//! binaries that compose [`hypha_node::serve_state`] with their planes.
//!
//! Env: `HYPHA_HOST`/`HYPHA_PORT` (or `PORT`), `HYPHA_TOKEN` (required
//! on non-loopback binds), `HYPHA_HOME`, `HYPHA_NODE`, `HYPHA_NAME`,
//! `HYPHA_ORIGINS`, `HYPHA_SSE_KEEPALIVE_MS`, `HYPHA_PAIR_LIMIT_*`,
//! `HYPHA_PUSH`/`HYPHA_PUSH_PATH`, and the OTEL vars
//! (`LAMPY_OTEL_*`/`HYPHA_OTEL_*`/`OTEL_EXPORTER_OTLP_*`).

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "hypha",
    version,
    about = "Hypha node host — the entity substrate daemon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the substrate node (`/api/*`).
    Serve,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // fmt + RUST_LOG as always; OTLP span export layered on when the
    // OTEL endpoint vars are set (LAMPY_OTEL_* canonical,
    // HYPHA_OTEL_* alias, OTEL_EXPORTER_OTLP_* standard). The guard's
    // Drop flushes on exit.
    let spec = lampy_telemetry::TelemetrySpec::service("hypha").with_alias("HYPHA");
    let _telemetry = lampy_telemetry_otel::init(&spec, env!("CARGO_PKG_NAME"));

    let cli = Cli::parse();
    match cli.command {
        Command::Serve => {
            let env = hypha_http::env::Env::parse().map_err(std::io::Error::other)?;
            hypha_node::serve(&env).await
        }
    }
}
