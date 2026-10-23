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
resume, text/tool history replay, session listing, model and reasoning selection,
text/image/context input, stdio and HTTP MCP configuration, streamed messages,
reasoning, tool activity, plans, context usage, approvals, form questions, and
cancellation. Codex owns the actual agent loop, persistence, policy enforcement,
and interruption. Native errors retain `codexErrorInfo` for limit-resume handling.
App-server termination ends the ACP connection; no model turn is automatically
retried. ACP disconnect kills and reaps the app-server child. Existing Agentworks
process-group supervision covers provider descendants.

This is intentionally smaller than the upstream adapter. It does not advertise
native fork, extra workspace directories, slash commands, or optional AIR
extensions. It keeps the host's sandbox/approval configuration; changing permission
modes from chat is not implemented. Subscription-window metadata from the vendored
JavaScript fork is not yet emitted. History replay uses `thread/read` with full
turns, rather than the newer paginated history API. Long or paginated histories
need further validation before making this the default.

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
pending approval withdrawal on cancellation, history replay, model configuration,
native quota errors, disconnect, and shutdown. The Rust integration test uses the
same typed ACP client as Agentworks, so schema compatibility is checked too.

The installed native Codex also passed initialization/model discovery, a live
`RUST_ACP_OK` text turn, and native resume with history replay. Live approvals,
images, MCP execution, and remote execution have not yet been validated.
