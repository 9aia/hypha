# Wire API

The entity-plane HTTP surface — served by the node host
(`hypha-http`), aggregated by the hub (`hypha-hub`). One canonical
spelling per route: `hypha-node-api`'s `HttpEntityApi` and the hub
speak the same paths. (The P3-era alias spellings —
`/api/entities/{id}/export`, `/api/entities/import`, `/api/index`,
`/api/outbox` — are gone.)

## Node surface

| Route | Verb | Purpose |
| --- | --- | --- |
| `/api/health` | GET | liveness probe (pre-auth) |
| `/api/pair` | POST | redeem a pairing code → bearer token |
| `/api/auth/login` `/api/auth/logout` | POST | auth-cookie exchange |
| `/api/user` | GET | OS user info for pickers |
| `/api/node` | GET/PATCH | node descriptor / rename |
| `/api/fs?path=` | GET | direct subdirectories of an absolute path |
| `/api/events` | GET | SSE event feed |
| `/api/entities` | GET/POST | list (`?kind=`) / create entity |
| `/api/entities/{id}` | GET/DELETE | entity doc / `entity.delete` |
| `/api/entities/{id}/ops` | POST | submit op `{kind, payload}` |
| `/api/entities/{id}/snapshot` | GET | export snapshot bytes |
| `/api/entities/snapshot` | POST | import snapshot (`body.kind` picks the plane) |
| `/api/entity-index` | GET | `{"index": [{node, entities}]}` — one row, this node |
| `/api/pending-ops` | GET | `{"ops": [...]}` — always empty on a bare node (the outbox is hub-side) |
| `/api/push/vapid` | GET | `{publicKey}` (501 without a push store) |
| `/api/push/subscribe` | POST/DELETE | push subscription add/remove |

## Hub surface

Same `/api/entities*` shape, aggregated: merged index from all
`HYPHA_NODES` nodes, ops routed to the owner (durable outbox when
offline), plus hub-owned routes:

| Route | Purpose |
| --- | --- |
| `/api/entity-index` | merged per-node entity buckets |
| `/api/node-status` | `{"nodes": [...]}` — per-node liveness: `status` (`unknown`/`up`/`down`), `lastSeenAt`, `downSince` (first observed failure), `ephemeral` (self-reported disposable lifecycle) |
| `/api/pending-ops` | outbox depth — queued + dead-letter ops |
| `/api/events` | SSE bridge to the upstream node |
| `/api/entities/{id}/stream` | per-entity live stream (kind policy's `stream_path`, owning node) |
| `/api/push/*` | hub-owned subscription store (fans out to every node) |
| `/api/{*rest}` | passthrough to the upstream node for everything else |

Not on the substrate: `/api/config*` — node config is
integration-owned (a composing integration mounts its own surface);
`EntityApi::get_config`/`set_config` are doc-hidden for that reason.

### Node lifecycle

The sync engine tracks each registered node on the projection's
`nodes` row: `status` moves `unknown → up` on a successful
subscribe+list and back to `down` on any failed cycle (reconnects
retry forever at capped exponential backoff). `downSince` stamps the
first observed failure and clears on reconnect. Merged entity lists
carry the owner's status inline as `nodeStatus`, so a dead node's
stale index entries are distinguishable from live ones; ops to a
down node queue durably in the hub outbox and replay on reconnect.

A node whose `GET /api/node` `capabilities` include `"ephemeral"`
(e.g. `peregrin serve --ephemeral` CI runners) is treated as
disposable: once it has been continuously `down` for
`SyncOptions::ephemeral_dead_after` (default 5 min) the engine drops
its projected entities — a resurrected node resyncs itself — and
dead-letters its queued writes rather than letting them pend
forever. The `nodes` row stays, so `/api/node-status` keeps reporting
`down` + `ephemeral`.

## Client

`hypha_node_api::HttpEntityApi` (ureq) implements `EntityApi`;
`FakeEntityApi` is the in-memory test double. Wire DTOs:
`EntityDto`, `EntityOp`, `EntityIndexDto`, `PendingOpDto`,
`NodeInfoDto`, `NodeStatusDto`, `PushSubscriptionDto` — see
`hypha-node-api/src/dto.rs` and the generated rustdoc under
`/hypha/api/`.
