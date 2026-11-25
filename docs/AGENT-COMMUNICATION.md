# Agent communication and Agentworks MCP

Design and implementation checkpoint, October 17, 2026.

Implemented: native Rust stdio MCP and private loopback service bridge; durable per-session capabilities, project-scoped text inboxes/replies/acknowledgements, audit activity, browser role presets and individual grants, explicit draft delivery, coordinator session creation/prompt/cancellation/archiving/manual automation runs, retry deduplication, and initiator visibility. Actual messaging has been verified with Claude, Codex, and OpenCode. Default-off bounded inbox delivery is available ([policy, ledger and integration notes](INBOX-DELIVERY.md)); explicit human inputs take priority over inbox delivery. Named external controller credentials for MCP clients outside Agentworks are implemented ([C-01](CONTROLLERS.md)). A single cross-project [orchestrator](#orchestrator-prototype) scope is prototyped; general cross-project grants and file attachments through MCP remain deferred. Remote-agent MCP now supports permission-checked peer discovery and inbox read/send/acknowledgment; see [the relay contract](REMOTE-HOSTS.md#agentworks-mcp-messaging-relay). Chat attachments are supported in the browser composer; see the [user guide](USER-GUIDE.md#chat-attachments). Server-authorized inbox turns reach remote sessions through ACP independently of MCP. Remote agents can invoke messaging tools themselves; coordinator and completion MCP operations remain outside the remote relay allowlist. See [usage and checks](USER-GUIDE.md#agent-communication-and-mcp-control).

The sections below describe the intended architecture, including later capabilities. The current implementation exposes no settings, deletion, access-grant, or harness-approval tools.

Review and merge coordination uses ordinary messages: send a review request with `expectReply` and the reply wakes you (see [inbox delivery](INBOX-DELIVERY.md#reply-requests)). The former review queue and its `manageReviewQueue` grant were removed; saved grants with that key are accepted and ignored.

## Communication role defaults

Global Settings → Communication selects Off, Participant, or Coordinator for new sessions created by a human. The built-in default is Participant, enabling project messages without coordinator permissions. Project settings can select a role or inherit the global default. The session creation form shows the resolved default and allows an explicit role for that session. Existing sessions retain their saved access when defaults change; use the session’s Agents & inbox controls to change it.

Participant enables project messages. Coordinator also enables starting sessions and turns, cancelling turns, archiving same-project sessions with safe automatic worktree cleanup, and running existing project automations. No default role enables service restart. Agent-created child sessions and automation runs retain their unprivileged behavior.

Global execution timeout defaults live in Settings. Session timeout overrides live in Session settings, alongside the session controls, and show the inherited default when no override is enabled.

## Orchestrator (prototype)

The orchestrator is a single slot pinned above the session browser. It is a session the slot owns, not a grant on an ordinary session. Sessions never move into or out of the slot.

- **New orchestrator** (`POST /api/orchestrator {provider, model?, modelProvider?, title?, requestId}`) creates a local, ungrouped session in a server-owned, empty `orchestrator` directory next to the catalog. It has coordinator access and, where the harness supports roles, the built-in Orchestrator role. The dialog asks only for harness, model and title. It is refused while the slot is filled.
- **Fork to another harness** (`POST /api/orchestrator/fork`, with the usual fork context fields) creates the fork in the same directory, moves the slot to it and deletes the old session.
- **Clear slot** (`DELETE /api/orchestrator`) deletes the orchestrator session.
- Fork and clear are refused while the orchestrator's turn runs.
- The slotted session cannot be archived, deleted, forked, moved, regrouped or given additional directories through the ordinary endpoints (409), and the UI hides those actions.
- `GET /api/orchestrator` and `/api/catalog` (`orchestratorId`) report the slot.

The orchestrator directory is not an allowed root, and `checked_cwd` never admits it. Only slot-created sessions are created there, without workspace preparation. Inbox delivery and restart handoff accept it only for the slotted session. So no project session, coordinator or human-created session can write instruction files into the orchestrator's directory.

- **Reach.** The orchestrator can message and steer every session, ungrouped ones included. Project sessions with the communication grant can message the orchestrator, and each other only within their project, as before. Sessions without the grant do not see it, either in `list_sessions` or as `@orchestrator`. Inbox delivery rechecks this at dispatch, and the orchestrator's own inbox turns are not held for lacking a project.
- **Coordination.** Its coordinator grants (start, prompt, cancel, archive, run automations, `get_completion`) apply everywhere. `start_session` takes `project` (an ID or slug) and still follows that project's workspace policy, roots, routing and limits. Sessions it starts inherit no grants. `inheritResumePolicy` is refused for it, because limit resume follows lineage within one project.
- **Hosts.** Any caller with `startSessions`, not only the orchestrator, can pass `hostId` (an ID or name) to `start_session`, for example to build on another platform. `list_hosts` lists the local host and every enrolled remote host, with status, allowed roots and installed harnesses. The default remains the caller's own host. `cwd` must be inside the chosen host's roots and the harness must be installed there. Unknown hosts return 404 and disconnected hosts 409. Remote agents still cannot call `start_session` through the relay.
- **Discovery.** `list_projects` returns each project's slug, name, default directories (with host name and status) and session counts. Its `list_sessions` covers every session, with `projectId`/`projectSlug`. Project sessions see it as `orchestrator`.
- **Tickets and automations.** Its task tools take `project` (or a task URI naming it) and work in any project with tasks enabled. `list_automations` covers every project (optionally filtered by `project`) with `projectSlug`. `get_automation`, `list_automation_runs` and `run_automation` accept any project definition but never Ungrouped ones. Writing automations stays with sessions inside the project.
- **Tool text.** Its tool descriptions say "any project" instead of "this project", and project-scoped tools list a `project` field.
- **UI.** Notification dots mark the slot row, and the orchestrator's alerts are labelled "Orchestrator" instead of bolding Ungrouped. The dashboard lists it first as "Orchestrator". Agents & inbox states that its grants apply to every project.
- **Mentions.** In the orchestrator's composer, `@` lists `@project/session`. Project sessions that can message also list `@orchestrator`. Mentions still expand to `agentworks://sessions/<id>` links.
- **Not coding.** The Orchestrator role tells it to delegate rather than edit, and its directory holds no code. This is a convention, not isolation: agents run as the service's OS user.

`scripts/orchestrator-smoke.mjs` exercises all of this end to end with the demo harness.

## Shared foundation

Expose one Agentworks MCP server backed by application services shared with the browser API. Agents use it for communication, inspection, and explicitly authorized orchestration. ACP supports supplying MCP server configurations on session creation, load, and resume; verify behavior against each installed adapter before enabling it. Temporary model discovery and history listing processes must not receive control credentials.

Start with a stdio MCP entry point in the existing executable (`agentworks mcp`). A small client bridge talks to the running service through a private local endpoint. Agent identity comes from a service-issued credential, never from a caller-supplied session ID. Use an official MCP Rust SDK after checking transport and runtime compatibility. This is separate from the existing ACP client.

Messaging bridge operations now travel through active credential-bound channels on the remote worker link. Remote paths remain owned by their execution host. An external MCP client can be added with its own explicitly granted identity; it must not impersonate an existing conversation.

References: [ACP session setup and MCP configuration](https://agentclientprotocol.com/protocol/v1/session-setup), [official MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk), [MCP security guidance](https://modelcontextprotocol.io/docs/2025-11-25/tutorials/security/security_best_practices).

## Communication

- Durable per-session inboxes, initially scoped to the same logical project. Ungrouped sessions do not implicitly share an inbox scope. Cross-project contact requires an explicit grant.
- Tools for discovering permitted peers, sending a message, reading messages with a cursor, and acknowledging receipt. Discovery returns minimal metadata, not transcripts or host credentials.
- Each message has an immutable sender and recipient, message ID, thread/reply IDs, timestamp, text, and optional file references. File references preserve host/location identity and do not grant filesystem access.
- Persist messages before acknowledging a send. A sender-scoped request ID prevents duplicate delivery on retries. Receipt/read state is separate from successful completion of requested work.
- Show messages and their delivery state in the browser, with a link to the other session. Keep them distinguishable from human instructions and normal model output.
- Sending a message does not interrupt a running turn or start a model request. Initially, agents read their inbox through tools and users can explicitly deliver pending messages as a new turn when a session is idle.
- Automatic wake-up is on by default and can be limited or disabled per session; see [inbox delivery](INBOX-DELIVERY.md). Never silently replay a possibly executed turn after a crash.
- Retain sender identity on deletion using a tombstone; do not let deleting a session erase the provenance of messages sent to another session.

## Permissions

Use explicit capabilities with convenient UI presets. Default existing sessions to no MCP access until enabled; newly enabled ordinary sessions receive project-scoped communication and minimal status access.

| Preset | Proposed capabilities |
| --- | --- |
| Off | No Agentworks MCP access |
| Participant | Read own inbox, send to permitted peers, inspect permitted peer status |
| Coordinator | Participant plus session creation, prompt submission, cancellation, safe session archiving, and running existing automation definitions within its project |
| Custom | Individually selected capabilities and project/host scope |

Treat starting model work, interrupting other agents, changing automation definitions, deleting records, and modifying application settings as distinct capabilities. Do not bundle them into messaging. Coordinator does not automatically include deletion, settings changes, cross-project transcript access, or automation editing.

The browser exposes a session-level Agentworks access editor showing preset, scope, and individual grants. Only the human control interface can change grants. Agents cannot grant privileges to themselves or descendants, mint broader credentials, or approve harness permission requests. Child sessions start without inherited coordinator privileges.

Other configuration changes and destructive operations remain separately gated. Exactly which major operations agents may perform remains a product decision; do not interpret a coordinator preset as blanket administrator access.

Validate authorization on every operation, including known IDs, reply targets, inbox cursors, and retry lookups. Filter tool discovery for usability but also enforce permissions in handlers. Check the grant again immediately before a queued operation executes. Scope changes and revocation apply to existing connections.

Store grants durably, issue process-lifetime credentials at harness launch, rotate on reconnect/restart, and never put secrets in browser snapshots, transcripts, audit events, or process arguments. Persist an audit record of actor, action, target, outcome, and request ID for both allowed mutations and rejected control attempts. Apply payload, inbox, concurrency, and request-rate limits.

## Trust boundary

These grants govern the Agentworks control interface. The current deployment runs trusted agents and the service as the same OS user; agents may already be able to read app data or call the existing private-network browser API using shell tools. Allowed root directories do not sandbox their shell execution.

Consequently, session credentials alone cannot enforce a hard boundary against a malicious same-user process. Do not describe the initial implementation as that boundary. Strong enforcement requires isolating agent processes from service files/credentials and protecting all control paths, including browser mutations, with a separate human identity. Keep privileged service ownership and an agent sandbox/separate OS identity as a distinct hardening step if required.

## Execution order

1. Introduce durable grants, message storage, audit events, and shared authorization-aware application operations. Keep existing session IDs and state compatible with migration.
2. Add the stdio MCP entry point and private service bridge; test caller identity, authorization, revocation, and request deduplication with deterministic clients.
3. Inject the MCP configuration into normal ACP create/load/resume paths. Verify actual tools/list and a harmless tool call with Claude, Codex, and OpenCode; protocol support alone is not evidence that each adapter wires it through.
4. Add browser access controls, inbox visibility, delivery state, and explicit idle-turn delivery. Verify phone layouts and persistence across updates.
5. Expand project-scoped coordinator tools using existing lifecycle, active-turn, automation, and approval rules. Starting work must retain configured model restrictions, allowed roots, and concurrency limits. Use idempotency keys for operations that start work.
6. Add optional wake-up routing and cross-host forwarding only after queue recovery and bounded conversation behavior are proven.

Acceptance: two sessions with different harnesses exchange durable messages; an ordinary session cannot start/cancel/delete another session or change settings; a granted coordinator can start permitted project work; privilege changes apply immediately; restart preserves messages and grants without replaying work; duplicate requests do not create extra turns; the UI shows who initiated each operation. Demonstrate denied operations even when a caller knows the target ID. Document the same-user trust limitation alongside the tested behavior.
