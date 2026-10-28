# Git settings and copy-on-write worktrees

[Documentation index](README.md) · [Worktree policy](WORKTREE-POLICY.md) · [Archive cleanup](WORKTREE-CLEANUP.md)

## Settings and repository configuration

**Settings → Git** controls how newly created session worktrees are populated:

- **Ordinary checkout** (default): use Git normally.
- **Prefer copy-on-write**: share unchanged file blocks when the filesystem supports reflinks; otherwise use ordinary checkout.
- **Require copy-on-write**: fail creation when reflinks or the repository's checkout format are unsupported.

Shared build directories are an explicit list of relative ignored directories, one per line. They are copied only on the CoW path. **Enable repository worktree hooks** enables commands for newly created session-owned worktrees; it defaults off. Existing sessions retain their captured hook definitions and enablement.

**Project settings → Git settings…** opens the same category with the project's directory and host. The repository editor can also load any allowed repository location. Type a path for folder suggestions from the selected host, or use **Browse…** to select a folder and load its configuration automatically. It reads and saves one `.agentworks/config.toml` in the repository's main checkout, including when opened from a linked worktree. Global **Save settings** saves defaults; **Save repository configuration** explicitly writes the file. Saving either does not implicitly save the other. The repository save rejects a stale fingerprint instead of overwriting a concurrently edited file.

```toml
version = 1

[git.worktrees]
copy = "prefer-cow"
share = ["target"]

[[git.hooks.enter]]
name = "Refresh compile commands"
command = ["./scripts/refresh-compile-commands"]
timeout_seconds = 120

[[git.hooks.exit]]
name = "Stop worktree Bazel server"
command = ["bazel", "shutdown"]
timeout_seconds = 30
```

Repository `git.worktrees.copy` and `share` override their global defaults. Omitted values inherit; an explicit empty `share = []` disables cache sharing for that repository. Hook definitions come from the repository file, and run only if enabled in Git settings when the session is created. Other top-level sections are allowed and preserved by the editor; unknown Git fields and unsupported configuration versions are rejected. Future features should use sections of this same file.

The file need not be committed to apply on the current host. Commit it if other checkouts/hosts should receive the settings. Agentworks' own repository ignores `.agentworks/` runtime state; tracking its config file may require an explicit ignore exception or `git add -f .agentworks/config.toml`. Never commit runtime catalogs, credentials, or databases alongside it.

## Enter and exit

Hooks apply only to worktrees Agentworks creates for a session (`newWorktree`), including those required by project policy. They are independent of foreground chat selection, browser connections, and normal harness suspension/resume.

| Event | Behavior |
| --- | --- |
| Creation | Enter runs after checkout verification and durable session creation, before starting the harness. |
| Archive | Exit runs after stopping the session's resources, before permitted checkout removal. Exit also runs when the checkout is kept. |
| Delete | Exit runs before deleting saved session metadata or provider history. Deleting history still keeps checkout files and branches. |
| Delete after archive | A successfully completed exit is not repeated. |
| Unarchive | Restore/adopt the owned checkout, then run enter again, including for a retained checkout. The harness stays suspended until resumed. |
| Selected existing worktree or directory mode | No hooks are captured or run. |
| Suspend/resume or browser navigation | No new enter/exit transition. A deliberate resume after failed initial enter retries that incomplete setup before starting the harness. |

Cascading project archive/unarchive/delete follows the same hook transitions for affected sessions. Project archive still keeps checkout files; ungrouping sessions does not run hooks.

Each owned session stores its hook snapshot and enter/exit state. Editing the repository file or global defaults affects future sessions; it does not replace an existing session's exit command. A restored session keeps the original definitions. Script paths still execute the script contents present in that worktree.

Commands run sequentially, with literal arguments and the worktree root as cwd, on the host that owns the files. Use an explicit shell or repository script for compound commands. They inherit that host process's environment, with inherited `GIT_*` overrides removed. Invoke `mise exec --` explicitly if a command needs project-specific tool activation. Commands receive:

- `AGENTWORKS_WORKTREE`: owned checkout path.
- `AGENTWORKS_REPOSITORY`: main checkout path.
- `AGENTWORKS_BRANCH`: recorded branch.
- `AGENTWORKS_HOOK_EVENT`: `enter` or `exit`.
- `AGENTWORKS_HOOK_REASON`: `create`, `archive`, `delete`, `unarchive`, or `retry`.

A Bazel shutdown or clear script should target that worktree's output base. Sharing or deleting a global cache should be an explicit project choice. Compile-command generation remains a repository script, so Agentworks does not need build-system-specific behavior.

## Failures and recovery

Enter failure leaves the session in error with its branch and files retained. Exit failure blocks archive/delete and retains session metadata and the checkout. An active turn, locked/changed checkout, or another session using it blocks exit; stop the turn or resolve the sharing before retrying. Hooks cannot bypass cleanup ownership, dirty-change confirmation, or branch retention.

**Workspace info** shows the population method, hook states, and bounded output, with **Retry enter hooks** / **Retry exit hooks** for failed or interrupted attempts. After successful enter retry, resume the session. After successful exit retry, retry archive/delete. Ordinary retries skip already successful phases.

The running state is persisted before executing commands. A crash or lost remote completion leaves an uncertain outcome, which requires inspection and deliberate retry rather than automatic replay. Commands should be idempotent; exactly-once execution across a crash is not guaranteed. Unarchive retries preserve an incomplete phase rather than resetting it repeatedly.

Each event allows at most eight commands and 300 seconds combined timeout. Individual timeouts range from 1 to 300 seconds (default 60). Each command has a 32 KiB limit on each output stream. Linux/Unix commands run in their own process group, killed on timeout, excess output, or cancellation. Output uses the host's existing diagnostic sanitization and is retained in session events and Workspace info.

If exit changes tracked/untracked contents, cleanup inspects again and retains the checkout or requires fresh dirty-removal confirmation. A hook cannot silently authorize deletion of newly generated changes.

## CoW implementation and limits

Linux uses the `FICLONE` ioctl for file data, with independent inodes. A clean detached template for each requested commit lives under the common Git directory's `agentworks-cow/<commit>` path. The first template requires an ordinary checkout; later worktrees share its file blocks. Template worktrees are hidden from Agentworks' worktree picker but visible in native `git worktree list`. A filesystem lock serializes template creation/population across processes. A partial or modified template is retained and rejected rather than force-reset.

Git still creates the linked worktree, branch, and private index. Template index stat information is refreshed for the new inodes. Dirty source edits, staged changes, and untracked files are not imported as tracked checkout contents. Only explicitly allowed ignored build directories are additionally cloned from the source checkout. Symlinks are preserved rather than followed when copying contents; absolute links and build metadata may still need project-specific relocation handling.

CoW support is probed between the actual cache and destination locations. Prefer falls back before destination Git mutation when cloning is unsupported, the locations cross filesystems, or the repository uses submodules, sparse checkout, split indexes, or worktree-specific configuration. The current backend is Linux-only; other platforms use fallback or fail Require. Permissions, capacity errors, timeouts, and failures after Git mutation are reported with retained state rather than retried as ordinary creation. Git subprocesses retain the existing environment/hook controls, bounded output, and ten-second per-command timeout; overall local preparation allows 120 seconds.

Directory/index metadata and subsequent writes still consume space. Explicitly shared caches can contain absolute paths, symlinks, or files being written by build tools; cloning does not establish cache portability or build consistency. Templates currently have no automatic eviction: retained commits and their blocks remain until deliberate maintenance. Do not remove or prune a template during an active creation operation. There is no overlay mount, daemon, external utility installation, or automatic branch deletion.

Both local and remote sessions use the shared workspace library. Remote workers advertise `gitSettings`; older workers must be updated before creating worktrees with this feature. Hooks execute on the worker and wait for stopped worker processes before accessing the checkout. A lost response is not treated as proof of command completion.

## Investigation and validation

The initial assessment compared [git-cow](https://github.com/gabrielkoerich/git-cow), which uses templates and can warm build output, and [simgit](https://github.com/abendrothj/simgit), which supports reflink/overlay baselines. Installed Git 2.55.0 had no native worktree reflink option. A disposable git-cow library test established clean checkout and cache-copy feasibility, but its synchronous subprocesses and automatic error cleanup did not match Agentworks' execution/ownership contracts. The implementation uses an Agentworks-owned native reflink backend while retaining Git for repository operations.

A Btrfs probe confirmed physically shared extents with separate inodes and independent writes. Automated checks cover CoW creation/restoration with a dirty source, cache copying, config validation and stale writes, hook context/snapshots/timeouts/output bounds, local and fake-SSH lifecycle/persistence, and desktop/mobile Git settings.

```bash
mise exec -- cargo test --locked -p agentworks-workspace
mise exec -- cargo build --locked
npm run test:git-settings
npm run test:worktree-cleanup
```
