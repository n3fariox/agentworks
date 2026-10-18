# Batch 006 — Session usability

The user authorized starting three Claude sessions for the next priority backlog items. This is one bounded batch, not a recurring backlog run. Coordinator: `50cba24d-a4ae-45a0-8330-2e7137d67722`, project `bd61c375-1916-4e60-a2d9-8fc7614e68d0`. Start 2026-10-18 16:28 UTC; initial absolute worker stop/checkpoint deadline **2026-10-18 17:15 UTC (12:15 Chicago)**. Worker quota continuation is off and was not inherited. The coordinator's separate human-enabled policy was retained. Workers checkpoint within 12 minutes because the current runtime has a fixed turn timeout; timed-out work must be inspected before any deliberate continuation.

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

Validation: 22 Rust tests, locked offline build, formatting/JS syntax/whitespace checks, and isolated demo API/browser/restart acceptance pass. The fixture verifies invalid and busy selection rejection, UI selection, same-native-context reconnect, and restoring the saved choice when the provider's saved mode differs, without replaying a turn or paid inference. Deployed repair commit `32449ed` as immutable `agentworks-permission-modes-32449ed` through supervised request `b006-enable-auto-32449ed`. Verified successor instance `51ddad1e-eab7-4f75-b123-e3ae9ae4698a`, PID 896077, matching ready receipt/release and no manager fault. The coordinator's same-context handoff was dispatched successfully.

At 2026-10-18 16:58 UTC all three existing harnesses resumed idle and their actual native adapters confirmed `currentValue:auto`. Native IDs remained unchanged: U-06 `e1123f04-1160-4ffb-b390-a3d88e335491`, U-07 `0b5c4f2e-6d80-41b3-84f3-1ae54a3525bf`, L-07 `1e7d14cc-63ed-4d2a-a86d-f0f7d6c309e4`. No replacement sessions were created and no bypass-permissions mode was selected.

Each worker then accepted one fresh inspection/continuation instruction using stable request ID `b006-auto-continue-<work-logs|usage-status|timeouts>`. They must inspect interrupted tool outcomes/files before proceeding, preserve original ownership, checkpoint within ten minutes and commit/hand off by 17:12 UTC ahead of the unchanged 17:15 UTC absolute deadline. No prior tool or prompt was replayed. Feature work remains unreviewed/unintegrated; only the permission-mode repair was deployed.

### Final worker handoffs and renewed review

The workers finished idle with clean commits: U-06 `70271f8`, U-07 `e6ac0be`, L-07 partial backend `8bb0ddc`. Saved handoffs #88–90 include checks, limitations and pending decisions. No feature commit was merged on the basis of these claims.

The coordinator's response to the user's first question about these handoffs hit a verified Codex quota limit. Its recorded timer targeted 2026-10-18 18:15:04 UTC, but a subsequent `/status` turn cleared it with zero automatic attempts. The user requested this defect be recorded at high priority: B-02 now captures evidence and regression acceptance. This does not authorize implementing B-02 during this review.

The user then explicitly authorized continuing review. At approximately 18:48 UTC the same three workers accepted targeted review/fix requests, retaining their branches, ownership and isolated test ports. New stop/checkpoint bound is 19:10 UTC; each turn must checkpoint within ten minutes and finish within twelve minutes. No fresh backlog assignments or quota policies were added.

Initial review blockers:

- U-06: real user-message IDs change on replay, invalidating stored expansion choices; explicit turn endings/cancellation and unresolved tool states need truthful summaries; folding must not hide failures; real application reload and ordering coverage needed.
- U-07: missing embedded module route; timestamp-less history is incorrectly freshened on reload; unrelated updates freshen old cost/quota; attribution and input validation need checking; unsupported cost assumptions and inferred compaction labels need correction. Live inspected events already contain Claude unified five-hour/seven-day utilization, which must be handled without account queries.
- L-07: original checkpoint omits per-session overrides, controls and acceptance tests. General settings saves bypass timeout validation/effective-policy synchronization; elapsed timing uses wall clock; repeated snapshots may defeat inactivity; timeout outcomes can be reported as normal success or schedule quota recovery. These block integration.

U-06 corrections through `1efe639` were inspected and integrated as `49e1034`. Stable session/sequence keys now preserve expansion across actual application reload and session switches. Errors/images/notices stay outside folded segments, persisted turn endings report cancellation, duplicate tool notifications merge, chronological member updates remain ordered, and unresolved tool status no longer claims a background process is alive. Live-observed duration is deliberately omitted after replay because event timestamps are absent.

U-07 corrections through `419bf8d` were inspected and integrated as `c16cb8b`, preserving work logs and permission controls. The module has an embedded route, separate field freshness, unknown freshness for timestamp-less history, conservative cost labels, attribution filtering and value checks. Verified Claude unified-window utilization fractions render explicit used/remaining percentages for five-hour/seven-day quotas. Unsupported account quota retrieval is not performed. No hidden model/CLI usage queries occur.

The combined main build passed 22 Rust tests, locked offline build, formatting/syntax/whitespace checks, usage fixture checks, permission-mode selection/native-reconnect acceptance and `scripts/verify.mjs` with isolated port4485/state. Full verification passed API/settings/import/workspace/automation/completion/worktree/quota/lifecycle/native MCP checks, six supervisor tests, isolated real restart/handoff/browser acceptance, **59 browser tests (one opt-in test skipped)** and graceful open-WebSocket shutdown. Logs: ignored `.cache/batch006-review-verify.log`. Immutable release `agentworks-c16cb8b` is staged with protocol1 metadata; live transition remains pending.

L-07 remains unmerged at `bda4508` after inspection. Its new UI/override and backend fixes improve the checkpoint, but later coordinator messages #99/#101 were not applied before its handoff. Remaining blockers include persistence failure acknowledgement, activity cleanup on aborted/error paths, duplicate tool-call activity, stale inherited-default display, undiagnosed session browser failures and no actual runtime expiry acceptance. The worker accepted `b006-review-timeouts-2` at 18:58 UTC to resolve only these findings, with the same 19:10 UTC absolute checkpoint deadline. A passing claim alone will not cause integration. B-02 remains planned at high priority, separate from these fixes.
