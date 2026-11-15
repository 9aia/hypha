# Concepts

The whole substrate is three primitives — **entity**, **op**,
**snapshot** — plus the planes and stores around them. Get these
and the rest is plumbing.

## Entity

`{id, kind, node, indexDoc, health}`

- `kind` — free-form tag; the integration's namespace
  (`sepia-session`, `campaign`, …).
- `indexDoc` — opaque JSON for list views. Hypha merges and
  columnizes it; meaning belongs to the integration.
- `health` — liveness/lock state reported by the owner node.

## Op

`{entity, kind, payload}` — submitted to an entity's **owner node**.

- `kind` is namespaced `<integration>.<verb>` (`sepia.prompt`,
  `campaign.start`); `entity.*` is the reserved substrate namespace
  (`entity.delete` only, today).
- `payload` is opaque to hypha — the owning plane interprets it.
- Ops are idempotent-keyed and TTL'd in the outbox; `Turn`-class ops
  dead-letter on first failure.

## Snapshot

`{entity, ref, bytes}` — the migration primitive.

Serialize on the owner (`GET /api/entities/{id}/snapshot`), import on
the target (`POST /api/entities/snapshot`). Bytes are opaque
end-to-end; resume semantics are the integration's
(`campaign-snapshot/v1`, sepia session IR export, …).

## Why generic

The distributed abilities — sync, arbitration, outbox, transfer,
pairing — are orchestration-domain but workload-agnostic. Shaping
them around sessions was the mistake ADR-0091 corrected; shaping
them around *entities* is what lets CI jobs, sturn claims, or
cicada schedules become kinds without touching this repo.

## Around the primitives

- **Node** — a `hypha serve` host owning a set of entity kinds via
  its `KindRegistry`. The node is the unit of ownership: an
  entity lives on exactly one node at a time.
- **Hub** — `hypha-hub`, the aggregation gateway: merged entity
  index, op routing with offline outbox, SSE diff tail, push.
- **Owner** — the node an entity's ops route to; arbitration
  (`hypha-control`) decides who may drive entity X on node Y.
- **Outbox** — `hypha-outbox`, a durable per-entity FIFO SQLite
  queue that replays oldest-first when a node reconnects.
- **Projection** — `hypha-sync`'s SQLite entity-index + sync
  engine on the hub side; reads merge through it.
- **Kind / integration** — a workload registers `kind="<name>"`
  entities by implementing the entity-plane contract in its own
  repo (sepia's `sepia-peregrin`, peregrin's `campaign`). Hypha
  never names the kind.
- **KV** — node-local `{ns}/{key}` → JSON store (`hypha-kv` +
  `-file`) with per-namespace visibility — the generalization of
  peregrin's config/projects surface.
