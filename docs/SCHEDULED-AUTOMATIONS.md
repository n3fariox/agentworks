# Scheduled automations (A-01)

A definition's recipe trigger can be `manual`, a weekly `schedule`, or a one-time `once`. Scheduled runs use the same path as **Run now**: the same harness/auth checks, workspace policy, one-active-run limit, restart drain gate and request-ID deduplication. GitHub/event triggers (A-02) are not part of this.

## Trigger

```json
{"kind":"schedule","timeZone":"Europe/Berlin","at":"09:30","days":[1,2,3,4,5],"missed":"skip"}
```

- `timeZone`: an IANA name resolved by the host's time-zone database (`jiff`). Unknown names are rejected when saving.
- `at`: local 24-hour `HH:MM`. `days`: ISO weekdays, 1 = Monday … 7 = Sunday. Days are sorted and deduplicated.
- Daylight saving: a local time that does not exist (spring forward) runs right after the gap; a repeated local time (fall back) runs once, at its first instance.
- `missed`: `skip` (default) or `runOnce`. See below.

### One-time trigger

```json
{"kind":"once","timeZone":"Europe/Berlin","at":"2026-11-03T09:30","missed":"skip","afterRun":"complete"}
```

- `at`: a local date and time, `YYYY-MM-DDTHH:MM`, resolved in `timeZone` with the same daylight-saving rule as weekly schedules. Saving or resuming an enabled one-time trigger requires it to be at least a minute and at most a year ahead, because arming happens at the save time and an earlier occurrence would never run. A paused definition may keep a past time.
- `missed` works as for weekly schedules.
- The occurrence is handled once, whatever its outcome (`started`, `missed` or `skipped`), and is never retried. In the same transaction that records it, `afterRun` applies:
  - `complete` (default): the definition is paused, gets `completed` (Unix ms) and a new revision, and loses its cursor. It stays listed as **Completed** and can be rescheduled by saving it with a new time, which clears `completed`.
  - `delete`: the definition is removed. Its run and occurrence history stays readable through `GET /api/automations/{id}/runs`.
- Run now remains available while the definition is enabled and does not consume the one-time occurrence.

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

One time per definition: weekly days or a single date (no cron, intervals or monthly rules). Interrupted scheduled runs follow existing automation recovery, so they are never replayed. Scheduler state is server-local. Remote-host definitions are dispatched by the server and are skipped while their host is unavailable.

## Agent-managed automations

Sessions and external controllers can manage project automations through MCP with the `manageAutomations` grant. The grant is off by default, is not part of the Participant or Coordinator presets, is never inherited by child or automation sessions, and does not imply coordinator authority. Humans grant it in **Agents & inbox** or when creating a controller.

| Tool | Grant | Effect |
| --- | --- | --- |
| `list_automations` | `runAutomations` or `manageAutomations` | Project definitions. Managers also see trigger, `nextRun`, `revision` and `createdBy`. |
| `get_automation`, `list_automation_runs` | same | One definition's full view; its recent runs and missed/skipped occurrences. |
| `create_automation` | `manageAutomations` + `startSessions` | Same fields and validation as the browser editor (`name`, `provider`, `cwd`, optional `model`, `modelProvider`, `enabled`, `prompt` or `workflow`). The project and host come from the credential: the caller's project, and the caller session's host (`local` for controllers). |
| `update_automation` | `manageAutomations` + `startSessions` | Replaces an agent-created definition. `revision` must match the current one, otherwise 409. |
| `set_automation_enabled` | `manageAutomations` | Pauses any project definition, or resumes an agent-created one. Resuming re-arms the schedule as in the browser, so nothing missed while paused runs. |
| `delete_automation` | `manageAutomations` | Deletes an agent-created definition. Run and occurrence history is kept. |

- **Authorship.** A definition created through MCP records `createdBy` (actor ID and title). Human edits keep it. Agents can only resume, replace or delete agent-created definitions. Human-created ones are human-managed, except that an agent may pause them.
- **Unattended authority.** A scheduled occurrence of an agent-created definition is checked against its author's current state: the author must still exist (controller not revoked, session not deleted), be in the definition's project, and hold `manageAutomations` and `startSessions`. Otherwise the occurrence is recorded `skipped` with the reason and nothing starts. Human-created definitions and human **Run now** are unaffected. `run_automation` uses the caller's own grants.
- **Idempotency.** Every mutation needs a `requestId` (1–100 characters). The request and its result are stored in `automation_agent_requests`, keyed `(actor, requestId)`, in the same transaction as the change. An identical retry replays the result with `duplicate: true`; reusing the ID for anything else returns 409.
- **Limits.** At most 20 agent-created definitions per project. Mutations are audited like other agent operations.
- **Resilience.** A stored definition that cannot be read is logged and skipped by listing and the scheduler instead of stopping every schedule. Restore validation still rejects it.

The same-user trust caveat in [Agent communication](AGENT-COMMUNICATION.md) applies: these are application-level controls, not isolation from processes running as the service's OS user.

Schema v12 adds `automation_agent_requests` and the `manageAutomations` grant. Older binaries refuse a v12 database.

## Checks

`cargo test -p agentworks automations` covers validation, time zones, DST, the grace/missed policy and the slot ledger. `npx playwright test tests/automation-workflow.spec.mjs` covers the editor, real dispatch at the next minute, deduplication across restart, and downtime simulated by moving the durable cursor back. `npm run test:automation-manager` drives the MCP tools through real controller clients: grants, project boundaries, authorship rules, revisions, request replay, audit, the fire-time author check after revocation, and skipping an unreadable row.
