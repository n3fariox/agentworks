# L-05 activity indicators and queued explicit inputs (batch 007 worker)

- Server: `src/input_queue.rs`. Queue state persists as `SessionData.input_queue` (serde default) via the existing session-state save, so there is no table or schema-version change. Queue changes are saved with `save_state` and broadcast as an unsequenced `input_queue` websocket event, so they don't consume the bounded transcript history.
- Limits: 20 pending items, 64 KB pending total, 32 KB per item, 100-char request IDs; only the last 5 sent items are kept.
- Identity: enqueue `requestId` deduplicates. Same payload counts as a duplicate, a different payload returns 409, and a retry after an edit stays a duplicate. Dispatch request ID is `queue:{id}:r{revision}`. Edits need the current revision and are refused once the item is claimed.
- Dispatch: `after_turn` runs only after `end_turn` with no cancel, timeout or error, no quota continuation (`continuation.limited`/`due_at`), no storage error and no suspend. It claims one item, persists `sending` before the prompt, and sends it through the session command channel. Any other outcome marks the in-flight item `error` (never resent) and pauses the queue. Resume is deliberate (`POST /queue/resume`); when idle it sends the next item now, still subject to the quota and storage checks.
- Restart: `recover()` turns `sending` into `error` (delivery unknown) and pauses the queue. Nothing is replayed.
- The internal `queued_prompt` (automation/handoff) is untouched.
- Routes: `GET/POST /api/sessions/{id}/queue`, `PATCH/DELETE /api/sessions/{id}/queue/{item}`, `POST /api/sessions/{id}/queue/resume`.
- Integration hooks: `input_queue::has_pending(&Session)` lets explicit human input outrank inbox wakes. Inbox dispatch should skip when it returns true, and also when the queue is paused.
- Web: `web/input-queue.js` provides the activity label (Model working / Running tool / Waiting for approval / Cancelling) and elapsed time. After replay or reconnect the start time is unknown, so it shows time "since reconnect". Animated dots are disabled under reduced motion, and the queue has 44px touch targets plus keyboard edit (Esc cancels, Ctrl/Cmd+Enter saves).
