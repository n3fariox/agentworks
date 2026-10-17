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

Both workers acknowledged their tasks through MCP and are implementing their assigned halves. They will commit only owned files and submit completion claims with checks and limitations. The coordinator will review implementation and reports, merge sequentially, run combined checks, and deploy only when the live activity endpoint permits shutdown and data has been backed up.

Next coordination work remains named external controller credentials and a review/merge queue. Session lifecycle controls can proceed independently after this batch; worktree cleanup requires its own safety and ownership design.
