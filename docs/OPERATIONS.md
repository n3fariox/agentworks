# Hosts, configuration and persistence

[Documentation index](README.md) · [Quick start](../README.md)

Run commands from the repository root.

## Hosts and projects

Use the sidebar's **Hosts / Projects** toggle to browse the same sessions in either order:

- Hosts: Host → Project → Session
- Projects: Project → Host → Session

Sessions run on the Local host. Other machines can be added as remote hosts (below). They appear in the list with their connection status and can run chat sessions. Projects are logical groups, independent of host paths: several directories/worktrees can share a project, and the same model supports grouping across remote hosts.

Use **＋** beside Sessions to create a project, or create one in the new-session dialog. The project's **⋯** button opens rename/delete controls. A session's project selector moves it between groups, including **Ungrouped**, while its agent, working directory, and conversation keep running. Deleting a group moves its sessions to Ungrouped.

Sidebar view and collapse state are remembered per browser. Switching views preserves the selected session, stream connection, draft, and scroll position. Drafts are remembered per session in the browser tab. Running/waiting counts appear on the parent groups.

Projects, settings, and host-local location IDs are saved in `.agentworks/catalog.json`; session metadata and transcripts are saved in `.agentworks/catalog.sessions.sqlite3`. `AGENTWORKS_CATALOG_PATH` can point to another catalog file. This catalog provides organization; file access still follows the server's allowed roots.

### Remote hosts

See [the implementation and protocol guide](REMOTE-HOSTS.md) for Mermaid diagrams, wire messages, ownership and recovery, timers, and compatibility rules.

In the Hosts view, **＋** opens host setup. Enter a name and an SSH destination (`user@host`, an SSH config alias, or `ssh://user@host:port`). **Install worker** is the default:

1. A live xterm.js terminal shows SSH's host fingerprint prompt and accepts a password, key passphrase, or keyboard-interactive authentication. Existing SSH keys/agents/configuration are tried by SSH. Passwords and terminal input/output are not persisted.
2. Agentworks generates a dedicated Ed25519 key for this host, appends its public key to `~/.ssh/authorized_keys` with `restrict`, and verifies another connection. The private key stays on this server, in a `0700` directory under `host-keys/` beside the catalog; the key file is `0600`. It is unencrypted so a restarted service can reconnect without an SSH agent. Host keys are pinned in that directory's own `known_hosts`; reconnects reject changed keys.
3. The server copies its running binary and a small launch script to a unique directory under `~/.local/share/agentworks/releases/`. The SHA-256 checksum is verified before extraction. The host needs `tar` and `sha256sum` or `shasum`; it needs no Node, npm or mise, and nothing is downloaded. Setup requires no sudo. Automatic copying currently supports a matching server/remote OS and CPU; incompatible binary loaders/libc fail during installation. The binary includes the demo agent and the Rust Claude and Codex adapters; the JavaScript **Claude** and **Codex** adapters are not installed on enrolled hosts. If mise is installed, the launcher runs the worker through it so CLIs that mise manages are found.
4. The server waits for the worker handshake, shows the available providers, and offers **Choose working directories**. Provider account sign-ins still belong to the remote user; this flow does not copy provider credentials.

Reconnects use the release's explicit launch path, so shell startup files and PATH changes are unnecessary. A convenience `~/.local/bin/agentworks` symlink is created if that path is unused; add `~/.local/bin` to your interactive PATH if desired. Closing setup cancels its local SSH/subprocesses. Failed/cancelled attempts remove temporary local keys, but can leave a remote installation or a public authorized-key entry labelled `agentworks:<attempt UUID>`; those can be removed manually. An installed host whose worker fails to connect remains saved with an error and can be reconnected.

**Use an existing worker** keeps the manual flow: provide the destination and optionally the command to run Agentworks there. The server runs:

```bash
ssh -T -o BatchMode=yes … <destination> '<command> agent --stdio'
```

The remote `agentworks agent` prints a ready line and then talks to the server over WebSocket on ssh's own stdin and stdout. Anything the remote shell prints before the ready line, such as a login banner, is skipped. All session state stays on this server. The worker only runs processes on its own machine. If the link drops, the worker stops the process groups it started and exits, and the server reconnects with backoff, the same as a server restart. A connected host reports its hostname, platform, version, and the providers that can actually start there. The worker also stops its process groups after 45 seconds without server frames, so a half-open SSH connection does not keep them running indefinitely. Protocol 3 requires the worker to hold an exclusive per-host lock before reporting ready; replacements wait until the old worker has finished cleanup. Zero-byte lock files live under `~/.cache/agentworks/workers/` and stay in place after exit; they store no session state.

**Chat sessions on a remote host.** Use **⋯** on the host to set its working directories, using the host's own paths (`~/` is the remote user's home). Then start a session with **＋** on the host, or choose the host in the new-session dialog. The worker starts the agent's adapter in that directory on the host and relays its ACP traffic. The server keeps the conversation, approvals, and history as for local sessions, and the agent uses the host's own sign-ins. If the link drops, the host's sessions are suspended with a notice and the interrupted turn is not retried. Resume them once the host reconnects.

Remote sessions support chat, model discovery in the new-session dialog, and creating/selecting Git worktrees on the host. Host settings show discovered CLI paths and where to sign in. Remote worktrees honor project isolation policies and host roots. Archiving removes clean owned worktrees, asks before deleting dirty changes, and retains branches; unarchiving restores the checkout from its retained branch. Locked, shared, running or replaced worktrees are retained. Deleting a session keeps its worktree, as on Local. Update and reconnect older workers to enable worktrees and cleanup.

Remote sessions also support queued inputs, browsing/importing CLI conversations, forks, manual automations, authorized quota continuation, and authorized inbox turns. Native forks stay on the provider history’s host; cross-host forks carry transcript/summary context into a fresh native session. Dispatch rechecks the owning host’s roots and provider availability. Link loss pauses queues and revokes inbox delivery; uncertain turns are never replayed automatically. Quota continuation uses structured ACP reset evidence or a human-selected reset time.

File browsing/previews/downloads, Git context/diffs, Agentworks MCP tools, remote Codex account probes, and explicit remote API-key mode remain unavailable. See Phase 6 in the roadmap for the plan.

Manual setup requirements:

- SSH from this server's user to the destination works non-interactively, using key or agent authentication and a host key already in `known_hosts`. Ports, users, and jump hosts come from `~/.ssh/config`. The manual flow uses the server user’s SSH configuration and credentials.
- The server process can use your key. A passphrase-protected key needs an SSH agent reachable through `SSH_AUTH_SOCK` in the server's environment. Services, including the Agentworks supervisor, often start without one. Either start the server with `SSH_AUTH_SOCK` pointing at a long-lived agent, such as systemd's `ssh-agent.service` socket at `$XDG_RUNTIME_DIR/ssh-agent.socket` with your key added, or use a dedicated key without a passphrase that is authorized only on the hosts you add. If a host reports `Permission denied` while `SSH_AUTH_SOCK` is unset, the status message says so.
- Agentworks is built on the remote host from the same version, with `npm ci --ignore-scripts` run there. The remote command defaults to `agentworks` on the remote `PATH`; set it to a full path such as `~/agentworks/target/release/agentworks` otherwise. Provider detection uses the adapter paths of the checkout the binary was built from.
- Tools on the host, such as the `opencode`, `codex`, and `claude` CLIs (and node, for the JavaScript adapters), come from its PATH. A non-interactive ssh command does not run shell startup files, so tools installed with mise would be missing. The worker therefore applies `mise env` for each session's directory itself, including that project's own mise configuration, falling back to the global configuration if the project's cannot be used (for example, if it is not trusted).

No ports or socket files are opened on either machine. SSH provides authentication and encryption, so there is no separate enrollment secret. Hover a host's status dot for details. When a host is offline, the reason, including ssh's last output, also appears under it. **↻** reconnects immediately, and **×** removes a host with no sessions, stops its worker, and deletes its managed private key from this server. Remote files and its public authorized-key entry remain; remove them manually if desired. `AGENTWORKS_SSH` selects a different ssh program.

## Multiple project directories

By default, sessions can select the current directory or its subdirectories. Use **Settings** to edit allowed roots, or supply initial defaults when starting the server:

```bash
AGENTWORKS_ROOTS="$HOME/projects/agentworks:$HOME/projects/morsh" mise exec -- cargo run --locked
```

Enter the full project path in the new-session dialog. Paths are canonicalized and checked against the allowed roots. File preview/download routes are restricted to that session's project directory, with a 10 MB limit. Markdown and raster images can be previewed; PDFs and other binary files can be downloaded. This file gateway restriction does not sandbox agent commands; provider permissions still govern agent execution.

Use `AGENTWORKS_PORT=4318` to choose a different port. The default bind address is loopback. For direct access over your private network/VPN:

```bash
AGENTWORKS_BIND=0.0.0.0 ./target/debug/agentworks
```

Open `http://<host-address>:4317` remotely. Network binding accepts the hostnames/IPs used to reach this listener and still requires matching browser origins for commands and WebSockets. The prototype relies on your private network for access; application login remains later work.

For HTTPS at a public subpath, configure the proxy to normalize the trailing slash,
strip the public prefix, preserve WebSocket upgrades, and overwrite forwarded
host/protocol headers. Keep the backend private and retain hosting authentication.
PWA support is enabled by default: install manifest, icons, standalone launch
and a network-only service worker. Set `AGENTWORKS_PWA=0` on the backend process
to opt out. Installation provides neither
offline agent execution nor background/mobile push. See [hosting setup and the
Traefik example](HOSTING.md).

## Runtime limits

- At most 12 active harnesses; saved inactive sessions do not count toward that limit.
- The browser loads the most recent 3,000 events / approximately 4 MB. The full event log stays in SQLite, without automatic transcript expiration.
- One ACP adapter process per session. Cancellation clears pending approvals; stopping the server ends its harness processes and terminals.
- File previews follow live paths rather than keeping artifact snapshots. Concurrent filesystem replacement is outside the trusted-project boundary.
- Image output, chat attachments, and supported question forms are available. Application login and native Windows delivery remain future work.

## Persistent sessions

Session options show the adapter’s advertised settings, including reasoning effort, collaboration/plan mode, and fast mode where available. Settings can be changed while the session is idle. Select choices (including grouped choices) and boolean toggles are supported; unknown setting types are retained without an editable control. Model and permission selectors stay synchronized with provider changes. ACP v1 adapters that expose only session modes use `session/set_mode`. Additional settings are saved and reapplied on reconnect when still advertised.

Session records, model configuration, project assignment, request IDs, provider session IDs, and transcript events are committed to SQLite as they arrive. The database uses WAL and atomic transactions. A process lock prevents two servers from opening the same session database. The default database is beside the catalog, named `catalog.sessions.sqlite3`; override it with `AGENTWORKS_DB_PATH`. Keep both the catalog and session database when updating the binary. While the app is running, SQLite’s `-wal`/`-shm` companion files belong to the database; stop the app before making a plain file-copy backup.

After restart, saved sessions appear in the sidebar without launching every harness. Opening one reconnects its original provider session through ACP resume (or load, with replay suppressed to avoid duplicate transcript entries). The browser keeps retrying while the server is down and reconnects after an update, retaining the current draft. A **Resume session** button lets you retry if the harness was temporarily unavailable. Never-used sessions can be recreated if a harness has not saved them yet. If a populated harness session no longer has its own saved context, Agentworks preserves the transcript and reports an error rather than silently starting a fresh conversation. Keep the host’s Claude/Codex data and `.agentworks/opencode`/`.agentworks/demo` directories as well as the app database.

Interrupted turns are marked in the transcript and are never automatically retried; old approval buttons are cleared. Check any last tool operation before sending another turn. **Delete history…** permanently deletes the Agentworks record/transcript and stops its harness process; provider conversation files remain. **Delete everywhere…** appears when the adapter advertises `session/delete`. It explicitly confirms deletion from both Agentworks and the provider, stops the live adapter, then deletes the original native session on its original host/profile without resuming or prompting it. Provider deletion must be confirmed before the app transcript is removed. Failed or uncertain deletion retains the transcript and fences resume across restarts; inspect the provider history and retry deletion, or delete only the app transcript. Files and Git branches remain.

**Suspend harness**, deliberate service restart, and normal SIGINT/SIGTERM shutdown use `session/close` when advertised. Closing preserves native history for `session/resume`. Unsupported, rejected, disconnected, or timed-out close falls back to terminating and joining the adapter, with no automatic turn retry. Graceful close is bounded to five seconds per adapter; service-wide shutdown closes adapters concurrently before disconnecting remote hosts. Restarting suspends sessions without deleting them.

The bundled adapters currently negotiate ACP v1. ACP v2’s prompt-acceptance and `state_update` completion model requires a separate protocol migration; a successful v1 prompt response remains the turn-completion boundary here.

The first migration can import the old server’s HTTP snapshots from `legacy-sessions.json` beside the database. It imports each session once and renames the file to `legacy-sessions.imported.json`. This is used to preserve the live conversation during this upgrade.

## Is it safe to close or restart?

`GET /api/activity` reports backend work, independently of browser connections:

```json
{"safeToClose":true,"activeTurns":0,"pendingApprovals":0,"busySessions":[],"sessions":[]}
```

Each session includes `turnRunning`, `pendingApprovals`, and `canClose`. Accepted prompts count as active until their response finishes or cancellation completes. Pending approvals and startup/model-configuration work block `safeToClose`. Idle sessions may be closed/restarted once work has finished; their saved history and harness session IDs remain available after restart.

This endpoint is a snapshot, not a shutdown lock: another browser can submit work after the check. Check it immediately before stopping the server. It does not itself close sessions or stop the listener.

Settings → **Service · all projects** offers a bounded wait-until-idle restart, deliberate interruption and cancellation. Run with the external supervisor/user-service setup in [service restart and recovery](SERVICE-RESTART.md); direct unsupervised servers refuse restart. The separate default-off `restartService` grant exposes read-only status, restart and own-request cancellation over MCP. A requesting coordinator can explicitly choose one bounded continuation in its saved native context after a successful supervised restart. Prepared release validation and verified private backups precede shutdown; uncertain dispatch or failed starts pause for manual inspection, with no tool replay or automatic database rollback.

For explicit backup pruning and restore/upgrade rehearsal into a fresh directory, see [retention and recovery](RETENTION-RECOVERY.md). The offline CLI never replaces live data.
