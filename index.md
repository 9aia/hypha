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
