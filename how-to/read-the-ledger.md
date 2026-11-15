---
url: /hypha/how-to/read-the-ledger.md
---
# Read the ledger

**Goal:** know what the org is doing in this repo before proposing
or picking up work.

**Prerequisites:** `myco` installed and the `project/` worktree
present (it is — `docs/` and `project/` are git worktrees of
orphan branches created by `myco init`).

## Steps

All project state lives in the `project/` worktree via
[myco](https://github.com/9aia/myco) — tasks, backlog, issues,
proposals, plans, ADRs, handoffs. Never hand-edit files there; the
`myco` verbs are the write path.

```bash
myco status --prompt   # the board, token-sized
myco resume PL-005     # the extraction plan — briefing + handoffs
myco view board        # kanban
```

The master plan for this repo's existence is **PL-005** in the org
ledger (`9aia/workspace → repos/project`): "hypha — entity substrate
extraction from peregrin". Its handoffs carry the phase-by-phase
execution log.

## Verify

`myco status --prompt` prints the current board; `myco resume
PL-005` shows the plan's status and handoff chain.

## Deeper

* [Architecture](../explanation/architecture.md) — the planes the
  ledger's plans are building toward
