#!/usr/bin/env bash
# Run manually: `./scripts/release.sh` from the repo root.
# bupha is deprecated — no watch registration; releases are operator-run.
# The leading sleep is the debounce window: a newer push kills this
# run via cancel_running, so only the last push in a quiet window
# releases. Runs inside a detached worktree — commits/tags are pushed
# from here, not from the main checkout.
#
# crates.io publish is retired: org policy keeps every repo but tamias
# off crates.io (the bare names `hypha`/`hypha-hub` are taken there
# anyway). Distribution rides the tamias packs —
# `provider/hypha-substrate` ships the `hypha`/`hypha-hub` daemons as
# systemd services — so a release is one workspace version bump + a
# `hypha-v<ver>` tag. The operator then publishes the pack itself
# (`tamias publish provider/hypha-substrate`). Binary dist stays out
# of scope: there is no release CI (GHA workflows were removed
# org-wide) and no cargo-installed `hypha`/`hypha-hub` artifact —
# dev installs use `cargo install --path` (see docs/getting-started).
# Needs: `cargo-edit` (cargo set-version).
set -euo pipefail
# worktree-local env (gitignored) — kept for parity with the other
# org release scripts; nothing here needs secrets today.
[ -f ./.env ] && { set -a; source ./.env; set +a; }
sleep "${HYPHA_RELEASE_DEBOUNCE:-300}"

git fetch origin --tags -q
last="$(git describe --tags --abbrev=0 2>/dev/null || true)"
if [ -z "$last" ]; then
    echo "no tags yet — tag the first release manually to seed the flow"; exit 0
fi
changed="$(git diff --name-only "${last}..HEAD")"

# release-worthy paths: workspace crates, the tamias pack payload,
# xtask, and the release machinery itself. Docs live on the orphan
# `docs` worktree branch — they never appear in this diff.
relevant="$(grep -E '^(crates/|provider/|xtask/|Cargo\.(toml|lock)|scripts/release\.sh|dist-workspace\.toml)' <<<"$changed" || true)"
if [ -z "$relevant" ]; then echo "no release-relevant changes since $last"; exit 0; fi

# every crate inherits `version.workspace` — one bump moves them all,
# and `cargo set-version --workspace` rewrites the workspace.package
# version, the workspace.dependencies reqs, and Cargo.lock together.
old="$(grep -m1 '^version = "' Cargo.toml | cut -d'"' -f2)"
cargo set-version --workspace --bump patch
ver="$(grep -m1 '^version = "' Cargo.toml | cut -d'"' -f2)"

# keep the user-facing version refs in lockstep: the tamias provider
# pack manifest.
sed -i "s/^version: \"${old}\"/version: \"${ver}\"/" provider/hypha-substrate/tamias.yaml

git commit -aqm "release: bump hypha to ${ver} — tamias pack release"
git tag "hypha-v${ver}"
git push -q origin HEAD:main --tags

echo "released: hypha ${ver} (tag hypha-v${ver})"
echo "next: tamias publish provider/hypha-substrate  # ship hypha-substrate@${ver} to the org registry"
