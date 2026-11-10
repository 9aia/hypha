# Contributing

Issues and PRs are **disabled** — the project is agent-maintained.
Fork freely under MIT.

## Layout

`crates/` — rust workspace, ports-and-adapters. `hypha-node-api` is
the node↔hub contract (trait + DTOs + ureq client + fake);
`hypha-outbox` the durable per-entity op queue; `hypha-control` the
pure arbitration policy; `hypha-{secret,keyring,dotenv}` the secrets
port; `hypha-{service,systemd}` the service port; `hypha-telemetry`
the OTEL layer. `project/` is the myco ledger worktree.

## Boundary

Hypha owns distributed *substrate* primitives — entities, ops,
snapshots, locks, projections, the hub gateway, the node host. It
never names an integration's type (`sepia-session`, `campaign`, …);
those live in their own repos behind the entity contract. If a
feature is "something you do to a workload" it belongs to the
integration; if it's "who may act, when, where" across nodes it
belongs here.

## Rules of thumb

- New capability = a new `hypha-{port}` / `hypha-{port}-{impl}`
  crate, not a core change.
- `cargo xtask check` and `cargo xtask test` must stay green.
- Coverage: `cargo xtask coverage` — cargo-llvm-cov report; the push
  gate (`scripts/ci.sh` via the peregrin ci-gate campaign) enforces 100%
  line coverage.
- Commits: conventional prefixes (`feat:`/`fix:`/`refactor:`/`docs:`/`chore:`).
- Never commit secrets — `hypha-secret` takes names, not values.

## Docs

Follow Diátaxis: `docs/tutorials/` (learn) · `docs/how-to/` (tasks) ·
`docs/reference/` (lookup) · `docs/explanation/` (why).

## Releasing

- crates.io publish is dep-ordered via `scripts/release.sh` (manual —
  bupha watch retired, releases are operator-run). First publish is
  manual: tag once, then the script bumps + publishes + tags.
- crates.io names: `hypha`/`hypha-hub` are taken — packages publish
  under crate names (`hypha-node` ships the `hypha` bin,
  `cargo install hypha-node`).
- Release binaries: run `scripts/release.sh` manually (there is no
  release CI — releases are operator-driven). It builds
  `hypha-<target>.tar.gz` (linux x86_64, macOS aarch64/x86_64)
  carrying the `hypha` + `hypha-hub` bins.
- Org publish order for the substrate stack: **hypha → sepia →
  peregrin** (sepia-peregrin and peregrin's git deps carry `version`
  reqs that resolve against the registry once published).
