# Contributing

Issues and PRs are **disabled** — the project is agent-maintained.
Fork freely under MIT.

## Layout

`crates/` — rust workspace, ports-and-adapters. `hypha-node-api` is
the node↔hub contract (trait + DTOs + ureq client + fake);
`hypha-outbox` the durable per-entity op queue; `hypha-control` the
pure arbitration policy; `hypha-{secret,keyring,dotenv}` the secrets
port; `hypha-{service,systemd}` the service port. Telemetry is a git dep
on 9aia/lampy (P-042). `project/` is the myco ledger worktree.

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
- Never commit secrets — secret refs take names, not values.

## Docs

Follow Diátaxis: `docs/tutorials/` (learn) · `docs/how-to/` (tasks) ·
`docs/reference/` (lookup) · `docs/explanation/` (why).

## Releasing

Releases are run manually by an operator on `main` via
`tarsi-serve release --dir . --yes` directly (the org release
plane, project ADR-0005; repo config: `tarsi.toml` — the per-repo
`scripts/release.sh` shims were removed org-wide): plan →
changelog → bump → tag → push. There is **no crates.io publish**: org policy
keeps every repo but tamias off crates.io (the `hypha`/`hypha-hub`
names there are taken anyway), and the crates are internal
lib-only engines. Distribution rides the tamias pack — after the
tag, the operator runs `tamias publish provider/hypha-substrate`
to ship the `hypha`/`hypha-hub` daemons to the org registry.
Prebuilt binary artifacts are not part of the flow (dev installs
use `cargo install --path`).
