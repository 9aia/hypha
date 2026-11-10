#!/usr/bin/env bash
# Run manually: `./scripts/release.sh` from the repo root (needs
# `.env` with CARGO_REGISTRY_TOKEN; dist + gh for binary artifacts).
# bupha is deprecated — no watch registration; releases are operator-run.
# The leading sleep is the debounce window: a newer push kills this
# run via cancel_running, so only the last push in a quiet window
# releases. Runs inside a detached worktree — commits/tags are pushed
# from here, not from the main checkout.
#
# Publishes to crates.io in dependency order (a crate's path deps must
# already be on the registry). Needs: `cargo-edit` (cargo set-version)
# and, for binary artifacts, `cargo-dist` (dist) + `gh`.
#
# crates.io note: the bare names `hypha`/`hypha-hub` are taken on
# crates.io — the packages publish under their crate names
# (`hypha-node` ships the `hypha` bin, `hypha-hub` the `hypha-hub`
# bin); `cargo install hypha-node` / `cargo install hypha-hub` work.
set -euo pipefail
# worktree-local secrets (gitignored); the service env file is canonical
[ -f ./.env ] && { set -a; source ./.env; set +a; }
sleep "${HYPHA_RELEASE_DEBOUNCE:-300}"

git fetch origin --tags -q
last="$(git describe --tags --abbrev=0 2>/dev/null || true)"
if [ -z "$last" ]; then
    echo "no tags yet — tag the first release manually to seed the flow"; exit 0
fi
changed="$(git diff --name-only "${last}..HEAD")"

# Publish order — dependencies first (workspace dep edges).
order="hypha-secret hypha-node-api hypha-outbox hypha-push \
       hypha-telemetry hypha-service hypha-control hypha-dotenv \
       hypha-keyring hypha-systemd hypha-sync hypha-http \
       hypha-hub hypha-node"

touched=""
for c in $order; do
    grep -q "^crates/$c/" <<<"$changed" && touched="$touched $c"
done
if [ -z "$touched" ]; then echo "no crate changes since $last"; exit 0; fi

published=""
tags=""
for c in $order; do
    grep -qw "$c" <<<"$touched" || continue
    cargo set-version -p "$c" --bump patch
    # crates.io rate-limits *new* crates (a time window per push); retry
    # through it rather than failing the release.
    for i in $(seq 1 60); do
        out="$(cargo publish -p "$c" --allow-dirty 2>&1)" && break
        if grep -q '429\|too many new crates' <<<"$out"; then
            echo "rate-limited on $c — sleeping 60s"; sleep 60
        elif grep -q 'already exists\|already uploaded' <<<"$out"; then
            break
        else
            echo "$out" | tail -3; sleep 30
        fi
    done
    published="$published $c"
    # Per-crate tag — versions diverge independently in a monorepo.
    ver_c="$(grep -m1 '^version' "crates/$c/Cargo.toml" | cut -d'"' -f2)"
    tags="$tags $c-v$ver_c"
done

git commit -aqm "release: bump crate versions" || true
for t in $tags; do git tag "$t"; done
ver="$(git describe --tags --abbrev=0 | sed 's/.*-v//')"
git push -q origin HEAD:main --tags

# binaries: tag-triggered .github/workflows/release.yml builds the
# release tarballs on `hypha-v*`/`v*` tags — push one to ship:
#   git tag hypha-v${ver} && git push origin hypha-v${ver}
# The local `dist build` path below is the ops fallback (host arch).
if command -v dist >/dev/null && command -v gh >/dev/null; then
    dist build --output-format=json > /tmp/dist-manifest.json 2>&1 || true
    for f in target/distrib/*; do
        [ -f "$f" ] && echo "dist artifact: $f"
    done
fi

echo "released:${published} (tags:${tags})"
