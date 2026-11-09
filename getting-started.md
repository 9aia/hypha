# Getting started

Hypha is the distributed substrate extracted from peregrin's entity
plane (org plan PL-005). It is **pre-binary**: the leaf crates are
landed and tested; the node host (`hypha serve`), the HTTP surface
(`hypha-http`), and the hub gateway (`hypha-hub`) land in phases
P2–P3 of the migration plan.

## What exists today

| Crate | What it gives you |
| --- | --- |
| `hypha-node-api` | The node↔hub contract — `EntityApi` trait, wire DTOs, `HttpEntityApi` client, `FakeEntityApi` for tests |
| `hypha-outbox` | Durable per-entity FIFO op queue (SQLite) — writes survive node downtime |
| `hypha-control` | Pure arbitration — lock registry, takeover decisions, `pick_owner` routing |
| `hypha-telemetry` | Opt-in OTEL layer over `tracing` (`HYPHA_OTEL_*` env vars) |
| `hypha-secret` + `-keyring`/`-dotenv` | Secrets port — names, not values |
| `hypha-service` + `-systemd` | Service-install port + systemd impl |

## Build from source

```bash
git clone git@github.com:9aia/hypha.git
cd hypha
./scripts/setup.sh      # wires git hooks
cargo xtask check       # fmt + clippy -D warnings
cargo xtask test        # whole suite (nextest if present)
```

Rust 1.98 (see `rust-toolchain.toml`), edition 2024.

## Where next

- [Your first entity](tutorials/first-entity.md) — the entity model
  end to end, with wire shapes.
- [Architecture](explanation/architecture.md) — planes, nodes, hub.
- [Boundary](explanation/boundary.md) — what hypha owns vs sepia and
  peregrin.
