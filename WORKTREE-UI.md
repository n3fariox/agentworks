# Worktree launch UI handoff

New session offers **Use project directory**, **Create new worktree**, and **Use existing worktree**. The existing directory field becomes the source directory for Git discovery/creation. New worktrees accept optional branch and base revision; blank values delegate unique branch selection and HEAD to the backend. New trees contain the committed revision; uncommitted source changes stay in the source directory. Existing discovery lists allowed worktrees, disables main/locked/prunable entries, and never launches an agent. Selecting a linked path sends that path as `cwd`, with `workspace.mode=existingWorktree`.

Project create/edit exposes the default mode and an isolation requirement. Requiring isolation disables directory mode, and checking the requirement changes a directory default to new worktree. Existing policy is fetched before editing; failed reads block saving/launching. Policy and discovery generations guard stale responses when switching project/source or reopening dialogs. Project switches clear branch/base/path overrides and retain harness/model selections. Policies apply to new sessions/automation runs; existing sessions and their drafts/cwd remain intact.

Project name and workspace policy use separate API operations. If policy saving fails after project creation or rename, the dialog explicitly reports that the name was saved and retries against the saved project ID. It never repeats project creation after a known successful POST. An ambiguous network failure during the project POST itself remains an API limitation because project creation has no idempotency contract.

Session create requests include a request ID. Identical failed attempts reuse it, including after a reload in the same tab (`sessionStorage`); changed payloads use a new ID. Success clears the pending attempt. Backend preparation errors remain visible; retry never performs client-side cleanup. If the backend marks a preparation permanently failed, inspect the retained branch/files before making a changed attempt. Session deletion explicitly retains worktree files and branches. Archive cleanup of session-created worktrees asks before deleting changes ([C-04b](work/WORKTREE-CLEANUP.md)); merge controls are deferred.

## Checks

No dependencies installed. The focused suite serves only static files on `127.0.0.1:4362`; every API/WebSocket is mocked, so it needs no Rust build, paid inference, catalog, database, or live service. Playwright artifacts use this worktree's `.cache/browser-results`.

```sh
node --check web/app.js
node --check web/worktree-controls.js
PLAYWRIGHT_BROWSERS_PATH=/home/captain/projects/agentworks/.cache/browsers npx playwright test tests/worktree-launch.spec.mjs
git diff --check
```

Desktop/mobile coverage includes defaults, enforcement, optional fields, actual payload cwd, ineligible worktrees, safe text, discovery errors/no auto start, source/project changes, slow policy/discovery responses, harness/model/draft preservation, ordinary directory launch, stable retry IDs across reload, editor loading/failure, partial-save retry, and deletion wording.

## Coordinator integration

The Rust router embeds individual assets. Backend owner must serve `GET /worktree-controls.js` as JavaScript using `include_str!("../web/worktree-controls.js")`. Coordinate merge order: the backend route cannot compile until this new file is present. UI branch does not modify Rust. Both halves must be integrated and rebuilt before real browser checks.

Use isolated explicit roots/catalog/SQLite/demo paths under `.cache`, bind `127.0.0.1`, and choose an available port in 4361–4365. Do not use the live service. With real API + demo provider, verify:

1. Old project defaults are directory/false; ordinary demo launch still works.
2. Set newWorktree + require=true, create from a fixture repository with an uncommitted source edit, and inspect Workspace info for the actual linked cwd/branch/committed revision. Confirm source edits remain only in source.
3. Repeat identical create HTTP request ID and verify same session/tree; changed payload conflicts. Check errors and failed-preparation retention without cleanup.
4. Discover/select a real linked worktree; check root filtering and disabled main/locked/prunable entries. Directory mode on a main checkout must fail under requiring policy; actual linked nested cwd may pass backend validation.
5. Verify MCP/manual automation/import paths enforce the same backend policy, while registered resume stays in its old cwd.
6. Delete the demo session and verify branch/worktree/files remain. Reopen an existing session and confirm unchanged draft/model/context.
7. Run existing session/workspace browser regression suites against the combined backend, overriding their server ports if their fixtures use ports outside this batch's range. Do not run tests against live data.

Mocked UI tests cannot establish Git security, durable backend deduplication, all-provider validation, or API compatibility. Those remain combined-backend checks for the coordinator/backend owner.
