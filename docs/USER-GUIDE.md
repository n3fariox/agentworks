# Using Agentworks

[Documentation index](README.md) · [Quick start](../README.md)

Run commands from the repository root.

## Workspace navigation and files

Each session has **Chat** and **Terminal** tabs. Opening Terminal starts one interactive `$SHELL` (falling back to `/bin/sh`) in that session’s directory, on its local or remote host. It retains the host user’s paths, credentials, locale, desktop/SSH integration, and `NO_BROWSER` setting. Launcher color/CI overrides, agent session markers, forced pagers, service controls, stale terminal metadata, and inherited working-directory/Git overrides are cleared; the interactive shell then loads its own configuration. The service keeps the PTY and libghostty emulator running when you switch tabs or sessions, reload, disconnect, or suspend the agent harness. Reopening attaches to the same shell. Archiving stops the shell and releases its PTY, saving its screen and scrollback for later viewing. Unarchive the session and select **Start new terminal** to start again. Deleting the session stops its terminal and deletes its saved history. After the shell exits, **Start new terminal** creates its replacement. Service restart ends terminals; a remote worker disconnect also ends terminals on that worker. Existing remote workers must be updated to advertise terminal support.

For a terminal outside any session, use **Open terminal** (`›_`) beside a host in the sidebar. Each host gets one shell, started lazily in that host user’s home directory, shared across browser views, and retained when navigating away or reloading. **Stop terminal** releases the shell, its child processes, PTY, and emulator while leaving its screen and scrollback available until service restart. **Start new terminal** replaces a stopped or exited shell. Host terminals are independent of session archive/deletion and need no session or configured working directories. Removing a host, disconnecting its worker, or restarting the service ends its live terminal. Remote host terminals require an updated worker that reports its home directory.

The browser uses the locally vendored xterm.js 6. Ghostty handles output, query replies, and screen state on the shell’s host; reconnect restores up to 10,000 rows of scrollback and the current screen, followed by ordered display updates. Scroll with the mouse wheel, scrollbar, or Shift+Page Up/Down. Reading older output keeps your position when new output arrives. Full-screen applications use a separate alternate screen and do not fill the shell’s scrollback. Live history is bounded in memory; service restart or starting a replacement shell clears it. Archived terminal snapshots are stored in the database and remain viewable after service restart. Multiple browser views share the same shell, with the latest visible view’s resize taking effect.

Use **Sessions**, **Files**, and **Automations** in the navigation rail (tabs across the top on mobile). The header selects the logical project and its Local host directory. **＋** beside the directory adds another location inside the allowed roots; it does not start an agent. Project/location associations persist even after their sessions are deleted.

**Files** browses one directory at a time with breadcrumbs, a parent link, refresh, and a hidden-file toggle. Markdown, raster images, and UTF-8 source/text files have previews; PDFs and other binary formats are downloads. Source/Markdown previews are shortened for large files, and preview/download requests are limited to 10 MB. Canonical paths and symlink targets must remain inside the selected directory; each request also checks the current allowed roots. A directory lists up to 5,000 scanned entries.

**Open beside chat** opens a file preview for a session in the same directory. The session’s Files and Diff panels fill the conversation area; closing the panel restores the chat and draft. File previews show clickable folder breadcrumbs beside the filename so you can return to any parent directory. **Ask about this file** adds a local-file reference to a draft without submitting it or uploading its contents. If no session in that directory is selected, it opens New session with the directory filled in and keeps the reference for that session’s draft. The agent can read the file through its own tools when you send the message. Existing transcript links still offer the file-preview dialog, with **Open in Files** for browsing.

Navigation preserves running sessions and drafts. The chosen project, directory, last file, and workspace view are remembered per browser. Settings are available from the navigation rail.

## Command and session palettes

**Ctrl/Cmd+K** opens the command palette and **Ctrl/Cmd+P** opens the session palette; the header ⌕ button and the sidebar **⌕ Commands & sessions** entry open them by touch or mouse. Neither shortcut is taken while another modal is open, and **Esc** always closes the palette. The **Actions & commands** and **Sessions** buttons switch lists; **Tab** walks the palette's own controls as in any dialog.

Entries are ranked by fuzzy matching, where every typed word must match something. A session is matched on its title first, then project, host and status, then its directory: the end of the path counts more than the leading directories shared by many sessions, which still match at a lower weight. Session rows show the status plus the current marker, agent, project, host and directory, and show more of the path when titles repeat so two sessions stay distinguishable. Running and waiting sessions are listed first until you type. Matching is scored over the whole alignment, so a word that only partly matches never displaces a complete one.

**↑ ↓** and **Page Up/Down** move the highlighted option and **Enter** runs it. Left/right arrows, **Home**, **End** and **Tab** keep their normal meaning in the search box.

Actions call existing controls only (new/resumed session, new project, files, automations, settings, agents and inbox, workspace info, resume, stop, usage, delete). Entries that cannot run right now stay readable, explain why, and refuse to run. Advertised harness commands are typed into the composer for review; choosing one never submits a turn. Switching sessions uses the same path as the sidebar, so each draft and every running turn is preserved.

## Automations

In **Automations**, create a named definition with a harness, optional model, project, host, working directory, and ordered agent-instruction steps. The trigger is **I click Run now** or **On a weekly schedule**: a local time on chosen weekdays in an IANA time zone. GitHub polling and webhooks are future work. A blank model uses the host’s configured default. Goose definitions can also choose a model provider profile enabled on their host. Runs then use that profile instead of the host’s Goose environment, and do not start if the profile is later removed or disabled there. Paused definitions cannot start new runs. Existing prompt-only definitions open as a single step. Scheduled definitions show their next run. Run now remains available. A scheduled time that is more than 5 minutes late (for example, while the server was stopped) is skipped and listed as missed, or is run once if the definition says so. Only the latest missed time is considered. Times while paused never run, and a time is skipped when the previous run is still active. See [scheduled automations](SCHEDULED-AUTOMATIONS.md).

**Run now** creates an ordinary persistent chat session and submits the saved instructions once the harness is ready. Multiple steps are combined into one ordered checklist for a single agent turn; they do not run as separately dispatched steps and have no per-step status, retry, or approval gates. Tool approvals, output, cancellation, file links, and continuing the conversation use the existing session interface. An automation badge links the session back to its definition and recent runs. Repeated requests with the same request ID return the existing run; only one run per automation can be active at a time. The browser retains an unacknowledged run request ID so retrying a dropped response does not silently start another run.

Definitions and run records are saved in SQLite alongside sessions. Completed, failed, cancelled, and interrupted outcomes describe the initial automation turn, independently of later manual conversation turns. Updates/crashes do not automatically repeat an interrupted or queued initial prompt. Deleting a definition keeps its sessions and run records. Deleting a session removes its transcript and marks its run as deleted while keeping the run record. Removing a project moves its definitions/sessions to Ungrouped.

## Session workspace context

Open **Workspace info** in a session to see its current Git branch, commit, repository/worktree paths, and whether it is a linked worktree. Detached HEAD, new branches without commits, non-Git directories and unavailable paths have explicit states. The dialog refreshes on open and preserves the chat stream and draft. See [workspace details and checks](WORKTREES.md).

**New session** starts in the project's directory when one is set (see [project directories](PROJECT-DIRECTORIES.md)); **Browse…** picks or creates a folder inside the allowed roots. It can use a directory, create a new Git worktree, or discover an existing linked worktree. Project settings choose the default and can require isolated worktrees for new sessions, including MCP starts and automation runs. New worktrees use a committed revision; uncommitted source changes stay in the source directory. Existing sessions resume in their original directory. Deleting a session keeps worktree files and branches. Archiving a session removes a clean worktree that the session created (its branch is kept and restore recreates it), asks before deleting uncommitted changes, and refuses unsafe cleanup; nothing is ever merged. Workspace info shows ownership and dirty state. See [ownership and cleanup](WORKTREE-CLEANUP.md). Launch request IDs survive retries and tab reloads; interrupted preparation retains files and requires inspection before a deliberate fresh attempt. See [policy/API details](WORKTREE-POLICY.md) and [browser controls](WORKTREE-UI.md).

## Session roles

Choose a **Role** in New session to add instructions to the agent's system prompt, such as Reviewer or Coordinator. The session keeps the text it was created with, and forks keep their parent's role. Roles never change Agentworks access. Claude supports roles; Codex and OpenCode do not yet. See [roles](ROLES.md).

## Agent communication and MCP control

Each normal chat session is connected to the native **Agentworks MCP** server (`agentworks mcp`). Claude, Codex, and OpenCode have each sent a real message through this connection. Temporary model-discovery/history-listing processes receive no Agentworks control connection. The MCP subprocess uses the official Rust SDK and a credential-protected loopback bridge to the running service; credentials rotate when the harness restarts and are not included in browser snapshots.

Choose **Agentworks access** in New session, or open **Agents & inbox** on an existing session. New sessions use the project's default role, falling back to the global default (Participant unless configured otherwise):

- **Off** (default): no granted tools.
- **Participant**: discover participating peers in the same logical project, send messages, read the inbox, and acknowledge receipt.
- **Coordinator**: Participant plus creating project sessions, starting turns in idle project sessions, cancelling turns, and running existing project automations.
- **Custom**: individually enable those capabilities. Coordinator does not include editing/deleting records, changing settings, granting access, or approving harness permission requests.

Only the human control interface changes grants. Coordinator-created sessions use the configured project/global default role; they do not copy grants from the creating coordinator. Automation runs remain Off. Put sessions in the same logical project to communicate, even when their working directories differ; Ungrouped does not provide a shared communication scope. Moving a session changes its permitted peers immediately. Historical messages remain in its own inbox.

Messages, replies, acknowledgements, grants, and control activity persist in SQLite. Sender/recipient IDs and names remain on messages after a session is deleted. Sending does not interrupt a turn, start inference, or wake an idle/suspended agent. Agents read their inbox through tools at useful checkpoints. The browser shows sent and received messages, supports acknowledgement, and can add a received message to your existing draft with **Use as next turn**; you review and send it explicitly. Acknowledgement records receipt, not completion of requested work. Inbox messages support text and reply references. Optional automatic delivery requires a separately enabled [bounded inbox policy](INBOX-DELIVERY.md).

### Subagent activity

When a harness delegates work through native ACP subagents, `Sub: Name` tabs beside Chat and Terminal show each child's task, activity, text and tools. Nested agents get their own tabs and identify their delegating parent. Child permissions and questions use the usual approval area. Your messages still go to the main agent. Tabs survive reloads and service restarts. **Stop** cancels the owning turn through the harness; individual child controls are not exposed. See [subagent support](SUBAGENTS.md).

### Chat attachments

Use the composer’s 📎 button, drag and drop, or paste to attach up to four files to a message. PNG, JPEG, GIF and WebP images are limited to 5 MB; UTF-8 text files are limited to 256 KB. Availability depends on the harness’s advertised ACP prompt capabilities; OpenCode images, PDFs and other binary files are refused. Files are stored per session outside the workspace and sent as embedded content, without exposing host paths. Each attachment is consumed by one request and is not replayed after cancellation, queueing, quota continuation, or restart. Attachments cannot be queued while an agent is turning. Session deletion removes stored attachments. MCP `prompt_session` remains text-only.

External MCP clients, such as a coordinating conversation, use their own named, project-scoped and revocable **external controller** credentials from Project settings; they never borrow a session's identity, and their operations are attributed by name. See [external controllers](CONTROLLERS.md).

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

## Resume conversations from another client

Click **Resume existing session** under New session (also available inside the new-session dialog). Select Claude, Codex, or OpenCode, optionally enter an exact working directory, then **Browse**. The picker shows titles, directories, last activity and session IDs. Use **Load more** for further pages and filter the loaded results by title, path or ID. Choose a conversation and an optional project, then **Resume session**.

The harness’s [ACP session list](https://agentclientprotocol.com/protocol/v1/session-list) supplies the choices. Only existing directories inside the allowed roots are shown; add roots in Settings to expose other projects. Listing and loading do not send inference prompts. Choose a session that has finished its turn in the other client; the picker does not attach to that client’s running terminal/process.

Agentworks loads the selected harness session and imports its earlier user/agent messages and tool output, preserving its original context and model rather than applying the new-session model default. Already-registered conversations open their existing Agentworks entry, so repeat selections do not create duplicates. Imported history is committed atomically; a failed load can be retried with **Resume session** without duplicating a partial replay. Missing external context fails explicitly and never creates a replacement conversation. Import replay is limited to 32 MB / 50,000 updates; normal browser history limits still apply.

Claude and Codex use the active host credential/configuration profile. OpenCode has a **Host CLI / Agentworks** history selector: Host CLI reads its normal existing store; Agentworks uses the isolated store created by this app. OpenCode remains restricted to free models; importing a session configured for a paid model reports an error without changing that model. Host-profile selection persists so future restarts resume from the correct store.

## Continuation and session actions

Usage-limit continuation is opt-in per session (off for existing and new sessions). Use the new-session checkbox or the composer’s **Usage-limit continuation** controls to authorize bounded continuation, inspect reset freshness/countdown, disable a timer, or resume deliberately. Verified Claude reset events and native Codex quota errors/read-only account quota queries support durable scheduling; unavailable resets remain manual and OpenCode automatic detection is unsupported. Stop, explicit completion reports and newer accepted instructions cancel pending work. See [L-09 behavior, provider evidence, durability and APIs](LIMIT-RESUME.md).

**Session actions** provides rename, archive, suspend and separate history deletion. Archive hides a session and stops its terminal while preserving its scrollback. Agent harness work continues, except that removing a session-created worktree first suspends its harness; Suspend releases its harness and cancels continuation while preserving context/history. The sidebar filter finds archived sessions. See [lifecycle behavior](LIFECYCLE.md). **Diff** inspects unstaged changes, branch changes from a selected merge base, or a commit, without modifying the worktree; staged/untracked paths are identified separately. See [diff limits and APIs](DIFF.md).

**Fork session** opens the new-session dialog pre-filled from the current conversation: its name, agent, model, project and directory, with a new worktree suggested at the current commit. The agent and model can be changed to branch onto a different one, and the workspace is a separate choice from the conversation. Fork from the latest completed turn or a selected earlier boundary. Prefer an anchored native Codex fork when supported, or explicitly create a fresh session with transcript or a supplied summary; fresh context reaches the agent with your first message without claiming hidden-state equivalence. Lineage is recorded and survives restarts, the original is never interrupted, and pending approvals, queued inputs and Agentworks access are not copied. See [fork behavior, guarantees and checks](FORKING.md).

## Display preferences

Settings includes browser-local wrapping, syntax-highlighting and plain-prose preferences for chat blocks and file previews. Blocks offer original-text copy and a plain/code toggle; unsupported or large blocks remain unhighlighted. Tool/approval literals follow wrapping preferences. Successful settings saves use green status feedback; failures remain explicit.

For development coordinated by a saved Agentworks session, see the [bounded self-improvement workflow and transfer helper](SELF-IMPROVEMENT.md). Supervised restart is implemented in R-06; the helper can explicitly grant it to the selected coordinator but does not start turns, enable timers or deploy.


## Goose subscription connections

In **Settings → Model providers**, add a connection, choose **ChatGPT subscription · Codex OAuth**, enter a name and a model available to your account, select its execution hosts, and save. Click **Sign in with ChatGPT** and finish authorization in the new browser tab. Then select Goose and that connection when creating a session. **Disconnect** removes the server's saved credentials and stops it from issuing access tokens to sessions.

Browser sign-in temporarily binds `127.0.0.1:1455` and redirects to `http://localhost:1455/auth/callback`, independently of the app's web port or HTTPS origin. The socket is released when a valid callback arrives, on cancellation, retry, switching to device login, disconnect, provider deletion, or after ten minutes without a callback. If another app or login owns that port, Agentworks leaves it alone and offers device login instead. For browser login on a remote server, forward the callback port (`ssh -L 1455:127.0.0.1:1455 user@server`) as well as any web tunnel you use. Neither device-code option opens a callback listener.

The **Device code** button uses Rig's device-login implementation. Agentworks displays a one-time code and an OpenAI verification link, then waits for approval without a localhost callback or extra SSH tunnel. It works through HTTPS deployments too. Device-code login may need enabling in ChatGPT security settings or by a workspace administrator. It uses Codex's existing client ID and publicly visible implementation endpoints, whose third-party compatibility and stability are not guaranteed.

Browser and device login save to the selected connection and share the server refresh and Goose worker path. Successful sign-in replaces that connection's account and closes the temporary sign-in panel; failed or cancelled attempts preserve existing credentials. Starting another login cancels the previous attempt on that connection. Rig stages its credentials in memory and cannot persist a cancelled or superseded result. Use separate provider connections for different accounts. Rig requires Rust 1.97.1 or newer.

Agentworks saves the OAuth credentials in the server's protected model-provider store and owns token refresh. Local and SSH Goose adapters request an access token over their existing ACP connection before each model request. They keep that token in memory and never receive the refresh token or write a Codex OAuth credential file. Host assignments are checked on each token request. Remote workers must be updated to a version supporting managed Codex authentication. Enterprise access continues to depend on your workspace's entitlements and permissions.

The embedded Goose snapshot and its small managed-auth patch are documented in [Goose sources](../vendor/GOOSE-SOURCES.md). `AGENTWORKS_CODEX_OAUTH_ISSUER` and `AGENTWORKS_CODEX_API_URL` are operator-controlled endpoint overrides used by integration fixtures; normally leave them unset.

**Claude subscription · Claude Code** continues to use the installed Claude Code CLI on each execution host. Its **Sign in** button invokes the native `claude auth login` flow in the host terminal. If its localhost callback cannot reach that host, open the printed URL and paste the returned code into the waiting terminal. `CLAUDE_CODE_EXECUTABLE` or Goose's `CLAUDE_CODE_COMMAND` can select the CLI executable. Claude Code owns its credentials and shares its account with other Claude Code sessions on that host.

Closing the Claude sign-in terminal leaves its flow running until it finishes or times out. **Stop sign-in** cancels it; **Retry sign-in** starts it after exit. For ChatGPT, **Cancel sign-in** invalidates the pending attempt; expired, cancelled, or repeated callbacks cannot complete it. Sign-in makes no inference request and does not verify model access. The first successful inference confirms access. Existing OpenAI-compatible connections continue using their saved URL and API key.
