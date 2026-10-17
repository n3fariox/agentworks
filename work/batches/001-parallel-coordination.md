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

Preparation in progress. Session IDs, commits, checks and deployment results will be recorded after integration.
