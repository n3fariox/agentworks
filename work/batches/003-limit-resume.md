# Batch 003 — Continue after usage limits reset

Status: implementation running. The user explicitly requested a new session for L-09 and wants this feature completed first. Other backlog work remains deferred.

One real Codex session is running inside Agentworks in a dedicated worktree created through the deployed worktree-launch API:

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

The worker acknowledged the assignment through MCP. It will commit its changes, publish a completion claim and send a handoff with checks/limitations. The coordinator reviews actual code and evidence, requests corrections as needed, runs combined regressions and a real UI check, then deploys an immutable tested executable only after an idle activity check and backup. Existing sessions, transcripts, completion claims and settings must survive the migration/update.
