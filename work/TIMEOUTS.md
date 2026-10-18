# Execution timeouts (L-07)

Settings exposes a saved default and an optional override for the selected session. A turn captures its policy when it launches; subsequent edits apply to later turns. Clearing the override restores the saved default. The default preserves the former 900-second total limit and 15-second cancellation grace, counts approval waits in the total, and has no inactivity warning or timeout.

The total limit can be disabled. Inactivity uses meaningful output rather than quota/status heartbeats, empty updates or repeated tool snapshots. Approval waits pause inactivity; the total-limit checkbox separately chooses whether those waits count toward total duration. Current supported question/approval UI uses the existing pending approval mechanism. Quiet tools can be healthy, so inactivity stopping is optional. Elapsed clocks are monotonic and per-turn activity is released on success, error or harness cancellation.

Policy and warning notices remain in the transcript. On expiry Agentworks requests cancellation once and waits for the configured grace period, closing an unresponsive harness afterward. A timeout is recorded even if the provider subsequently returns success. It never repeats a tool or schedules usage-limit continuation. Existing Stop/Suspend controls and saved native conversations remain available.

## API

- `GET /api/timeouts`: returns `{default: policy}`.
- `PUT /api/timeouts`: validates and persists a default policy. General settings saves use the same validation and preserve an omitted timeout policy.
- `PUT /api/sessions/{id}/timeouts`: accepts `{policy: policy}` or `{policy: null}` to clear the override. Returns the saved override, effective next-turn policy and `appliesTo: "nextTurn"`. A failed session write returns an error without applying the requested policy.
- Session snapshots expose `timeouts` and `effectiveTimeouts`. `timeout_policy` events record the policy fixed for an individual turn; these are distinct from later settings edits.

Policy fields use seconds: `totalSeconds` (null or 60–604800), `totalIncludesHumanWait` (boolean, default true), `inactivityWarningSeconds` (null or 30–86400), `inactivityTimeoutSeconds` (null or 60–86400), and `cancelGraceSeconds` (5–300). A configured warning must precede the configured inactivity timeout.

## Verification

Use locked offline Rust build/tests and the existing Playwright browser cache. `tests/timeout-controls.spec.mjs` runs its own isolated server on port4472. `node scripts/timeout-acceptance.mjs` runs an isolated fixture on port4487 for approximately one minute: both settings paths, session override persistence, injected SQLite write failure, same-native-context restart, real timeout cancellation, paused approval waits and prevention of quota replay. Fixtures use temporary state and demo agents without model inference or live-service access.

Progress parsing and richer terminal-output normalization remain U-05 work. Tool output unavailable through the harness cannot reset inactivity until it is reported. There is no queued-input implementation or automatic timeout retry.
