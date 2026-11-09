# Hypha

The distributed substrate for agent workloads — entities, ops,
snapshots, locks, projections, node host, hub gateway. Rust.
Named for the *hypha*, the fungal strand connecting a mycelial
network — the connective layer between nodes and workloads.

## Layout

Ports-and-adapters. Crates are named `hypha-<port>` (trait) and
`hypha-<port>-<impl>` (adapter); the composition roots
(`hypha serve` node host, `hypha-hub` gateway) land with the
`hypha-http`/`hypha-node` split (migration plan PL-005 in the org
ledger).

| Crate | Role |
| --- | --- |
| `crates/hypha-node-api` | node↔hub contract — `EntityApi` trait, wire DTOs, `HttpEntityApi` ureq client, `FakeEntityApi` |
| `crates/hypha-outbox` | durable per-entity FIFO op queue (SQLite) for writes while a node is offline |
| `crates/hypha-control` | pure arbitration policy — lock registry, takeover, merge/route (`pick_owner`) |
| `crates/hypha-telemetry` | opt-in OTEL layer over `tracing` (OTLP export, W3C propagation) |
| `crates/hypha-secret` + `-keyring`/`-dotenv` | secrets port + OS-keyring and dotenv impls |
| `crates/hypha-service` + `-systemd` | service-install port + systemd impl |
| `xtask` | check / test / coverage / fmt |

Landed from peregrin's entity plane (PL-005); `hypha-sync`,
`hypha-http`, `hypha-hub`, and the `hypha`/`hypha-hub` binaries are
phases P2–P3.

## Boundary

Hypha owns the **substrate** — distributed primitives over generic
`Entity {id, kind, indexDoc, health}` values and namespaced ops.
It never names an integration's type. Neighbors own theirs:

- **sepia** — session mechanism; registers `kind="sepia-session"`
  via the `sepia-peregrin` adapter (lives in the sepia repo).
- **peregrin** — orchestration: campaigns/drivers/loaders;
  `kind="campaign"` via `peregrin-node`'s entity plane.
- **tamias** — content distribution/provisioning/service lifecycle.

Op kinds are namespaced (`sepia.prompt`, `campaign.start`,
`entity.delete`); snapshot bytes are opaque end-to-end.

## Commands

```bash
cargo xtask check          # fmt + clippy -D warnings
cargo xtask test           # whole suite (nextest if present)
cargo xtask coverage       # llvm-cov report; extra args pass through
cargo xtask fmt            # cargo fmt --all
```

Git hooks live in `.hooks/` (`core.hooksPath` = `.hooks`):
pre-commit runs `cargo fmt --check`, pre-push `cargo xtask check`.
`./scripts/setup.sh` wires them on fresh clones.

## The worktree anatomy

`project/` is a git worktree of an orphan branch — the myco ledger,
created by `myco init`. Gitignored on `main`; never commit its
contents to `main`. `docs/` follows the same pattern when the docs
site lands.

## Rules

- Workspace lints are strict (pedantic + `unwrap_used`/`expect_used`
  warn, `unsafe_code` forbid — `Cargo.toml [workspace.lints]`);
  `-D warnings` in `xtask check` means no warnings tolerated.
  Tests `#![allow]` liberally.
- No `unsafe`. Edition 2024, rust-version 1.85.
- The substrate stays kind-agnostic: no `sepia`/`campaign`/session
  types in these crates — integrations plug in via the entity
  contract.
- `.config/nextest.toml` is the test-runner config.
- `.agents/skills/` is the canonical skill location.

## Docs

`docs/` worktree — VitePress + Diátaxis when the site lands.
Design history lives in peregrin's `project/adrs/` (ADR-0091 is the
entity-plane contract); hypha's own ADRs will live in `project/adrs/`.

<!-- myco:agent-awareness -->

## Agent awareness

- Session start / any work discussion: run `myco status --prompt`
  before proposing next steps — know the board first.
- User describes new work → offer to capture it (`myco todo add`,
  `issue new`, `proposal new`). Don't wait to be asked.
- Picking up a task: `myco resume <ID>` is the briefing.
- Wrapping up: `myco handoff <ID>` anything resumable; keep statuses
  honest (claimed → in-progress → blocked/done).

<!-- /myco:agent-awareness -->

<!-- sturn:agent-awareness -->

## Sturn awareness

- **No PRs.** The pushed branch/commit is the deliverable; review
  streams continuously (sturn WAL), merge is a human/sturn call.
- sturn-wired repo (`sturn.toml` present): `sturn claim "<glob>"
  --ws <ws>` *before* edits — claims surface conflicts in ms.
- Ops via `sturn write|rename|delete`; `sturn snap` folds the ws
  into a real commit; `sturn push` publishes heads + refs/sturn/*.
- `sturn leave` on exit — drops the actor's claims.

<!-- /sturn:agent-awareness -->

<!-- kuda:agent-awareness -->

## Kuda awareness

- `.kuda/` cards are committed artifacts — read them before
  re-exploring (`kuda map`, `kuda card <mod>`); `kuda status`
  reports per-card drift vs HEAD.
- Code changes that invalidate a card → regenerate it (`kuda
  sync`) in the same change; cards ride the diff they describe.
- `.kuda/index.sqlite` is gitignored + rebuildable; the `*.yaml`
  cards are the checked-in source of truth.

<!-- /kuda:agent-awareness -->
