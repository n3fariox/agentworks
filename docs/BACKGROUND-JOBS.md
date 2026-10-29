# Background jobs

Agentworks negotiates the optional, versioned AIR `asyncTasks` ACP extension with
compatible adapters. The [Rust Codex adapter](RUST-CODEX-ACP.md) and the Rust Claude
adapter (`claude-rust`, see [RUST-CLAUDE-ACP.md](RUST-CLAUDE-ACP.md)) implement it.
Ordinary tool activity remains available for other harnesses.

**Background jobs** appears above the composer when the session has reported jobs.
Expand it to inspect their names, status, reported summaries/progress, and output
file references. Jobs launched by a child session are identified as subagent jobs.
The original tool card shows the linked job status separately from the tool's
launch result. A job may continue after its launching tool call and the model turn
have finished. The dropdown remains available in a child tab and after turn end.
Output file references are displayed as text; Agentworks does not read or tail
these files automatically.

**Stop** requests termination of that one job through the adapter. It does not
send a prompt or cancel the conversation's turn. The control is enabled only for a
job confirmed running on the current connection whose adapter permits stopping.
Adapter errors appear on that job. A false stop response does not manufacture a
terminal status, and a timeout leaves the outcome uncertain; inspect subsequent
status before retrying. The ordinary composer Stop still cancels the root turn.

## Wire contract and ownership

The client advertises only this AIR capability, alongside existing ACP features:

```json
{"_meta":{"jetbrains":{"air":{"version":1,"capabilities":["asyncTasks"]}}}}
```

Support is negotiated from the adapter initialization response's matching `_meta`.
Agentworks accepts `async_task_spawned`, `async_task_progress`, and
`async_task_state_update` on the root or an explicitly announced child session.
Ownership comes from the ACP notification's session ID, never command prose.
Unknown fields in async-task notifications remain in the stored event. A spawn
registers a job; progress/state updates patch an existing job with the same owning
session and job ID. Terminal states cannot be reversed by duplicate spawns.

`POST /api/sessions/{id}/async-tasks/stop` takes
`{asyncTaskId, acpSessionId}`. Both refer to a known job in that Agentworks session.
The runtime validates the target again before sending
`_session/async_task/stop` with the adapter's root `sessionId` and `asyncTaskId`.
The adapters own child-job routing and process termination. This endpoint never
starts or resumes a harness and never invokes a model.

Declaring the AIR namespace also selects the JavaScript Claude adapter's older
subagent event format. Agentworks already accepts that format and the current ACP
format. It does not change client identity or advertise other AIR capabilities.

## Persistence and connection loss

Job observations are saved in the existing session JSON transaction with each
event. Snapshots carry them independently of the bounded transcript window, so
reload, reconnect, and loading older transcript pages do not lose running jobs.
The latest 100 terminal jobs are retained in this view; the original notifications
remain in the full event history. Live jobs are retained until their status changes.

Turn completion alone never completes a job. Losing the harness connection makes
unfinished observations **Status unknown** and disables their Stop controls.
Restart does not restore live control from saved metadata. A new adapter spawn or
explicit running-state update must confirm the job again. An adapter terminal
update can resolve an unknown job. Previously confirmed terminal outcomes remain.

## Verification

No provider inference is required:

```sh
cargo test --locked --offline -p agentworks-session -p agentworks-runtime -p agentworks
cargo build --locked --offline -p agentworks
npx playwright test tests/async-tasks.spec.mjs tests/subagents.spec.mjs
python3 tests/claude-acp.test.py -v
npx playwright test tests/claude-rust-subagents.spec.mjs
```

The fixture covers background shell launches, child jobs, turn completion, failed
jobs, duplicate spawns, progress, individual stopping during/after a turn, adapter
stop errors, reload, service restart, foreign-session rejection, and mobile width.
The Rust Claude adapter's wire test and browser test drive it with
a fake stream-json CLI. Live provider lifecycle behavior has not been exercised.
