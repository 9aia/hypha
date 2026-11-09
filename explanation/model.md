---
url: /hypha/explanation/model.md
---
# Entities, ops, snapshots

The whole substrate is three primitives. Get these and the rest is
plumbing.

## Entity

`{id, kind, node, indexDoc, health}`

* `kind` — free-form tag; the integration's namespace
  (`sepia-session`, `campaign`, …).
* `indexDoc` — opaque JSON for list views. Hypha merges and
  columnizes it; meaning belongs to the integration.
* `health` — liveness/lock state reported by the owner node.

## Op

`{entity, kind, payload}` — submitted to an entity's **owner node**.

* `kind` is namespaced `<integration>.<verb>` (`sepia.prompt`,
  `campaign.start`); `entity.*` is the reserved substrate namespace
  (`entity.delete` only, today).
* `payload` is opaque to hypha — the owning plane interprets it.
* Ops are idempotent-keyed and TTL'd in the outbox; `Turn`-class ops
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
them around *entities* is what lets bupha jobs, sturn claims, or
cicada schedules become kinds without touching this repo.
