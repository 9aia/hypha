# Read the ledger

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
