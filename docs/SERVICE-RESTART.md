# Supervised service restart (R-06)

The Settings **Service · all projects** control and explicitly granted MCP tools request a durable, global restart. A direct server invocation refuses restart. The Python launcher owns the successor independently of the requesting browser, harness and terminal. Linux user services are the supported deployment; no inference, build, download, merge or installation happens in a restart request.

## Operator installation

Prepare an immutable executable in a private trusted release directory. Never run Cargo builds over that executable: MCP launches `current_exe`. Keep the repository and its existing `node_modules` available because the native adapters still use the configured repository adapter paths. Keep HOME, existing sign-ins, working directory, roots, catalog and SQLite paths unchanged. This feature neither changes credentials nor grants itself permission.

Example operator commands (review paths and existing live ownership before executing):

```sh
install -d -m 700 "$HOME/.local/lib/agentworks/releases" "$HOME/.local/state/agentworks/service" "$HOME/.config/agentworks" "$HOME/.config/systemd/user"
install -m 700 target/debug/agentworks "$HOME/.local/lib/agentworks/releases/agentworks-REVISION"
install -m 700 scripts/service/supervise.py "$HOME/.local/lib/agentworks/supervise.py"
```

Create `$HOME/.config/agentworks/service.env` with mode 0600 and explicit absolute paths, for example:

```ini
PATH=/home/agentworks/.local/share/mise/installs/node/latest/bin:/home/agentworks/.local/bin:/usr/local/bin:/usr/bin:/bin
AGENTWORKS_AUTH_MODE=existing
AGENTWORKS_BIND=127.0.0.1
AGENTWORKS_PORT=4318
AGENTWORKS_ROOTS=/home/agentworks/projects
AGENTWORKS_CATALOG_PATH=/absolute/existing/catalog.json
AGENTWORKS_DB_PATH=/absolute/existing/sessions.sqlite3
```

Copy `scripts/service/agentworks.service.in` to `$HOME/.config/systemd/user/agentworks.service`, replacing every `@…@` placeholder with an absolute path (and the initial executable basename). The unit uses `/usr/bin/python3`, an explicit environment file, `KillMode=control-group`, graceful SIGINT, bounded shutdown, and `Restart=no`. The supervisor CLI is:

```sh
/usr/bin/python3 /absolute/supervise.py \
  --release-dir /absolute/private/releases \
  --release agentworks-REVISION \
  --control-dir /absolute/private/service
```

After the operator has stopped the old instance and verified its backup, install/start using `systemctl --user daemon-reload` and `systemctl --user enable --now agentworks.service`. Inspect `systemctl --user status agentworks.service` and `journalctl --user -u agentworks.service`. This implementation does not install a unit, stop an existing instance, enable lingering or alter live data. User service lifetime after logout depends on the host's existing user-manager/linger policy; enabling linger is a separate human host decision.

The private control directory contains a locked supervisor lease, heartbeat/nonce, external outcome, dispatch ticket, backups and `active-release.json`. The live parent PID, matching nonce and heartbeat less than two seconds old are required before the backend advertises support. SIGKILL of the supervisor kills its child through Linux parent-death signaling; the unit also owns the entire cgroup. A normal exit or operator stop never loops. Each exit 75 needs a matching fresh dispatch ticket; a failed launch or readiness deadline stops visibly rather than retrying.

A release name is a prepared executable basename directly in the configured canonical directory. Escaping symlinks, missing files, directories, nonexecutables and group/world-writable executables are rejected. A bounded `--service-protocol` metadata probe (protocol1, two seconds, 4096 bytes, null stdin/stderr) rejects incompatible builds before accepting a request and again before dispatch. It runs only the explicitly prepared trusted executable; no model is started. Revalidation occurs at dispatch and launch. Treat release files and the directory as operator-owned immutable artifacts; same-user arbitrary filesystem mutation is outside the existing trust boundary. After successful PID/instance/executable readiness validation, the supervisor atomically persists `active-release.json`; cold starts retain that last successful release. `--release` is the initial fallback when no selector exists. For manual recovery, stop the unit, inspect the failed outcome and database, deliberately select a known compatible release in that private selector (or remove it to use the initial CLI release), then start the unit. No automatic data rollback occurs.

## API and decisions

`GET /api/service/restart` returns instance ID, PID/executable, live supervision support, active turn/approval/harness/preparation counts, drain state, recent durable requests, manager errors and external supervisor outcome. Reading never starts or repeats work.

`POST /api/service/restart` accepts:

```json
{"requestId":"stable-human-action-id","mode":"wait","expiresInSeconds":300,"release":"agentworks-REVISION"}
```

`release` may be omitted to keep the current immutable executable. `mode` is `wait` or `interrupt`; expiry is 5–900 seconds. IDs bind to the full payload and actor: identical retries return the original result, changed payloads conflict, and concurrent pending requests conflict. Requests are persisted with FULL synchronous SQLite commits before acknowledgment; dispatch intent and an fsynced atomic supervisor ticket precede exit. The Settings button requires explicit global impact confirmation; polling and reload only read GET, never replay POST.

`wait` blocks new session/turn/model/discovery/automation starts through shared backend paths, including private MCP and usage-limit scheduler, then waits for active turns, approvals, harness startup and workspace preparation. Approvals and Stop remain available. Expiry, requester Stop/suspend and live grant revocation cancel pending work. `interrupt` deliberately stops all harnesses and approvals, recording interrupted work without replay. A preparation already running finishes under its workspace lock before a restart decision can dispatch. New starts conflict while draining; they are not silently queued.

`POST /api/service/restart/cancel` accepts `{"actor":"human","requestId":"…"}` or an attributed actor ID from status to cancel that request/handoff. Once dispatching, ownership has transferred to the supervisor and cancellation is too late. Native Stop and suspend also cancel the requesting session's pending handoff. A storage failure pauses the manager visibly and blocks starts until manual inspection; it does not repeatedly shut down workers or retry mutations.

Before dispatch, the manager captures catalog JSON and SQLite using `VACUUM INTO`, verifies `integrity_check`, compares complete ordered contents of every application table (including history, grants, completion claims, timers in session state and restart records), verifies JSON roundtrip, and fsyncs the backup. The private backups use directory0700/file0600. A preflight snapshot must succeed before any harness is stopped; a final snapshot records shutdown state. Nothing restores or rewrites the live database from a backup automatically. Backup retention is manual in this release.

A fresh supervised successor loads the same store/catalog and marks the matching dispatch ready. An interrupted drain/unknown dispatch becomes `uncertain`; it never automatically dispatches again. A successor unable to become ready within 30 seconds produces `outcome.json` phase `failed` and exits the launcher. During such an outage the browser cannot fetch an API: inspect that external receipt/journal, repair manually and start the service. On recovery, the request ledger exposes uncertainty. No paid model is used for health checks.

## Explicit coordinator permission and native handoff

`restartService` defaults false for old, new, child and automation sessions. **Agents & inbox** has a separate checkbox labeled GLOBAL/all projects. Existing coordinator presets exclude it. Existing communicate/start/prompt/cancel/runAutomations grants do not imply restart. The private bridge checks the live grant for tools/list and each dispatch; existing cache/credential isolation stays unchanged. It attributes the request to the authenticated requesting session, never a borrowed inbox identity.

The three gated MCP tools are `get_service_status`, `restart_service` and `cancel_service_restart` (own request only). `restart_service` returns durable request identity immediately; the manager, not the requesting tool, owns dispatch. Status is read-only and can be polled inside the current turn. New turns remain blocked during drain.

For unattended self-update, explicitly request `mode:"wait", handoff:true, handoffDeadline:<epoch milliseconds within 30 minutes>`. Only the requesting MCP session can opt into its own continuation. Finish the current turn after accepting the request. After restart and a matching external supervisor readiness receipt (instance, PID, request and release; selector already saved), the manager opens that same saved native provider ID with its original cwd and grants, checks capacity/root/harness availability, and submits exactly one fresh attributed instruction to inspect the assignment and current tool results before continuing unfinished authorized work. It never resubmits the old prompt/tool. No missing or unsupported native context is replaced with a fresh context. No other session, approval or queued input is resumed by this mechanism; separately human-authorized usage-limit policies retain their own scheduler behavior.

The handoff claim is persisted before dispatch. A crash during opening/claimed/dispatched handoff becomes visibly uncertain on the next startup, and never auto-replays. Stop, suspend, cancellation, revocation and deadline take precedence under the shared locks. A failed/ambiguous send pauses for manual inspection. `dispatched` describes submission, not completion of the coordinator assignment.

## Deterministic acceptance

Use existing dependencies; no installation is required:

```sh
CARGO_TARGET_DIR="$PWD/target" cargo build --locked --offline
CARGO_TARGET_DIR="$PWD/target" cargo test --locked --offline service_restart
python3 scripts/service/supervisor-test.py
node scripts/service-restart-smoke.mjs
```

The fake server suite uses port4442 and covers one-ticket/one-successor, normal exit, stale ticket, bounded failed readiness and killed-supervisor cleanup. The real fixture uses port4441, private temporary catalog/SQLite/demo state/releases/control directory, local demo native sessions and no paid inference. It covers unsupported direct server, invalid/missing releases, durable retry/payload conflict, concurrent requests, approval drain/start gates/cancel/expiry/revocation/Stop, child default grants, native handoff, process/release restart, preserved history/native context/grants/timer state, backup integrity, cold-start release retention, bounded failed successor, uncertain recovery, mobile dark controls and browser GET-only reconnect. All fixture processes close in `finally`; no live4318 access occurs. The embedded JS/CSS assets require rebuilding the Rust executable after every web change.
