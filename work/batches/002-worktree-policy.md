# Batch 002 — Enforced worktree launches

The coordinator continues driving the backlog after batch 001. Two real Codex sessions run inside Agentworks, on separate branches from `b3dc853`. Workers report through the existing MCP inbox and completion tools. Only the coordinator integrates and deploys.

| Task | Branch | Session | Ownership |
| --- | --- | --- | --- |
| Worktree policy and launch preparation | `batch/002-worktree-policy` | `d27cfea0-05c4-4903-a173-b700953fc2f0` | Backend, policy/discovery routes, common launch gate, durable retry ledger, API smoke tests and API documentation |
| Worktree controls and project settings | `batch/002-worktree-ui` | `68ebdfb1-2378-4f82-815c-b70d6ad65671` | New-session controls, existing-tree picker, project policy settings, focused desktop/mobile tests and UI documentation |

Project: **Agentworks · parallel batch 002** (`caa7dc76-19b1-45f0-8005-4475469f354f`). Coordination inbox: `8cb22521-056a-4b60-947e-4dd5b105bd51`. Worktrees: `.worktrees/002-worktree-policy` and `.worktrees/002-worktree-ui`. Each worker uses its own build target, test data and assigned ports; shared dependencies and the live service remain untouched.

## Acceptance

- Project defaults select directory, new linked worktree or existing linked worktree. A project can require actual linked-worktree isolation for new sessions.
- Browser creation, MCP starts and automation runs use the same backend checks. Imports retain their original provider directory; registered sessions resume in place.
- New worktrees start from a committed revision. Git calls validate roots, symlinks and refs, use bounded subprocesses and never overwrite a branch or destination.
- Retrying a launch does not create extra worktrees, including after restart. Changed payloads under the same request ID conflict.
- Deleting a session preserves its worktree and files. Cleanup and merge controls remain separate future work.
- Controls preserve existing harness/model selection and work on phones. Discovery does not launch agents or perform inference.

## Integration and status

Both workers finished and published completion claims through MCP. UI commit: `2103fb29f388f1faeee2013cc044080a4745281b`; backend commit: `534308a9354805b009d81f2147e3102bc893ddea`. Worktrees remain clean and retained. The coordinator merged the UI as `6f50c12` with its embedded asset route and staged the backend sequentially without conflicts. Combined validation is in progress.

Review caught two implementation issues before integration: asynchronous worktree preparation needed to prevent `safeToClose:true`, and changing the shared Git output cap would weaken the existing inspection bound. The backend now reports preparation activity and keeps separate inspection/worktree output limits. A subsequent read-only review by the UI worker found that mutation-free preflight failures consumed request IDs permanently. The backend split durable payload binding from the mutation marker and added recovery tests; unchanged retries recover after capacity or destination preflight failures, while changed payloads conflict and uncertain mutations cannot repeat automatically.

The reviewer also reported a test-port mismatch, but that finding was rejected: the backend's assigned range was 4351–4355, distinct from the UI's 4361–4365. The original assignments remain valid. No worker changed the live service, grants, shared dependencies or another checkout.

The coordinator updated the existing missing-import check to expect rejection before registration, added policy/UI test commands and verification wiring, and supplied a real browser/API integration check using demo agents and isolated Git fixtures.

Combined main passed 10 Rust unit tests, formatting/whitespace checks, 26 Playwright tests, native MCP legacy/modern discovery, existing API/project/settings/import/workspace/automation/completion checks, real Git worktree policy/retry/activity/timeout/output-bound checks, communication/coordinator regressions, restart/crash persistence, and graceful shutdown with an open browser WebSocket. A separate browser check against the actual backend created a requiring project and new worktree, inspected Git context, and selected the same existing linked worktree on mobile without inference. Its first run needed a test-script correction to open the mobile drawer; the corrected run passed with no page errors.

Strict Clippy retains seven pre-existing style findings in unchanged code. Failed or interrupted mutation may retain a branch/directory and needs inspection before a new request ID; safe preflight failures can retry unchanged. Missing/prunable paths are omitted, Git administrative paths must be inside allowed roots, and protection against another process racing filesystem changes remains bounded by the same-user trust model. Cleanup/merge controls remain deferred.

Deployment requires an idle activity endpoint and a catalog/database backup. Validation artifacts remain ignored under `.cache/batch-002/`.

The user has requested stopping after this round is integrated. No further feature batch will be dispatched. Named external controller credentials, session lifecycle controls and a review/merge queue remain backlog items; worktree cleanup requires its own safety and ownership design.
