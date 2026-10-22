# Worktree launch controls

New session offers **Use project directory**, **Create new worktree**, and **Use existing worktree**. The existing directory field becomes the source directory for Git discovery/creation. New worktrees accept optional branch and base revision; blank values delegate unique branch selection and HEAD to the backend. New trees contain the committed revision; uncommitted source changes stay in the source directory. Existing discovery lists allowed worktrees, disables main/locked/prunable entries, and never launches an agent. Selecting a linked path sends that path as `cwd`, with `workspace.mode=existingWorktree`.

Project create/edit exposes the default mode and an isolation requirement. Requiring isolation disables directory mode, and checking the requirement changes a directory default to new worktree. Existing policy is fetched before editing; failed reads block saving/launching. Policy and discovery generations guard stale responses when switching project/source or reopening dialogs. Project switches clear branch/base/path overrides and retain harness/model selections. Policies apply to new sessions/automation runs; existing sessions and their drafts/cwd remain intact.

Project name and workspace policy use separate API operations. If policy saving fails after project creation or rename, the dialog explicitly reports that the name was saved and retries against the saved project ID. It never repeats project creation after a known successful POST. An ambiguous network failure during the project POST itself remains an API limitation because project creation has no idempotency contract.

Session create requests include a request ID. Identical failed attempts reuse it, including after a reload in the same tab (`sessionStorage`); changed payloads use a new ID. Success clears the pending attempt. Backend preparation errors remain visible; retry never performs client-side cleanup. If the backend marks a preparation permanently failed, inspect the retained branch/files before making a changed attempt. Session deletion explicitly retains worktree files and branches. Archive cleanup of session-created worktrees asks before deleting changes ([C-04b](WORKTREE-CLEANUP.md)); Review/integration tracking is described in the [review queue](REVIEW-QUEUE.md).

## Checks

The focused suite serves only static files on `127.0.0.1:4362`; every API/WebSocket is mocked, so it needs no Rust build, paid inference, catalog, database, or live service. Playwright artifacts use this worktree's `.cache/browser-results`.

```sh
node --check web/app.js
node --check web/worktree-controls.js
npx playwright test tests/worktree-launch.spec.mjs
git diff --check
```

Desktop/mobile coverage includes defaults, enforcement, optional fields, actual payload cwd, ineligible worktrees, safe text, discovery errors/no auto start, source/project changes, slow policy/discovery responses, harness/model/draft preservation, ordinary directory launch, stable retry IDs across reload, editor loading/failure, partial-save retry, and deletion wording.
