# Agent communication and Agentworks MCP

Design and implementation checkpoint, October 17, 2026.

Implemented: native Rust stdio MCP and private loopback service bridge; durable per-session capabilities, project-scoped text inboxes/replies/acknowledgements, audit activity, browser role presets and individual grants, explicit draft delivery, coordinator session creation/prompt/cancellation/manual automation runs, retry deduplication, and initiator visibility. Actual messaging has been verified with Claude, Codex, and OpenCode. Batch 007 adds default-off bounded inbox delivery ([policy, ledger and integration notes](work/INBOX-DELIVERY.md)); combined human-input queue integration remains a coordinator acceptance check. Named external controller credentials for MCP clients outside Agentworks are implemented ([C-01](work/CONTROLLERS.md)). Cross-project grants, cross-host forwarding, and file attachments remain deferred. See [usage and checks](README.md#agent-communication-and-mcp-control).

Batch 001 additionally delivered durable self-authored completion reports and live Git/worktree context, using two concurrent Codex sessions inside Agentworks. See [the integration record](work/batches/001-parallel-coordination.md).

The sections below describe the intended architecture, including later capabilities. The current implementation exposes no settings, deletion, access-grant, or harness-approval tools.

The [review queue](REVIEW-QUEUE.md) adds separately opted-in `manageReviewQueue` MCP access for scoped review records and manual integration reservations. Existing Coordinator presets do not grant it; no queue tool executes Git.

## Communication role defaults

Global Settings → Communication selects Off, Participant, or Coordinator for new sessions created by a human. The built-in default is Participant, enabling project messages without coordinator permissions. Project settings can select a role or inherit the global default. The session creation form shows the resolved default and allows an explicit role for that session. Existing sessions retain their saved access when defaults change; use the session’s Agents & inbox controls to change it.

Participant enables project messages. Coordinator also enables starting sessions and turns, cancelling turns, and running existing project automations. No default role enables service restart. Agent-created child sessions and automation runs retain their unprivileged behavior.

Global execution timeout defaults live in Settings. Session timeout overrides live in Session settings, alongside the session controls, and show the inherited default when no override is enabled.

## Shared foundation

Expose one Agentworks MCP server backed by application services shared with the browser API. Agents use it for communication, inspection, and explicitly authorized orchestration. ACP supports supplying MCP server configurations on session creation, load, and resume; verify behavior against each installed adapter before enabling it. Temporary model discovery and history listing processes must not receive control credentials.

Start with a stdio MCP entry point in the existing executable (`agentworks mcp`). A small client bridge talks to the running service through a private local endpoint. Agent identity comes from a service-issued credential, never from a caller-supplied session ID. Use an official MCP Rust SDK after checking transport and runtime compatibility. This is separate from the existing ACP client.

Later, route operations by stable host/session IDs through the planned SSH-connected host service. Remote paths remain owned by their execution host. An external MCP client can be added with its own explicitly granted identity; it must not impersonate an existing conversation.

References: [ACP session setup and MCP configuration](https://agentclientprotocol.com/protocol/v1/session-setup), [official MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk), [MCP security guidance](https://modelcontextprotocol.io/docs/2025-11-25/tutorials/security/security_best_practices).

## Communication

- Durable per-session inboxes, initially scoped to the same logical project. Ungrouped sessions do not implicitly share an inbox scope. Cross-project contact requires an explicit grant.
- Tools for discovering permitted peers, sending a message, reading messages with a cursor, and acknowledging receipt. Discovery returns minimal metadata, not transcripts or host credentials.
- Each message has an immutable sender and recipient, message ID, thread/reply IDs, timestamp, text, and optional file references. File references preserve host/location identity and do not grant filesystem access.
- Persist messages before acknowledging a send. A sender-scoped request ID prevents duplicate delivery on retries. Receipt/read state is separate from successful completion of requested work.
- Show messages and their delivery state in the browser, with a link to the other session. Keep them distinguishable from human instructions and normal model output.
- Sending a message does not interrupt a running turn or start a model request. Initially, agents read their inbox through tools and users can explicitly deliver pending messages as a new turn when a session is idle.
- Automatic wake-up is a separate, opt-in capability. It needs queue ordering, cancellation, request deduplication, turn/cost limits, and loop protection before enabling unattended agent conversations. Never silently replay a possibly executed turn after a crash.
- Retain sender identity on deletion using a tombstone; do not let deleting a session erase the provenance of messages sent to another session.

## Permissions

Use explicit capabilities with convenient UI presets. Default existing sessions to no MCP access until enabled; newly enabled ordinary sessions receive project-scoped communication and minimal status access.

| Preset | Proposed capabilities |
| --- | --- |
| Off | No Agentworks MCP access |
| Participant | Read own inbox, send to permitted peers, inspect permitted peer status |
| Coordinator | Participant plus explicitly granted session creation, prompt submission, cancellation, and running existing automation definitions within its project |
| Custom | Individually selected capabilities and project/host scope |

Treat starting model work, interrupting other agents, changing automation definitions, deleting records, and modifying application settings as distinct capabilities. Do not bundle them into messaging. Coordinator does not automatically include deletion, settings changes, cross-project transcript access, or automation editing.

The browser exposes a session-level Agentworks access editor showing preset, scope, and individual grants. Only the human control interface can change grants. Agents cannot grant privileges to themselves or descendants, mint broader credentials, or approve harness permission requests. Child sessions start without inherited coordinator privileges.

Configuration changes and destructive operations can be added as individually granted tools after the initial communication/orchestration slice. Exactly which major operations the user wants agents to perform remains a product decision; do not interpret a coordinator preset as blanket administrator access.

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
5. Add project-scoped coordinator tools using existing lifecycle, active-turn, automation, and approval rules. Starting work must retain configured model restrictions, allowed roots, and concurrency limits. Use idempotency keys for operations that start work.
6. Add optional wake-up routing and cross-host forwarding only after queue recovery and bounded conversation behavior are proven.

Acceptance: two sessions with different harnesses exchange durable messages; an ordinary session cannot start/cancel/delete another session or change settings; a granted coordinator can start permitted project work; privilege changes apply immediately; restart preserves messages and grants without replaying work; duplicate requests do not create extra turns; the UI shows who initiated each operation. Demonstrate denied operations even when a caller knows the target ID. Document the same-user trust limitation alongside the tested behavior.
