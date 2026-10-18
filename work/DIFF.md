# Session diff viewer (D-01)

Open **Diff** in a session's action bar. The dialog shows the saved session directory, actual Git worktree, branch and inspected HEAD. Nested session directories inspect the entire worktree. Opening and closing the dialog preserves the conversation, draft and connection.

- **Current unstaged changes** compares the index with working files (`git diff`). A file changed in both places shows only its unstaged patch. Staged changes and untracked paths are listed separately under the disclosure; their contents are excluded from this view.
- **Branch against base** accepts a local/remote branch or commit revision. Suggestions include up to 500 local/remote branches. It resolves the selected base and HEAD to commits, computes their merge base, and compares that tree with HEAD. Uncommitted changes are excluded. The display includes the resolved base, merge base and compared HEAD IDs.
- **Specific commit** compares the resolved commit with its first parent. An initial commit is compared with the empty tree. Merge commits show changes against their first parent.

Choose a changed file to load its patch. Each selection refreshes the comparison and returns its own file list and resolved IDs, so the patch and comparison label describe the same committed trees even if a branch moved since the previous request. Current unstaged changes are a live inspection, not an atomic filesystem snapshot; refresh after concurrent edits. The context HEAD is explicitly labeled as the value at context inspection.

Renames appear as deletion/addition. Submodule contents are excluded. Binary changes show Git's binary summary, without binary contents. Non-UTF-8 patch output, oversized output and Git failures show an explicit unavailable message. Arbitrary file downloads and staged-content previews are outside this viewer's scope.

## Bounds and protections

The read-only endpoint is `GET /api/sessions/{id}/diff`, with `mode=unstaged|branch|commit`, optional `base`, `commit`, and optional exact `file`. Revisions are argv arguments, reject options/whitespace/control characters and are limited to 256 bytes; Git verifies them as commits before comparison. Files must belong to the freshly computed comparison, and selection uses literal pathspecs. No shell is used.

The endpoint reuses current allowed-root/worktree checks and the existing Git environment filtering, disabled optional locks, hooks and fsmonitor protections. External diff and textconv helpers, color and recursive submodule inspection are disabled explicitly. No Git mutations occur.

The entire request, including workspace inspection, has a ten-second limit. Individual stdout/stderr streams are bounded. Primary file lists are limited to 128 KiB and 2000 paths; over-limit comparisons fail explicitly. Auxiliary staged/untracked lists show an explicit unavailable/truncation notice. Patches are limited to 256 KiB and retain whitespace exactly; an oversized patch is omitted with an explanation. Ref suggestions are bounded by 128 KiB/500 entries. A failed suggestion list does not prevent a typed revision.

## Verification

Build the binary with the assigned offline Cargo environment, then run:

```
PLAYWRIGHT_BROWSERS_PATH=/home/captain/projects/agentworks/.cache/browsers node_modules/.bin/playwright test tests/diff.spec.mjs
```

The fixture owns only `127.0.0.1:4421` and temporary repositories, SQLite database, catalog and demo state. It tests nested linked worktrees, diverged/selected bases, root/merge/detached commits, staged/unstaged separation, literal unusual filenames, exact patch preservation, binary/large output, configured helper suppression, unchanged index bytes, invalid refs, symlink/current-root checks, a timed-out Git process, and desktop/mobile dialog behavior. It makes no provider/model calls.
