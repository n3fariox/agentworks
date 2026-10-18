# Batch 006 — Session usability

The user authorized starting three Claude sessions for the next priority backlog items. This is one bounded batch, not a recurring backlog run. Coordinator: `50cba24d-a4ae-45a0-8330-2e7137d67722`, project `bd61c375-1916-4e60-a2d9-8fc7614e68d0`. Start 2026-10-18 16:28 UTC; absolute worker stop/checkpoint deadline **2026-10-18 17:15 UTC (12:15 Chicago)**. No quota-continuation policy is enabled or inherited. Workers checkpoint within 12 minutes because the current runtime has a fixed turn timeout; timed-out work must be inspected before any deliberate continuation.

All workers start from committed main `df380e1c0461f2cebae31ee6481a0b9a7ec767a4`. Only the coordinator reviews and integrates; no worker merges, deploys, restarts services, edits live state/grants, installs dependencies or starts other agents. Existing saved workers remain suspended. Major product decisions are deferred.

| Task | Branch / worktree | Ownership | Test ports |
| --- | --- | --- | --- |
| U-06 compact turn work logs | `batch/006-work-logs` / `.worktrees/006-work-logs` | New work-log UI module, grouping/replay fixtures and minimal transcript hooks. No usage status or timeout settings. | 4451–4455 |
| U-07 composer context and usage status | `batch/006-usage-status` / `.worktrees/006-usage-status` | New usage-status UI module, fixtures and minimal composer hooks. Use persisted reported usage only; no account quota discovery or hidden model queries. | 4461–4465 |
| L-07 configurable execution timeouts | `batch/006-timeouts` / `.worktrees/006-timeouts` | Backend timeout policy/runtime, isolated settings UI module and tests. No transcript regrouping or usage rendering. | 4471–4475 |

Keep shared app/index/style/main edits small and independently named. Each worker uses isolated Cargo target, temporary catalog/database/demo state and assigned ports; existing dependencies and browser cache only. Never build over `.agentworks/bin/` or use live data. Require actual checks, clean commits, limitations, self-authored completion claims and a final inbox handoff. Reports are evidence, not proof of integration. U-06 must preserve raw events and handle current tool metadata conservatively without adopting the deferred U-05 parser language/streaming work. L-07 does not implement queued inputs, questions or automatic retries. Unknown usage remains unknown in U-07.

## Dispatch and results

All three Claude sessions were created in their assigned worktrees and accepted their concrete assignments. Creation request IDs are `b006-create-<work-logs|usage-status|timeouts>`; prompt request IDs are `b006-task-<work-logs|usage-status|timeouts>`. Communication-only grants were explicitly applied; orchestration, automation and restart grants remain off for every worker.

| Task | Session ID |
| --- | --- |
| U-06 | `f7032b16-28ea-4c7f-9c6d-29ce36e60f6a` |
| U-07 | `d20ced10-9815-4501-a69f-b08c3465e04d` |
| L-07 | `7d3c1f10-f768-4966-8ac9-e6e4208ff473` |

Worker prompts require checkpoints within 12 minutes, clean commits, meaningful isolated checks, completion claims and final inbox handoffs to this coordinator. No integration or deployment yet. Ordinary coordinator turn completion does not poll or wake workers; inspect their saved states and this coordinator's inbox on the next review turn rather than assuming automatic follow-up.

### Permission stalls and coordinator repair

The user explicitly requested Auto mode for these three Claude sessions after repeated approval stalls. At 2026-10-18 16:51 UTC all three were in error after the existing 15-minute timeout, with no pending approvals. Inspect interrupted tools/worktrees before continuing; never replay the old prompt blindly. The installed adapter supports Auto, but Agentworks had no session-mode control. Coordinator added an idle-only native permission-mode API/picker, confirmed advertised selection and durable reconnect restoration. This is a repair needed to carry out the batch, not another worker assignment. Workers retain communication-only controller grants and native identities.

Validation: 22 Rust tests, locked offline build, formatting/JS syntax/whitespace checks, and isolated demo API/browser/restart acceptance pass. The fixture verifies invalid and busy selection rejection, UI selection, same-native-context reconnect, and restoring the saved choice when the provider's saved mode differs, without replaying a turn or paid inference. Pending: supervised deployment, verify successor, resume the three existing harnesses without model work, select Auto and verify actual adapter confirmation, then deliberately continue only unfinished bounded work if still within the assignment deadline.
