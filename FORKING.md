# Forking a session (L-08)

**Fork session** in Session actions or the command palette opens the normal creation
form with the parent's title plus `(fork)`, harness, current model, project and source
directory. Choose the latest completed turn at submission or a retained earlier
completed-turn boundary. A running parent continues uninterrupted. A session without
a completed turn cannot be forked; cancelled, timed-out and failed turns are excluded.

Host, harness/model and workspace are independent choices. Forks default to the parent’s host. The form suggests a new
worktree at the source directory's current Git commit; you can reuse the directory
or select an existing worktree under ordinary project policy. A conversation boundary
is not a historical filesystem snapshot. New worktrees contain committed files only;
uncommitted edits remain in the source. No merge or cleanup is implicit.

## Context methods

- **Auto** prefers native Codex ACP `session/fork` when the adapter advertises it and
  the boundary has a stable assistant message ID. The tracked adapter accepts an
  anchored fork through its `jetbrains.air.fork` v1 metadata contract. The child sends
  its own MCP configuration. An anchored request can snapshot a running parent safely.
- **Transcript** creates a fresh provider session and sends the recorded conversation
  as a separate background block with the first user message. It does not reproduce
  hidden provider state. Auto uses this method when a safe native anchor is unavailable,
  including cross-host/cross-harness forks and current Claude/OpenCode integrations. Native provider IDs are never sent to a different host or provider.
- **Summary** creates a fresh provider session with the user's supplied summary as
  background with the first message. No model is called to generate a summary.

The child's transcript preserves recorded conversation and identifies the source;
provider startup emits the actual native/fresh method. It does not execute an inherited
assignment automatically. User messages remain verbatim; handoff context is separate.
No pending approvals, queued inputs, automation ownership, continuation timers, or
Agentworks grants are inherited. Grants and quota policy can be chosen explicitly in
the creation form under normal checks; coordinator rights are never automatic.

Original directory provenance is attached to carried events. Relative Markdown file
links in inherited replies continue resolving against that original directory, even
across successive forks. Absolute paths keep their meaning and ordinary file-root
checks apply. The handoff explains source and destination directories and warns against
silently retargeting paths. Files are not copied or rewritten.

## Persistence and retries

`POST /api/sessions/{id}/fork` accepts the usual creation fields plus:

- `requestId`: required, stable, 1–100 bytes without control characters.
- `boundarySeq`: optional completed `turn_end` event sequence; omission selects latest.
- `context`: `auto` (default), `transcript`, or `summary`.
- `summary`: required, nonempty and at most 64 KB for summary context.

Defaults are evaluated once for a successful creation. Retry identity binds the raw
request and parent ID, before re-reading parent history or model defaults. An identical
retry returns the durable child, even after parent advancement, deletion or restart;
a conflicting payload is rejected. Child state, retained transcript, lineage and
creation result commit in one SQLite transaction. Existing worktree preparation
reservations prevent blindly repeating an interrupted Git mutation.

Parent ID, title/provider/host, source directory, boundary sequence/count, creation time,
context method and native outcome survive restart and parent deletion. Creating a
child never mutates the parent. Parent history is copied under its short state lock.

Native creation is durably armed before provider dispatch. A failed/ambiguous attempt
is not silently substituted or automatically forked again: inspect provider history
and explicitly create a fresh fork if needed. Successful native IDs are saved before
normal startup so reconnect resumes the child. The provider remains responsible for
its native state's semantics; Agentworks does not claim provider-internal equivalence.

Context delivery is also claimed durably before the first prompt is dispatched.
An interrupted or failed first send may have consumed none or all of the context;
Agentworks preserves history and shows the claim, but does not replay it automatically.
Inspect the child/provider history and supply context again explicitly if necessary.

## Limits and L-06 handoff

Fork selection uses retained Agentworks history (up to 3,000 events / 4 MB). Older
boundaries outside retention are rejected; partial retained history is marked in
lineage and the fresh-session notice. Text sent to a fresh provider is bounded to
192 KB plus framing, with individual event text/tool output clipped to 16 KB. Clipping
is marked. Carried state/history remains subject to normal retention. Tool context is
recorded data, never replayed operations. Attachments and arbitrary tool-specific file
syntax are not remapped; the original directory is provided for interpretation.

Native support currently targets the tracked Codex adapter's explicit message anchor.
Other harnesses, missing anchors, and cross-harness handoffs use the explicit fresh
methods. Live authenticated providers were not exercised; tests use deterministic ACP.

L-06 can reuse `fork::seed`, the durable carried-context block and dispatch claim for
fresh-context handoff. `context=transcript|summary` already provides the explicit reset
contract across harnesses, preserving the source if startup fails. This change does
not implement L-06's separate successor/switch workflow or automatically retire a parent.

## Recovery and verification

Recovered the uncommitted implementation from OpenCode session
`e19550ab-1f3a-4590-9743-cd01f24d87a2` (`Session Forking`), whose workspace is
`session-70f6189c-7eb7-4e92-a356-e34312d338f8`. Reviewed its session history, branch
and diff, then integrated its L-08 code using a three-way merge onto main `a3c90d8`.
The original workspace was left untouched. Added earlier boundaries, safe native
anchoring, summary choice, atomic insertion, stable retry binding and path provenance.

Checks: `cargo test --locked --offline -- --test-threads=1`, `cargo build --locked --offline`,
`node scripts/fork-smoke.mjs`, and `playwright test tests/fork.spec.mjs`.
Fixtures own temporary repositories, databases and loopback servers; no provider calls
or live-service restart/deployment are involved. The 11 desktop/mobile browser checks
and fork smoke passed, as did the existing worktree-policy and persistence smoke suites.

The default parallel Rust run showed an intermittent existing SQLite lock-reopen test
failure while subprocess tests were running; the full serial run passed all 78 tests.
