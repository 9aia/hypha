---
url: /hypha/tutorials/getting-started.md
---
# Getting started

Hypha is the distributed substrate extracted from peregrin's entity
plane (org plan PL-005, ADR-0001). It ships two binaries: `hypha`
(the node host, `hypha serve`) and `hypha-hub` (the multi-node
gateway). A bare node mounts zero entity kinds — integrations
register theirs at composition.

## Build from source

```bash
git clone git@github.com:9aia/hypha.git
cd hypha
./scripts/setup.sh      # wires git hooks
cargo xtask check       # fmt + clippy -D warnings
cargo xtask test        # whole suite (nextest if present)
```

Rust 1.98 (see `rust-toolchain.toml`), edition 2024.

## Install the binaries

```bash
cargo xtask install   # release-builds → ~/.local/bin/{hypha,hypha-hub}
```

Or install straight from the workspace (into `~/.cargo/bin`):

```bash
cargo install --path crates/hypha-node --locked
cargo install --path crates/hypha-hub --locked
```

## Smoke run

A bare node needs no kinds and no config — loopback binds may go
tokenless:

```bash
hypha serve            # HYPHA_HOST/HYPHA_PORT, default 127.0.0.1:18787
curl localhost:18787/api/health        # {"ok":true}
curl localhost:18787/api/node          # node descriptor
curl localhost:18787/api/entities      # {"entities":[]} — no kinds yet
```

The hub gateways that node (`HYPHA_NODE_URL` defaults to
`http://127.0.0.1:18787`):

```bash
hypha-hub              # HYPHA_HUB_HOST/HYPHA_HUB_PORT, default 127.0.0.1:13000
curl localhost:13000/api/entity-index  # merged index (one row)
curl localhost:13000/api/pending-ops   # outbox depth
```

See [Environment](/reference/environment.md) for the full `HYPHA_*`
surface and [Wire API](/reference/wire-api.md) for the routes.

## What you just ran — the entity model

Everything hypha moves is one of three primitives (full wire
shapes: [Wire API](/reference/wire-api); vocabulary:
[Concepts](/explanation/concepts)):

* **Entity** — the thing being orchestrated: `{id, kind, node,
  indexDoc, health}`. `kind` is a free-form tag an integration
  registers; `indexDoc` is opaque JSON the integration writes for
  list views — hypha stores and merges it, never interprets it.
* **Op** — a command to an entity's owner node:
  `{entity, kind, payload}` with kinds namespaced
  `<integration>.<verb>`; payloads are opaque to hypha.
  `entity.delete` is the one reserved substrate op.
* **Snapshot** — serialize → transfer → resume: `{entity, ref,
  bytes}`; bytes are opaque end-to-end and export/import semantics
  belong to the integration that owns the kind.

The flow: a client lists entities (`GET /api/entities` → merged
index rows), submits an op (`POST /api/entities/{id}/ops` — if the
owner is down, the hub's outbox queues it per-entity FIFO and
replays on reconnect), `hypha-control` arbitrates locks/ownership
before routing, and moving a workload is `GET …/snapshot` on the
owner + `POST /api/entities/snapshot` on the target.

To exercise the model without I/O, `hypha-node-api` ships
`FakeEntityApi` (in-memory `EntityApi` for tests) and
`HttpEntityApi` (ureq client for real nodes);
`hypha-control`'s `pick_owner`/`Arbitration` are pure functions —
see [reference/crates](/reference/crates) for the full surface.

## Upgrading from peregrin — secrets move

`hypha-keyring` stores under the `secret-tool` service attribute
`"hypha"` (was `"peregrin"`), and `hypha-hub` node credentials live
under `"hypha-hub"` (was `"peregrin-hub"`). **There is no automatic
migration** — old entries are orphaned, not moved. After upgrading:

* re-run pairing (`POST /api/pair` flow) or re-store secrets with
  `secret-tool store --label "hypha: <name>" service hypha name <name>`
* hub node tokens: either re-pair each node, or let an env
  `HYPHA_NODES` `@token`/`HYPHA_NODE_TOKEN` re-seed the store on the
  next boot (`HYPHA_CREDENTIAL_STORE` picks keyring vs file backend)

## Where next

* [Run a node and hub](/how-to/run-a-node) — the two binaries up
  and answering.
* [Architecture](/explanation/architecture.md) — planes, nodes, hub.
* [Concepts](/explanation/concepts.md) ·
  [decisions](/explanation/decisions.md) ·
  [boundary](/explanation/boundaries.md) — what hypha owns vs sepia and
  peregrin.
