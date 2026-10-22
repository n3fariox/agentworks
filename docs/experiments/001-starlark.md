# E-01 — Embedded Starlark for progress and workflows

**Outcome:** Adopted for U-05 typed progress parsers. The built-in Bazel parser and
user-installed parser API run through the Starlark engine with type checking
enabled. Workflow interpretation remains a prototype and is not the production
automation runner. See [tool activity](../TOOL-ACTIVITY.md).

Status: **adopted and deployed for U-05 progress parsing**. The service now runs the Bazel parser as the built-in Starlark script; see [Production integration](#production-integration-october-21-2026). The October 20 evidence below recommended "adjust". On review, the user chose to adopt it for parsers: scripts are trusted on this single-user host (a bad script only affects the user's own service), and scripts are stored in the database. Workflow scripts remain prototype-only, gated on a concrete workflow need.

## Question

Can `starlark-rust` provide one practical extension language for tool-output parsers and workflow decisions while Rust retains control of execution, scheduling, authorization and durable state?

Evaluate the [canonical implementation](https://github.com/facebook/starlark-rust) and its [evaluator resource controls](https://docs.rs/starlark/latest/starlark/eval/struct.Evaluator.html). The implementation documents best-effort heap limits; assess a supervised interpreter worker process when firm CPU/memory bounds are needed. Do not treat language restrictions as an OS sandbox.

## Prototype scope

1. **Bazel progress parser.** Feed normalized tool metadata and output lines into a Starlark callback with explicit previous state. Return serializable state and structured progress: phase, label, completed/total counts and units. Rust handles chunking, snapshots versus deltas, ANSI/carriage returns and host-provided pattern matching. Use recorded streams, dynamic totals, malformed output and failure fixtures. Verify actual streaming availability separately per harness; retain elapsed/raw-output fallback when live data is absent.
2. **Conditional two-step workflow.** Use demo agents or deterministic fake effects: run a first step, then choose whether to run the second based on its result. A callback receives durable workflow state plus an event and returns new state plus proposed actions. Rust persists decisions/pending actions and dispatches them with stable request IDs. Exercise success, failure, cancellation and restart without paid inference.
3. **Shared embedding, distinct capabilities.** Reuse the interpreter library and versioned data conventions. Give parsers only interpretation helpers; give workflow evaluation action constructors that return data. Rust validates and authorizes every dispatched action using the existing project/workspace/grant rules. Parser code cannot launch agents or acquire workflow privileges. Scripts are explicitly installed/enabled; generated repository content is not implicitly trusted.

## State and execution contract

- Keep each evaluation short; Rust waits for agents, timers and human decisions, then invokes a callback for the next event. Do not keep a Starlark call stack alive across a workflow's lifetime.
- Persist JSON-compatible state, input/event IDs, pending action IDs, host API version and script identity/content hash. Pin a script version for each running workflow; define an explicit upgrade path rather than silently reevaluating old state against edited code.
- Cache parsed/frozen modules where supported. Keep mutable state scoped to one parser call stream or workflow run, and avoid retaining an ever-growing interpreter heap.
- Keep interpreter execution off async request threads. Bound input, state, output and evaluation resources; investigate hard-stop behavior for runaway code and blocking host helpers. Parser failure must leave agent execution and raw logs usable.
- Preserve raw events. Derived progress can be rebuilt from replay, but workflow replay must not repeat committed effects. Rust owns cancellation, retries, concurrency and approval waits; evaluation proposes decisions.

## Acceptance and decision evidence

- Bazel fixtures yield expected phase/count updates across split lines, replacement snapshots, deltas, duplicate events, changing totals, ANSI/carriage returns and command failures. No fabricated percentage when a total is unavailable.
- Restart between workflow steps restores state and continues the intended next step. Duplicate completion events and crashes between decision persistence and action dispatch do not create duplicate sessions/turns. Interrupted/cancelled work is not blindly retried.
- Capability denial and grant revocation remain effective at dispatch. Invalid return values, script exceptions and excessive resource use produce attributable diagnostics without taking down the main service.
- Record dependency growth, build time/binary impact, callback latency, memory growth over a long output stream, worker-process overhead and script-authoring/test ergonomics. Compare at least with a built-in Rust parser plus configuration rules; compare the local-helper alternative if needed.
- Deliver sample scripts, deterministic fixtures/checks, a minimal versioned host API proposal and a written adopt/adjust/reject decision. Explain what remains prototype-only and whether this justifies replacing the initially proposed parser-helper approach.

## Boundaries and sequencing

This experiment informs U-05 and a future workflow engine. It does not implement a general workflow editor, production scheduling, GitHub triggers, automatic inbox wake-ups, merge privileges or a plugin marketplace. It does not authorize a deployment or resume the stopped feature batches. Keep changes isolated and review the evidence before making the interpreter a production dependency.

## Results (October 20, 2026)

### What was built

A separate Cargo workspace member, `agentworks-starlark` (`crates/agentworks-starlark`). Plain `cargo build`/`cargo run` at the root still build only the service (it is the only default member), and the service does not link the interpreter (`cargo tree -p agentworks -i starlark` finds nothing). `npm run test:starlark` runs its tests and `npm run verify` now includes them. The production U-05 browser parser is unchanged.

- `engine.rs`: shared embedding of `starlark = "=0.14.2"`, with one dialect, per-capability globals, host-owned typed preludes, compile caching keyed by content hash, a fresh heap per call, limits, and panic capture. Every failure becomes a `Diagnostic` naming the script, its hash and a kind (`install`, `syntax`, `type`, `capability`, `runtime`, `limit`, `invalid_result`, `panic` or `worker`).
- `host/progress.star` and `host/workflow.star`: versioned host API preludes. Parsers can load only the progress prelude and get one native helper, `regex_match`, which uses the linear-time `regex` crate with a bounded pattern cache. Workflows can load only the workflow prelude, whose constructors return data. Neither capability has `print`, `json`, file or network access. A parser cannot load action constructors, and a workflow cannot use the pattern helper; both are tested.
- `progress.rs`: Rust-owned stream handling, ported from the U-05 browser implementation. It covers snapshots versus deltas, sequence deduplication, ANSI/OSC/CR/LF handling, 16 KiB line bounds, provisional parsing of the unterminated line, and recognizing Bazel commands. It also includes a `BuiltinBazel` Rust baseline with the same rules. The `LineParser` trait lets tests run every case against both parsers.
- `workflow.rs`: a SQLite-backed host. Each workflow run is pinned to a script hash and host API version. Every event is deduplicated by a stable ID. A decision, its state and its pending actions are committed in one transaction. Dispatch re-checks authorization through an `Authorizer` trait and uses stable request IDs (`wf:<workflow>:<key>`) with an idempotent `Dispatcher`. Cancellation, explicit `upgrade`, and replay without dispatch are all supported.
- `worker.rs` and the `agentworks-starlark-worker` binary: a supervised interpreter worker speaking JSON lines. It applies `RLIMIT_AS`, `RLIMIT_CPU` and `RLIMIT_CORE=0`, sets `PR_SET_DUMPABLE=0`, and is killed on a wall-clock deadline. It respawns lazily.
- Sample scripts: [`scripts/bazel_progress.star`](../../crates/agentworks-starlark/scripts/bazel_progress.star) and [`scripts/check_then_fix.star`](../../crates/agentworks-starlark/scripts/check_then_fix.star). [`examples/measure.rs`](../../crates/agentworks-starlark/examples/measure.rs) produces the numbers below.

### Type hints and type checking

starlark-rust's canonical typing support is enabled at every level it offers:

1. `Dialect { enable_types: DialectTypes::Enable, .. }` turns on runtime checking of annotations on every call, including record field types.
2. `AstModuleTypecheck::typecheck` lints the whole module at install time against the capability's globals and prelude interface.
3. `Evaluator::enable_static_typechecking(true)` makes compiling the module typecheck every `def`, called or not, using real values.

The host also requires the compiled entry point's type to equal the host API signature exactly: `def(ctx: LineContext, line: str) -> None | Progress` or `def(ctx: WorkflowContext, event: Event) -> list[Action]`. Unannotated or mistyped entry points are rejected at install.

These errors are caught at install time and tested: misspelled record fields, returning the wrong record type, wrong record field types, wrong argument types to host helpers, a mistyped helper that is never called, unknown globals, and a mismatched or missing entry signature. Values that arrive from JSON (`typing.Any`) are checked at runtime when they reach a typed record field.

Limitations of the library, recorded rather than worked around:

- The standalone `typecheck` does not evaluate, so it treats annotations that name loaded `record` types as `typing.Any`. The compile-time checker sees the real types, so the entry contract is checked against the compiled value. Giving the standalone checker a value-derived prelude `Interface` did not change how it resolves annotations.
- Starlark has no `TypedDict`. Script-owned `ctx.state` is therefore `dict[str, typing.Any]`, and Rust validates it as JSON with size bounds. Scripts can use records internally. See [Parser state model](#parser-state-model) for why it can't be a record.
- Static checking is gradual: some errors, such as `str + int` inside a body, surface only at runtime as attributable `runtime` diagnostics.
- The checker validates attribute names when they are read (`ctx.stat`), but not on the left of an assignment (`ctx.stat["k"] = v`). That typo is reported only when the script runs, as a parser failure for that tool.

### Parser state model

Both hooks have the same shape: `on_line(ctx: LineContext, line: str) -> Progress | None` and `on_event(ctx: WorkflowContext, event: Event) -> list[Action]`. `ctx` carries read-only host facts plus the script-owned `state` dict, which the script mutates in place, and the return value is the hook's output. Workflow state is durable: Rust commits it with each decision. Parser state is the script's own working memory, mutated in place through `ctx.state`. It is temporary: Rust keeps it in memory per output stream, never persists it, and rebuilds it after a reload by replaying the raw output. Rust copies it in as a fresh mutable dict before each call. For committed lines it copies the dict back out and enforces the 16 KiB limit. For the provisional unterminated line it discards the changes. Multi-line values, such as a target named on one line and counted on a later one, are ordinary keys. A failed call leaves the previous state untouched.

Keeping state as frozen Starlark values between calls, so that it never leaves the interpreter, was tried and rejected. `examples/probe_chain.rs` measures it. Using the previous state inside a call requires registering its frozen heap on the call's heap, so each frozen state keeps every predecessor alive. RSS grew by about 0.75 KiB per line (+225 MB after 300,000 lines), whether or not the script reused old values, and dropping the chain overflowed the stack. starlark-rust 0.14 tracks heap references per heap, keeps mutable heaps confined to a closure (`Module::with_heap` is crate-private), and makes record types private (`RecordGen::typ` is `pub(crate)`). So a safe, bounded carry-over needs a copy, and copying typed records generically is not possible through the public API. State is therefore plain data (str, int, float, bool, None, list, dict), typed `dict[str, typing.Any]` and checked at runtime. Host-provided records such as `Counts` and `Progress` remain fully typed. Typed record state would need either an upstream accessor for a record's type or an unsafe long-lived heap with manual garbage collection.

### Acceptance evidence

All 29 experiment tests pass under `cargo test --locked -p agentworks-starlark`. The service's 114 Rust tests and the 12 U-05 node tests are unchanged and pass.

| Requirement | Evidence (test) |
| --- | --- |
| Split lines, snapshots, deltas, duplicates, changing totals, ANSI/OSC/CR, failure | `parser.rs`: the U-05 fixture `tests/fixtures/bazel-events.json` gives the same views as U-05 at every checkpoint, and the Starlark and built-in snapshots are identical. Every chunk boundary of an ANSI/OSC/CR stream gives identical progress and byte-exact raw output. |
| No fabricated percentage when the total is unavailable | A partial `[1 / 2` shows no progress. Invalid, unsafe and success-only lines produce no counts. There is no percentage field anywhere in the API. |
| Output before the command is known | Output retained before the `tool_call` names the command is re-parsed once Bazel is recognized. Writing the Rust port found this case: the line `Load` + `ing:` would otherwise have been misread. |
| Multi-line state, provisional lines | `state_spans_lines_and_provisional_changes_are_discarded`: a target named on one line labels later progress. Changes made for the unterminated line are visible in its provisional progress but are not kept, so the line count stays correct as the line completes. State that is too large or not plain data, such as a function, is an attributable `limit` or `invalid_result` failure. |
| Parser failure isolation | Exceptions, tick/wall limits, invalid results (`done > total`) and oversized results each set an attributable `parser_failure` and remove derived progress for that tool only. The view falls back to "In progress"/elapsed. Raw output, later events, tool status and exit code remain exact. No further calls are made to the failed parser. |
| Restart between steps continues the intended next step | `failure_runs_the_second_step_across_a_restart`: the service crashes after persisting the "fix" decision and before dispatch. After reopening, "fix" is dispatched exactly once with the failure summary. |
| No duplicate sessions on duplicate completion or crash around dispatch | Duplicate start and completion events are `Duplicate`. Re-proposing an existing key is a single effect. If the effect commits but the service dies before marking it dispatched, the retried dispatch is deduplicated by request ID and one session exists. |
| Interrupted or cancelled work is not blindly retried | Cancelling before dispatch leaves the action `cancelled` across a restart. A check reported as `interrupted` ends the run with "stopped" and dispatches nothing more. |
| Denial and revocation effective at dispatch | Revoking between decision and dispatch gives `denied`, and the script is told and finishes with "denied". A project that was never granted is denied without creating a session. |
| Script version pinning and upgrade | An edited script is refused for a run pinned to the old hash (`VersionMismatch`). An explicit `upgrade` re-pins it and records a host event. |
| Deterministic replay | Replay re-evaluates persisted applied events, reproduces the stored state, proposals and outcome, is repeatable, and dispatches nothing. |
| Invalid return values, exceptions and excess resources | Workflow `fail()`, an invalid action key, more than four actions, state over 64 KiB, `finish` combined with another action, `finish` twice, and non-data state each fail the run with `script_error`, an attributable diagnostic and no pending actions. |
| Hard stops | `worker.rs`: a single string repeat allocates about 1 GiB between heap-limit checks. Under `RLIMIT_AS=256 MiB` the worker aborts (SIGABRT), the client reports `worker exited`, and the next request is served by a fresh worker. A slow evaluation is SIGKILLed at a 20 ms wall deadline. Neither affects the host process. |

### Measurements

Measured on this host (12 threads, rustc 1.98.1) with release builds, `examples/measure.rs`, and two runs that agreed within about 5%.

| Cost | Result |
| --- | --- |
| New crates compared with the service's Linux dependency graph | **106**, including `starlark`, `starlark_syntax`/`_map`/`_derive`, `allocative`, `logos`, `num-bigint`, `blake3`, `regex`, `dashmap`, `bumpalo` and `fancy-regex`. `starlark` 0.14.2 also unconditionally pulls `rustyline`, `lsp-types`, `debugserver-types`, `schemafy`, `quickcheck`, `rand` and `rayon-core`, which an embedder does not need. `Cargo.lock` grew by about 1,200 lines. |
| Cold build, separate target directory | Service alone: 75 s (release). Experiment alone: 57 s (release) and 31 s (dev). Incremental rebuild of the experiment: under 1 s. |
| Binary size | The worker, a minimal embedder, is 14.7 MB (8.4 MB stripped) in release. For comparison, the service release binary is 22.0 MB. The service binary itself is unchanged because it does not link the interpreter. |
| Engine and prelude setup | 2.9 ms once. Compiling and typechecking a sample script takes 0.2–0.4 ms; results are cached by hash. |
| Parser callback latency (per line) | Starlark: 12.7 µs mean, 12.7 µs p50, 18.8 µs p99 (14 ticks per line). Built-in Rust, using the same plain-data state: 1.0 µs mean, so Starlark is about 12× slower. Fixed embedding overhead (fresh heap, copying `ctx.state` in and out, call, validation), measured with a no-op script: 1.5 µs. |
| Stream host end to end | 100,000 lines in 5,000 deltas: 1.30 s, i.e. 105,000 calls including the provisional parses. |
| Memory over a long stream | RSS 15,084 KiB after 100,000 lines and after 1,000,000 lines (+0 KiB). Per-call heaps do not accumulate. |
| Worker process | Spawn, compile and first call: 6 ms. Steady state: 36 µs per round trip, including JSON framing with the source in each request. |
| Workflow decision | 5.8 µs per event, excluding SQLite. |

An early run measured 36 µs per line. The cause was cloning `regex::Regex`, which gives every call a cold match cache. Sharing compiled patterns through `Arc` fixed it.

### Comparison

- **Built-in Rust parser plus configuration rules.** This is about 12× faster, adds no dependencies, and gives identical output on every fixture. For the single Bazel parser, Starlark adds cost without benefit. Configurable rules would cover regex-plus-capture extensions. Starlark becomes worthwhile only once parsers need control flow, such as multi-line state machines or per-tool quirks, from several authors.
- **Local helper process.** The worker measures this shape: about 6 ms to start and about 36 µs per round trip. The process boundary is also needed to run untrusted Starlark with firm limits, so in-process Starlark is only acceptable for scripts the user installed and trusts. With either option, an out-of-process helper is the safe default for untrusted code.
- **Architecture.** U-05 derives progress in the browser from persisted events. Moving parsing into Rust, which this experiment does, would require a server-side derived-progress channel and replay story in addition to the interpreter.

### Minimal host API proposal (v1)

- Installation record: `{name, capability: parser | workflow, hash (blake3 of source), enabled}`. The host compiles only enabled installations whose hash matches. Repository content is never loaded implicitly.
- Parser: `on_line(ctx: LineContext, line: str) -> Progress | None`. `LineContext = record(command: str, stream: str, provisional: bool, state: dict[str, typing.Any])`. `Counts = record(done: int, total: int, unit: str)`. `Progress = record(phase: str, label: str | None, counts: list[Counts])`. The host supplies complete or provisional lines after ANSI stripping; the only helper is `regex_match(pattern: str, text: str) -> list[str] | None`. New host facts become `ctx` fields, so the signature stays stable.
- Workflow: `on_event(ctx: WorkflowContext, event: Event) -> list[Action]`. `WorkflowContext = record(workflow: str, owner: str, state: dict[str, typing.Any])`. `Event = record(id, kind, action_key, outcome, data)`, with kinds `start`, `action_completed` and `action_denied` and outcomes `succeeded`, `failed`, `cancelled` and `interrupted`. The constructors are `start_session(key, project, prompt) -> Action` and `finish(outcome) -> Action`. `finish` ends the run and must be the only action returned.
- Default limits per call: parser 20,000 ticks, 8 MiB heap, 50 ms wall, 16 KiB input/state/output. Workflow 50,000 ticks, 16 MiB heap, 200 ms wall, 64 KiB, with at most 4 actions per decision, 16 per run and 256 events per run. Call stack depth is 32.
- Persisted with each run: script name, hash and API version, JSON state, each event with its ID and disposition, and each action with its key, request ID and status.

### Decision: adjust

- **Do not replace U-05.** Keep the built-in parser. One parser does not justify 106 crates, about a minute of extra cold release build time and a roughly 12× slower path. Revisit if there are several real requests for user-installed parsers. Even then, run them in the worker process unless they are user-trusted.
- **Workflow decisions:** the shape worked. Pure typed decisions over durable Rust state made restart, idempotency, revocation and replay straightforward to test, and decisions are cheap. Most of that value comes from the Rust host contract, which is language-independent. Starlark is the preferred decision language if a workflow engine is approved, but it is not adopted now.
- **Prerequisites before any production use:** prune or feature-gate starlark's unneeded dependencies upstream, or vendor-review them; wire `Authorizer` to `communication::require_actor` grants and `Dispatcher` to `control_start`; run evaluation off async threads (`spawn_blocking` or the worker); and add a server-side progress channel if parsing moves to Rust.

### Prototype-only and not done

- Evaluation is synchronous; nothing here runs inside the Tokio service. The fakes stand in for real grants, sessions and harnesses, and no adapter streaming was re-verified.
- There is no UI, installation API or persistent installation registry; installation records are constructed in code.
- On Linux, the worker sends the script source with each request and relies on `RLIMIT_AS` and `RLIMIT_CPU`. It has no seccomp, namespaces or cgroup memory accounting, so it is not an OS sandbox.
- `upgrade` re-pins without migrating state, and replay after an upgrade uses the new script for all events.
- Measurements come from one host and are indicative, not benchmarks.

## Production integration (October 21, 2026)

U-05 Bazel progress now runs on this engine. The crate moved to `crates/agentworks-starlark`, and the service depends on it.

- **Trust and storage.** Scripts are trusted and run in-process on one dedicated parser thread, never on async request threads. Per-call tick, heap, wall-clock and size limits still apply. The worker process stays available but is unused. Scripts live in the SQLite `scripts` table (schema v9) and are compiled once; there is no reloading from disk. `bazel_progress.star` is seeded as a built-in.
- **Routing and display.** Parser scripts must declare `COMMAND_PATTERN`, which Rust uses for routing (first enabled match wins, in name order), and `DISPLAY_NAME`. Both are validated at install, next to the typed entry contract.
- **Publishing.** Progress is derived on the server and published as unsequenced `tool_progress` events plus snapshot `toolProgress`. It is never stored and is rebuilt by replay after a restart. Rust keeps raw output only until a tool's command is known, because the service already stores raw events. It frees stream state when a tool finishes and keeps at most 1,000 views per session.
- **Browser and tests.** The browser renders server views and no longer parses output. A golden file (`tests/fixtures/bazel-progress-events.json`), checked by a Rust test, feeds the browser unit and Playwright tests, so they replay exactly what the service publishes.

Evidence:

- **Rust tests.** 117 service tests and 32 crate tests pass. These cover the golden U-05 fixture, duplicate and unchanged views, untracked tools holding no state, seeding/updating/disabling built-ins and user-owned saves, routing among several parsers, the required declarations, and service-mode raw retention.
- **Browser tests.** `tests/bazel-progress.test.mjs` (9) and `tests/bazel-progress.spec.mjs` (5, desktop and phone) pass.
- **End to end.** `scripts/progress-smoke.mjs`, run with the demo agent's deterministic `bazel demo` replay, shows live progress events, the snapshot view, that history contains no stored progress, that invalid scripts are rejected with diagnostics, that built-ins can't be deleted, and that disabling a parser stops progress. A manual restart of the service rebuilt `Bazel · Testing · 4 / 9 · 3 / 5` from the stored window.
- **Cost.** The service release binary grew from 22.0 MB to 36.4 MB (unstripped). An incremental release build of the service after the change took 77 s.

Settings → Progress parsers (`web/parser-scripts.js`, `tests/parser-scripts.spec.mjs`) lists, enables, edits, installs, deletes and restores scripts. It shows server diagnostics in place, including for enabled scripts that no longer compile. It is checked on desktop and at 390×844.

Not done: workflow scripts in the service, and per-harness verification of live streaming cadence (unchanged from U-05).
