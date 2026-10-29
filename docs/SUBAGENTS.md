# Native subagent activity

Agentworks advertises ACP's optional `subagents` capability. Harnesses that
support it expose child sessions in the owning conversation's Chat/Terminal tab strip as `Sub: Name` tabs. Each tab shows the child's name, task, reported activity, streamed text,
plans, delegated instructions and tools; nested agents get their own tabs and identify their delegating parent. Child
permissions and questions use the conversation's existing approval controls and
show the child's name when the request carries its session ID.

The strip scrolls horizontally on narrow screens, and arrow keys, Home and End
navigate its tabs. Child views keep the parent approval controls and composer;
composer messages are sent to the main agent.

Background work reported through the optional [async-task extension](BACKGROUND-JOBS.md) appears in the Background jobs dropdown, including jobs owned by a child. Its individual Stop control targets only that reported job.

Children are created by the harness. Agentworks does not start, prompt, resume,
or cancel individual children. **Stop** cancels the root turn; the harness owns
cancelling its children. A child with no confirmed activity shows **Activity
unconfirmed**, including a previously running child after the root stops. An
idle child is reusable and is not treated as a permanently closed session.

## Protocol and persistence

The Rust SDK's current draft uses `subagent_update` with patch semantics:
omitted fields retain their previous values, `null` clears them, and concrete
state/capability objects replace the previous object. Some adapters use the earlier
`subagent_spawned` and `subagent_state_update` draft. The receive boundary accepts
both, while ordinary updates still pass through the SDK's
schema. The optional feature does not require an adapter to support subagents;
older adapters retain their ordinary tool-call presentation.

Canonical subagent negotiation is retained. Declaring the separate AIR `asyncTasks`
capability selects Claude’s older subagent event format, which is also supported;
the client identity remains Agentworks. The Rust Claude
harness (`claude-rust`) announces each Claude task as a current-draft child session;
see [its notes](RUST-CLAUDE-ACP.md).

Every persisted update carries `acpSessionId`; child updates also carry
`parentSessionId`. Ownership comes from explicit announcements, never tool text.
Unannounced traffic and attempts to reparent an existing child are ignored.
Known ownership is reconstructed from retained events when resuming a worker.
Tabs and transcripts rebuild from persisted events after browser reload and service restart.
The existing recent-history bounds apply; truncated history may omit earlier
names or activity, and does not establish completion.

Child configuration, slash commands, context usage and rate-limit observations
do not overwrite the parent's controls or authorize quota continuation. Tool IDs
are scoped per child in the UI. Child raw output uses the normal tool renderer;
server-side Starlark progress parsing remains limited to root tools. Meaningful
child output counts towards root inactivity tracking, with snapshots scoped by
ACP session. Native forks select root message boundaries, and carried child
activity is attributed to its originating session.

## Verification

No paid inference is needed:

```bash
mise exec -- cargo test --locked --offline --workspace
mise exec -- cargo build --locked --offline
node tests/subagents.test.mjs
node tests/usage-status.test.mjs
node node_modules/@playwright/test/cli.js test tests/subagents.spec.mjs tests/codex-permission-default.spec.mjs
```

The browser fixture owns an isolated localhost server, database and runtime.
It exercises both event drafts, nested children, colliding tool/message IDs,
parent setting isolation, child permission decisions across reload, persisted
tabs across service restart, and mobile root cancellation. Adapter checks run
the shipped capability parsers without starting a provider CLI. Live provider
inference has not been exercised by these checks.
