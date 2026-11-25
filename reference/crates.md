---
url: /hypha/reference/crates.md
---
# Crates

Rust workspace, ports-and-adapters. Lints are strict (pedantic warn,
`unwrap_used`/`expect_used` warn, `unsafe_code` forbid); edition
2024, rust-version 1.85.

| crate | layer | purpose |
|---|---|---|
| `hypha-control` | domain | Arbitration — lock registry, takeover policy, merge/route |
| `hypha-kv` | port | Node-local key/value store port — namespaced JSON values with per-namespace visibility (P-006) |
| `hypha-node-api` | port | Node API port + wire DTOs — the hub/client ↔ node contract. NodeApi trait, ureq client, FakeNodeApi stub, rewind selector, convert targets. No leptos, no DOM. |
| `hypha-http` | adapter | Hypha HTTP substrate — axum adapter serving /api/entities\*, /api/events, node/pairing/auth, and push over a HyphaState |
| `hypha-kv-file` | adapter | JSON-file impl of the hypha-kv store port — atomic tmp+rename writes (P-006) |
| `hypha-outbox` | service | Durable per-entity FIFO outbox for writes queued while a node is offline |
| `hypha-push` | service | Web Push notifications over a JSON document store — VAPID keys persist in config; subscriptions prune on 404/410 |
| `hypha-sync` | service | Hub-side sync layer — SQLite projection of paired nodes' entity indexes + outbox drain on reconnect |
| `hypha-hub` | engine | Entity-API gateway — multi-node sync engine, HTTP/SSE bridges, push, auth |
| `hypha-node` | engine | Node host — HyphaState composition + serve() for the substrate daemon |
| `hypha-serve-cli` | engine | One-shot argv→op-params shim for ndjson \*-serve binaries — the generic CLI front door (P-020). |
| `xtask` | tooling | hypha-xtask — workspace automation. |

Migration state lives in org-ledger plan **PL-005**; crate-level
API docs build via `bun run docs:api` (cargo doc →
`public/api/`).
