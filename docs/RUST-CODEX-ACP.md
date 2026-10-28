# Experimental Rust Codex ACP bridge

Select **Codex · Rust ACP** in the harness picker (`provider: "codex-rust"` in the
API). It is a separate harness alongside **Codex**, which continues to use the
JavaScript adapter. The former `AGENTWORKS_CODEX_ADAPTER` switch has been removed.
Each saved session retains its adapter identity across restart/resume. Both harnesses
share Codex authentication, API-key storage, and account quota reporting, while model
defaults are independent. Import registration treats native thread IDs as belonging
to the same Codex provider to avoid opening duplicate writers through both adapters.

The same Agentworks executable runs `agentworks codex-acp`, which starts the native
`codex app-server` and translates its JSON-RPC interface into ACP v1 using the ACP
Rust SDK. Set `CODEX_PATH` to a native Codex executable, or put it on PATH. No npm
package or Node interpreter is used for this execution path. Existing sign-in and
explicit API-key isolation follow the same provider launch configuration as before.
Native Codex must be installed separately; pointing CODEX_PATH at an npm JavaScript
launcher still requires Node.

The bridge lives in the `agentworks-codex-acp` workspace crate, which provides both
the library entry point and a standalone ACP executable. It builds independently
of the Agentworks runtime and terminal:

```sh
cargo build -p agentworks-codex-acp
./target/debug/agentworks-codex-acp
```

The bridge handles initialization, API-key authentication, session creation and
resume, paginated and legacy text/tool history replay, session listing, native fork
at a recorded message boundary, extra workspace directories, graceful close,
explicit provider-history deletion, model and reasoning selection,
text/image/context input, stdio and HTTP MCP configuration, streamed messages,
reasoning, tool activity, plans, context usage, approvals, form questions, and
cancellation. Settings include permission presets, planning mode, and fast mode
(boolean when negotiated, on/off choices otherwise). Changes go through native
`thread/settings/update`; a rejected change does not update the bridge's settings.
Codex owns the actual agent loop, persistence, policy enforcement,
and interruption. Native errors retain `codexErrorInfo` for limit-resume handling.
App-server termination ends the ACP connection; no model turn is automatically
retried. ACP disconnect kills and reaps the app-server child. Existing Agentworks
process-group supervision covers provider descendants.

Closing unsubscribes from the live thread without removing its history. Explicit
deletion uses native `thread/delete`, which permanently deletes the selected
thread and spawned descendants. Failed operations retain local session state;
neither closing nor deletion resumes or prompts the thread. Active turns must be
cancelled before either operation. These methods match the ACP State client's
new lifecycle interfaces without requiring its changes to be copied into this
branch.

Agentworks passes a captured default or saved permission mode in
`_meta.agentworks.permissionMode` when opening a session, after negotiating
`_meta.agentworks.permissionModeOnOpen` in the initialize response. The bridge
sets native sandbox and approval overrides on `thread/start`, `thread/resume`,
and `thread/fork`, before native initialization can invoke filesystem helpers.
The usual settings update still confirms the mode before readiness or a prompt.
Saved modes take precedence over defaults; imports and old saved rows without a
captured mode retain the native policy. With no explicit mode (or `host`), the
host's sandbox and approval settings remain in effect. The host preset restores
the policy captured at thread opening.

Model discovery negotiates `_meta.agentworks.modelDiscovery`, then calls
`_agentworks/models/list`, which maps directly to native `model/list`. It never
creates a native thread, starts MCP servers, or executes a model turn. Clients
using older adapters without this capability retain temporary-session discovery;
errors from the advertised extension propagate without retrying through a thread.
The same ACP negotiation and metadata work through remote worker transports;
update the worker executable to use these capabilities on another host.

Subscription-window push notifications use the same timestamped
`_codex/subscriptionLimits` metadata contract as the vendored adapter: sparse
updates retain older field times and attach only to genuine session usage events.
No account polling or quota-only events are added.

The bridge advertises `/compact`, `/review`, `/review-branch`, `/review-commit`,
`/plan`, `/model`, `/permissions`, `/fast`, `/status`, `/skills`, `/mcp`, and `/rename`.
Commands call native operations and report their results through ACP. Reviews and
compaction use the normal turn/event/cancellation path. `/model` and `/permissions`
list choices without arguments and select an advertised value when given one.
`/fast` toggles, or accepts `on`/`off`; the option is available only for models
advertising fast service. `/status` displays cached settings and last reported usage
without starting inference or querying account services. `/mcp` lists server status;
reconnect and authentication commands are not implemented. `/rename` changes the
native conversation name; the Agentworks display title is managed separately.
Unknown slash commands and unsupported arguments fail instead of becoming model
instructions. Send slash commands separately from attachments or carried context.
Native `$skill` invocations remain normal prompts and run in the Codex harness.
Review reports also arrive through native review-mode items. The adapter preserves
the review API's owning turn ID when Codex emits an internal turn-start ID, and
waits for the owning turn's completion before accepting the next prompt.

Native subagents use ACP's negotiated `subagents` extension. The bridge announces
children before forwarding their text, reasoning, tools, plans, usage, and approval
requests under the child's session ID. It handles both `subAgentActivity` and older
`collabAgentToolCall` records, including nested ownership and state updates. Loading
a parent replays its recorded child tree using read-only history requests; missing
or deleted child history does not prevent the parent from loading. Child mutations
are not advertised: send instructions and cancellation through the parent, leaving
execution to Codex. Clients without the extension receive ordinary collaboration
tool calls and child approvals on the root session instead of child traffic.

The optional AIR `asyncTasks` extension exposes Codex background terminals in the
shared [Background jobs](BACKGROUND-JOBS.md) dropdown. It is negotiated only when
the client advertises versioned `asyncTasks` support. A native command's process ID
alone does not announce a background job: the bridge confirms it against paginated
`thread/backgroundTerminals/list` snapshots. Discovery runs on session load and
relevant command/turn events, without a polling timer or model prompt. Child jobs
retain their child session and use thread-qualified task IDs; clients without
subagent support receive those jobs on the root with distinct IDs.

Command completion publishes the job's terminal outcome, including nonzero exits.
A complete list snapshot can resolve a disappeared job as stopped; malformed,
incomplete, or repeated-cursor responses do not alter existing jobs. Per-thread
reads are serialized, completion/stop tombstones reject stale list entries, and
session generations fence responses from before a reload or close. Loading a
session rechecks its live terminals after announcing its recorded child tree.

`_session/async_task/stop` takes the root ACP session and the reported task ID. It
uses the stored native thread/process IDs with
`thread/backgroundTerminals/terminate`, without interrupting a model turn. Unknown,
foreign-session, completed, or already-stopping jobs return `stopped: false`.
Rejected/failed stops leave the observation live. If native completion wins the
race with termination, its outcome is preserved. Older Codex builds that reject
the background-list method keep ordinary ACP functionality. No new background
notifications or native background-API calls are made without client negotiation.

This is intentionally smaller than the upstream adapter. Other optional AIR
extensions and automatic app-server crash recovery remain unimplemented.
Fork boundaries use stable native item IDs (including
Agentworks' visible segment suffix); unknown boundaries fail rather than inheriting
newer history. Fingerprint-only boundary recovery is not implemented.

Workers advertise `codex-rust` independently when native Codex is available, even
without Node. Older workers that do not advertise it cannot start this harness.
Remote enrollment still installs the existing npm dependencies; this experiment
does not yet make enrollment or Claude execution independent of Node.

Validation (no Node or inference needed):

```sh
cargo test -p agentworks-codex-acp
python3 tests/codex-acp.test.py -v
cargo build -p agentworks
python3 tests/codex-rust-harness.test.py -v
```

The Python test uses a fake app-server and the built standalone binary. It checks
completion before the start response, multiple turns, approval/question round trips,
pending approval withdrawal on cancellation, legacy and multi-page history replay,
exact fork boundaries, workspace roots, confirmed/rejected settings, close/delete,
sparse quota observations, native quota errors, slash-command routing, compaction
completion/cancellation, native review event ordering/IDs, disconnect, and shutdown.
The application smoke test checks selectable harness registration, discovery,
model defaults, automation definitions, attachment capabilities, command
execution, and persistence/resume with a deliberately unavailable Node interpreter.
The Rust integration test uses the
same typed ACP client as Agentworks, so schema compatibility is checked too.
Background-job fixtures cover discovery, child routing with and without subagent
support, paging, late completion, list/stop races, reload fencing, failed stops,
malformed snapshots, and unsupported native methods. The application smoke also
checks Rust Codex job tracking and individual child-job termination through the
shared Agentworks API. These cases use the fake app-server; live native background
terminal behavior has not yet been exercised.

The installed native Codex also passed initialization/model discovery, live
text turns, permission/reasoning/planning/fast-mode settings, close/resume with
paginated history, fork at a recorded message boundary, and deletion of disposable
test threads. Slash-command validation includes status, model listing, permission
and planning changes, skill and MCP status listing, rename, and compaction.
Native custom review also passed with a displayed report and an ordinary prompt
immediately afterward.
Live approvals, images, MCP execution, and remote execution have not
yet been validated. The existing JavaScript adapter remains separately available.
