# Tool activity, progress and compact work logs

Proposed follow-up work from the user's requests. This document records a design; no implementation batch is running.

## Current behavior and transport work

The runtime persists ACP session notifications and broadcasts them to the browser. The browser merges tool updates by tool-call ID and renders one details element per call, showing the latest content/raw output as generic text/JSON. It does not normalize terminal-output metadata into an output stream. ACP initialization currently requests v1 without explicitly advertising terminal-output extensions.

The installed Claude and Codex adapter sources include terminal-output metadata paths, including output deltas. Availability and actual streaming cadence must be verified per harness: an adapter's support for a metadata shape does not guarantee every command provides incremental stdout/stderr. Negotiate only capabilities we implement, preserve unknown/raw events, and provide a plain activity fallback for commands that produce output only at completion. A parser cannot manufacture live build progress from a completed result.

## Configurable timeouts — L-07

Expose saved defaults and per-session overrides for an optional total-turn limit, an inactivity warning/optional timeout, and cancellation grace. Allow the total limit to be disabled for deliberately long-running work. Surface the effective policy before launch and make changes to an active turn explicit.

Use distinct clocks for total duration, meaningful last activity and time waiting for human input. Pause inactivity timing during approvals/questions; state explicitly whether a configured total limit includes that wait. Transport heartbeats must not indefinitely defeat an inactivity timeout. Quiet commands can still be healthy: default to a warning before stopping them. Never automatically repeat a potentially mutating tool after timeout/restart. Coordinate timeout/cancel outcomes with queued inputs so a failed or interrupted turn pauses automatic dispatch for review.

## Progress parser interface — U-05

Build a host-side normalization layer keyed by session, turn, tool-call and terminal IDs. Its events distinguish start, output snapshot, output delta and exit, with stable sequence information for deduplication. Handle output arriving before its tool card, mixed content types, carriage-return updates, ANSI sequences and incomplete lines across chunks. Snapshots replace prior output; deltas append. Raw transcript events remain available and progress is derived presentation data.

Feed normalized events through a small versioned parser interface. A parser matches tool metadata/command and maintains per-call state, emitting structured progress such as phase, label, completed count, total count and units. The UI uses the same progress component regardless of parser or harness. Tool status/exit remains authoritative; parsed text cannot turn a failed command into success or dispatch another tool.

Start with a built-in Bazel parser and user-configurable match/capture rules. Bazel displays should show the current phase and reported action/test counts. Totals can change, so label them as reported action progress rather than a prediction of remaining time; show indeterminate activity when no meaningful total exists.

Provide a documented registry so other build/test/download parsers can be added independently of the renderer. Simple rules should be configuration-only. If complex custom parsers are needed, an explicitly user-installed local helper can consume/emit versioned JSON events over stdio; bound its processing time, state and output, and let parser failures fall back to raw output without affecting execution. Tool output or repository content must not implicitly install or execute a parser.

Record deterministic sample streams for tests, including dynamic totals, failures, duplicate events, snapshots/deltas, ANSI/carriage returns, replay and harnesses without streaming. The user's existing Bazel mod can provide parsing rules and representative fixtures when available. Do not require harness-specific CLI modifications just to render progress in Agentworks; investigate an adapter extension or an opt-in build progress source if a harness does not expose live output.

## Compact work logs — U-06

Introduce a turn boundary and a presentation model for its chronological work log. During execution show a compact summary of the current tools/progress, with details available on demand. Keep the eventual answer and user messages prominent. Preserve the order of intermediate assistant updates and tool activity when the work log is expanded.

After a turn completes, collapse its work into a summary such as “18 tool calls · 2m 14s · 1 failed”. Expanding reveals the individual calls and original details. Counts describe calls, not update chunks; no raw events are deleted or sent back differently to the model. Avoid silently folding an activity panel the user is inspecting. Remember explicit expansion choices and offer an expanded/compact preference.

Approvals/questions remain actionable outside the folded log. Failures and interrupted/cancelled outcomes remain apparent in its summary and can be expanded directly. A background command that outlives the turn retains a visible running badge/progress entry until its actual exit; turn completion alone must not imply every process finished. Keep grouping stable after browser reconnect, persisted replay and imported history; infer boundaries conservatively where imported history lacks explicit turn-end markers. Handle parallel/subagent calls using their available parent metadata without pretending unknown relationships are known.

## Suggested implementation order

1. Normalize and test tool/terminal events against deterministic fixtures and verify each installed harness's live output capabilities.
2. Add the progress interface and Bazel renderer with raw-output fallback.
3. Add turn work-log grouping, accessible expansion and mobile behavior.
4. Expose timeout settings and integrate their outcomes with queued-input dispatch. This backend policy can also proceed independently once its shared session/event contract is agreed.

These features do not require a PTY terminal session or changing a session's fixed chat/terminal mode.
