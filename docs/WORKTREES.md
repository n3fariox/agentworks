# Session workspace context

Open a chat and select **Workspace info** in the session toolbar. The dialog shows the session directory, current branch or **Detached HEAD**, full commit SHA, worktree root, repository root, and whether this is a linked Git worktree. It works on desktop and mobile, closes with Escape or its close button, and preserves the chat draft and ongoing stream. Reopen it to refresh after changing branches outside Agentworks.

`GET /api/sessions/{id}/workspace` uses the existing HTTP guard, session lookup, and current allowed roots. It does not start a model turn, read arbitrary files through the browser, or change stored session data. There is no schema migration. Removed session directories return a usable `unavailable` response; directories outside current allowed roots return HTTP 403, and unknown sessions return 404.

Example Git response (paths abbreviated):

```json
{
  "cwd": "/projects/app-feature/src",
  "status": "git",
  "repositoryRoot": "/projects/app",
  "worktreeRoot": "/projects/app-feature",
  "gitDir": "/projects/app/.git/worktrees/app-feature",
  "gitCommonDir": "/projects/app/.git",
  "branch": "feature/workspace",
  "head": "branch",
  "commit": "full Git commit SHA",
  "linkedWorktree": true,
  "message": null
}
```

`status` is `git`, `non_git`, or `unavailable`. Detached HEAD has `head: "detached"` and `branch: null`. A branch with no commits has `commit: null` and `message: "No commits yet"`. Non-Git and unavailable responses retain `cwd`, include a human-readable `message`, and set Git fields to null. A conventional shared `.git` directory identifies the main repository root; for repositories with a separate/custom Git directory, `repositoryRoot` identifies that shared metadata directory. `worktreeRoot` always identifies the checkout root. Bare repositories have no worktree and report unavailable.

Inspection runs read-only Git argument arrays, with no shell. It removes inherited `GIT_*` overrides, ignores global/system config, disables optional locks, hooks and fsmonitor, and prevents ancestor discovery beyond allowed roots. Linked worktree inspection necessarily reads Git's shared metadata. Each stdout/stderr stream is capped at 16 KiB and the whole Git inspection has a three-second timeout; children are killed on timeout or oversized output. Missing Git, removed directories, invalid UTF-8 output, and Git errors show an unavailable state. Inspection is best effort while another process changes Git metadata; reopen the dialog after concurrent Git operations finish.

## Focused checks

Run from this worktree with the installed dependencies; no install is needed:

```bash
\
CARGO_TARGET_DIR="$PWD/target" cargo test --locked --offline
\
CARGO_TARGET_DIR="$PWD/target" cargo build --locked --offline
\
node node_modules/@playwright/test/cli.js test tests/git-context.spec.mjs
cargo fmt --check
```

The focused Playwright file starts and stops its own server at `127.0.0.1:4331` with explicit roots and isolated catalog, SQLite, and demo paths in a temporary `.cache/git-context-*` directory. Port 4331 must be free. It creates real temporary Git repositories and a linked worktree, exercises nested paths (including spaces), branch refresh, detached HEAD, unborn branches, non-Git/removed directories, changed roots and symlink escape rejection, inherited Git override isolation, output caps, and timeout/failure behavior. Desktop and 390px mobile checks cover dialog open/close, safe text rendering, error/non-Git states, draft and stream preservation, and viewport bounds. All sessions use the deterministic demo harness. It never checks out or modifies the source checkout and removes its fixtures afterward.
