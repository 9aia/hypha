#!/usr/bin/env bash
# hypha release — thin shim over tarsi-serve (the org release plane,
# project ADR-0005; repo config: ./tarsi.toml). One call runs
# plan → changelog → bump → tag → push; publish targets are off
# (ADR-0004: crates.io is tamias-only, GitHub Releases unused).
# Operator-run on main — `tarsi-serve release` is dry-run by
# default; --yes is the explicit opt-in.
#
# Pack-ref sync (`[release.sync_refs]` in tarsi.toml) rewrites the
# tamias provider-pack manifest version inside the release commit,
# so the tagged tree carries consistent versions. The pack publish
# itself stays manual (`tamias publish provider/hypha-substrate`).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# worktree-local env (gitignored) — kept for parity with the other
# org release scripts; nothing here needs secrets today.
[ -f ./.env ] && { set -a; source ./.env; set +a; }

tarsi-serve release --dir . --yes

echo "released: see tags hypha-*-v<ver>"
echo "next: tamias publish provider/hypha-substrate  # ship hypha-substrate@<ver> to the org registry"
