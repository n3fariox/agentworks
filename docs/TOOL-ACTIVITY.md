# Tool activity, progress and compact work logs

Compact work logs, [configurable timeouts](TIMEOUTS.md), and built-in Bazel progress are implemented. Bazel progress requires terminal output metadata from the adapter. Work-log durations currently use live browser observations and are omitted after replay because stored events do not carry timestamps; unknown background process state is labelled unresolved rather than running.

## Current behavior and transport work

The runtime persists ACP session notifications and broadcasts them to the browser. ACP v1 initialization advertises only the supported terminal-output metadata extensions; this does not enable terminal process hosting. The browser rebuilds private per-tool derived state from persisted events, appends explicit output deltas, replaces snapshots, and deduplicates replay by event sequence. Raw ACP metadata and terminal output remain available in the expandable tool card.

The installed Claude and Codex adapter sources include terminal-output metadata paths, including output deltas. Availability and actual streaming cadence must be verified per harness: an adapter's support for a metadata shape does not guarantee every command provides incremental stdout/stderr. Negotiate only capabilities we implement, preserve unknown/raw events, and provide a plain activity fallback for commands that produce output only at completion. A parser cannot manufacture live build progress from a completed result.

## Configurable timeouts — L-07

Expose saved defaults and per-session overrides for an optional total-turn limit, an inactivity warning/optional timeout, and cancellation grace. Allow the total limit to be disabled for deliberately long-running work. Surface the effective policy before launch and make changes to an active turn explicit.

Use distinct clocks for total duration, meaningful last activity and time waiting for human input. Pause inactivity timing during approvals/questions; state explicitly whether a configured total limit includes that wait. Transport heartbeats must not indefinitely defeat an inactivity timeout. Quiet commands can still be healthy: default to a warning before stopping them. Never automatically repeat a potentially mutating tool after timeout/restart. Coordinate timeout/cancel outcomes with queued inputs so a failed or interrupted turn pauses automatic dispatch for review.

[L-09 limit-reset continuation](LIMIT-RESUME.md), is the explicitly opt-in exception for recognized provider quota exhaustion: a durable timer may start a new continuation turn after the reset, retaining native context and checking current state. It does not retry arbitrary timeouts, replay a tool invocation or override a human stop. The suggested tool-display work below remains deferred.

## Built-in Bazel progress — U-05 first slice delivered

For recognized Bazel commands, derive a compact phase and reported action/test counts from available terminal metadata, output content or raw output. Parse output incrementally across split lines, ANSI/OSC control sequences, carriage-return updates and dynamic totals. Keep raw data unchanged. Progress is presentation only; command/tool status remains authoritative and no percentage is invented. When a useful stream is absent, retain the ordinary elapsed-activity display. The implementation is built into the existing tool cards and intentionally has no public parser registry or plugin system.

Streaming cadence is adapter-dependent. An adapter advertising terminal-output support does not guarantee that every command reports incremental output; no live provider query or command is started to obtain progress. Terminal exit metadata can enrich the display but does not override the harness's tool result.

## Deferred extensibility

General cross-harness normalization, a versioned parser contract, configurable match/capture rules, and user-installed parsers remain future work. Add them only after a concrete need justifies the extra interface; any helper must be explicitly installed, bounded, isolated from tool authority, and unable to alter command outcomes.

The optional [Experiment E-01](experiments/001-starlark.md) is deferred with the broader extension work; it is not a dependency of Bazel progress.

Record deterministic sample streams for tests, including dynamic totals, failures, duplicate events, snapshots/deltas, ANSI/carriage returns, replay and harnesses without streaming. The user's existing Bazel mod can provide parsing rules and representative fixtures when available. Do not require harness-specific CLI modifications just to render progress in Agentworks; investigate an adapter extension or an opt-in build progress source if a harness does not expose live output.

## Compact work logs — U-06

Introduce a turn boundary and a presentation model for its chronological work log. During execution show a compact summary of the current tools/progress, with details available on demand. Keep the eventual answer and user messages prominent. Preserve the order of intermediate assistant updates and tool activity when the work log is expanded.

After a turn completes, collapse its work into a summary such as “18 tool calls · 2m 14s · 1 failed”. Expanding reveals the individual calls and original details. Counts describe calls, not update chunks; no raw events are deleted or sent back differently to the model. Avoid silently folding an activity panel the user is inspecting. Remember explicit expansion choices and offer an expanded/compact preference.

Approvals/questions remain actionable outside the folded log. Failures and interrupted/cancelled outcomes remain apparent in its summary and can be expanded directly. A background command that outlives the turn retains a visible running badge/progress entry until its actual exit; turn completion alone must not imply every process finished. Keep grouping stable after browser reconnect, persisted replay and imported history; infer boundaries conservatively where imported history lacks explicit turn-end markers. Handle parallel/subagent calls using their available parent metadata without pretending unknown relationships are known.
