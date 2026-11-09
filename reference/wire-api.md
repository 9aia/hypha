# Wire API

The entity-plane HTTP surface (served by the node host, aggregated by
the hub). Landed with `hypha-http`/`hypha-hub` (P3); the shapes
below are the contract `hypha-node-api` already codes against.

## Node surface — `/api/entities*`

| Route | Verb | Purpose |
| --- | --- | --- |
| `/api/entities` | GET | list entities (`?kind=`, `?withLocks=1`) |
| `/api/entities` | POST | create entity (kind-registered) |
| `/api/entities/{id}` | GET/DELETE | entity detail / delete (`entity.delete` op) |
| `/api/entities/{id}/ops` | POST | submit op `{kind, payload}` |
| `/api/entities/{id}/snapshot` | GET | export snapshot bytes |
| `/api/entities/snapshot` | POST | import snapshot onto this node |

The hub additionally serves the `HttpEntityApi` spellings as aliases:
`/api/entities/{id}/export`, `/api/entities/import`, `/api/index`,
`/api/outbox` map onto the same handlers as `snapshot`/`entity-index`/
`pending-ops`, plus `GET /api/entities/{id}/stream` (the kind
policy's `stream_path`, routed to the owning node).

## Hub surface

Same `/api/entities*` shape, aggregated: merged index from all
`HYPHA_NODES` nodes, ops routed to the owner (outbox queued when
offline), `/api/events` SSE diff feed, `/api/entities/{id}/stream`
entity streams, push bridges, and `/api/{*rest}` passthrough to the
upstream node.

## Client

`hypha_node_api::HttpEntityApi` (ureq) implements `EntityApi`;
`FakeEntityApi` is the in-memory test double. Wire DTOs:
`EntityDto`, `EntityOp`, `EntityIndexDto`, `PendingOpDto`,
`NodeInfoDto`, `NodeStatusDto`, `PushSubscriptionDto` — see
`hypha-node-api/src/dto.rs` and the generated rustdoc under
`/hypha/api/`.
