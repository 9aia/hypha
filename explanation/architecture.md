# Architecture

Hypha is the *substrate* layer: distributed primitives over generic
entities. It owns who may act on an entity, when, and on which
machine — plus the transport that gets ops and snapshots there.

```
                 ┌─────────────── hypha-hub ───────────────┐
                 │  merged index · op routing · push · SSE  │
                 └──────────────┬───────────────┬──────────┘
                        HTTP+SSE │               │ HTTP+SSE
                  ┌──────────────▼──┐    ┌───────▼──────────┐
                  │ hypha node A    │    │ hypha node B     │
                  │ /api/entities*  │    │ /api/entities*   │
                  │ KindRegistry    │    │ KindRegistry     │
                  └───────┬─────────┘    └────────┬─────────┘
                          │  integrations register entity planes:
              ┌───────────┼───────────────────────┼───────────┐
              ▼           ▼                       ▼           ▼
        sepia-session  campaign-run          <kind-N>     entity.*
        (sepia repo)   (peregrin repo)       (any repo)   (substrate)
```

## Planes

- **Entity plane** — this repo: `EntityApi` contract
  (`hypha-node-api`), node host (`hypha serve`, `hypha-node` +
  `hypha-http`), hub gateway (`hypha-hub`), projection
  (`hypha-sync`), outbox (`hypha-outbox`), arbitration
  (`hypha-control`), push.
- **Integration planes** — one per workload kind, living in the
  workload's own repo: `sepia-peregrin` (sepia repo) registers
  `kind="sepia-session"`; peregrin registers `kind="campaign"`.
  An integration translates entity ops ↔ its verbs and implements
  snapshot as its own serialize/resume.
- **Orchestration plane** — campaigns, drivers, loaders. Stays in
  peregrin; becomes a *workload on* hypha rather than a peer of it.

## Ports and adapters

Every I/O seam is a port (`hypha-secret`, `hypha-service`, …) with
impls behind it (`-keyring`, `-dotenv`, `-systemd`). The substrate
contracts are pure — `hypha-control` has no I/O at all.

## Dataflow

Client → hub `POST /api/entities/{id}/ops` → lock gate
(`hypha-control`) → owner node `POST /api/entities/{id}/ops` → the
kind's `EntityPlane::op`. Node down? `hypha-outbox` FIFO per entity,
replayed oldest-first on reconnect. Reads merge through
`hypha-sync`'s SQLite projection + SSE diff tail.
