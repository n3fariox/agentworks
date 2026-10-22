# E-01 — Embedded Starlark for progress and workflows

Status: planned experiment, requested by the user. No implementation or agent batch has started. Adoption remains undecided.

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
