# Batch 004 — Eight-hour backlog run

The user authorized autonomous backlog work for eight hours starting 2026-10-18 04:29 UTC (2026-10-17 23:29 Chicago), using Claude, Codex and OpenCode as appropriate. Stop new work early enough to review and leave a safe handoff by approximately 12:29 UTC. Routine reversible decisions are delegated; major product decisions are deferred for tomorrow. Unrelated projects, including Model Prompts, do not gate this work or deployments.

The coordinator runs actual Agentworks sessions in separate worktrees, reviews commits and checks, and merges sequentially. Workers cannot merge/deploy or alter live grants. The live service remains shared; deployment restarts may suspend unrelated harnesses while preserving saved sessions.

## First group

Project: `bd61c375-1916-4e60-a2d9-8fc7614e68d0`, Agentworks · overnight backlog. Coordination inbox: `3b9cdfba-fd23-4d5d-94f6-75a291eabbb7`.

| Work | Harness | Branch | Scope |
| --- | --- | --- | --- |
| L-01 | Codex | `batch/004-lifecycle` | Rename, archive, suspend and stop; durable history and safe runtime transitions |
| U-01 / B-01 | Claude | `batch/004-display` | Code/literal display preferences and successful-settings feedback |
| D-01 | Codex | `batch/004-diff` | Bounded read-only worktree/branch/commit diff viewer |

Each worker starts from `f175a64` in `.worktrees/004-<task>`, with communication-only grants. Explicit limit continuation is enabled for this overnight assignment, capped at eight attempts and the eight-hour deadline; a completed/blocked report ends it. Tests use isolated data and ports, existing dependencies and no paid inference. Workers retain their branches and commits for coordinator review; reports are claims, not integration evidence.

## Following candidates

After review of the first group, choose the next useful independent slices from U-04 palettes, U-06 compact work logs, U-07 context statusline, L-07 execution timeouts, U-02 notifications, U-03 mobile simplification and R-04/R-05 hosting. Avoid overlapping active UI/runtime edits where practical. Defer features requiring a major unresolved decision and record that decision here. No cross-host or terminal implementation is required merely to fill the window.

## Progress and handoff

First group dispatched. Implementation, review, integration and deployment evidence will be recorded as work completes. Detailed local task/state artifacts remain ignored under `.cache/batch-004/`.
