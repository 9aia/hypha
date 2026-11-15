# Run a node and hub locally

**Goal:** stand up a bare hypha node plus its hub gateway and
confirm the substrate answers.

**Prerequisites:** `hypha` and `hypha-hub` installed
(`cargo xtask install`, or `cargo install --path` per
[Getting started](../getting-started.md)). Loopback binds may go
tokenless — no secrets needed for this smoke run.

## Steps

1. Start the node host:

   ```bash
   hypha serve            # HYPHA_HOST/HYPHA_PORT, default 127.0.0.1:18787
   ```

2. In another shell, start the hub:

   ```bash
   hypha-hub              # HYPHA_HUB_HOST/HYPHA_HUB_PORT, default 127.0.0.1:13000
   ```

   The hub gateways the node at `HYPHA_NODE_URL`
   (default `http://127.0.0.1:18787`).

## Verify

```bash
curl localhost:18787/api/health        # {"ok":true}
curl localhost:18787/api/node          # node descriptor
curl localhost:18787/api/entities      # {"entities":[]} — no kinds yet
curl localhost:13000/api/entity-index  # merged index (one row)
curl localhost:13000/api/pending-ops   # outbox depth
```

A bare node mounts zero entity kinds — integrations register theirs
at composition (see the [entity tutorial](../tutorials/first-entity.md)
for the wire shapes, and [Environment](../reference/environment.md)
for the full `HYPHA_*` surface).

## Deeper

- [Wire API](../reference/wire-api.md) — every route
- [Architecture](../explanation/architecture.md) — planes, hub, outbox
