# Crates

Rust workspace, ports-and-adapters. Lints are strict (pedantic warn,
`unwrap_used`/`expect_used` warn, `unsafe_code` forbid); edition
2024, rust-version 1.85.

| Crate | Role | Status |
| --- | --- | --- |
| `hypha-node-api` | `EntityApi` trait + wire DTOs + `HttpEntityApi`/`FakeEntityApi` | landed |
| `hypha-outbox` | durable per-entity FIFO op queue (SQLite) | landed |
| `hypha-control` | pure arbitration — locks, takeover, `pick_owner` | landed |
| `hypha-telemetry` | opt-in OTEL over `tracing` | landed |
| `hypha-secret` | secrets port (`get`/`set`, names not values) | landed |
| `hypha-keyring` | OS-keyring impl (`secret-tool`) | landed |
| `hypha-dotenv` | dotenv-file impl | landed |
| `hypha-service` | service-install port | landed |
| `hypha-systemd` | systemd unit render/install impl | landed |
| `hypha-sync` | entity-index projection + sync engine (ex-`peregrin-sync`) | landed |
| `hypha-push` | Web Push over a JSON doc store — VAPID, subscriptions, 404/410 prune (ex-`peregrin-push`) | landed |
| `hypha-http` | substrate HTTP surface — `/api/entities*` kind planes, `/api/events` feed, node/pairing/auth, `/api/push/*` (ex-`peregrin-http` split); node host still P3 | landed |
| `hypha-hub` | aggregation gateway (ex-`peregrin-hub`) | P3 |
| `hypha` / `hypha-hub` bins | `hypha serve` node host, hub binary | P3 |

Migration state lives in org-ledger plan **PL-005**; crate-level
API docs build via `bun run docs:api` (cargo doc →
`public/api/`).
