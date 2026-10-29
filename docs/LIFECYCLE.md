# Session lifecycle (L-01)

Rename and archive change saved session metadata. Archive hides a session from the default list, stops its terminal and child processes, and saves terminal scrollback for later viewing. It releases the PTY and emulator; archived snapshots survive service restart and are deleted with the session. Unarchiving keeps the saved terminal read-only until **Start new terminal** is selected. The agent harness, active turn and continuation policy keep running. The exception is a clean (or explicitly confirmed) worktree created by the session: archive suspends the idle harness, removes that checkout, and keeps its branch for restore on unarchive. See [C-04b](WORKTREE-CLEANUP.md). The sidebar offers Active, Archived and All filters. External harness discovery identifies an already-registered archived conversation and opens that conversation instead of importing a duplicate.

Stop cancels an active turn and disables usage-limit continuation, including pending timers. The harness normally remains available for another human turn. If Stop arrives while the native context is opening or model configuration is in progress, Agentworks suspends the harness instead of queuing a cancellation behind initialization.

Suspend harness cancels continuation authorization, releases the harness, clears approvals and retains transcript, provider context, completion claims and workspace. Opening or reloading an explicitly suspended transcript does not restart it. Resume session deliberately opens its saved native context without replaying a prompt. Ordinary sessions saved by a server shutdown retain the previous lazy-resume behavior. Interrupted external tool effects cannot be rolled back: inspect the last tool result before continuing.

Restart harness (`POST /api/sessions/{id}/restart`) stops an idle harness and resumes its native context in a fresh process, without replaying a prompt and without the explicit-suspend pause; a suspended or failed session is simply resumed. It is refused during a turn. Each local harness start reads the user's current environment, falling back to the service environment on any failure, so edits reach the restarted harness without a service restart. On Unix this runs `$SHELL -l -i` in the session directory (five-second limit); one read is reused for 60 seconds per directory so a burst of session starts runs the shell once, and Restart harness always reads afresh. On Windows it is the registry environment a new logon would get (`CreateEnvironmentBlock`: system, then user variables, expanded), read on every start. Only variables that differ from the service's are applied, harness settings win, and a variable deleted from the startup files remains inherited from the service. `AGENTWORKS_LOGIN_ENV=0` disables this. Remote harnesses keep using the worker's `mise env`. In-process harness state (background shells, running subagents, MCP connections) does not survive a restart.

Delete history remains a separate destructive action with browser confirmation. For API compatibility, DELETE also shuts down an attached harness before removing history. It never removes worktree files or branches. Existing completion claims retain their original author-title snapshot; new messages and claims use the current session name.

## HTTP API

- `PATCH /api/sessions/{id}` accepts `title` (1–80 characters after trimming, no control characters) and/or `archived` (boolean).
- `GET /api/sessions` lists active sessions; `?archived=true` lists archived sessions; `?archived=all` lists both.
- `POST /api/sessions/{id}/stop` (also existing `/cancel`) stops a turn and continuation authorization.
- `POST /api/sessions/{id}/suspend` releases the harness and preserves history.
- Existing `/resume` and `DELETE /api/sessions/{id}` remain available.

Lifecycle state is stored with the session's SQLite state; older databases default to the original title, visible and not explicitly suspended. Worker shutdown is joined before acknowledging suspension. Resume, Stop, Suspend, approval submissions and continuation dispatch share the execution gate to prevent conflicting lifecycle operations. Late approval requests after cancellation are rejected.

## Verification

Build with the authorized offline Cargo environment, then run `node scripts/lifecycle-smoke.mjs`. The fixture uses only deterministic local demo ACP, an isolated temporary SQLite/catalog/demo directory and Git linked worktree, and `127.0.0.1:4401` (override with `AGENTWORKS_LIFECYCLE_PORT`, restricted to 4401–4405). It exercises metadata validation/filtering/discovery, archive during work, Stop, pending approval cancellation, quota timer cancellation across a crash/restart, saved native identity and completion claims, delayed native-open races, deletion compatibility, retained dirty/untracked worktree files, and dark desktop/mobile browser controls. Browser screenshots remain in the printed temporary artifact directory. No paid provider calls are made.
