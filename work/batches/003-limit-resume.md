# Batch 003 — Continue after usage limits reset

Status: completed and deployed. The user explicitly requested a new session for L-09 and this feature was completed first. Other backlog work remains deferred.

One real Codex session implemented the feature inside Agentworks in a dedicated worktree created through the deployed worktree-launch API:

- Session: `f0ef54d0-adcd-4b9d-bdfe-8fd68f323977`, **Limit reset · Codex · implementation**.
- Project: `0bba737d-c653-45db-938e-e1e96e7d03c8`, **Agentworks · limit-reset continuation**.
- Coordination inbox: `5e1324e8-43e7-4716-b663-24b230ac8bdc`.
- Branch/worktree: `batch/003-limit-resume`, `.worktrees/003-limit-resume`, based on `1209464`.
- Grants: communication only. The worker cannot use Agentworks controls to start other sessions or alter grants.

## Scope and acceptance

Implement [L-09](../LIMIT-RESUME.md) across backend, saved session state, durable scheduler, MCP child inheritance, and dark/mobile controls. Default off; recognize actual provider limit evidence; persist reset timers and deduplicate continuation across crash/restart. Preserve native context and worktree, stop on human intervention/completion, and never blindly replay commands. Make unknown or unsupported reset data explicit and provide a human-controlled fallback. Recheck authorization, roots, approvals, current turn and inherited constraints at dispatch.

Investigate provider reset evidence early, including whether native Codex exposes an authoritative timestamp. A demo-only scheduler is insufficient. Use native adapter source shapes for deterministic fixtures, document exact provider support, and keep model calls unnecessary for tests.

## Ownership and execution

The worker owns needed runtime/store/MCP/UI/demo/tests/docs changes in its worktree. The coordinator owns the backlog/batch record and integration. No edits to main/other worktrees/live data, shared dependency changes, merges, deployment or live policy/grant changes by the worker. Shared node_modules is linked read-only by convention; Cargo targets and all test data are separate. Worker ports are 4371–4375, localhost only; coordinator checks use other isolated ports.

The worker acknowledged the assignment through MCP, committed its changes, published completion claim #5 and sent a handoff with checks/limitations. The coordinator reviewed actual code and evidence, requested corrections, completed combined regressions and real UI checks, then deployed an immutable tested executable after idle activity checks and backups. The implementation session and clean source worktree remain saved; no next feature session was dispatched.

## Implementation and review

Worker commit: `9968c94f48ceb830b12e3d3c5283951621872fa3`; clean worktree retained. The worker published an authenticated completion claim and final MCP handoff. The coordinator staged the branch on main without conflicts, adding npm/verification wiring for the new smoke suite.

The coordinator verified a real read-only native Codex `account/rateLimits/read` request using the existing sign-in, with no thread or turn creation, and supplied that evidence so Codex has an automatic reset-time path beyond ACP's context-only usage events. Deterministic fixtures test that protocol and exact provider error shapes; no real paid quota exhaustion was induced.

Review corrections included scheduler communication/run/workspace lock ordering; avoiding nested cross-session capacity locks; rejecting empty inherited project scope; separating parent authorization revision from timer generation; manual-resume request deduplication; policy state in events instead of repeated transcript downloads; visible adjustable attempt/deadline bounds; Claude first-turn structured rate-limit fallback; Codex individual spend-window shape; native-opening crash recovery; and aborting/awaiting the scheduler before shutdown suspends harnesses. Two incoming review messages were initially skipped because the worker advanced its inbox cursor using outgoing message IDs; the coordinator identified this, had the worker reread from the received cursor, and verified the missed corrections landed before integration.

## Validation

- Combined main: offline locked build and 17 Rust tests passed, including schema-v5 migration preserving existing tables, exact evidence exclusions, multiwindow reset selection, native Codex read-only protocol, and crash-state reconciliation. Formatting, syntax and whitespace checks passed.
- Full verification: existing API/MCP legacy/modern wire, project/settings/import/workspace/automation/completion/worktree checks, the new local ACP/MCP/mobile timer suite, 26 Playwright tests, and graceful shutdown with an open browser WebSocket passed.
- Existing communication/coordinator/grant/revocation and restart/crash/context/transcript persistence regressions passed separately.
- An independent actual browser/API check verified a future countdown and custom seven-attempt/two-day policy on desktop/mobile, preserved the timer through graceful restart, and confirmed Stop removes the timer and authorization. Mobile panel geometry fits the viewport; no page errors or model calls. Artifacts remain ignored under `.cache/batch-003/`.
- A final live visual check exposed advanced controls pushing the composer below the mobile viewport. The coordinator bounded the expanded panel with internal scrolling and a sticky summary, and added assertions that its last control and the composer remain reachable. The timer acceptance suite and all 26 browser regressions passed again against that focused layout fix.

## Behavior and limitations

Off by default. Human controls expose three-attempt/24-hour defaults with configurable 1–100 attempts and a deadline within 30 days. MCP `inheritResumePolicy:true` propagates an enabled parent's remaining per-session attempt bound/deadline without granting coordinator access. Stop, explicit completion/blocked reports and policy revocation prevent later wakes; normal successful turns do not schedule new work. Scope remains in the native conversation; natural-language completion is not inferred.

Claude requires structured rejected metadata for an automatic timestamp; its exact first-turn rate-limit error can produce an unknown-reset manual pause. Codex exact quota errors trigger a bounded read-only quota request. Unavailable/stale resets and API-key mode remain manual; OpenCode automatic reset detection is unsupported. Unknown claim/dispatch outcomes after a crash pause for inspection rather than automatically replaying work. Provider wake-ups are conservatively staggered across accounts. Full behavior is documented in [L-09](../LIMIT-RESUME.md).

## Deployment and preservation

Feature integration: `770b281`; final mobile correction: `c15ba87773fc`. The tested immutable executable `.agentworks/bin/agentworks-c15ba87773fc` listens on `0.0.0.0:4318` through the existing VPN. No automatic policy was enabled as part of deployment.

Before migration, idle checks confirmed zero turns/approvals and an integrity-checked schema-v5 SQLite/catalog backup was saved under `.agentworks/backups/batch003-20261017-225726/`. Post-migration comparison preserved all 12 session IDs/metadata, 12,776 pre-update transcript events byte-for-byte, all 5 completion reports and the project/location/settings catalog; schema v6 and integrity passed.

For the final layout update the coordinator waited for the user's active **Model Prompts** Claude turn to finish. A fresh schema-v6/catalog backup was taken under `.agentworks/backups/batch003-layout-20261017-230224/`. The final update preserved all 12 sessions, 13,095 then-existing transcript events and 5 completion reports. Backup directories use 0700 and catalog/database copies use 0600.

Final VPN-facing desktop/mobile checks confirmed default-off new/existing-session controls, visible editable bounds, reachable timer controls and composer, no page errors, no turns started by verification and no scheduled continuations. The web service remains running; other feature work remains deferred.
