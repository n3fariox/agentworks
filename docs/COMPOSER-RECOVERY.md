# Stop recovery and composer height

Stop copies the submitted prompt back to the editor without sending it. It retains
all user, partial assistant, tool and terminal events. The current demo, Claude,
Codex and OpenCode adapters use ACP `session/cancel`; no adapter in this app exposes
history rollback. Retrying is a new turn in the existing provider session, whose
prior prompt, partial output and tool effects remain. Stop cannot undo file or
external effects. The UI states this limitation and distinguishes cancellation
requested, confirmed cancellation, completion winning the race and errors.

The browser binds Stop to the displayed user event sequence. The server validates
that sequence before stopping continuation, queue dispatch or the provider; stale
requests get 409 without cancelling a newer turn. Existing untargeted Stop and MCP
callers remain compatible. A durable `stop_requested` event identifies the prompt,
request and sequence; `historyRewound` is always false. The latest user event is also
saved with session metadata so long output cannot evict the recovery source.
Provider completion remains authoritative; Stop is not treated as acknowledgement
of cancellation. Late ACP updates remain recorded and visible.

Editor drafts are retained by request ID in tab session storage before submission,
separately from expanded mention payloads. Recovery therefore preserves whitespace
and the user's original mention text, including when later inputs were queued.
Another browser can recover the submitted wire text from the durable event; it
cannot reconstruct original mention shorthand from a different tab's storage.
An occupied composer is preserved and offers an explicit swap with the recovered
draft. Replay only recovers the latest turn, and acknowledged recovery never
replaces subsequent edits. A late POST response cannot clear recovered text;
deliberate retry receives a fresh request ID. Queue dispatch remains paused by Stop.

The textarea grows and shrinks with content up to 320 px or 40% of viewport height,
then scrolls. Native vertical resize remains available. Manual sizing becomes a
preferred minimum, allowing further growth for content and respecting the viewport
cap. Programmatic draft insertion and session switching also update height.

Validation (isolated fixtures only):

- `cargo test --locked --offline` and `cargo build --locked --offline`.
- `node scripts/composer-smoke.mjs`: private port 4531, temporary SQLite/catalog and
  demo ACP state; stale targets, durable text, completion race, provider history,
  deliberate retry and approval cancellation. No service restart or real models.
- `playwright test tests/composer-controls.spec.mjs`: private static port 4523 and
  mocked HTTP/WebSocket events; recovery races/replay, exact editor vs expanded
  text, draft preservation, errors, and desktop/mobile sizing/manual resize.
