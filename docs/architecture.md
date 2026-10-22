# Rust workspace

The root is a virtual Cargo workspace. `cargo build`, `cargo run`, and
`npm start` still select the `agentworks` service by default, and binaries stay
in the root `target/` directory. Use `cargo test --workspace` to test every crate.
The pinned Rust and Zig toolchains are selected by `mise exec -- cargo …` or
`scripts/mise-cargo.sh`.

| Crate | Owns |
| --- | --- |
| `agentworks` | Executable, live application/session handles, HTTP and WebSocket handlers, MCP, persistence, authorization, and scheduling |
| `agentworks-runtime` | Provider setup and executable discovery, ACP negotiation/model/history discovery, SSH supervision and transport, remote worker execution |
| `agentworks-session` | Serialized session payloads and identities, input queue transitions, continuation decisions, timeout policies and activity clocks |
| `agentworks-terminal` | PTY processes and cleanup, Ghostty emulation, presentation frames, incremental history, and terminal input |
| `agentworks-workspace` | Bounded Git execution and inspection, comparisons, worktree policies, preparation, ownership verification, dirty fingerprints, removal, and restoration |
| `agentworks-starlark` | Typed interpreter APIs, progress parsing, message formatting, experimental workflow decisions, worker binary, and bundled Starlark sources |

The service depends on all five libraries. Runtime depends on terminal and
workspace; session depends on workspace for recorded worktree ownership. There
are no dependencies back to the service. The runtime, session, terminal, and
workspace libraries do not accept `App` or live `Session` handles, and do not
depend on Axum or SQLite. Starlark is separate: its experimental workflow engine
owns a SQLite-backed store, while the application owns parser installation and
event delivery.

## Application adapters

The terminal registry, saved-history persistence, session archive checks, remote
terminal selection, and browser WebSocket protocol remain in the application.
The terminal library exports handles, frames, inputs, and local PTY spawning.

`agentworks-runtime` owns provider environment configuration and the stateless
SSH worker. ACP discovery accepts a transport selected by the application; the
application keeps its remote-process guard alive for the entire call. The
prompt loop remains in `crates/agentworks/src/session_runtime.rs`, where it
coordinates approvals, session persistence, inbox ownership, and scheduling.
Remote host configuration is defined by runtime and reexported by the catalog;
catalog validation and persistence remain in the application.

`agentworks-session::SessionData` preserves the existing serialized field names,
defaults, and skipped transient fields. Launch origins, inbox submission tokens,
fork lineage, and permission defaults have a single definition shared by the
application and persistence. Queue and continuation transitions operate on those
models; application adapters hold locks, verify authorization, persist before
acknowledging, and choose when to dispatch. Transient continuation evidence stays
private to its state machine. The timeout activity registry is shared through
the session library, preserving per-turn guard cleanup.

Workspace operations use `worktrees::Context` to obtain permitted roots,
validate paths, reserve mutations durably, and sanitize host diagnostics. The
service and SSH worker implement that interface. Session activity checks,
shared-worktree checks, locks, authorization, and metadata persistence remain
application responsibilities. Filesystem ownership and confirmation checks
remain in the library and run again immediately before removal.

Workspace and session errors retain the existing `(http::StatusCode, String)`
classification so this extraction preserves API responses. They use `http`, not Axum;
a future typed domain error can replace this separately. The existing SHA-256
implementation lives with workspace fingerprints and is also reused by the
application for credential digests.

The Starlark crate exposes its bundled Bazel parser as
`BAZEL_PROGRESS_SCRIPT`. The service owns parser installation records, reloads,
the parser thread, and publishing derived progress events.

## Assets and runtime files

`web/`, `scripts/`, `tests/`, and `vendor/` stay at the repository root. Embedded
assets resolve from the application's new source directory. The default runtime
root and catalog location still resolve to the repository root;
`AGENTWORKS_RUNTIME_DIR` and `AGENTWORKS_CATALOG_PATH` retain their existing
overrides. `agentworks agent --stdio` remains the remote worker entry point.

## Further extraction

The six-crate split is established. Further work can organize application feature
modules, separate HTTP routing/assets from startup, and reduce `main.rs` without
adding crates. Storage and coordination remain together until their transaction
boundaries are explicit. The live `Session` handle stays with that coordination;
it owns locks, persistence, approvals, and worker handles rather than model data
alone.
