+++
id = "ADR-0002"
title = "hypha-config — generic config loading for any 9aia project"
status = "done"
created = "2026-11-10"
priority = "med"
labels = ["config", "hypha"]
+++

## Context

Every Rust repo in the org carried its own config stack — eight
near-identical copies of the same machinery (org proposal P-015):

- `bupha`/`peregrin` — richest shape: `ConfigSource::load()` +
  `-file` discovery + `-toml`/`-yaml` adapters.
- `tamias` — `ConfigFormat` (extensions/parse/render) + `load_file`/
  `load_in` helpers; YAML standard, TOML alternative.
- `cuora`/`myco`/`sturn`/`mimus`/`kuda` — `ConfigSource` + TOML only.

The traits were shape-identical and the *machinery* — explicit path →
`$<NAME>_CONFIG` → `./<name>.{yaml,yml,toml}` → `~/.config/<name>/`,
YAML-wins tie-break, serde parse/render — was copy-pasted. Only the
config **model** is genuinely per-repo.

## Decision

Extract the generic half into hypha as four crates:

- **`hypha-config`** — the port. `ConfigSource<T>` (`load() ->
  Result<T, String>`) for sources, `ConfigFormat<T>`
  (`extensions`/`parse`/`render`) for syntaxes, `format_for`
  extension dispatch, and `Discovery` — the search spec (app
  basename, env var, dirs) plus `home_dir`/`expand_tilde`/
  `user_config_dir` helpers shared with peregrin's conventions.
- **`hypha-config-file`** — `FileSource<T>`: explicit path → env var
  → `<dir>/<name>.<ext>` per dir, formats tried in preference order
  (YAML before TOML via `standard_formats`). Env lookups are
  injectable (`*_with` variants) so tests never touch the process
  environment.
- **`hypha-config-toml` / `hypha-config-yaml`** — `TomlFormat` /
  `YamlFormat` unit impls with blanket `ConfigFormat<T>` impls for
  any `T: DeserializeOwned + Serialize`.

Key trait-shape call: `ConfigFormat` is generic over `T`
(`dyn ConfigFormat<T>`), not a non-generic trait with generic methods
— generic methods are not dyn-compatible, and the trait object is
what `FileSource` stores. `FileSource<T>` is likewise generic; one
value can't serve two models, which keeps the pairing honest.

Deliberate semantic choices vs the per-repo copies:

- An explicit/env path that doesn't exist is an **error naming the
  path** (myco semantics), not a silent skip to the next candidate.
- An explicit path with an **unclaimed extension** is an error; an
  **extensionless** path parses with the first (preferred) format —
  the back-compat seam for `--config` without a suffix.
- `YamlFormat` claims `["yaml", "yml"]` so `<name>.yaml` beats
  `<name>.yml`; tamias's `["yml", "yaml"]` order is a per-repo choice
  adopters can flip via `with_formats`.

## Consequences

- Repos keep only their serde `Config` model; the port/impl crates
  disappear on adoption. `tarsi` is the first consumer (P-015).
- Migration is per-repo and optional — no big-bang rewrite of the
  eight existing stacks.
- Out of scope (open in P-015): layered/overlay loading
  (user + project merge — model-specific, stays per-repo),
  non-file sources (env/remote = new `ConfigSource` impls), and
  config-adjacent surfaces like pavo-driver-config.
