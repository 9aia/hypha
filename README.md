# Hypha

The distributed substrate for agent workloads — entities, ops,
snapshots, and locks across your machines. Rust.

Hypha is the generic entity plane: any workload registers an entity
`kind`, receives namespaced ops on its owner node, and can be
snapshotted → transferred → resumed elsewhere. It knows nothing about
what a workload *is* — integrations (sepia sessions, peregrin
campaigns, …) plug in as kinds.

Named for the *hypha* — the fungal strand that connects a
mycelial network. The connective layer between nodes and workloads.

- **Entities** — `{id, kind, node, indexDoc, health}`; `indexDoc` is
  an opaque JSON blob the integration writes for list views.
- **Ops** — `{entity, kind, payload}` namespaced (`<kind>.<verb>`)
  submitted to an entity's owner node; payloads opaque to hypha.
- **Snapshots** — `{entity, ref, bytes}` — serialize → transfer →
  resume on another node; export/import is kind-specific.
- **Arbitration** — lock registry, takeover policy, merge/route:
  who may drive entity X on node Y.
- **Hub** — aggregation across nodes: merged index, op routing,
  push, SSE bridges.

## Boundary

Hypha owns the **substrate** — distributed primitives over generic
entities. Workload *mechanism* lives in the integration repos:

- [sepia](https://github.com/9aia/sepia) — session lifecycle
  (`kind="sepia-session"` via the `sepia-peregrin` adapter).
- [peregrin](https://github.com/9aia/peregrin) — orchestration:
  campaigns, drivers, loaders (`kind="campaign"`).

Hypha never names an integration's type; integrations never reach
around the entity contract.

## Status

Extracted from peregrin's entity plane (design contract:
peregrin `project/adrs/0091`). Crates are landing phase by phase —
see the org ledger plan for the migration state.

## Develop

```bash
cargo xtask check    # fmt + clippy -D warnings
cargo xtask test     # whole suite (nextest if present)
cargo xtask coverage # llvm-cov report (HTML → target/llvm-cov/html)
```

[CONTRIBUTING](CONTRIBUTING.md) · [SECURITY](SECURITY.md) ·
[LICENSE](LICENSE) (MIT)
