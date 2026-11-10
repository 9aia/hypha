# hypha-substrate — tamias provider package

The hypha substrate daemons (`hypha serve` node host + `hypha-hub`
aggregation gateway) packaged as a tamias provider — the second proof
of the provider model after ci-gate's `peregrin-bundle` (PL-008 T8-6).

Unlike ci-gate this package needs **no content driver**: `type: files`
is tamias's built-in kind, and `tamias deploy` renders the
`provider.services[]` block into per-user systemd units natively. The
daemons are pure env-driven services, so the entire runtime spec is
manifest data.

## Install

```sh
tamias install ./provider/hypha-substrate   # or any ref to this dir
tamias deploy hypha-substrate               # writes the units
```

Units rendered (under `$XDG_CONFIG_HOME/systemd/user`):

- `hypha-substrate-node.service` — `hypha serve`, `127.0.0.1:18788`
- `hypha-substrate-hub.service` — `hypha-hub`, `127.0.0.1:13001`,
  `HYPHA_NODE_URL=http://127.0.0.1:18788`

Ports sit one off the defaults (`18787`/`13000`) so a live hypha
node/hub can coexist. Units are written but **not enabled** —
`tamias service enable <unit>` is the operator's step.

## Env file

Both units read `~/.config/hypha-substrate/env` (see `env.example`).
Set `HYPHA_HOME` there to an isolated dir — the default
`~/.local/share/hypha` holds `node.json`, the node identity file, and
sharing it with a live node would collide. Tokens
(`HYPHA_TOKEN`, `HYPHA_HUB_TOKEN`, `HYPHA_NODE_TOKEN`) go in the env
file, never inline; they are only *required* off loopback.

## Inspect

```sh
tamias provider list            # services, verbs, health probe
tamias run hypha-substrate node # foreground exec (instead of systemd)
tamias logs hypha-substrate     # journal over both units
tamias uninstall hypha-substrate
```
