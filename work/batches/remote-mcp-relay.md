# Remote-host enrichment: MCP messaging relay

Completed implementation on `agentworks/remote-worker-enrichment-mcp-relay`,
2026-10-26. The delivery response records the resulting commit. Work stayed in
the assigned worktree. No merge, deployment or deployment-service restart occurred.
Servers/workers launched and restarted for checks were isolated test fixtures.

Existing remote support already covered ACP chat, native resume, approvals,
worktree lifecycle and server-owned inbox dispatch. Remote ACP session configuration
omitted Agentworks MCP because the executable and private bridge endpoint were
server-local. This slice supplies the worker's own native executable and loopback
bridge through an optional protocol-3 capability, then relays bounded native facade
requests over the existing SSH WebSocket.

The boundary is the existing facade bridge, rather than independent worker-native
inbox/send commands: it configures actual provider MCP clients and retains one
server-owned durable mail, grant, project, recipient and audit implementation.
Allowed operations are discovery (`tools/list`, `list_sessions`), `read_inbox`,
`send_message` and `acknowledge_message`. Discovery is intersected with live grants.
Both relay ends reject other operations. No grants or role defaults are changed.
The server destination and session identity come from an active runtime channel
binding, never a caller-selected address or actor. Controller credentials and
credentials from other links have no binding. Exit/drop/disconnect invalidate
bindings. Temporary discovery channels receive none.

Requests/replies have size, concurrency and time bounds. Invalid/truncated replies,
unavailable sockets, missing replies and transport teardown fail closed. Late
responses are discarded. No mutation is automatically retried; already admitted
operations can complete before a lost reply, so deliberate retries must preserve
`requestId`. Reconnect/native resume supplies fresh facade configuration and a
rotated runtime token. Older workers retain chat without MCP configuration.

Checks against the final implementation:

- `cargo build --locked`: passed with pinned Zig 0.15.2 on PATH.
- `cargo test --locked -p agentworks-runtime --lib`: 26 passed, including seven new
  relay tests for frame bounds, discovery filtering, destination restrictions,
  bound identities/channel lifetimes, correlation, timeout/no replay, malformed
  replies and teardown.
- `cargo test --locked -p agentworks --bin agentworks -- --test-threads=1`:
  147 passed. The initial parallel run passed 146 and hit an existing review-queue
  database-lock failure on reopen; the full serial rerun passed without code changes.
- `node scripts/remote-mcp-smoke.mjs`: passed. Real native stdio MCP through the
  demo adapter on two SSH-stand-in workers, remote directories outside server roots,
  remote/local peers, project/default-role enforcement, off access, live revocation,
  spoof/cross-project denial, send/ack/read, retries/conflicts, direct exclusion of
  coordinator methods and deliberate reconnect/native resume.
- `node scripts/communication-smoke.mjs`: passed (Local MCP regression).
- `node scripts/remote-host-smoke.mjs`: passed.
- `node scripts/remote-partition-smoke.mjs`: passed (silence, exclusive replacement,
  adapter teardown, pending kill and worker exit).
- `node scripts/remote-plumbing-smoke.mjs`: passed (queues, forks/imports,
  automations, continuations, inbox turns, disconnect and fixture restart).
- `cargo fmt --all --check`, `git diff --check`, and JavaScript syntax check: passed.

The new remote MCP smoke is also included in `npm run verify`. The full browser
verify suite was not run for this backend slice. The first plain Cargo invocation
failed because Zig was absent from PATH; the installed pinned toolchain resolved it.

Limitations: file browsing and Git views remain deferred. Coordinator, review,
restart, automation and completion MCP tools are intentionally excluded from this
messaging allowlist, even when otherwise granted. Real SSH and paid-provider MCP
execution were not exercised. The remote OS account and executables remain trusted;
loopback credentials are not isolation between processes sharing that Unix user.
Sustained-load behavior has not been validated. See `docs/REMOTE-HOSTS.md` for the
wire/security contract, deadlines and compatibility behavior.

## Rebase onto portable PTY main

Rebased the relay onto main `49d520a738468dcdbf25609efb7cd1e8e212af71`
(including `a8d6479` portable PTY/toolchain support and `49d520a` shared enrollment
PTY support). Git applied the relay commit without conflicts. Before adding this
validation note, `git range-diff 4f09f36..6cf0801 main..HEAD` showed an identical
relay patch. The delivered exact tip is recorded in the delivery response.

Verified there is no diff against the new base in `crates/agentworks-terminal/`,
`crates/agentworks/src/enrollment.rs`, `Cargo.lock`, `mise.toml`, or
`rust-toolchain.toml`. The Windows PTY implementation and toolchain pins are
preserved exactly; no Windows-specific source was edited for this relay.

Rebase validation on Linux:

- Locked build passed with pinned Zig 0.15.2 and Rust 1.98.1.
- Native remote MCP and local communication smoke tests passed again.
- All 12 terminal browser tests passed, covering local/remote PTYs, host terminals,
  persistence, reconnect, resize, queries, cleanup and isolated-fixture shutdown.
- Real SSH enrollment smoke passed, including hidden passphrase input, host-key
  confirmation, cancellation, isolated worker installation, remote demo operation
  and managed-key fixture restart.
- `cargo test --locked -p agentworks-runtime -p agentworks-terminal -p agentworks -- --test-threads=1`
  passed: 147 service tests, 26 runtime tests, 3 discovery integration tests and
  9 terminal tests (185 total), plus doc tests.
- Format and diff checks passed. The worktree is clean after amending the rebased
  relay commit with this report update. No merge, deployment or deployment-service
  restart was performed. Windows execution/cross-compilation was not rerun;
  preservation is verified by exact file equality with main, and PTY behavior was
  exercised on Linux.
