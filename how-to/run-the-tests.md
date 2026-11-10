# Build and test

```bash
cargo xtask check     # cargo fmt --check + clippy --workspace --all-targets -D warnings
cargo xtask test      # cargo nextest run --workspace (falls back to cargo test)
cargo xtask coverage  # cargo llvm-cov HTML report → target/llvm-cov/html
cargo xtask fmt       # apply formatting
```

The push gate (`scripts/ci.sh`, run by the peregrin ci-gate
campaign on every push): runs lint, nextest
+ doctests, and a coverage check that
**gates on 100% line coverage** — any `DA:<line>,0` in the lcov report
fails the build. There is no GitHub Actions CI; run the same commands
locally before pushing.

Scope a single crate while iterating:

```bash
cargo nextest run -p hypha-outbox
cargo clippy -p hypha-control --all-targets
```
