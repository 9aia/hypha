# Build and test

**Goal:** run the same gates CI runs before pushing.

**Prerequisites:** a checkout of the repo, Rust toolchain per
`rust-toolchain.toml`, `cargo-nextest` and `cargo-llvm-cov` for the
full suite (both optional — fallbacks exist).

## Steps

```bash
cargo xtask check     # cargo fmt --check + clippy --workspace --all-targets -D warnings
cargo xtask test      # cargo nextest run --workspace (falls back to cargo test)
cargo xtask coverage  # cargo llvm-cov HTML report → target/llvm-cov/html
cargo xtask fmt       # apply formatting
```

The push gate (`scripts/ci.sh`, run by the peregrin ci-gate
campaign on every push): runs lint, nextest + doctests, and a
coverage check that **gates on 100% line coverage** — any
`DA:<line>,0` in the lcov report fails the build. There is no
GitHub Actions CI; run the same commands locally before pushing.

Scope a single crate while iterating:

```bash
cargo nextest run -p hypha-outbox
cargo clippy -p hypha-control --all-targets
```

## Verify

`cargo xtask check` exits 0 with no warnings; `cargo xtask test`
reports all suites green; `cargo xtask coverage` emits the lcov
report with no uncovered lines.

## Deeper

- [Architecture](../explanation/architecture.md) — which crate owns
  what before you start editing
