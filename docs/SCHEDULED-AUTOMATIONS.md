# Scheduled automations (A-01)

A definition's recipe trigger can be `manual` or a weekly `schedule`. Scheduled runs use the same path as **Run now**: the same harness/auth checks, workspace policy, one-active-run limit, restart drain gate and request-ID deduplication. GitHub/event triggers (A-02) are not part of this.

## Trigger

```json
{"kind":"schedule","timeZone":"Europe/Berlin","at":"09:30","days":[1,2,3,4,5],"missed":"skip"}
```

- `timeZone`: an IANA name resolved by the host's time-zone database (`jiff`). Unknown names are rejected when saving.
- `at`: local 24-hour `HH:MM`. `days`: ISO weekdays, 1 = Monday … 7 = Sunday. Days are sorted and deduplicated.
- Daylight saving: a local time that does not exist (spring forward) runs right after the gap; a repeated local time (fall back) runs once, at its first instance.
- `missed`: `skip` (default) or `runOnce`. See below.

`GET /api/automations` adds `nextRun` (Unix ms, or `null` when paused or manual). Run now stays available on scheduled definitions.

## Dispatch and deduplication

A scheduler checks enabled scheduled definitions every 5 seconds, under the same `run_lock` as manual runs, edits and deletion. Durable state in SQLite (introduced in schema v10; preserved in the combined v11 retention/recovery release):

- `automation_schedules.cursor`: occurrences at or before this instant have been handled.
- `automation_slots`: one row per handled occurrence (`started`, `missed` or `skipped`, with an optional detail).
- Started runs use request ID `schedule:<occurrence ms>` in `automation_runs`, so an occurrence can start at most one session. If the server stops after starting the session but before writing the slot row, the next check finds the run and records it without starting another one.

An occurrence that cannot start (harness unavailable, previous run still active, workspace or host error) is recorded as `skipped` with the reason. It is not retried. While the service is draining for a restart, nothing is claimed. The occurrence is handled after startup.

## Missed occurrences, restarts and pauses

An occurrence is on time if it is handled within 5 minutes. Otherwise, for example after downtime, a long restart drain or a host sleep:

- `skip` records the latest passed occurrence as `missed` and starts nothing.
- `runOnce` starts the latest passed occurrence once, with the detail "Started late under the run-once policy."

Only the latest passed occurrence is considered. Earlier ones are collapsed into it, and the detail notes this. Nothing is ever replayed more than once.

Creating a schedule, changing it, or resuming a paused definition arms the cursor at the current time. Occurrences before saving, or while paused, are never run. Pausing keeps the definition and its history. Turning the trigger back to manual, or deleting the definition, removes the cursor. Slot and run history is kept.

`GET /api/automations/{id}/runs` returns runs with `scheduledFor` for scheduled ones, plus `missed`/`skipped` occurrence rows (`sessionId: null`, `detail`).

## Limits

One time per definition, weekly days only (no cron, intervals or monthly rules). Interrupted scheduled runs follow existing automation recovery, so they are never replayed. Scheduler state is server-local. Remote-host definitions are dispatched by the server and are skipped while their host is unavailable.

## Checks

`cargo test -p agentworks automations` covers validation, time zones, DST, the grace/missed policy and the slot ledger. `npx playwright test tests/automation-workflow.spec.mjs` covers the editor, real dispatch at the next minute, deduplication across restart, and downtime simulated by moving the durable cursor back.
