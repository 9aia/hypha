---
layout: home

hero:
  name: hypha
  text: The distributed substrate
  tagline: Entities, ops, snapshots, and locks across your machines — the connective layer between nodes and workloads.
  actions:
    - theme: brand
      text: Get started
      link: /getting-started
    - theme: alt
      text: Architecture
      link: /explanation/architecture

features:
  - title: Entities
    details: "{id, kind, indexDoc, health} — any workload registers a kind; the substrate never names it."
  - title: Ops & snapshots
    details: Namespaced ops routed to the owner node; opaque snapshots serialize, transfer, and resume anywhere.
  - title: Arbitration
    details: Lock registry, takeover policy, merge/route — who may drive entity X on node Y.
  - title: Hub
    details: Merged index across nodes, op routing with offline outbox, push, SSE bridges.
---

hypha is the org's **distributed substrate** — entities, ops,
snapshots, locks, and projections that let workloads span machines.
It owns the connective layer between nodes and workloads and
nothing else: session mechanism is sepia's, orchestration policy
is peregrin's, distribution is tamias's.

## Which quadrant answers which goal

- **Learn it** → [Tutorial: your first entity](tutorials/first-entity.md)
- **Do a task** → [How-to](/how-to/run-a-node) — run a node+hub,
  build and test, read the ledger
- **Understand why** → [Architecture](/explanation/architecture),
  [concepts](/explanation/concepts),
  [decisions](/explanation/decisions),
  [boundary](/explanation/boundary)
- **Look something up** → [CLI](/reference/cli),
  [crates](/reference/crates),
  [environment](/reference/environment),
  [wire API](/reference/wire-api)
