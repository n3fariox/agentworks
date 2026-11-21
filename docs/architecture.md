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
| `agentworks-presentation` | Sanitized Markdown and inline HTML, prose-line selection/application, transcript compaction, tool identity and bounded display summaries |
| `agentworks-scripting` | Installed hook definitions, built-in upgrades, precedence, compilation, interpreter scheduling and ephemeral hook results |
| `agentworks-starlark` | Typed interpreter APIs, capability checks, progress parsing, experimental workflow decisions, worker binary, and bundled Starlark sources |
| `agentworks-worker` | The `agentworks-worker` executable for remote hosts and the subcommand dispatch shared with the service: worker loop, ACP bridges, MCP stdio relay. Feature `goose` (default) embeds Goose |
| `agentworks-goose-acp` | Embedded Goose harness behind ACP: system prompts, exact forks, Codex subscription tokens, usage reporting |
| `agentworks-claude-acp`, `agentworks-codex-acp`, `agentworks-demo-acp` | Native ACP bridges started as `agentworks <bridge>` subprocesses |

The service depends on runtime, session, terminal, workspace, presentation,
scripting, Starlark and worker; the bridges and Goose reach it only through worker.
Runtime depends on terminal and workspace; session depends on workspace for
recorded worktree ownership. Scripting depends on presentation and Starlark;
presentation depends on neither scripting nor the server. There are no dependencies
back to the service. The runtime, session, terminal, and workspace libraries do
not accept `App` or live `Session` handles, and do not depend on Axum or SQLite.
Scripting accepts an existing SQLite connection so the application can retain its
transaction and locking boundaries. The lower-level Starlark crate also owns an
experimental workflow store.

## Presentation and scripting

`agentworks-presentation` owns display transformations of authoritative content.
Its code does not open storage, execute scripts, start threads, or access live
sessions. The `chat::LineFormatter` interface receives the selected prose lines;
the caller supplies evaluation. The library preserves code fences and line
positions, bounds tool summaries, sanitizes HTML, and removes tool bodies from
browser snapshots without modifying stored events. Its dependency graph contains
no Starlark interpreter, SQLite, Tokio or Axum.

`agentworks-scripting` owns installed hooks and their execution lifecycle. Its
registry handles source, enablement, precedence and built-in upgrades. Its host
compiles handlers and schedules progress, header, chat, inbox and GitHub hooks on
one interpreter thread. Hook evaluation and ephemeral caches belong here; HTTP
routes, live session lookup, authorization and durable effect commits do not.
The former standalone `agentworks-tool-headers` crate is absorbed into these two
components: presentation owns bounded display input/output, scripting owns hook
evaluation and reload state.

The application adapter is `crates/agentworks/src/scripting.rs`. Startup preserves
the existing sequence of schema setup, built-in seeding, automation migration and
hook reload. The adapter binds badge effects to live sessions and commits them
through the existing storage path. `App::scripts` and `Session::scripts` identify
the shared scripting host rather than suggesting it only handles tool progress.
Markdown and tool-body HTTP endpoints remain in the server. SQL schema versions,
HTTP payloads, stored events and script registration formats are unchanged.

Bazel progress fixtures are compile inputs of the scripting **unit-test target**,
not the production library or server. Presentation tests can run without linking
the interpreter or the server. Cargo and Bazel test commands are listed in
[development and verification](TESTING.md#rust-library-loops).

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
`BAZEL_PROGRESS_SCRIPT`. Scripting owns installation records, reloads and the
interpreter thread. The service feeds persisted events into the host and sends
its derived progress/header events to browser subscribers.

## Assets and runtime files

`web/`, `scripts/`, `tests/`, and `vendor/` stay at the repository root. Embedded
assets resolve from the application's new source directory. The default runtime
root and catalog location still resolve to the repository root;
`AGENTWORKS_RUNTIME_DIR` and `AGENTWORKS_CATALOG_PATH` retain their existing
overrides. `agentworks agent --stdio` remains the remote worker entry point.

## Further extraction

Further boundaries should follow ownership rather than individual helper files:

- Persistence and recovery: event storage, transcript queries, migrations,
  retention and restore. Keep cross-feature transactions explicit before moving
  the shared database facade.
- Coordination: attributed messages, tasks, completions, controller relationships
  and their permissions.
- Automation: schedules, trigger evaluation, execution ledgers and action requests;
  application adapters perform authorized session operations.
- GitHub: credentials, API queries, polling and normalized observations; the
  application joins those observations to live sessions and automation actions.
- Host administration and access: enrollment/service management and browser
  authentication/trust, with transport adapters remaining separate from policy.

Establish internal module/API boundaries before adding more crate targets. The
live `Session` handle remains in application coordination: it owns locks,
persistence, approvals and worker handles rather than just serialized model data.
