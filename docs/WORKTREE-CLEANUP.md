# Worktree ownership and archive cleanup (C-04b)

A session **owns** a worktree only when Agentworks created it for that session (`workspace.mode=newWorktree`, including forks that create a new worktree). Ownership is recorded in session state as path, branch, repository common directory, main checkout and base commit. Sessions using a project directory, a selected existing worktree, imports and every session created before this change are unowned. Agentworks never removes an unowned worktree.

Branches are never deleted, reset, merged or pushed. Cleanup removes the checkout directory only, so committed work stays on the retained branch. Integrating a branch is a separate, explicit human or coordinator action. Cleanup is exposed only through the human HTTP API and browser: no MCP tool and no coordinator grant can remove, restore or merge a worktree.

## Archive and restore

- **Clean owned worktree:** archive suspends the harness (no turn may be active) and runs `git worktree remove` without `--force`. The session is archived with `worktree.removed=true` and stays explicitly suspended. Its branch and path metadata are kept.
- **Dirty owned worktree** (staged, unstaged or untracked files): a plain archive archives the session and keeps the worktree. The browser checks first and asks **Archive, delete changes**, **Archive, keep worktree** or **Cancel**. Cancel changes nothing; Keep archives and retains every file.
- **Delete changes:** the request carries the status fingerprint the human saw. The fingerprint covers HEAD, porcelain status and the size and mtime of each dirty path. If anything changed since then, the request returns 409 and the session is not archived. The harness is suspended and the state re-inspected before `git worktree remove --force` runs.
- **Unarchive** recreates the checkout at its recorded path from the retained branch (`git worktree add -- <path> <branch>`), after the same destination checks used for creation: absolute path, no symlinked ancestors, inside allowed roots, and exclusive reservation of an absent directory. The session stays suspended; resume it deliberately.
  - **Occupied restore path:** unarchive returns 409, the session stays archived, and the occupant is untouched. The exception is a path that is already this repository's linked checkout of the recorded branch (for example, recreated manually), which is adopted.
  - **Missing branch, changed repository, or a failed `git worktree add`:** restore is refused. Only the empty directory Agentworks reserved is removed.
- **Resume** of a session whose worktree was removed returns 409, asking you to unarchive it.

Cleanup is refused (an automatic archive retains the worktree and reports why; explicit removal returns 409) when:

- the session has an active turn or a starting harness;
- the worktree is locked;
- another registered session's cwd is inside it;
- HEAD is detached or on a branch other than the recorded one, since commits might exist only in that checkout;
- the path no longer resolves to the recorded linked worktree of the recorded repository, or lies outside the allowed roots;
- Git status cannot be read within its bounds.

Ignored files do not count as dirty and are deleted with a clean checkout, as with `git worktree remove`. This includes build output such as `target/`, and also ignored local secrets such as `.env`. Keep anything you need outside ignored paths, or choose **Archive, keep worktree**.

Project archive/unarchive and history deletion do not run worktree cleanup; they keep their earlier behavior of retaining every file.

## HTTP API

- `GET /api/sessions/{id}/worktree` returns ownership and dirty state:
  - `{owned, state, path, branch, baseCommit, removedAt, dirty, fingerprint, locked, sharedWith, cleanup:{allowed, requiresConfirmation, reason}, integration:{merge:false, note}}`
  - `state` is `unowned`, `present`, `removed` or `missing`.
  - `dirty` is `{staged, unstaged, untracked, paths (first 20), truncated}`.
- `PATCH /api/sessions/{id}` adds optional `worktree: "auto"|"keep"|"remove"` and `confirmDirty` when archiving. Omitted means `auto`. Using either field without `archived:true` is rejected.
  - The response adds `worktreeCleanup: {outcome: "removed"|"retained"|"none", reason?, forced?, branch?, path?}`.
  - Unarchive adds `worktreeRestore: {outcome: "restored"|"adopted", path, branch}`.
- `POST /api/sessions/{id}/worktree/remove {confirmDirty?}` removes a retained owned worktree of an already archived session, under the same checks.
- Session metadata and snapshots include `worktree: {owned, path, branch, removed}` or null.

Git runs with the existing bounded runner: argument arrays, no hooks or fsmonitor, cleared `GIT_*` overrides, a 10-second timeout and a 256 KiB output cap. Archive cleanup holds the run lock and then the workspace lock. The same-OS-user trust limitation from [WORKTREE-POLICY.md](WORKTREE-POLICY.md) applies: pathname checks do not defeat a concurrent malicious process, and a small window remains between the final status check and `git worktree remove --force` for confirmed removals.

## Checks

```bash
CARGO_TARGET_DIR="$PWD/target" cargo build --locked --offline
npm run test:worktree-cleanup
```

The smoke owns `127.0.0.1:4521` (4521–4525 via `AGENTWORKS_CLEANUP_PORT`), real Git fixtures, SQLite, catalog and demo state under `.cache/worktree-cleanup-*`, which it keeps for inspection. It uses only the demo harness. It covers:

- clean removal with ignored output, branch retention, refused resume, persistence across restart, and restore and resume;
- dirty counts, automatic retention, keep, stale or missing confirmation, and confirmed forced removal;
- occupied, adopted and missing-branch restores;
- unowned, shared, locked, detached, running and not-archived refusals;
- the browser Cancel, Keep and Delete flow on desktop and at a 390px phone width.
