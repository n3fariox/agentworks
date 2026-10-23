# Experimental Rust Codex ACP bridge

Enable with `AGENTWORKS_CODEX_ADAPTER=rust` in the environment of the Agentworks
service or worker. The existing JavaScript adapter remains the default while this
implementation is evaluated. Unset the variable to return to it.

The same Agentworks executable runs `agentworks codex-acp`, which starts the native
`codex app-server` and translates its JSON-RPC interface into ACP v1 using the ACP
Rust SDK. Set `CODEX_PATH` to a native Codex executable, or put it on PATH. No npm
package or Node interpreter is used for this execution path. Existing sign-in and
explicit API-key isolation follow the same provider launch configuration as before.
Native Codex must be installed separately; pointing CODEX_PATH at an npm JavaScript
launcher still requires Node.

For a standalone ACP executable:

```sh
cargo build -p agentworks-runtime --bin agentworks-codex-acp
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

The host's sandbox and approval settings remain in effect until a mode is explicitly
selected. The host preset restores the policy captured when this adapter opened
the session. Subscription-window push notifications use the same timestamped
`_codex/subscriptionLimits` metadata contract as the vendored adapter: sparse
updates retain older field times and attach only to genuine session usage events.
No account polling or quota-only events are added.

This is intentionally smaller than the upstream adapter. Slash commands, native
subagent sessions, other optional AIR extensions, and automatic app-server crash
recovery remain unimplemented. Fork boundaries use stable native item IDs (including
Agentworks' visible segment suffix); unknown boundaries fail rather than inheriting
newer history. Fingerprint-only boundary recovery is not implemented.

The switch is local to each execution host. Remote enrollment still installs the
existing npm dependencies; this experiment does not yet make enrollment or Claude
execution independent of Node.

Validation (no Node or inference needed):

```sh
cargo test -p agentworks-runtime --test codex_acp
python3 tests/codex-acp.test.py -v
```

The Python test uses a fake app-server and the built standalone binary. It checks
completion before the start response, multiple turns, approval/question round trips,
pending approval withdrawal on cancellation, legacy and multi-page history replay,
exact fork boundaries, workspace roots, confirmed/rejected settings, close/delete,
sparse quota observations, native quota errors, disconnect, and shutdown.
The Rust integration test uses the
same typed ACP client as Agentworks, so schema compatibility is checked too.

The installed native Codex also passed initialization/model discovery, a live
text turns, permission/reasoning/planning/fast-mode settings, close/resume with
paginated history, fork at a recorded message boundary, and deletion of disposable
test threads. Live approvals, images, MCP execution, and remote execution have not
yet been validated. The existing JavaScript adapter remains the default.
