# Supervised service restart (R-06)

The Settings **Service · all projects** control and explicitly granted MCP tools request a durable, global restart. A direct server invocation refuses restart. The systemd user service owns the successor independently of the requesting browser, harness and terminal. Linux user services are the supported deployment; no inference, build, download, merge or installation happens in a restart request.

## Operator installation

The service runs as the systemd user unit `agentworks.service`. systemd launches the server, waits for its readiness notification and starts the successor after a deliberate restart. There is no separate supervisor process. A direct server invocation refuses restart.

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

`AGENTWORKS_CATALOG_PATH` and `AGENTWORKS_DB_PATH` are required: the built-in default is reserved for development runs, and `--install` refuses an environment file without both. systemd rereads this file before every start, including the start that follows a restart request, so edits apply on the next restart.

Then install from the build you want to run:

```sh
target/debug/agentworks service --install            # write and enable the unit
target/debug/agentworks service --install --restart  # also switch to it now
```

`--install`:

- copies the running executable into the release directory as `agentworks-<sha256 prefix>` (mode 0500), or uses it in place if it is already there; `--release NAME` picks the name;
- points the `current` and `stable` links in the release directory at it;
- checks that `service.env` sets absolute `AGENTWORKS_CATALOG_PATH` and `AGENTWORKS_DB_PATH`;
- renders `scripts/service/agentworks.service.in` (embedded in the binary) into `$XDG_CONFIG_HOME/systemd/user/agentworks.service`, keeping a differing previous unit as `agentworks.service.bak`;
- runs `systemctl --user daemon-reload` and `enable`, and with `--restart` queues `systemctl --user restart --no-block` (safe to run from a session inside the service).

Paths come from `--release-dir`, `--service-dir` and `--working-directory`. Otherwise an installed unit's paths are reused, including a unit written for the former Python supervisor (`--release-dir`/`--control-dir`). New installs default to `~/.local/share/agentworks/releases`, `~/.local/state/agentworks/service` and the current directory. The installer does not create `service.env`, enable lingering or alter live data. User service lifetime after logout depends on the host's user-manager/linger policy.

Inspect with `systemctl --user status agentworks.service` and `journalctl --user -u agentworks.service`.

### Unit contract

- `Type=notify`: the server sends `READY=1` once its listener is serving. Before that it repoints `stable` at its own release. A start that does not reach readiness within `TimeoutStartSec=60` fails.
- `ExecStart=<releases>/current`. A restart request repoints `current` at the requested release, writes a dispatch ticket and exits 75. `RestartForceExitStatus=75` makes systemd start the successor regardless of `Restart=`.
- `ExecStopPost=-<releases>/stable service --rollback`: when the unit stops for any reason other than success or exit 75, `current` is repointed at `stable`. A release that fails to start therefore gives way to the last release that became ready, and `Restart=on-failure` starts that one.
- `StartLimitBurst=5` in `StartLimitIntervalSec=120` bounds crash loops. Deliberate restarts count towards it too.
- `KillMode=control-group`, graceful SIGINT and `TimeoutStopSec=15`: systemd owns every harness and terminal process in the cgroup.

The server advertises restart support only when it is a direct child of systemd in a `.service` cgroup with `NOTIFY_SOCKET`, `INVOCATION_ID`, `AGENTWORKS_RELEASE_DIR` and `AGENTWORKS_SERVICE_DIR` set. Descendants that inherit that environment (harnesses, test servers) are not supported.

The service directory holds the dispatch ticket and backups. A release name is an executable basename directly in the canonical release directory. Escaping symlinks, missing files, directories, nonexecutables and group/world-writable executables are rejected. A bounded `--service-protocol` metadata probe (protocol 2, two seconds, 4096 bytes, null stdin/stderr) rejects incompatible builds before accepting a request and again before dispatch. Releases built for the Python supervisor report protocol 1 and are refused as restart targets. Treat release files and the directory as operator-owned immutable artifacts. Same-user filesystem mutation is outside the existing trust boundary. For manual recovery, stop the unit, inspect the journal and database, point `current` (and `stable`) at a known compatible release, or rerun `service --install` from it, then start the unit. No automatic data rollback occurs.

## API and decisions

`GET /api/service/restart` returns instance ID, PID/executable, live systemd support, readiness, active turn/approval/harness/preparation counts, drain state, recent durable requests and manager errors. Reading never starts or repeats work.

`POST /api/service/restart` accepts:

```json
{"requestId":"stable-human-action-id","mode":"wait","expiresInSeconds":300,"release":"agentworks-REVISION"}
```

`release` may be omitted to keep the current immutable executable. `mode` is `wait` or `interrupt`; expiry is 5–900 seconds. IDs bind to the full payload and actor: identical retries return the original result, changed payloads conflict, and concurrent pending requests conflict. Requests are persisted with FULL synchronous SQLite commits before acknowledgment; dispatch intent, an fsynced atomic dispatch ticket and the `current` link update precede exit 75. The Settings button requires explicit global impact confirmation; polling and reload only read GET, never replay POST.

`wait` blocks new session/turn/model/discovery/automation starts through shared backend paths, including private MCP and usage-limit scheduler, then waits for active turns, approvals, harness startup and workspace preparation. Approvals and Stop remain available. Expiry, requester Stop/suspend and live grant revocation cancel pending work. `interrupt` deliberately stops all harnesses and approvals, recording interrupted work without replay. A preparation already running finishes under its workspace lock before a restart decision can dispatch. New starts conflict while draining; they are not silently queued.

`POST /api/service/restart/cancel` accepts `{"actor":"human","requestId":"…"}` or an attributed actor ID from status to cancel that request/handoff. Once dispatching, ownership has transferred to systemd and cancellation is too late. Native Stop and suspend also cancel the requesting session's pending handoff. A storage failure pauses the manager visibly and blocks starts until manual inspection; it does not repeatedly shut down workers or retry mutations.

Before dispatch, the manager captures catalog JSON and SQLite using `VACUUM INTO`, verifies `integrity_check`, compares complete ordered contents of every application table (including history, grants, completion claims, timers in session state and restart records), verifies JSON roundtrip, and fsyncs the backup. The private backups use directory0700/file0600. A preflight snapshot must succeed before any harness is stopped; a final snapshot records shutdown state. Nothing restores or rewrites the live database from a backup automatically. Completed snapshots carry checksummed completion manifests. The offline [retention and staged recovery CLI](RETENTION-RECOVERY.md) previews configurable keep-count retention and restores/migrates into a new directory; pruning requires explicit `--apply`. Nothing replaces live data automatically.

A successor consumes the dispatch ticket at startup (every start deletes it, so a later cold start never reuses it). If the ticket matches the request identity and requesting instance, is less than ten minutes old, and the successor runs the requested executable, the request becomes `ready`. If the requested release failed and was rolled back, the server that systemd started instead marks the request `uncertain` and names both releases. An interrupted drain/unknown dispatch also becomes `uncertain`; it never automatically dispatches again. If both the new and the stable release fail, the start limit stops the unit: inspect the journal, repair manually and start the service. No paid model is used for health checks.

## Explicit coordinator permission and native handoff

`restartService` defaults false for old, new, child and automation sessions. **Agents & inbox** has a separate checkbox labeled GLOBAL/all projects. Existing coordinator presets exclude it. Existing communicate/start/prompt/cancel/runAutomations grants do not imply restart. The private bridge checks the live grant for tools/list and each dispatch; existing cache/credential isolation stays unchanged. It attributes the request to the authenticated requesting session, never a borrowed inbox identity.

The three gated MCP tools are `get_service_status`, `restart_service` and `cancel_service_restart` (own request only). `restart_service` returns durable request identity immediately; the manager, not the requesting tool, owns dispatch. Status is read-only and can be polled inside the current turn. New turns remain blocked during drain.

For unattended self-update, explicitly request `mode:"wait", handoff:true, handoffDeadline:<epoch milliseconds within 30 minutes>`. Only the requesting MCP session can opt into its own continuation. Finish the current turn after accepting the request. After restart, once the successor that consumed the matching ticket has recorded itself as `stable` and sent `READY=1`, the manager opens that same saved native provider ID with its original cwd and grants, checks capacity/root/harness availability, and submits exactly one fresh attributed instruction to inspect the assignment and current tool results before continuing unfinished authorized work. It never resubmits the old prompt/tool. No missing or unsupported native context is replaced with a fresh context. No other session, approval or queued input is resumed by this mechanism; separately human-authorized usage-limit policies retain their own scheduler behavior.

The handoff claim is persisted before dispatch. A crash during opening/claimed/dispatched handoff becomes visibly uncertain on the next startup, and never auto-replays. Stop, suspend, cancellation, revocation and deadline take precedence under the shared locks. A failed/ambiguous send pauses for manual inspection. `dispatched` describes submission, not completion of the coordinator assignment.

## Deterministic acceptance

Use existing dependencies; no installation is required:

```sh
CARGO_TARGET_DIR="$PWD/target" cargo build --locked --offline
CARGO_TARGET_DIR="$PWD/target" cargo test --locked --offline service_restart
node scripts/service-restart-smoke.mjs
```

The fixture uses port4441, a transient `systemd-run --user` unit with the installed unit's restart contract, private temporary catalog/SQLite/demo state/releases/service directory, local demo native sessions and no paid inference. It covers unsupported direct server, invalid/missing releases, durable retry/payload conflict, concurrent requests, approval drain/start gates/cancel/expiry/revocation/Stop, child default grants, native handoff, process/release restart, preserved history/native context/grants/timer state, backup integrity, cold-start release retention, failed successor rolled back to the stable release, uncertain recovery, mobile dark controls and browser GET-only reconnect. All fixture processes close in `finally`; no live4318 access occurs. The embedded JS/CSS assets require rebuilding the Rust executable after every web change.
