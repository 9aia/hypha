//! `hypha` — the node host binary. `hypha serve` runs the substrate
//! daemon with zero integration kinds; integrations ship their own
//! binaries that compose [`hypha_node::serve_state`] with their planes.
//!
//! Env: `HYPHA_HOST`/`HYPHA_PORT` (or `PORT`), `HYPHA_TOKEN` (required
//! on non-loopback binds), `HYPHA_HOME`, `HYPHA_NODE`, `HYPHA_NAME`,
//! `HYPHA_ORIGINS`, `HYPHA_SSE_KEEPALIVE_MS`, `HYPHA_PAIR_LIMIT_*`,
//! `HYPHA_PUSH`/`HYPHA_PUSH_PATH`, and the OTEL vars
//! (`HYPHA_OTEL_*`/`OTEL_EXPORTER_OTLP_*`).

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
    // OTEL endpoint vars are set. The guard's Drop flushes on exit.
    let _telemetry = hypha_telemetry::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve => {
            let env = hypha_http::env::Env::parse().map_err(std::io::Error::other)?;
            hypha_node::serve(&env).await
        }
    }
}
