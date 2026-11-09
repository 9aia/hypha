---
url: /hypha/explanation/boundary.md
---
# Boundary with sepia and peregrin

Hypha sits between two neighbors and must stay generic against both.

## sepia — session *mechanism*

Sepia owns everything you can do **to** a session: attach, prompt,
restore, rewind, per-agent stores. It is deliberately just the
agent-session abstraction — it must never learn about locks,
multi-node, or orchestration.

Hypha's side of the contract: the `sepia-peregrin` adapter (which
lives **in the sepia repo**) registers `kind="sepia-session"`
entities, translates entity ops ↔ session verbs, and implements
snapshot as sepia's session IR export/import. Hypha never names a
sepia type; sepia never learns hypha exists — the adapter is the
only coupling, and it depends on hypha's public API, never the
reverse.

## peregrin — orchestration *policy + campaigns*

Peregrin owns campaigns (declarative event-driven workflows),
drivers, loaders, and policy (who may act, when, from where). Post
PL-005, peregrin's distributed layer **is** hypha: `peregrin-node`
becomes a thin composition root that registers entity planes —
`sepia-session` (via the adapter) and `campaign` (its own
`campaign_entity.rs`) — on a hypha node host.

The dependency direction is one-way:

```
peregrin (orchestration) → hypha (substrate) → ports
```

Orchestration may sit *on* the substrate; the substrate never
imports orchestration. `peregrin-hub`'s gateway role moves to
`hypha-hub`; peregrin keeps campaign verbs in its CLI.

## The rule of thumb

* "Something you do to a workload" → the integration repo.
* "Who may act on entity X, when, from where" → hypha.
* "A workflow of events and steps" → peregrin.
