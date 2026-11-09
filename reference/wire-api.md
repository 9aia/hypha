---
url: /hypha/reference/wire-api.md
---
# Wire API

The entity-plane HTTP surface (served by the node host, aggregated by
the hub). Routes land with `hypha-http`/`hypha-hub` in P3; the shapes
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

## Hub surface

Same `/api/entities*` shape, aggregated: merged index from all
`HYPHA_NODES`/`PEREGRIN_NODES`, ops routed to the owner (outbox
queued when offline), `/api/events` SSE diff feed, push bridges.

## Client

`hypha_node_api::HttpEntityApi` (ureq) implements `EntityApi`;
`FakeEntityApi` is the in-memory test double. Wire DTOs:
`EntityDto`, `EntityOp`, `EntityIndexDto`, `PendingOpDto`,
`NodeInfoDto`, `NodeStatusDto`, `PushSubscriptionDto` — see
`hypha-node-api/src/dto.rs` and the generated rustdoc under
`/hypha/api/`.
