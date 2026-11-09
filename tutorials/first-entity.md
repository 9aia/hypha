---
url: /hypha/tutorials/first-entity.md
---
# Your first entity

This tutorial walks the entity model end to end — the shapes the
substrate moves, before any binary exists to drive them. Everything
here is the wire contract `hypha-node-api` already implements.

## The three primitives

**Entity** — the thing being orchestrated:

```json
{
  "id": "sess-42",
  "kind": "sepia-session",
  "node": "node-a",
  "indexDoc": { "title": "fix the flaky test", "locked": false },
  "health": { "live": true }
}
```

`kind` is a free-form tag registered by an integration.
`indexDoc` is opaque JSON the integration writes for list views —
hypha stores and merges it, never interprets it.

**Op** — a command submitted to an entity's owner node:

```json
{ "entity": "sess-42", "kind": "sepia.prompt", "payload": { "text": "hi" } }
```

Op kinds are namespaced `<integration>.<verb>`; payloads are opaque
to hypha. `entity.delete` is the one reserved substrate op.

**Snapshot** — serialize → transfer → resume:

```json
{ "entity": "sess-42", "ref": "…", "bytes": "…" }
```

Bytes are opaque end-to-end; export/import semantics belong to the
integration that owns the kind.

## The flow

1. A client (CLI, hub, another integration) lists entities:
   `GET /api/entities` → merged index rows `{id, kind, indexDoc, …}`.
2. It submits an op: `POST /api/entities/{id}/ops`. If the owner node
   is down, the hub's `hypha-outbox` queues it per-entity FIFO and
   replays on reconnect.
3. Before routing, `hypha-control` arbitrates: is the entity locked,
   who owns it, does this op preempt or queue?
4. To move a workload: `GET …/snapshot` on the owner, `POST
   /api/entities/snapshot` on the target — the integration's
   export/import does the rest.

## Try it in code

`hypha-node-api` ships `FakeEntityApi` — an in-memory `EntityApi`
impl for tests — and `HttpEntityApi`, the ureq client for real
nodes. `hypha-control`'s `pick_owner`/`Arbitration` are pure functions
you can exercise without any I/O.

```rust
use hypha_control::{pick_owner, Arbitration};
```

See [reference/crates](../reference/crates.md) for the full surface.
