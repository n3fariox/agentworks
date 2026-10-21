# Agentworks roadmap

Status: architecture reference and execution plan, October 17, 2026. Current task status is tracked in [the execution backlog](work/BACKLOG.md).

Confirmed decisions: Linux hosting for MVP; keep the architecture portable to Windows. Select chat or terminal when creating a session, with no switching. Reuse existing Claude/Codex host CLI sign-ins by default, with API keys as an optional explicit mode and the user's existing VPN for remote browser access. Add multiple execution hosts after MVP. The server starts a stateless worker over SSH and speaks WebSocket with it over ssh's stdin and stdout. All session state stays on the server.

Hosting follow-up: also support the user's public Traefik HTTPS proxy with a UUID mount path and StripPrefix. Web URLs and proxy-aware origin checks must support that deployment; PWA installation is optional at hosting time, with manifest/worker scope confined to the mount. See [the hosting plan](work/HOSTING.md) and backlog R-04/R-05.

Build a local web application for managing Claude and Codex agents across multiple project directories, with a responsive dark interface, structured chat, full terminal sessions, and local file previews.

## Prototype checkpoint

The sidebar now supports Hosts/Projects hierarchies with a Local host, persistent logical projects and host-local location IDs, project creation/rename/deletion, session reassignment, and independent browser expansion preferences. View changes preserve the conversation connection and draft.

Claude and Codex have now each completed a real ACP prompt using the existing host sign-in. `GET /api/activity` reports explicit active-turn state, pending approvals, and whether all work has ended so the listener can be closed.

A runnable chat prototype now exists; see [README.md](README.md) for startup and configuration. It uses Rust/Axum and the official ACP Rust SDK, with a small buildless browser interface to validate the flow before a React frontend.

Verified: native OpenCode `2.0.26` ACP, an advertised free-model picker, and a real Big Pickle streamed reply without an API key; ACP v1 initialization against Claude adapter `0.93.0` and Codex adapter `2.2.2`; multi-turn demo sessions through the same ACP client; streaming, explicit approvals, cancellation, browser reconnect, session switching across directories, Markdown/local file preview, image output, and desktop/mobile browser tests.

Verified additionally: model discovery and selection before the first turn for Claude/Codex/OpenCode, persistent host settings (allowed roots and per-harness defaults), ACP-advertised command/skill selection, and personal-subscription quota output through Claude `/usage` and Codex `/status`. A dedicated usage dashboard remains follow-up work. Session metadata, transcripts, model choices, grouping and request IDs now persist in SQLite, with lazy native ACP resume after restarts, interrupted-turn recovery and duplicate-replay suppression. Allowed roots use add/remove rows. An existing-session picker now imports host CLI/client conversations through ACP list/load, with pagination, root filtering, atomic replay, duplicate detection, and OpenCode host/isolated profile selection.


Workspace navigation now shares project/directory context across Sessions, Files, and Automations. Files supports standalone directory browsing, Markdown/image/source previews, downloads, and a companion preview beside chat. Manual automation definitions and runs persist in SQLite, reuse ordinary chat sessions/approvals, enforce request deduplication and one active run per definition, and recover interrupted runs without automatically replaying instructions. GitHub triggers and schedules remain subsequent work.

Implemented: durable agent-to-agent text inboxes and a native Rust Agentworks MCP interface, with browser-managed per-session capabilities and project-scoped coordinator actions. Actual message delivery works with Claude, Codex, and OpenCode. Automatic wake-up, cross-project grants, and cross-host routing remain deferred. See [communication and MCP execution plan](AGENT-COMMUNICATION.md). MCP permissions do not isolate agents running as the service's OS user.

Batch 001 established the Git baseline and proved two concurrent Codex sessions in separate worktrees with MCP handoffs and sequential reviewed merges. Read-only session workspace context and durable completion reports are now delivered; see [the batch record](work/batches/001-parallel-coordination.md).

Pending: further Claude/Codex tool-use and resume validation, question/elicitation forms, model attachments, provider-specific configuration, application login, terminal mode, and multi-host execution. Prototype sessions persist in SQLite, support configurable private-network binding, and do not mark the full MVP phases complete.

## 1. Proposed scope

### First release

- One user and one host machine, with multiple registered project directories.
- Multiple concurrent sessions, each assigned a provider, execution mode, working directory, and explicit additional directories where needed.
- Choose chat or terminal at session creation; the mode is fixed for the session's lifetime.
- Chat mode: streamed messages, tool activity, approvals, questions, errors, file links, and usage information when the provider supplies it.
- Terminal mode: the actual installed Claude Code or Codex CLI running in a pseudo-terminal (PTY), with keyboard input, resize, colors, interactive prompts, and scrolling.
- Sidebar toggle between Hosts and Projects, showing the same sessions through two hierarchies. Projects are logical groups that can span host machines and working directories. MVP starts with one Local host.
- Session states include running, waiting for input, idle, stopped, and failed, with activity counts on parent groups.
- Start, continue, interrupt, stop, rename, archive, and delete session history. Deleting history and stopping a process are separate actions.
- Sessions continue when the browser disconnects; reconnect restores available output and pending approvals.
- Persistent chat history and project configuration on the host.
- Local file browsing, references, previews, and downloads from registered roots; uploaded attachments are stored in an explicit session directory.
- Responsive dark layout, including usable phone controls and mobile keyboard handling.
- Local startup command, provider availability checks, and clear installation/authentication errors.

### Initial assumptions to confirm

- This is a personal local application. Team accounts and public hosting are outside the first release.
- The application launches and manages its own sessions. Attaching to arbitrary already-running terminal processes is a later investigation.
- Each project can have several sessions. Parallel implementation work uses separate Git branches/worktrees, with one coordinator reviewing and integrating changes sequentially. Batch 002 adds managed worktree launches and project isolation requirements; cleanup and merge controls remain follow-up work.
- Local execution means files and processes live on the host; Claude/Codex model requests still use their configured remote providers.
- Read-only file previews are in scope; a full browser IDE is deferred.
- Linux is the first hosting target. Keep PTY creation, process control, paths, and service installation behind platform interfaces so Windows can be added later using ConPTY. macOS is not a launch requirement.

## 2. Backend decision: Rust with ACP for chat

| Consideration | Python / FastAPI | Rust / Axum |
| --- | --- | --- |
| Claude chat integration | Official Python SDK or ACP client | Official ACP Rust SDK + Claude ACP adapter |
| Codex chat integration | ACP client or direct app-server | Same ACP Rust client + Codex ACP adapter |
| Process and terminal handling | Async subprocesses plus a PTY abstraction | Tokio subprocesses plus a PTY abstraction |
| Web streaming | WebSockets supported | WebSockets supported |
| Development tradeoff | Less integration work for this proposed stack | More up-front integration work; strong compile-time checking |
| Distribution tradeoff | Requires packaging a Python runtime or environment | Can package the Rust service as a native executable; provider tools remain dependencies |

Current recommendation: Rust + Tokio + Axum, using the ACP project’s official Rust client SDK for chat with both providers. Retain React + TypeScript, SQLite, and xterm.js. Validate the published Claude and Codex ACP adapters in Phase 0 before committing dependencies. See [Rust/ACP integration assessment](RUST-INTEGRATION.md) for sources and the compatibility checklist.

The reason to favor Rust is deployment and process ownership, not an assumed performance need. ACP removes the need for separate provider-specific Rust SDKs in the primary chat path. Keep supervision and application event persistence behind explicit interfaces. Do not build both backends. Verify adapter runtime/binary dependencies per host; a native Agentworks executable does not make all provider dependencies disappear.

FastAPI and Axum both document WebSocket support: [FastAPI](https://fastapi.tiangolo.com/advanced/websockets/), [Axum](https://docs.rs/axum/latest/axum/extract/ws/index.html).

## 3. Architecture

```text
Desktop / phone browser
  React UI: projects, sessions, chat, xterm.js, file previews
        |
        | HTTP commands / history / files + authenticated WebSocket streams
        v
Local Rust/Axum web service
  Project registry | session coordinator | file gateway | access control
        |                       |
        v                       v
  SQLite event history     Runtime supervisor
                           |-- ACP client -> Claude ACP adapter (chat)
                           |-- ACP client -> Codex ACP adapter (chat)
                           `-- PTY workers running actual CLIs (terminal)
```

- Serve the built UI and API from the same origin.
- Define a host execution interface for session lifecycle, events, terminal I/O, approvals, and files. The MVP uses an in-process local implementation. A later remote implementation drives a stateless worker started over SSH, through a WebSocket over ssh's stdio. Do not add remote deployment machinery to MVP.
- Keep runtime ownership independent of browser connections. For the MVP, use one service process with supervised child workers; avoid multiple API workers accidentally owning the same session.
- Browser disconnects do not stop agents. A service restart preserves history but does not promise to preserve live processes; interrupted sessions are marked accurately and resumed where supported.
- Isolate blocking SDK or PTY operations from the API event loop. Bound output queues, preserve stream ordering, and prevent one noisy terminal from starving other sessions.
- Persist normalized chat events with session-local sequence numbers before broadcasting. Reconnect requests supply the last received sequence; clients deduplicate replayed events.
- Keep each terminal PTY/process alive in the host supervisor and continuously feed its output into a persistent Ghostty emulator through the morsh bindings, even without a browser. On reconnect, create a fresh browser terminal and send a full redraw of the current screen, then ordered display updates. No terminal scrollback restoration or historical byte replay is required for MVP. The live server emulator preserves its internal state; reconnect does not reconstruct it from a screen snapshot.
- Use one active terminal input controller per session initially; additional tabs can observe. Expire control on disconnect to avoid conflicting keystrokes.
- Pin and record tested provider versions. Display unsupported capabilities explicitly and retain unknown events for diagnosis without crashing rendering.

### Provider integration

**Shared chat integration:** use the ACP project's official Rust SDK, `agent-client-protocol`, as a client. Start the selected adapter over stdio, negotiate protocol/capabilities, and map session updates, content, tool calls, permission requests, cancellation, and supported session loading into application events. Start with a mutually supported stable protocol version; do not assume draft or vendor extensions are portable. ACP permission/session modes are provider controls, distinct from this app's fixed chat/terminal presentation choice. [ACP introduction](https://agentclientprotocol.com/get-started/introduction), [official Rust SDK](https://github.com/agentclientprotocol/rust-sdk).

**Codex chat:** use `@agentclientprotocol/codex-acp`, which translates ACP into Codex app-server operations. Direct app-server integration is a fallback for a demonstrated capability gap, not a second MVP implementation. [Codex adapter](https://github.com/agentclientprotocol/codex-acp).

**Claude chat:** use `@agentclientprotocol/claude-agent-acp`, which wraps the official Claude Agent SDK and advertises images and permission-bearing tool calls. No custom Claude Rust SDK is needed for this route. [Claude adapter](https://github.com/agentclientprotocol/claude-agent-acp).

**Capability and host boundaries:** test existing host sign-in and optional API-key authentication, streaming, images, approvals, questions, cancellation, and history loading for each pinned adapter. Advertise only filesystem/terminal client capabilities that the host service implements and authorizes. Any such callbacks execute on the session's execution host. ACP does not replace our browser replay, output retention, file-preview gateway, process supervision, or post-MVP remote worker link.

**Provider authentication:** reuse the existing host CLI sign-in for Claude and Codex by default. Inherit the normal credential locations without copying tokens or forcing a new login. An explicit optional API-key mode uses isolated provider stores. Keep all credentials on the host, outside browser payloads and session events. [Claude SDK authentication guidance](https://code.claude.com/docs/en/agent-sdk/overview)

**Terminal mode:** launch the installed `claude` or `codex` executable with fixed argument arrays and a validated working directory. Use a PTY and xterm.js to retain the real CLI experience. Chat transcripts come from structured provider events; terminal output is not the chat protocol. [xterm.js documentation](https://xtermjs.org/docs/)

**Existing terminal code:** use `/home/captain/projects/morsh` as a source for focused PTY lifecycle, input batching, resize, and Windows handling patterns. See [morsh reuse assessment](MORSH-REUSE.md). Reuse its Ghostty screen/redraw path for fresh browser attachment while retaining the live host emulator. Validate xterm.js rendering, input modes, and terminal-query handling; full emulator serialization is unnecessary for MVP.

**Session mode:** chat and terminal use separate runtime implementations. Mode is immutable after creation, including when resuming a session. Never allow two processes to mutate the same provider session concurrently.

### Core records

- `Host`: stable ID, display name, capabilities; MVP seeds a single local host. Remote hosts add an SSH destination and the remote command that runs Agentworks.
- `Project`: stable ID, display name, optional description, and session defaults. A project is a logical grouping independent of any host, checkout, or repository.
- `Location`: stable ID, host ID, canonical working-directory root, permitted extra roots, optional display label. Several locations can exist on one host, including separate worktrees. All filesystem authorization remains host/location scoped.
- `Session`: project ID (optional for ungrouped sessions), location ID, provider, mode, provider session ID, runtime state, model, timestamps, last sequence. Location determines the execution host and working directory; project determines organization.
- `Event`: schema version, session ID, sequence, type, timestamp, normalized payload; bounded/redacted provider details where needed.
- `Approval`: provider request ID, session, requested action, status, expiry, decision. Decisions are idempotent and checked against the active request.
- `Artifact`: session, authorized root ID, relative path, MIME type, size, optional hash/snapshot reference.
- `Runtime`: worker identity, process group, lifecycle state, exit status, heartbeat where needed. Process identity must not rely on a persisted PID alone.

Initial event types: message delta/completion, tool start/update/result, approval requested/resolved, question, artifact reference, usage, state change, and error. Preserve provider-specific features through capability metadata instead of forcing identical behavior.

### Sidebar organization

Provide a compact `Hosts | Projects` toggle above the session tree:

| View | Hierarchy | Main use |
| --- | --- | --- |
| Hosts | Host → Project → Session | See what is running on a machine and whether that host is reachable |
| Projects | Project → Host → Session | Follow related work across machines and checkouts |

- Both trees reference the same session IDs. Switching views preserves the selected session, draft message, stream connection, and scroll position; remember expansion state separately for each view.
- Show provider and activity state on session rows; show the working-directory/checkout label as secondary text when needed. Avoid adding another mandatory tree level for paths, especially on mobile.
- Show running and approval counts on host/project groups and connectivity on hosts. An unreachable host has unknown current activity until refreshed; absence of a connection is not proof that its agents stopped.
- Allow an Ungrouped bucket, creating/renaming projects, and moving sessions between projects. Reassigning a project updates organization only; the session keeps its execution host, working directory, and conversation.
- At session creation, choose project, execution host, and working directory, prefilled from the selected tree branch or previous choices. A project can include different directories or repositories on different hosts.
- Project grouping does not grant file access or move/synchronize code and conversation context. Each session retains its host-local runtime and file boundaries.
- Build logical projects and locations during the single-host phase so multi-host support adds real hosts without redefining project identity.

## 4. Local files and project boundaries

- The backend reads local files; browser `file://` links cannot provide this experience.
- Resolve links through project/root IDs and relative paths. Canonicalize and authorize each request; reject traversal and symlink escapes, including changed symlink targets. Avoid arbitrary absolute-path file endpoints.
- Required MVP previews: Markdown and raster images. Basic text/code/diff viewing is useful supporting scope. PDFs and other formats need download support only; no embedded PDF viewer is required. Add richer previews later if needed.
- Sanitize Markdown and disable embedded raw HTML by default. Treat SVG and HTML as active content: download or render through an isolated preview origin without application credentials, with restrictive sandboxing and CSP.
- Add size limits, lazy loading, and partial/range reads where appropriate. Clearly distinguish an unavailable file from an unsupported format.
- Distinguish live path references from saved artifacts: a path preview shows current disk contents; a retained snapshot preserves the result from that turn. Label which is displayed.
- Selecting a file for model context is distinct from previewing it. The adapter must explicitly transmit supported content or a readable path; displaying an attachment does not prove the model received it.
- Never automatically index or attach every file in a project. Keep credential files out of default listings and attachments.
- The file gateway's root restrictions do not sandbox the agent process. Apply provider permissions and supported OS sandboxing separately, and display the effective access level honestly.

## 5. Access, lifecycle, and mobile requirements

- Bind to loopback by default; configure a VPN-reachable listener or private reverse proxy for phone and remote browser access through the user's existing VPN. Document the bind address and firewall configuration. No application-managed VPN, public tunnel, or internet-facing deployment is required.
- Add authenticated browser sessions, approved hosts/origins, CSRF protection for commands, and WebSocket origin/auth checks. The VPN supplies encrypted remote transport; use HTTPS where required for browser features or by the chosen proxy setup. Keep provider credentials on the server and out of URLs/logs.
- Use an explicit login/pairing flow over the private connection. Test access from an actual phone on the VPN.
- Native provider approvals remain meaningful. Display pending requests after reconnect, reject stale responses, and do not auto-approve on browser disconnect.
- Stop the managed process group, with graceful interrupt followed by bounded escalation. Test child cleanup and avoid affecting unrelated processes.
- Define transcript deletion, output retention, attachment cleanup, and diagnostic redaction. Report usage only when supplied; label cost estimates.
- Mobile: project/session drawer, one primary pane, sticky composer above the keyboard, touch-sized controls, readable previews, and terminal toolbar for Esc, Ctrl, Tab, arrows, paste, and interrupt.
- Handle phone sleep and background-tab reconnection without duplicate turns or lost approvals. Include keyboard navigation and visible focus states on desktop.

## 6. Execution phases and exit criteria

### Phase 0 — Prove integrations and decide scope

1. Record the confirmed baseline: Linux hosting, fixed session modes, existing host CLI sign-ins with optional API keys, and browser access through the existing VPN. Validate remaining assumptions during the integration probes.
2. Inventory installed CLIs and versions without changing their configuration.
3. Run the Rust/ACP compatibility spike in [RUST-INTEGRATION.md](RUST-INTEGRATION.md): negotiate capabilities, start, stream, tool use, approval, questions, cancel, images, and history loading with both adapters in two different project directories. Record adapter/runtime packaging requirements.
4. Defer the PTY/Ghostty integration probe to Phase 3 so terminal work does not block the first working chat interface.
5. Verify configuration loading and multi-directory behavior for each adapter.
6. Record a capability/authentication matrix and short architecture decision. Resolve any blocking gap before building the full interface.

Exit: a demonstrated ACP chat path for both providers, or a documented capability gap requiring a scope decision. Terminal validation follows in Phase 3.

### Phase 1 — Foundation and responsive shell

1. Scaffold backend, frontend, development commands, and same-origin production serving.
2. Add SQLite migrations, logical projects, host-local locations, session records, settings, and provider health checks.
3. Define event schemas, adapter contracts, host execution and platform interfaces, lifecycle transitions, and a deterministic fake provider for UI development. Implement only the local Linux host.
4. Build the dark sidebar with Hosts/Projects views, initially one Local host, plus mobile navigation and session grouping controls.
5. Implement local access controls and file-root authorization before exposing file endpoints.

Exit: register two locations, group sessions into logical projects, switch between Hosts/Projects views on desktop/phone without interrupting a session, and persist settings through restart.

### Phase 2 — Chat vertical slices

1. Implement the shared Rust ACP client and provider launch configurations for both adapters against the application lifecycle contract.
2. Render streamed turns with Markdown and image content, tool details, approvals, questions, and available usage. Authorized local-file routes are added in Phase 4.
3. Add turn submission idempotency, interrupt/stop, event replay, and provider-native continuation.
4. Render unsupported events safely and surface authentication/rate-limit failures clearly.

Exit: complete multiple tool-using turns with both providers; approve and deny actions; reconnect during a running turn and a pending approval without duplicates.

### Phase 3 — Terminal vertical slice

1. Adapt the relevant morsh PTY patterns into supervised PTY creation, streamed output, input, resize, and process-group shutdown. Address actual exit status, final-output drain, blocking I/O isolation, and initialization cleanup described in [MORSH-REUSE.md](MORSH-REUSE.md).
2. Keep a live Ghostty emulator per terminal session, feed it all PTY output, and render its full-screen redraw/display updates in xterm.js. Launch and validate both provider CLIs.
3. Add fresh browser attachment using the current screen redraw, ordered subsequent updates, bounded queues, and input ownership. Preserve the PTY and host emulator across browser disconnects; historical scrollback replay is deferred.
4. Add mobile terminal controls and exit/error states.

Exit: use both actual CLIs in separate projects; reload the browser without stopping them; redraw the current screen in a fresh browser terminal and stop without orphan children.

### Phase 4 — Files and artifacts

1. Add project file selection, authorized file routes, MIME handling, previews, and downloads.
2. Make file references and generated outputs open from chat.
3. Add explicit model attachments and optional retained artifact snapshots.
4. Verify preview isolation, traversal/symlink defenses, large-file handling, and missing-file behavior.

Exit: preview a generated Markdown file and image from chat, download a PDF, and verify that unrelated host files cannot be read through the file gateway.

### Phase 5 — Daily-use release

1. Exercise browser access through the existing VPN, including configured listener/proxy, authentication, and WebSocket reconnection.
2. Test real phone browsers, including keyboard layout, background/resume, and touch terminal input.
3. Add configurable concurrency limits, output retention, backup/export, and recovery diagnostics.
4. Package a local startup command and optional user service; document installation, credentials, data locations, and supported versions.
5. Run the release acceptance matrix and document limitations.

Exit: manage sessions across two projects from desktop and phone, in both modes, with both providers; recover predictably from browser disconnect, provider crash, and service restart.

### Phase 6 — Multiple hosts (post-MVP)

Decided: the server connects to each host with `ssh <destination> '<command> agent --stdio'`. The worker prints a ready line, then speaks WebSocket over ssh's stdin and stdout. This replaced an earlier `-R` reverse-forwarded Unix socket, which leaked socket files on the remote host, depended on sshd's stream-local forwarding setting, and ran into Unix socket path limits. SSH provides authentication, encryption, and launch; WebSocket carries the protocol. All session state stays on the server: metadata, transcripts, approvals, settings, and event history. The worker only owns processes and filesystem access on its machine. A dropped link is handled exactly like a server restart. The worker exits and stops its processes, sessions on that host are marked interrupted, and they resume lazily through native ACP resume once the server reconnects. The harnesses' own conversation stores and the working directories stay on the host, which is what makes that resume possible. This replaces the earlier plan of a persistent, state-owning host service.

Implementation reference: [remote hosts and protocol diagrams](REMOTE-HOSTS.md).

1. **Done:** host configuration (name, SSH destination, remote command), ssh launch, startup chatter skipped before the ready line, a versioned WebSocket hello reporting platform, version, and providers, pings, offline reasons that include ssh's output, reconnect with backoff, and browser add/reconnect/remove. Verified with `npm run test:remote-hosts`, which uses an ssh stand-in, and against a real remote host over OpenSSH: connect, worker kill and reconnect, ping keepalive, a clear authentication failure, and no worker processes left behind.
2. **Done: chat sessions on remote hosts.** The server keeps its ACP client unchanged and connects it to a relayed line transport (`Lines`) instead of a local child process. The link multiplexes per-session channels with `spawn`, `data`, `exit`, and `kill`. The worker builds the adapter command with the same `runtime` code, using its own local paths. The server only names the provider, directory, and flags. The worker stops its process groups on link loss or 45 seconds of server silence. Protocol 3 fences replacements with an exclusive per-host cache lock, retained through teardown; the replacement reports ready only after acquiring it. Agentworks advertises no ACP client file-system or terminal capabilities, so there are no ACP callbacks to relay.
3. **Done: remote tool environment.** A non-interactive ssh command does not activate mise, so the worker applies `mise env --json -C <session dir>` to each process it starts. This picks up global and per-project mise tools (node, claude, codex, opencode) without shell startup files. The ACP adapters (`claude-agent-acp`, `codex-acp`) come from an Agentworks checkout on the host, with `npm ci`, so their versions match what the server is tested against. OpenCode speaks ACP natively.
4. **Done: per-host working directories.** Each host has its own allowed folders in its settings. The new-session dialog offers them, and the worker checks that the chosen directory exists (`checkDir`) before a session starts. These folders are a convenience, not a security boundary: the server already has a shell on the host as that user.
5. **Workspace features**, the first version being chat-only. Because the worker is the same binary, it runs the existing Rust code locally behind request/response operations on the link:
   - Model and history discovery use temporary ACP channels over the relay. Remote history import validates the provider-reported cwd and scopes native IDs by host/provider/profile. In-session `/usage`/`/status` commands already use the session transport.
   - File browsing, previews, and downloads use bounded worker list/read operations within the host's folders. The server proxies them and never treats a remote path as local.
   - Git context runs the existing `git_context` code on the worker.
   - Worktrees run the existing `worktrees` code on the worker.
   - Manual automations now select a host and create sessions in its validated location; run IDs remain durable and idempotent.
6. **Agentworks MCP for remote sessions.** The adapter on the host launches `agentworks mcp` from the worker's own binary, which reports its path in the hello. That stdio front end connects to a loopback port the worker opens and reports in the hello. The worker tunnels each connection over the link, and the server attaches it to its existing MCP bridge. Tools and state stay on the server, and the per-session token authenticates exactly as it does locally.
7. **Test drops:** kill the link mid-turn and confirm restart-equivalent behavior with no duplicate turns or processes. Command request IDs stay idempotent across reconnects.
8. **Session creation and trees:** offer connected remote hosts and their folders when creating a session. Verify that one logical project shows sessions on multiple hosts, with distinct paths and accurate connection/activity badges.

Status of steps 2–4: verified with the ssh stand-in (`npm run test:remote-hosts`) and over real OpenSSH against a remote host. The demo agent and OpenCode (free model) answered prompts there, and killing the worker mid-session left no agent processes and the session resumed after the reconnect. Remote Claude and Codex have completed the ACP handshake on the host, but no remote prompt has been sent with them yet. Sessions dropped with their host are suspended, as after a restart. Remote worktree discovery/creation/selection now use worker-side Git operations with host roots and project policy; archive cleanup and restore now use the shared lifecycle implementation on the worker, with dirty confirmation and ownership checks. Server-owned queueing, imports, forks, automations, ACP/manual-reset quota continuation and inbox dispatch now work remotely, with host-aware admission and disconnect/restart tests. Remaining: file/Git views, the remote MCP connection, account/quota probes and explicit remote API-key mode.

Exit: launch sessions on two hosts, drop and restore a link, resume without duplicate turns/processes, and view each host's files through the same browser interface.

### Later, based on actual use

- Git worktrees and per-session isolation for parallel edits.
- Session search, branching, and comparison.
- Local HTML/app previews, richer documents, and file editing.
- Notifications, queued jobs, templates, schedules, and resource budgets.
- Shared users and team authorization.
- Native Windows hosting; revisiting Rust if measurements or distribution needs justify it.

## 7. Validation plan

Use focused unit tests for lifecycle transitions, authorization, adapter event mapping, replay, and idempotency. Use real subprocess integration tests for PTYs, resize, process groups, and output pressure. Use browser tests for chat approvals, file previews, reconnect, and mobile layouts. Keep live provider smoke tests opt-in because they require credentials and can incur usage.

Release matrix: both providers × both modes × supported host OSes, plus desktop and actual phone browsers. Cover concurrent sessions in two projects, stale approvals, malformed provider events, dropped connections, output floods, symlink escapes, active-content previews, provider crashes, and service restart recovery.

## 8. Decisions still needed

These are optional refinements; the initial assumptions above allow implementation to proceed.

1. Should the app discover existing provider histories or only manage sessions launched here?
2. Is one project root per session plus explicitly added directories enough, and should concurrent edits use separate worktrees immediately?

ACP/chat, persistence, workspace files, manual automations and scoped MCP messaging/control have been implemented and exercised. Continue from [the execution backlog](work/BACKLOG.md), beginning with the coordination batch; the phase descriptions above remain architecture/acceptance references, not a claim that completed prototype work must be repeated. Probe PTY/Ghostty behavior when starting terminal work.
