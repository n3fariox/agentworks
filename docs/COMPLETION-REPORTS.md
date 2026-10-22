# Completion reports

Completion reports are durable **agent claims**, not proof of review, merge, or successful tests. They do not change session/turn status, submit prompts, trigger models, approve tools, merge code, or grant access. The human session completion endpoint and Agents & inbox dialog remain available independently of agent grants.

The separate [review and merge queue](REVIEW-QUEUE.md) pins proposals, independent review/test evidence and manual integration reservations. Completion claims never populate or advance it.

## MCP tools

`report_completion` records a report for the authenticated MCP session only. Participant (`communicate`) access is required on every call, including retries. No actor, author, recipient, or session ID can be supplied. Unknown fields and invalid types are rejected.

```json
{
  "status": "completed",
  "summary": "Implemented completion reports; awaiting integration review.",
  "branch": "batch/001-completion-reports",
  "commit": "0123456789012345678901234567890123456789",
  "checks": [{"name": "Completion smoke", "result": "passed"}],
  "requestId": "completion-1"
}
```

Required fields: `status` (`completed` or `blocked`), nonblank `summary` (up to 4096 UTF-8 bytes), nonblank `requestId` (up to 100 bytes). Optional `branch` must be nonblank and at most 256 bytes. Optional `commit` must be a full 40- or 64-character hexadecimal object ID. A blocked report can omit a commit; completed reports may also omit one for non-code tasks. Optional `checks` defaults to `[]`: at most 32 objects, each with nonblank `name` (256 bytes) and `result` (2048 bytes). The serialized normalized report input is capped at 24000 bytes. Bounds are enforced server-side; schema maxLength is advisory for clients. Optional branch/commit accept null as absence.

Response: `{ "report": {...}, "duplicate": false }`. The report includes the input fields plus database `id`, immutable `author` session ID, `authorTitle`, project snapshot `projectId`, and millisecond timestamp `created`.

`get_completion` takes `{}` for self or `{"sessionId":"..."}` for a peer and returns `{ "report": {...} }`, or `{ "report": null }` when none exists. A Participant can query a currently participating same-project peer. Coordinators with any session-control/automation capability can inspect same-project sessions even if those targets have messaging disabled. Every query checks live caller/target grants and project membership. Self queries require communication or coordinator access. Cross-project targets are forbidden. Peer results also match the report's saved project, preventing historical reports from following an author into a different project. Self/human queries retain access to that session's own history's latest report. Arbitrary actor fields are rejected.

Revocation applies immediately, including to already cached MCP tools. The server uses its authenticated native MCP token, never a caller-provided actor identity.

## Persistence, retries, and audit

SQLite schema version 4 adds `agent_completions` and a latest-report index in the existing migration transaction. Version 3 sessions, events, grants, messages, approvals in session state, and other data remain intact. Future versions above 4 are rejected. Reports are append-only rows with unique `(author, request_id)` and monotonic database IDs; latest means highest ID, not timestamp. Author ID/title and project snapshot survive session deletion; reports never transfer to another session. The human endpoint still requires the session to exist.

Retrying the same requestId with the same normalized typed fields returns the original report and `duplicate:true`, even after restart or after newer reports. JSON key order and omitted/default empty checks do not change the normalized payload. Changed content under the same key returns 409 and leaves latest unchanged. There is no second report row or `completion_reported` event on retry. The bridge still audits each requested/accepted/rejected attempt, including retries. A successful new report and `completion_reported` audit row commit atomically under the communication lock, followed by a `completion_reported` session event containing `reportId`. A crash between commit and event emission cannot lose the report; polling reads the table rather than relying on events. Rejected queries are audited through the existing bridge.

Human API: `GET /api/sessions/{id}/completion` returns `{report:...}` with normal human API visibility. The existing dialog polls this endpoint every three seconds, separately from inbox contents; report strings use textContent and never become HTML or submit prompts. Existing dark/mobile dialog classes are reused.

## Integration and checks

Run in the integrated checkout/worktree:

```sh
CARGO_TARGET_DIR="$PWD/target" cargo build --locked --offline
CARGO_TARGET_DIR="$PWD/target" cargo test --locked --offline
node scripts/completion-smoke.mjs
```

The smoke script uses port 4341 by default, binds 127.0.0.1, sets an explicit worktree root, and creates catalog/SQLite/demo data under a unique `.cache/completion-*` directory. It performs only deterministic demo ACP -> native stdio MCP work. It checks tool discovery, valid reporting/querying, spoof and cross-project denial, project movement, coordinator visibility, live revocation, field/type/commit/total bounds, exact retry and conflicting payloads, event/audit behavior, safe real-dialog rendering and polling with an empty inbox, server restart persistence and original retry ordering. It also launches local headless Chromium. `AGENTWORKS_COMPLETION_PORT` may override the port for an explicitly assigned isolated port. No real inference or external models are used.

The store unit test exercises version-3 upgrade with existing session state, events, grants, messages, audit records, and retained author identity after deletion. Existing unit tests also run.
