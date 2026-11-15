---
url: /hypha/explanation/decisions.md
---
# Decisions

ADR-lite — the decisions that shape hypha, the alternatives
rejected, and the consequence each carries. Full history lives in
the org ledger (`project/adrs/` — ADR-0091 is the entity-plane
contract; PL-005 is the extraction plan).

## Substrate over sessions (ADR-0091)

**Decision:** the distributed layer is shaped around generic
`Entity {id, kind, indexDoc, health}` values and namespaced ops —
never around sessions or campaigns.

**Alternatives:** keep the entity plane inside peregrin shaped
around `sepia-session` (every new workload kind — CI jobs, sturn
claims, cicada schedules — would fork the substrate); a
workload-specific plane per integration (N sync/arbitration
implementations to keep correct).

**Consequence:** integrations plug in by implementing the entity
contract in their own repo; the substrate stays kind-agnostic —
no `sepia`/`campaign`/session types in these crates, enforced by
review.

## Snapshot bytes are opaque end-to-end

**Decision:** `Snapshot {entity, ref, bytes}` — serialize on the
owner, import on the target; hypha never parses the payload.

**Alternatives:** a substrate-owned snapshot schema (every
integration's state model becomes hypha's problem); references
to external stores (transfer breaks when the store is unreachable
or versioned differently).

**Consequence:** migration semantics (`campaign-snapshot/v1`,
sepia session IR) belong entirely to the integration — hypha
guarantees transport, ordering, and arbitration, nothing more.

## Ops are namespaced and TTL'd

**Decision:** op `kind` is `<integration>.<verb>` with `entity.*`
reserved for the substrate; outbox entries carry idempotency keys
and TTLs, and `Turn`-class ops dead-letter on first failure.

**Alternatives:** a flat global verb namespace (collisions across
integrations); infinite retries (a poisoned op blocks its entity's
FIFO forever).

**Consequence:** the substrate can route and dedupe without
understanding any verb; a failing op surfaces as dead-letter
instead of silent backlog.

## Policy is injected, not owned

**Decision:** `hypha-control` is a pure arbitration library —
lock registry, takeover, `pick_owner` — with no I/O; the hub
boots an empty `KindPolicies` and integrations supply policy.

**Alternatives:** policy baked into the hub (who-may-act rules
become substrate concerns); policy in each node (split-brain
arbitration).

**Consequence:** arbitration is testable pure code; orchestration
policy stays in peregrin where it belongs — the substrate decides
*mechanism*, never *intent*.

## Extracted from peregrin, composed upward

**Decision:** hypha is the entity plane lifted out of peregrin
(PL-005); `peregrin-node` becomes a thin composition root that
registers entity planes on a hypha node host.

**Alternatives:** keep the plane inside peregrin (the substrate
could never serve non-orchestration workloads); a ground-up
rewrite (the wire shapes and outbox semantics were already proven
in production inside peregrin).

**Consequence:** dependency direction is one-way —
`peregrin → hypha → ports`. Orchestration sits *on* the substrate;
the substrate never imports orchestration.
