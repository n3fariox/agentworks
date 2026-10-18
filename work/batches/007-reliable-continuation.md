# Batch 007 — Reliable continuation and queued input

The user authorized Codex and Claude (Auto mode) sessions to implement B-02, C-06 and L-05. This is one bounded batch, not an open-ended backlog run. Coordinator `50cba24d-a4ae-45a0-8330-2e7137d67722`, project `bd61c375-1916-4e60-a2d9-8fc7614e68d0`. Launch preparation 2026-10-18 20:30 UTC. Absolute worker stop/checkpoint deadline **2026-10-18 21:15 UTC (16:15 Chicago)**. First checkpoint within ten minutes; first clean implementation handoff by 21:00 UTC. Worker total-turn policy is explicitly 1800 seconds with 15-second cancellation grace and no inactivity stopping. Usage-limit continuation stays off; no old timer or closed assignment is revived.

All three branches start from committed main `d8b505fcddb67d62d3295cef78952be63d9a2564`. Previous completed workers remain archived/suspended. Only the coordinator reviews, merges sequentially, runs combined acceptance and performs supervised deployment. Workers may communicate, report completion and commit in their own worktrees; they cannot orchestrate, restart/deploy, edit live state/grants or install dependencies. Reuse existing dependencies and isolated Cargo targets. A local worktree symlink to the existing repository `node_modules` is permitted for adapter discovery; its contents must not be changed.

| Work | Harness | Branch / worktree | Ownership | Test ports |
| --- | --- | --- | --- | --- |
| B-02 status checks preserve quota recovery | Codex | `batch/007-quota-status` / `.worktrees/007-quota-status` | Quota state/purpose handling in `limit_resume.rs`, small prompt/runtime hooks and quota fixtures. No generic queues, inbox scheduler or unrelated UI changes. | 4491–4495 |
| C-06 optional inbox delivery/wake | Codex | `batch/007-inbox-wake` / `.worktrees/007-inbox-wake` | New inbox-delivery module, durable opt-in policy/ledger, grants/status controls and tests. Own any schema-version migration; coordinate additive queue tables during integration. | 4501–4505 |
| L-05 activity and queued inputs | Claude, confirmed Auto before prompt | `batch/007-queued-inputs` / `.worktrees/007-queued-inputs` | New input-queue/activity modules, durable explicit-input queue, composer controls and tests. No quota-state rewrite or inbox auto-wake policy. | 4511–4515 |

## Shared integration contract

Keep `main.rs`, `runtime.rs`, `store.rs`, `app.js` and index/style edits minimal and explicitly identify their hooks. Do not merge siblings or introduce competing generic dispatcher rewrites. Reuse existing per-session/run serialization and restart admission checks. Explicit human queued inputs take priority over optional inbox wakes. Quota-paused sessions must not be woken around their provider reset by either queue. Stop, Suspend, completion, revocation, deadlines and storage errors must win over automatic dispatch.

B-02 preserves the original unfinished assignment, attempt/deadline bounds and timer through narrowly recognized provider-local status-only commands; it never restores a policy after a human stop/revocation or replays a previous mutation. Replacement work retains deliberate cancellation semantics. C-06 remains off by default and needs explicit bounded recipient authorization; messages are peer data, never permission to enable a policy or escalate worker grants. Its automatic turns need stable IDs, bounded attempts/cooldown/deadline, durable attribution and uncertain-dispatch recovery. L-05 queues explicit inputs with visible order/edit/remove/sending states, stable identities and bounded payloads. Errors/cancellation/timeouts/quota/crash ambiguity pause automatic queue dispatch for inspection; no blind retry. Preserve the separate internal initial-prompt path used by existing automation/handoffs.

Require focused deterministic Rust/API/MCP/restart/browser checks with temporary catalog/database/demo state and assigned ports; no paid inference in tests. Completion claims are evidence to inspect. Workers send checkpoints and final clean commit/checks/limitations to this coordinator, then await review. Defer major product decisions and report incomplete scope accurately.

## Dispatch and results

All three workers accepted their assignments with communication-only grants, 1800-second turn limits and disabled quota continuation. The native Codex permission mode is `agent` (advertised as **Auto review**); Claude's mode is confirmed `auto` (**Auto**). No full-access/bypass mode was selected.

| Work | Active worker session | Native session |
| --- | --- | --- |
| B-02 | `da07bf56-a10c-4327-af6f-707566ab90c2` | `01a150b6-52a2-7192-948a-da43732e599f` |
| C-06 | `a86deeb4-11de-4199-a9ad-1658b0651c8c` | `01a150b6-52b4-7811-8b73-85a5e94d443f` |
| L-05 | `69137be9-2abf-4112-8ae2-353f9dedf206` | `1ecb4869-71be-4920-8724-bbfe2b1bfbee` |

Stable task request IDs are `b007-task-<quota-status|inbox-wake|queued-inputs>`.

Startup correction: the first empty Codex launches (`126eef53-ba4c-41f1-8365-a36d7fc26d4f`, `4a90808d-250f-4db3-801e-fdcdb374f180`) had no task turns. Applying a grant to an idle fresh Codex session reconnects its harness, and the adapter cannot yet resume its unsaved rollout (`no rollout found for thread id`). Both empty launches were explicitly suspended/archived, preserving their records. Their original clean, separate worktrees were reused through the normal human creation API with communication-only grants set at creation, using stable IDs `b007-ready-quota-status` and `b007-ready-inbox-wake`. The actual task turns are attributed to this coordinator through MCP; no task prompt was replayed and no worked native context was replaced. Claude's original launch was retained.

No implementation is integrated or deployed yet. Inbox wake-up itself remains unavailable until C-06 is reviewed and installed; the current manual coordinator must read inbox at active checkpoints rather than assume messages start turns.
