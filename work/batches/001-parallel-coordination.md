# Batch 001 — Parallel Codex coordination

Two implementation sessions run inside Agentworks, each in a dedicated Git branch/worktree. This conversation coordinates through the authorized local API; a dedicated Agentworks inbox receives worker messages. A named external MCP controller credential remains a later task, not an identity borrowed from another session.

## Tasks

| Task | Branch | Working tree | Ownership | Acceptance |
| --- | --- | --- | --- | --- |
| Session workspace context | batch/001-workspace-context | .worktrees/001-workspace-context | src/git_context.rs, src/main.rs, web/git-context.js, web/index.html, web/app.js, web/workspace.css, tests/git-context.spec.mjs, WORKTREES.md | Read-only Git context endpoint; distinguish main checkout, linked worktree, detached HEAD and non-Git directories; show branch/path in responsive UI; focused tests |
| Durable completion reports | batch/001-completion-reports | .worktrees/001-completion-reports | src/completion.rs, src/communication.rs, src/mcp.rs, src/store.rs, web/communication.js, scripts/completion-smoke.mjs, COMPLETION-REPORTS.md | MCP report/query tools; durable reports with status, summary, commit/branch/tests, request deduplication and scope checks; browser display; restart recovery and permission tests |

Only the coordinator merges and deploys. Workers commit changes on their assigned branch and report commit IDs, validation, and limitations to the coordination inbox. Runtime data, credentials, dependency installations and build outputs stay ignored. Tests must use isolated databases, app roots, process ports and build targets. Do not touch the live service or its .agentworks data. No automatic model work is triggered by a completion report.

## Integration

Start from a committed prototype baseline. Use two Codex sessions with Participant grants only. Session startup and prompts are authorized by the user; tool permissions remain enforced by the harness. The coordinator handles task-related permission requests individually. Shared module/route registration for completion reports is supplied as a handoff and applied during integration, avoiding edits to src/main.rs by both workers.

Review each branch, run focused checks, merge sequentially, then run the API/browser, MCP communication and restart/crash suites on the combined main branch. Deploy only after safeToClose is true and catalog/database backup succeeds. Preserve worker sessions and worktrees for inspection.

## Status

Completed: both Codex implementation sessions ran concurrently inside Agentworks, committed only their assigned source files, and sent MCP handoffs. The coordinator reviewed and merged each branch sequentially without conflicts, completed combined validation, backed up/migrated the live data, deployed the tested binary, and resumed both original Codex conversations to publish reports through the new MCP tool.

| Task | Agentworks session | Worker commit | Merge | Published report |
| --- | --- | --- | --- | --- |
| Workspace context | `43035deb-f1d7-4464-a9ca-625549b01d7a` | `02a565626415c8ed86c346d636eb248e658915f0` | `6430ae2` | #2 |
| Completion reports | `a00e77a4-4835-4d97-b49d-8f21f8e2b6f5` | `0683785076caae3540fd5da5b5eace291dd1704b` | `c38ba86` (includes module/route handoff) | #1 |

Project: **Agentworks · parallel batch 001** (`49f78def-c50d-4f88-a5bd-896edd1e4e9c`). Coordination inbox: `ecc74f56-4e23-40e4-a46f-8449b2564bfe`. Worker sessions and their clean source worktrees remain available for inspection. Generated dependencies and build/runtime data remain ignored.

Baseline: `9fb00fa`; backlog reconciliation: `edd8a14`. The completed feature code is deployed from `c38ba8605c3d` using an immutable executable under `.agentworks/bin`, so future Cargo builds do not replace the running service's MCP executable.

## Validation and review

- Baseline verification passed before launch: 8 Rust tests, 10 browser tests and existing API/MCP checks.
- Workspace worker passed its offline build, 8 Rust tests and 5 focused Git/API/desktop/mobile tests. The coordinator reran the 5 focused tests on the first staged merge before committing it.
- Completion worker passed its offline build, 9 Rust tests including version-3 migration/data retention, and deterministic MCP/browser/restart smoke checks. Its handoff kept shared main-server edits out of the worker commit.
- Review identified a potential Store/SessionData lock inversion in report creation. The worker moved project reads before the store lock and continued emitting events after releasing the store; the merged code includes that fix.
- Combined main passed 9 Rust tests, 15 Playwright tests, MCP legacy/modern wire checks, existing API/project/settings/import/workspace/automation checks, completion identity/scope/validation/idempotency/audit/polling/restart checks, existing agent communication/coordinator checks and the restart/crash persistence suite. Graceful shutdown with an open WebSocket passed.
- After deployment, the coordinator verified all 4 session IDs and all 3,332 pre-deployment transcript events were preserved byte-for-byte, SQLite schema version 4 and integrity were correct, and each worker's live branch/commit/worktree metadata matched its committed branch.
- Read-only checks of the VPN-facing desktop/mobile UI displayed both actual worker branches and their published completion reports without browser errors.
- Both preserved real Codex sessions discovered and called `report_completion` on their resumed conversations, then confirmed report IDs through MCP inbox messages. Completion reports remain agent-authored claims; coordinator tests and merge results are recorded separately here.

Known limitation: the completion worker's optional strict Clippy run found seven pre-existing warnings in `catalog.rs`, `runtime.rs` and `main.rs`; those unrelated lint issues were not part of this batch. Workspace context is a best-effort current Git snapshot. Managed worktree creation/cleanup, named external controller credentials, merge queues and automatic inbox wake-ups remain backlog items.

Backup: `.agentworks/backups/batch001-20261017-164012/` (catalog and SQLite snapshot taken while idle, before migration). Listener: `0.0.0.0:4318`, reachable through the existing private VPN.
