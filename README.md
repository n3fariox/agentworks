# Agentworks

A small, local Rust prototype for running independent Claude, Codex, OpenCode, and demo chat sessions through **Agent Client Protocol (ACP)**. The interface is dark and responsive, with persistent sessions, session switching, streamed Markdown, image output, local file previews/downloads, tool activity, explicit approvals, and cancellation.

## Run

Requires Rust 1.90+, [mise](https://mise.jdx.dev/) and Node (use a current LTS supported by the adapters). The repository pins Zig 0.15.2 in `mise.toml`; the build and start commands install/select it through mise so Zig does not have to be added to your shell `PATH` manually. The first Rust build fetches Ghostty’s pinned native sources and Zig dependencies.

```bash
mise trust mise.toml # once per checkout, if mise asks
npm ci --ignore-scripts
npm run build
npm start
```

For direct Cargo commands, use mise to select the pinned Zig version:

```bash
mise install
mise exec -- cargo run --locked
```

Or pass Cargo build options through the project wrapper, for example `npm run build -- --release`.

Open **http://127.0.0.1:4317** and create a **Demo agent** session. Try `hello`, `approval`, `slow`, or `image`. The demo is a real local ACP subprocess, not a simulated browser response, and never calls a model or edits project files. Its own session state is saved locally for restart tests.

The prototype uses plain HTML/CSS/JavaScript so there is no frontend build step. UI assets are embedded in the Rust executable; rebuild/restart after changing them. A React frontend can replace this shell after validating the session flow.

## Real agents

By default, Claude and Codex reuse the **existing agent sign-in on the host**. Run Agentworks as the same OS user that runs the CLIs. It inherits their normal credential locations (including any existing `CODEX_HOME` or `CLAUDE_CONFIG_DIR` override), without copying tokens or starting a new login flow.

If a sign-in has expired, refresh it through `codex login` or Claude's `/login` command on the host, then try a new session. The browser never asks for account credentials. Default mode clears API-key overrides in the child environment so an unrelated key does not change the intended account/billing path.

API keys are optional. To explicitly use them instead, provide `ANTHROPIC_API_KEY` for Claude or `CODEX_API_KEY` / `OPENAI_API_KEY` for Codex and start with:

```bash
AGENTWORKS_AUTH_MODE=api_key mise exec -- cargo run --locked
```

Only this explicit API-key mode isolates provider configuration/authentication under `.agentworks/claude` and `.agentworks/codex`, keeping it separate from the host CLI login store.

The app uses pinned local ACP adapters installed by `npm ci`, without downloading an adapter at session creation. Claude wraps its SDK; Codex wraps app-server. Project-local agent instructions may be loaded by the harness. Run `npm ci --ignore-scripts` on each execution machine; the Rust binary alone does not contain Node/adapters/provider dependencies.

Codex uses the tracked local `2.2.2-agentworks.1` fork in `vendor/codex-acp` to attach cached subscription windows to genuine context usage events. The usage display preserves each window's source observation time separately from context. No extra account queries or polling are performed; quotas appear after native push notifications and a genuine usage event. See the [implemented metadata contract and limitations](work/CODEX-SUBSCRIPTION-QUOTA-PROPOSAL.md).

To check the ACP handshake without making an inference request:

```bash
mise exec -- cargo run --locked -- --probe demo
mise exec -- cargo run --locked -- --probe claude
mise exec -- cargo run --locked -- --probe codex
```

Probes report negotiated capabilities/authentication methods only. A successful probe does not prove inference access or complete provider behavior. Actual prompts consume provider usage.

## Models, commands, skills, and settings

In **New session**, choose a directory and harness, then click **Load models**. This starts a temporary ACP session to discover the harness’s advertised models without sending a prompt. Select a model before creating the session; it is applied before the first turn. The session toolbar also allows changes while idle. OpenCode stays restricted to its advertised free models.

The sidebar’s **Settings** button saves default model IDs per harness and allowed root directories on this host. Each root has its own input row; use **Add root** or its remove button to edit the list. Each harness has a **Load models** button to populate model-ID suggestions using the first currently allowed root. Save root changes before discovering models in a newly added directory. Blank defaults use the harness configuration. A session can explicitly select **Harness default** to override a saved default. Missing/stale model choices fail visibly instead of silently choosing another model.

New Claude sessions default to the adapter's normal **Auto** permission mode. Settings → **Claude default permission mode**, beside the Claude model default, can save Auto, Default, Accept edits, or Plan (`claudeDefaultPermissionMode` in the catalog; an absent value means Auto). The choice is captured for new Claude sessions only, then requested after the adapter advertises it and confirmed before the initial prompt. If unavailable, the adapter's current mode is retained with an explanation in Session options. Rejected or unconfirmed selections stop initialization before any prompt. Existing sessions, native reconnects, Codex/OpenCode defaults, and controller grants retain their existing behavior.

Settings live in `.agentworks/catalog.json` and survive restart. Before roots have been saved, `AGENTWORKS_ROOTS` (or the startup directory) supplies the defaults; saved roots take precedence thereafter. Changes apply to new sessions and model discovery; existing sessions retain their cwd/model and file gateway. Roots control which directories can be selected, not the tool sandbox or agent permissions.

Settings → **Notifications** offers an immediate-save opt-in for turn completion, approvals/questions and failures across sessions. Click **Allow notifications in this browser** to request permission, then **Send a test notification** to check display. Permission is never requested automatically. Trusted HTTPS or HTTP localhost/loopback is required; ordinary HTTP LAN/VPN addresses cannot use this feature. Root-hosted TLS proxies can derive the public origin from overwritten `X-Forwarded-Proto` and `X-Forwarded-Host` headers; keep the backend private. See [setup, permission flow and limitations](work/NOTIFICATIONS.md). Delivery requires an open, connected tab; mobile/background push is not implemented.

Use **/ Commands & skills**, or type `/` or `$`, to browse the session’s ACP-advertised commands. Picking one inserts it into the composer; sending executes it through the harness. Claude skill/command names use `/name`; Codex advertises skills as `$name`. The harness discovers skills using its own host/project configuration and session directory. This list is retained across browser reconnects. Only advertised ACP commands are offered; the full terminal CLI command set is not guaranteed in chat mode.

Type `@` anywhere in the composer (at the start or after a space) to fuzzy-search other non-archived sessions in the same project; ↑/↓ moves, Enter or Tab inserts `@session-title`, Escape dismisses. When you send, each recognized mention becomes a link such as `[@session-title](agentworks://sessions/<id>)`. Agentworks MCP tools that take a session (`send_message`, `get_completion`, `prompt_session`, `cancel_session`) accept that URI or a bare ID, and `list_sessions` returns both, so agents can pass the URI through unchanged. Messages work the same way: `read_inbox` and `send_message` return `agentworks://messages/<n>` (plus sender/recipient session URIs), and `acknowledge_message` / `replyTo` accept either that URI or the number. The agent still needs Agentworks communication access to actually message the peer.

Use the 📎 button, drag and drop, or paste to attach files to a message. Supported: PNG, JPEG, GIF and WebP images up to 5 MB (Claude, Codex and Demo) and UTF-8 text files up to 256 KB (Claude, Codex, OpenCode and Demo), at most 4 per message. Types are detected from content, and each harness must advertise the matching ACP prompt capability. OpenCode images are refused because its free models are not verified to read them. PDFs and other binary files are refused for every harness: Claude's adapter drops them and Codex's would inline them as base64 text. Uploads are stored per session beside the session database (not in the workspace) and are deleted with the session; unsent uploads expire after 24 hours. Content reaches the harness only as embedded ACP blocks with an `agentworks://attachments/<id>/<name>` URI, so no host path is sent. Each upload is sent with exactly one message: retries reuse its request ID, and nothing is resent after Stop, queueing, quota continuation or restart. Attachments cannot be queued while a turn runs, and MCP `prompt_session` remains text-only.

The **Usage** button runs Claude’s `/usage` or Codex’s `/status` in the current session and displays its output in the transcript without replacing a draft. Verified with existing personal sign-ins: both returned 5-hour and weekly quota percentages and reset times. Claude also returned request/session totals. These are account-level subscription limits shared with other clients, not exact per-session message counts; availability depends on the harness/account. No inference prompt is needed for these two installed adapter commands. There is no background usage polling/dashboard yet.

An opt-in check initializes both signed-in harnesses, selects advertised models, and runs their local usage commands:

```bash
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:capabilities
```

## OpenCode free-model testing

OpenCode has native ACP support, so no npm adapter is required. Install `opencode` on the host (the tested version here is `2.0.26`), or set `AGENTWORKS_OPENCODE_PATH` to its executable. Select **OpenCode · free models** when creating a session.

Agentworks reads the model options advertised by OpenCode and offers the `opencode/*-free` entries plus Big Pickle in the session's model picker. It explicitly selects one before accepting a prompt and restricts picker changes to that advertised free list. Available models, free status, and limits can change; this is not a permanent pricing guarantee. See [native ACP support](https://docs.opencode.ai/docs/acp/) and [current OpenCode free models](https://opencode.ai/v2/docs/console/models/#free-models).

OpenCode runs with isolated XDG data/config/cache/state under `.agentworks/opencode/`. The app does not import your existing OpenCode login/configuration. No API key was required for the tested Big Pickle prompt. The initial integration exposes free models for testing; paid providers and account management are outside this slice.

An opt-in real model check sends one short prompt in an empty test directory:

```bash
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:opencode
```

To include the native session/model-picker browser check without inference:

```bash
AGENTWORKS_TEST_OPENCODE=1 npm run test:browser
```

## Workspace navigation and files

Each session has **Chat** and **Terminal** tabs. Opening Terminal starts one interactive `$SHELL` (falling back to `/bin/sh`) in that session’s directory, on its local or remote host. It retains the host user’s paths, credentials, locale, desktop/SSH integration, and `NO_BROWSER` setting. Launcher color/CI overrides, agent session markers, forced pagers, service controls, stale terminal metadata, and inherited working-directory/Git overrides are cleared; the interactive shell then loads its own configuration. The service keeps the PTY and libghostty emulator running when you switch tabs or sessions, reload, disconnect, or suspend the agent harness. Reopening attaches to the same shell. Archiving stops the shell and releases its PTY, saving its screen and scrollback for later viewing. Unarchive the session and select **Start new terminal** to start again. Deleting the session stops its terminal and deletes its saved history. After the shell exits, **Start new terminal** creates its replacement. Service restart ends terminals; a remote worker disconnect also ends terminals on that worker. Existing remote workers must be updated to advertise terminal support.

For a terminal outside any session, use **Open terminal** (`›_`) beside a host in the sidebar. Each host gets one shell, started lazily in that host user’s home directory, shared across browser views, and retained when navigating away or reloading. **Stop terminal** releases the shell, its child processes, PTY, and emulator while leaving its screen and scrollback available until service restart. **Start new terminal** replaces a stopped or exited shell. Host terminals are independent of session archive/deletion and need no session or configured working directories. Removing a host, disconnecting its worker, or restarting the service ends its live terminal. Remote host terminals require an updated worker that reports its home directory.

The browser uses the locally vendored xterm.js 6. Ghostty handles output, query replies, and screen state on the shell’s host; reconnect restores up to 10,000 rows of scrollback and the current screen, followed by ordered display updates. Scroll with the mouse wheel, scrollbar, or Shift+Page Up/Down. Reading older output keeps your position when new output arrives. Full-screen applications use a separate alternate screen and do not fill the shell’s scrollback. Live history is bounded in memory; service restart or starting a replacement shell clears it. Archived terminal snapshots are stored in the database and remain viewable after service restart. Multiple browser views share the same shell, with the latest visible view’s resize taking effect.

Use **Sessions**, **Files**, and **Automations** in the navigation rail (tabs across the top on mobile). The header selects the logical project and its Local host directory. **＋** beside the directory adds another location inside the allowed roots; it does not start an agent. Project/location associations persist even after their sessions are deleted.

**Files** browses one directory at a time with breadcrumbs, a parent link, refresh, and a hidden-file toggle. Markdown, raster images, and UTF-8 source/text files have previews; PDFs and other binary formats are downloads. Source/Markdown previews are shortened for large files, and preview/download requests are limited to 10 MB. Canonical paths and symlink targets must remain inside the selected directory; each request also checks the current allowed roots. A directory lists up to 5,000 scanned entries.

**Open beside chat** keeps a file preview next to a session in the same directory. On mobile, the preview occupies the conversation area with a **Back to chat** button. **Ask about this file** adds a local-file reference to a draft without submitting it or uploading its contents. If no session in that directory is selected, it opens New session with the directory filled in and keeps the reference for that session’s draft. The agent can read the file through its own tools when you send the message. Existing transcript links still offer the file-preview dialog, with **Open in Files** for browsing.

Navigation preserves running sessions and drafts. The chosen project, directory, last file, and workspace view are remembered per browser. Settings are available from the navigation rail.

## Command and session palettes

**Ctrl/Cmd+K** opens the command palette and **Ctrl/Cmd+P** opens the session palette; the header ⌕ button and the sidebar **⌕ Commands & sessions** entry open them by touch or mouse. Neither shortcut is taken while another modal is open, and **Esc** always closes the palette. The **Actions & commands** and **Sessions** buttons switch lists; **Tab** walks the palette's own controls as in any dialog.

Entries are ranked by fuzzy matching, where every typed word must match something. A session is matched on its title first, then project, host and status, then its directory: the end of the path counts more than the leading directories shared by many sessions, which still match at a lower weight. Session rows show the status plus the current marker, agent, project, host and directory, and show more of the path when titles repeat so two sessions stay distinguishable. Running and waiting sessions are listed first until you type. Matching is scored over the whole alignment, so a word that only partly matches never displaces a complete one.

**↑ ↓** and **Page Up/Down** move the highlighted option and **Enter** runs it. Left/right arrows, **Home**, **End** and **Tab** keep their normal meaning in the search box.

Actions call existing controls only (new/resumed session, new project, files, automations, settings, agents and inbox, workspace info, resume, stop, usage, delete). Entries that cannot run right now stay readable, explain why, and refuse to run. Advertised harness commands are typed into the composer for review; choosing one never submits a turn. Switching sessions uses the same path as the sidebar, so each draft and every running turn is preserved.

## Manual automations

In **Automations**, create a named definition with a harness, optional model, project, host, working directory, and instructions. The initial trigger is **Manual**; GitHub polling, webhooks, and schedules are future work. A blank model uses the host’s configured default. Paused definitions cannot start new runs.

**Run now** creates an ordinary persistent chat session and submits the saved instructions once the harness is ready. Tool approvals, output, cancellation, file links, and continuing the conversation use the existing session interface. An automation badge links the session back to its definition and recent runs. Repeated requests with the same request ID return the existing run; only one run per automation can be active at a time. The browser retains an unacknowledged run request ID so retrying a dropped response does not silently start another run.

Definitions and run records are saved in SQLite alongside sessions. Completed, failed, cancelled, and interrupted outcomes describe the initial automation turn, independently of later manual conversation turns. Updates/crashes do not automatically repeat an interrupted or queued initial prompt. Deleting a definition keeps its sessions and run records. Deleting a session removes its transcript and marks its run as deleted while keeping the run record. Removing a project moves its definitions/sessions to Ungrouped.

## Session workspace context

Open **Workspace info** in a session to see its current Git branch, commit, repository/worktree paths, and whether it is a linked worktree. Detached HEAD, new branches without commits, non-Git directories and unavailable paths have explicit states. The dialog refreshes on open and preserves the chat stream and draft. See [workspace details and checks](WORKTREES.md).

**New session** can use a directory, create a new Git worktree, or discover an existing linked worktree. Project settings choose the default and can require isolated worktrees for new sessions, including MCP starts and automation runs. New worktrees use a committed revision; uncommitted source changes stay in the source directory. Existing sessions resume in their original directory. Deleting a session keeps worktree files and branches. Archiving a session removes a clean worktree that the session created (its branch is kept and restore recreates it), asks before deleting uncommitted changes, and refuses unsafe cleanup; nothing is ever merged. Workspace info shows ownership and dirty state. See [ownership and cleanup](work/WORKTREE-CLEANUP.md). Launch request IDs survive retries and tab reloads; interrupted preparation retains files and requires inspection before a deliberate fresh attempt. See [policy/API details](WORKTREE-POLICY.md) and [browser controls](WORKTREE-UI.md).

## Agent communication and MCP control

Each normal chat session is connected to the native **Agentworks MCP** server (`agentworks mcp`). Claude, Codex, and OpenCode have each sent a real message through this connection. Temporary model-discovery/history-listing processes receive no Agentworks control connection. The MCP subprocess uses the official Rust SDK and a credential-protected loopback bridge to the running service; credentials rotate when the harness restarts and are not included in browser snapshots.

Choose **Agentworks access** in New session, or open **Agents & inbox** on an existing session. New sessions use the project's default role, falling back to the global default (Participant unless configured otherwise):

- **Off** (default): no granted tools.
- **Participant**: discover participating peers in the same logical project, send messages, read the inbox, and acknowledge receipt.
- **Coordinator**: Participant plus creating project sessions, starting turns in idle project sessions, cancelling turns, and running existing project automations.
- **Custom**: individually enable those capabilities. Coordinator does not include editing/deleting records, changing settings, granting access, or approving harness permission requests.

Only the human control interface changes grants. Coordinator-created sessions use the configured project/global default role; they do not copy grants from the creating coordinator. Automation runs remain Off. Put sessions in the same logical project to communicate, even when their working directories differ; Ungrouped does not provide a shared communication scope. Moving a session changes its permitted peers immediately. Historical messages remain in its own inbox.

Messages, replies, acknowledgements, grants, and control activity persist in SQLite. Sender/recipient IDs and names remain on messages after a session is deleted. Sending does not interrupt a turn, start inference, or wake an idle/suspended agent. Agents read their inbox through tools at useful checkpoints. The browser shows sent and received messages, supports acknowledgement, and can add a received message to your existing draft with **Use as next turn**; you review and send it explicitly. Acknowledgement records receipt, not completion of requested work. The current slice supports text and reply references; file attachments and automatic wake-up are later work.

External MCP clients, such as a coordinating conversation, use their own named, project-scoped and revocable **external controller** credentials from Project settings; they never borrow a session's identity, and their operations are attributed by name. See [external controllers](work/CONTROLLERS.md).

Grant checks run on every MCP operation, including retries and calls to hidden tools. Revocation takes effect immediately for subsequent calls. Tool catalogs reconnect immediately when idle or after the current turn finishes. MCP tools cannot promote their caller or approve harness tools. Major actions retain normal model restrictions, root checks, active-turn checks, concurrency limits, and harness approvals. Coordinator-started sessions link back to their initiator; agent-submitted turns are labelled in the transcript. Recent control activity appears in Agents & inbox.

`send_message`, `start_session`, `prompt_session`, and `run_automation` require stable request IDs for safe retries. Message IDs are scoped to the sender; session creation IDs to the coordinator; prompt/run IDs include the coordinator identity. Message request IDs allow 100 bytes, coordinator operation IDs 50 bytes. Reusing a message/session-creation ID with different content is rejected. Deleted coordinator-started sessions are not silently recreated on retry. Mailbox reads use an increasing cursor and return up to 20 messages; text is limited to 16 KB, with up to 1,000 unacknowledged messages per recipient and 60 new messages per sender per minute.

These are application permissions for trusted local agents, **not an OS sandbox**. Agents currently share the service's OS user and retain their shell access; the existing browser API relies on the private VPN and same-origin browser checks. MCP grants do not prevent a same-user process from accessing app files or another control path. Strong separation would require isolated agent processes and protected human authentication across control paths.

Deterministic MCP checks use isolated data and no model calls:

```bash
npm run test:mcp-wire
npm run test:communication
npm run test:completion
npm run test:worktree-policy
npm run test:worktree-cleanup
npm run test:controllers
npm run test:worktree-ui
npm run test:git-context
```

The wire check covers both legacy initialization and modern MCP discovery, including the response/cache fields needed by the installed Claude SDK. The communication check exercises actual ACP → native stdio MCP → service operations, denied actions, coordinator work, retry deduplication, revocation during a turn, restart persistence, and retained sender identity.

Opt-in real-harness verification makes one short model request per selected harness against a running test server, using existing host sign-ins and OpenCode's allowed free models:

```bash
AGENTWORKS_TEST_MCP_HARNESSES=claude,codex,opencode \
AGENTWORKS_URL=http://127.0.0.1:4323 npm run test:mcp-harnesses
```

## Agent completion reports

Participating agents can call **report_completion** to persist a completed/blocked claim with a summary, branch/commit, checks and a retry request ID. **get_completion** reads a permitted session's latest report. Reports appear in **Agents & inbox**, survive updates/restarts and retain authenticated author identity. They do not end turns, start work, approve changes or merge commits. Review and integration remain separate from an agent's claim. See [report schemas, permissions and checks](COMPLETION-REPORTS.md).

Parallel development uses real Codex sessions in separate branches/worktrees, with MCP handoffs and sequential reviewed merges. See [batch 001](work/batches/001-parallel-coordination.md), [batch 002](work/batches/002-worktree-policy.md), and [remaining work](work/BACKLOG.md).

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

In the Hosts view, **＋** opens host setup. Enter a name and an SSH destination (`user@host`, an SSH config alias, or `ssh://user@host:port`). **Install worker with mise** is the default:

1. A live xterm.js terminal shows SSH's host fingerprint prompt and accepts a password, key passphrase, or keyboard-interactive authentication. Existing SSH keys/agents/configuration are tried by SSH. Passwords and terminal input/output are not persisted.
2. Agentworks generates a dedicated Ed25519 key for this host, appends its public key to `~/.ssh/authorized_keys` with `restrict`, and verifies another connection. The private key stays on this server, in a `0700` directory under `host-keys/` beside the catalog; the key file is `0600`. It is unencrypted so a restarted service can reconnect without an SSH agent. Host keys are pinned in that directory's own `known_hosts`; reconnects reject changed keys.
3. The server copies its running binary, locked adapter package files, vendored Codex adapter, and mise configuration to a unique directory under `~/.local/share/agentworks/releases/`. The SHA-256 checksum is verified before extraction. The host must already have **mise**, `tar`, and `sha256sum` or `shasum`, and must be able to download Node and npm dependencies. Setup requires no sudo. Automatic copying currently supports a matching server/remote OS and CPU; incompatible binary loaders/libc fail during installation.
4. Inside that installation, mise installs **Node 24** and runs `npm ci --ignore-scripts --omit=dev`. This uses a dedicated `mise.toml`, leaving the host's global Node selection alone. Adapter paths resolve against the installed bundle; adapters use its Node interpreter even when a workspace selects another Node version.
5. The server waits for the worker handshake, shows the available providers, and offers **Choose working directories**. Provider account sign-ins still belong to the remote user; this flow does not copy provider credentials.

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
- Tools on the host, such as node for the adapters and the `opencode`, `codex`, and `claude` CLIs, come from its PATH. A non-interactive ssh command does not run shell startup files, so tools installed with mise would be missing. The worker therefore applies `mise env` for each session's directory itself, including that project's own mise configuration, falling back to the global configuration if the project's cannot be used (for example, if it is not trusted).

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
Traefik example](work/HOSTING.md).

## Prototype limits

- Chat uses ACP; the Terminal tab opens an independent interactive shell, rather than attaching to the agent harness.
- Sessions and transcripts survive browser reconnects, server restarts, and application updates. Harness processes stop with the server and are resumed when their conversation is opened. Running model/tool processes cannot continue through a server restart.
- Maximum 12 active harnesses; saved inactive sessions do not count toward this limit. The UI loads the most recent 3,000 events / approximately 4 MB; the full event log remains in SQLite. There is no automatic transcript expiration.
- One ACP adapter process per session. Closing a session stops that process group on Linux. Pending approval requests are cancelled on turn cancellation.
- Image output is supported; image upload/model attachments and general question/elicitation forms are not implemented in this prototype.
- Local previews follow live paths rather than retaining artifact snapshots. Use trusted local projects; concurrent filesystem replacement is not hardened into a sandbox.
- Native Windows, remote execution hosts, full provider-specific configuration are deferred.

## Resume conversations from another client

Click **Resume existing session** under New session (also available inside the new-session dialog). Select Claude, Codex, or OpenCode, optionally enter an exact working directory, then **Browse**. The picker shows titles, directories, last activity and session IDs. Use **Load more** for further pages and filter the loaded results by title, path or ID. Choose a conversation and an optional project, then **Resume session**.

The harness’s [ACP session list](https://agentclientprotocol.com/protocol/v1/session-list) supplies the choices. Only existing directories inside the allowed roots are shown; add roots in Settings to expose other projects. Listing and loading do not send inference prompts. Choose a session that has finished its turn in the other client; the picker does not attach to that client’s running terminal/process.

Agentworks loads the selected harness session and imports its earlier user/agent messages and tool output, preserving its original context and model rather than applying the new-session model default. Already-registered conversations open their existing Agentworks entry, so repeat selections do not create duplicates. Imported history is committed atomically; a failed load can be retried with **Resume session** without duplicating a partial replay. Missing external context fails explicitly and never creates a replacement conversation. Import replay is limited to 32 MB / 50,000 updates; normal browser history limits still apply.

Claude and Codex use the active host credential/configuration profile. OpenCode has a **Host CLI / Agentworks** history selector: Host CLI reads its normal existing store; Agentworks uses the isolated store created by this app. OpenCode remains restricted to free models; importing a session configured for a paid model reports an error without changing that model. Host-profile selection persists so future restarts resume from the correct store.

## Persistent sessions

Session records, model configuration, project assignment, request IDs, provider session IDs, and transcript events are committed to SQLite as they arrive. The database uses WAL and atomic transactions. A process lock prevents two servers from opening the same session database. The default database is beside the catalog, named `catalog.sessions.sqlite3`; override it with `AGENTWORKS_DB_PATH`. Keep both the catalog and session database when updating the binary. While the app is running, SQLite’s `-wal`/`-shm` companion files belong to the database; stop the app before making a plain file-copy backup.

After restart, saved sessions appear in the sidebar without launching every harness. Opening one reconnects its original provider session through ACP resume (or load, with replay suppressed to avoid duplicate transcript entries). The browser keeps retrying while the server is down and reconnects after an update, retaining the current draft. A **Resume session** button lets you retry if the harness was temporarily unavailable. Never-used sessions can be recreated if a harness has not saved them yet. If a populated harness session no longer has its own saved context, Agentworks preserves the transcript and reports an error rather than silently starting a fresh conversation. Keep the host’s Claude/Codex data and `.agentworks/opencode`/`.agentworks/demo` directories as well as the app database.

Interrupted turns are marked in the transcript and are never automatically retried; old approval buttons are cleared. Check any last tool operation before sending another turn. **Delete session** permanently deletes the Agentworks record/transcript and stops its harness process; it does not remove the provider’s own conversation files. Restarting the server suspends sessions without deleting them.

The first migration can import the old server’s HTTP snapshots from `legacy-sessions.json` beside the database. It imports each session once and renames the file to `legacy-sessions.imported.json`. This is used to preserve the live conversation during this upgrade.

## Is it safe to close or restart?

`GET /api/activity` reports backend work, independently of browser connections:

```json
{"safeToClose":true,"activeTurns":0,"pendingApprovals":0,"busySessions":[],"sessions":[]}
```

Each session includes `turnRunning`, `pendingApprovals`, and `canClose`. Accepted prompts count as active until their response finishes or cancellation completes. Pending approvals and startup/model-configuration work block `safeToClose`. Idle sessions may be closed/restarted once work has finished; their saved history and harness session IDs remain available after restart.

This endpoint is a snapshot, not a shutdown lock: another browser can submit work after the check. Check it immediately before stopping the server. It does not itself close sessions or stop the listener.

## Checks

With the server running:

```bash
cargo test --locked
npm run test:smoke
```

Remote host links have a separate check that starts its own server and uses `scripts/fake-ssh.sh` in place of ssh, so it needs no SSH server or keys:

```bash
cargo build --locked && npm run test:remote-hosts
npx playwright test tests/terminal.spec.mjs
npm run test:remote-partitions # ~50s: silent transport and replacement ownership
```

Guided enrollment has an isolated real-SSH check (requires `sshd`, `ssh`, and `ssh-keygen`). It creates temporary host keys, a passphrase-protected bootstrap identity, an isolated remote home and authorization file, and a loopback SSH server. By default mise/npm are deterministic fixtures, with no downloads or provider calls:

```bash
npm run test:host-enrollment
# Also exercise real mise, Node download and locked npm installation in that temporary home:
AGENTWORKS_ENROLL_REAL_MISE=1 npm run test:host-enrollment
```

The browser flow is covered by `npx playwright test tests/hosts.spec.mjs`, including mobile layout, terminal input, setup errors/retries, cancellation, and directory selection.

The smoke test uses the demo agent to exercise ACP streaming, multiple turns, disconnect survival, idempotency, approval allow/stale/invalid responses, cancellation, Markdown sanitization, and directory/origin checks. Workspace checks also cover file boundaries, manual automation runs, approval/concurrency handling, and retained run history. They make no paid model requests.

For browser checks:

```bash
npx playwright install chromium
npx playwright test
```

Browser checks cover desktop/mobile layout, session switching, permission requests across reload, cancellation, images, and Markdown file preview.

After `cargo build --locked`, `npm run verify` starts an isolated server on port 4318, runs the API, remote-host relay, project, settings/model, import, workspace/automation, completion-report, worktree-policy, Git-context and browser suites, checks shutdown with an open browser connection, and stops its server. Use `AGENTWORKS_VERIFY_PORT` to change the main test port; worktree policy owns a separate server on port 4351.

Architecture and follow-up work: [ROADMAP.md](ROADMAP.md), [Rust/ACP assessment](RUST-INTEGRATION.md), and [morsh reuse notes](MORSH-REUSE.md).

For an opt-in verification of existing CLI sign-ins (one short model prompt per provider):

```bash
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:signin
```

To verify persistence with an isolated database and real process restarts/crashes:

```bash
npm run test:persistence
# Opt-in: prove Claude/Codex/OpenCode context retention with two short model prompts each:
AGENTWORKS_KEEP_TEST_FIXTURES=1 AGENTWORKS_TEST_HARNESS_RESUME=1 npm run test:persistence
```

Isolated smoke fixtures (catalogs, databases, demo state and Git worktrees) are removed after server shutdown, including on assertion failures. Set `AGENTWORKS_KEEP_TEST_FIXTURES=1` to retain them for inspection; remove the printed fixture directory when finished. Verification also uses a fresh fixture root on every run. Browser screenshots and failure reports remain in `.cache`. See the [fixture audit](work/TEST-FIXTURE-AUDIT.md) for coverage and remaining constraints.

To check native history import without new inference, reuse the populated test fixtures from a prior native persistence run:

```bash
AGENTWORKS_IMPORT_FIXTURE_DB=.cache/persistence-<run>/sessions.sqlite3 \
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:import-capabilities
```

Usage-limit continuation is opt-in per session (off for existing and new sessions). Use the new-session checkbox or the composer’s **Usage-limit continuation** controls to authorize bounded continuation, inspect reset freshness/countdown, disable a timer, or resume deliberately. Verified Claude reset events and native Codex quota errors/read-only account quota queries support durable scheduling; unavailable resets remain manual and OpenCode automatic detection is unsupported. Stop, explicit completion reports and newer accepted instructions cancel pending work. See [L-09 behavior, provider evidence, durability and APIs](work/LIMIT-RESUME.md).

**Session actions** provides rename, archive, suspend and separate history deletion. Archive hides a session and stops its terminal while preserving its scrollback. Agent harness work continues, except that removing a session-created worktree first suspends its harness; Suspend releases its harness and cancels continuation while preserving context/history. The sidebar filter finds archived sessions. See [lifecycle behavior](work/LIFECYCLE.md). **Diff** inspects unstaged changes, branch changes from a selected merge base, or a commit, without modifying the worktree; staged/untracked paths are identified separately. See [diff limits and APIs](work/DIFF.md).

**Fork session** opens the new-session dialog pre-filled from the current conversation: its name, agent, model, project and directory, with a new worktree suggested at the current commit. The agent and model can be changed to branch onto a different one, and the workspace is a separate choice from the conversation. Fork from the latest completed turn or a selected earlier boundary. Prefer an anchored native Codex fork when supported, or explicitly create a fresh session with transcript or a supplied summary; fresh context reaches the agent with your first message without claiming hidden-state equivalence. Lineage is recorded and survives restarts, the original is never interrupted, and pending approvals, queued inputs and Agentworks access are not copied. See [fork behavior, guarantees and checks](FORKING.md).

Settings includes browser-local wrapping, syntax-highlighting and plain-prose preferences for chat blocks and file previews. Blocks offer original-text copy and a plain/code toggle; unsupported or large blocks remain unhighlighted. Tool/approval literals follow wrapping preferences. Successful settings saves use green status feedback; failures remain explicit.

For development coordinated by a saved Agentworks session, see the [bounded self-improvement workflow and transfer helper](work/SELF-IMPROVEMENT.md). Supervised restart is implemented in R-06; the helper can explicitly grant it to the selected coordinator but does not start turns, enable timers or deploy.

Settings → **Service · all projects** offers a bounded wait-until-idle restart, deliberate interruption and cancellation. Run with the external supervisor/user-service setup in [service restart and recovery](work/SERVICE-RESTART.md); direct unsupervised servers refuse restart. The separate default-off `restartService` grant exposes read-only status, restart and own-request cancellation over MCP. A requesting coordinator can explicitly choose one bounded continuation in its saved native context after a successful supervised restart. Prepared release validation and verified private backups precede shutdown; uncertain dispatch or failed starts pause for manual inspection, with no tool replay or automatic database rollback. See [live installation/proof](work/batches/005-service-restart.md).

Session permission modes advertised by the native harness appear beside the model picker while idle. Selecting Auto uses the harness's own review policy, and confirmed selection persists across reconnects. Unsupported modes remain hidden; any model-dependent fallback is shown as reported by the adapter. `GET /api/sessions/{id}/permission-mode` reads the configuration; `POST` with `{"mode":"auto"}` requests an advertised mode only while idle. This does not change Agentworks controller grants or automatically retry interrupted turns. Isolated acceptance: `PLAYWRIGHT_BROWSERS_PATH=$PWD/.cache/browsers node scripts/permission-modes-smoke.mjs` (set `AGENTWORKS_TEST_BINARY` to a built executable if needed).
