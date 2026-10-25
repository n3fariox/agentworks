# Named external controllers (C-01)

An external controller is an explicit, named, revocable identity for an MCP client that is not an Agentworks session, such as a coordinating conversation in another tool. It never borrows a session's identity:

- its actor ID is `controller:<uuid>` and its URI is `agentworks://controllers/<uuid>`;
- its token (`awc_` followed by 64 hex characters) cannot match a session's rotating harness credential;
- every operation is authorized and audited as that controller.

## Scope and grants

Each controller belongs to exactly one project and is created by a human in **Project settings → External controllers…**. Grants use the existing capability names. The default is messaging only (`communicate`). `startSessions`, `promptSessions`, `cancelSessions`, `archiveSessions` and `runAutomations` must be selected explicitly. `restartService` is rejected, and at least one grant is required.

| Capability | Controller tools |
| --- | --- |
| any grant | `list_sessions` (same-project sessions; coordinators and archive controllers also see non-messaging sessions) |
| `communicate` or coordinator grants | `get_completion` |
| `communicate` | `read_inbox`, `send_message`, `acknowledge_message` |
| `startSessions` | `start_session` in its project; started sessions inherit no grants and record the controller as initiator |
| `promptSessions` / `cancelSessions` | `prompt_session` / `cancel_session` on same-project sessions |
| `archiveSessions` | `archive_session {sessionId}` on same-project sessions, including non-messaging sessions. Controller grant remains explicit. |
| `runAutomations` | `list_automations`, `run_automation` for project automations |

These are never available to controllers, regardless of stored grants:

- `report_completion` (claims are self-authored by sessions);
- the service restart tools;
- `inheritResumePolicy`;
- any human-only operation: grants, settings, deletion, explicit worktree removal and harness approvals.

For session-based MCP callers, coordinator grants also enable `archive_session`; a participant cannot gain it by setting only an `archiveSessions` field. Older saved coordinator grants gain archive access through the same coordinator capability check. External controllers still require the explicit `archiveSessions` grant. The tool accepts a bare session ID or session URI and rejects all additional fields. It uses the HTTP lifecycle archive path with default (`auto`) cleanup: clean, safe owned worktrees may be removed (branch retained), while dirty, busy, shared, locked or unsafe worktrees are retained. Terminal shutdown, persistence and events are unchanged. It cannot rename, unarchive, force removal or confirm dirty deletion. Repeating a call on an already archived session returns its metadata without rerunning cleanup. Every call, including retries, checks current grants and project scope and is audited.

Project boundaries are enforced in every handler, including for known IDs, retries and reply targets.

Sessions with `communicate` see live messaging controllers in a `controllers` array of `list_sessions` and can reply with `recipient: "agentworks://controllers/<id>"`. Messages from controllers are persisted and visible but **never feed opt-in inbox wake delivery**; starting work requires an explicit `promptSessions` grant. Controller-started turns and sessions are labelled in the browser with the controller's name (`initiatedBy`/`initiatorTitle` on the `user` event, `initiatedBy.actorTitle` on sessions). Agents & inbox activity shows the controller by name.

## Credentials and revocation

- The token is returned once, inside a ready-to-use MCP server configuration. Only its SHA-256 digest is stored (table `agent_controllers`; additive, so the schema stays at version 7 and older binaries ignore it). Listings never include tokens or digests.
- Every bridge request resolves the token to a live controller and rereads its grants. Revocation (`POST /api/controllers/{id}/revoke`) serializes with in-flight bridge operations through the communication lock; the next request from an already-connected client fails with 401. Revoked records stay for attribution.
- Deleting a project revokes its controllers. A project may have at most 20 live controllers.
- Each request records `last_used`. Mutations are audited before and after dispatch, as for sessions. Human creation and revocation are also audited.

## Connecting a client

Agentworks writes its per-process loopback bridge address to `<database>.mcp-endpoint`, mode 0600, at startup. `agentworks mcp` accepts:

- `AGENTWORKS_MCP_ENDPOINT_FILE`, reread on each request, so a long-lived client follows service restarts;
- `AGENTWORKS_MCP_TOKEN_FILE`, which keeps the secret out of client configuration;

alongside the existing `AGENTWORKS_MCP_ENDPOINT`/`AGENTWORKS_MCP_TOKEN` variables. Example:

```json
{"mcpServers":{"agentworks":{"command":"/path/to/agentworks","args":["mcp"],
  "env":{"AGENTWORKS_MCP_ENDPOINT_FILE":"/path/to/catalog.sessions.mcp-endpoint",
         "AGENTWORKS_MCP_TOKEN_FILE":"/private/agentworks-controller-token"}}}}
```

The endpoint must be loopback, so the client must run on the service host.

## HTTP API (human only; existing Origin checks)

- `GET /api/projects/{id}/controllers` returns `{controllers:[{id, actor, uri, name, projectId, access, created, revoked, lastUsed}]}`.
- `POST /api/projects/{id}/controllers {name, access?}` returns `{controller, token, mcpServer, note}`.
- `POST /api/controllers/{id}/revoke` returns `{revoked:true, alreadyRevoked}`.
- `GET /api/controllers/{id}/audit` returns the latest 100 events where the controller is actor or target.

## Trust boundary

These are application permissions, as described in [AGENT-COMMUNICATION.md](AGENT-COMMUNICATION.md#trust-boundary). Anyone who can read the token, or who shares the service's OS user, can act as the controller. Agents running as the same user can also reach the human HTTP API. Store tokens in private files and revoke exposed ones.

## Checks

```bash
CARGO_TARGET_DIR="$PWD/target" cargo test --locked --offline
npm run test:controllers
npm run test:controller-archive
```

The smoke owns `127.0.0.1:4531` (4531–4535 via `AGENTWORKS_CONTROLLER_PORT`) and isolated data under `.cache/controllers-*`. It drives the real `agentworks mcp` stdio bridge with the MCP SDK, as an external client does, using the endpoint and token files. It covers:

- creation validation, least-privilege defaults, token secrecy in listings and SQLite;
- filtered tool lists and two-way attributed messaging;
- cross-project and ungranted denials with known IDs, and refusal of session-only tools;
- coordinator start and prompt with attribution and retry deduplication, plus the audit trail;
- immediate revocation of a connected client and forged tokens;
- restart persistence with endpoint following, and revocation on project deletion;
- the browser create, show-once, revoke and phone-layout flow.

The controller archive smoke uses its own server on ports 4536–4540 (`AGENTWORKS_CONTROLLER_ARCHIVE_PORT`) and disposable Git/database/demo fixtures. It covers explicit grants, hidden-tool denial, invalid fields, project movement, session identity denial, clean/dirty/locked/shared/busy worktree handling, terminal shutdown, retry behavior, audit attribution and connected-client revocation. It never connects to the live service or restarts a service.
