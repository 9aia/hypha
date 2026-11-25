---
url: /hypha/reference/environment.md
---
# Environment

Hypha-native variables (landed crates):

| Variable | Crate | Meaning |
| --- | --- | --- |
| `LAMPY_OTEL_ENDPOINT` | `lampy-telemetry` | OTLP base endpoint (canonical); `/v1/traces` appended when no path present |
| `HYPHA_OTEL_ENDPOINT` | `lampy-telemetry` | legacy alias for the endpoint — still honored |
| `LAMPY_OTEL_SERVICE_NAME` / `HYPHA_OTEL_SERVICE_NAME` | `lampy-telemetry` | service.name (default `"hypha"`, `OTEL_SERVICE_NAME` also honored) |
| `OTEL_EXPORTER_OTLP_*` | `lampy-telemetry` | standard OTLP vars also enable export |

Telemetry lives in [9aia/lampy](https://github.com/9aia/lampy)
(`lampy-telemetry` port + `lampy-telemetry-otel` impl, P-042) —
`LAMPY_OTEL_*` is the canonical prefix, `HYPHA_OTEL_*` a supported
per-consumer alias.

Standard OTEL vars (`OTEL_*`) are honored per OpenTelemetry semantic
conventions.

Node host (`hypha serve`, `hypha-http::env::Env`):

| Variable | Meaning |
| --- | --- |
| `HYPHA_HOST` / `HYPHA_PORT` (or `PORT`) | bind address (default `127.0.0.1:18787`) |
| `HYPHA_TOKEN` | bearer auth; required on non-loopback binds |
| `HYPHA_HOME` | hypha data dir (default `~/.local/share/hypha`) |
| `HYPHA_NODE` | node identity file (default `<home>/node.json`) |
| `HYPHA_NAME` | display name on `GET /api/node` |
| `HYPHA_ORIGINS` | CORS allowlist |
| `HYPHA_SSE_KEEPALIVE_MS` | `/api/events` keep-alive cadence (`0` disables) |
| `HYPHA_PAIR_LIMIT_PER_MIN` / `HYPHA_PAIR_LIMIT_PER_DAY` | `/api/pair` throttle per client IP |
| `HYPHA_PUSH` / `HYPHA_PUSH_PATH` | push config document (default `<home>/push.json`) |
| `HYPHA_KV` / `HYPHA_KV_PATH` | node-local KV document for `/api/kv*` (default `<home>/kv.json`) |

Hub (`hypha-hub`, `hypha_hub::HubConfig` + `hub_state_from_with`):

| Variable | Meaning |
| --- | --- |
| `HYPHA_HUB_HOST` / `HYPHA_HUB_PORT` (or `HYPHA_PORT`) | bind (default `127.0.0.1:13000`) |
| `HYPHA_HUB_TOKEN` | client→hub auth; required on non-loopback binds |
| `HYPHA_NODE_URL` / `HYPHA_NODE_TOKEN` | single-node upstream shorthand |
| `HYPHA_NODES` | `id=url[@token];…` multi-node registry |
| `HYPHA_HOME` | hub data under `<home>/hub/` (projection, outbox, push, credentials) |
| `HYPHA_CREDENTIAL_STORE` | `auto`/`keyring`/`file` node-token backend |
| `HYPHA_EPHEMERAL_DEAD_AFTER` | seconds; ephemeral-node reap window (default 300) |

Both binaries (`hypha serve`, `hypha-hub`) initialize the same OTEL
layer — the `HYPHA_OTEL_*`/`OTEL_EXPORTER_OTLP_*` vars above apply to
each process.
