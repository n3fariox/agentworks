# Batch 005 — Supervised service restart

The user requires R-06 to be implemented, tested and running before transferring the coordinator conversation into Agentworks. This supersedes leaving restart as the next planned assignment. Scope includes R-01 Linux supervision only as needed; no unrelated backlog work.

Dedicated Codex Agentworks session `a51fafd6-b287-4a33-ae8e-ab5aa70ae7d9`, branch `batch/005-service-restart`, worktree `.worktrees/005-service-restart`, base `ee84380`. Project `88714822-785c-4958-ae8c-4c55db2854ed`; coordination inbox `f033a03b-271c-4579-8627-f27b8af52f3a`. The worker has communication-only access and cannot restart/deploy the live service. Tests own ports4441–4447 and isolated state. The coordinator reviews, integrates, installs live supervision and verifies a real restart and preservation before handoff.

Acceptance: an external supervisor owns restart/readiness; unsupported direct mode refuses restart. Durable request IDs, bounded idle drain or explicit interruption, cancellation, failure/reconnect status and consistent backup. Freeze new work through API/MCP/automations/quota dispatch while draining, retaining approvals/Stop needed to finish. A separate default-off `restartService` grant controls the global MCP operation and is never silently added by coordinator presets or copied to children. Optional explicit bounded native-context requester handoff should make unattended self-updates practical without replaying tools or silently creating fresh context. Prepared immutable releases may be selected explicitly, with no implicit build/download/merge or arbitrary shell execution.

Implementation and review are active. Final behavior, limits, checks and deployment evidence will be recorded here.
