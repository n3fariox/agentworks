# Dashboard quota observation refresh

Implemented on `feature/quota-refresh-enterprise`, October 20, 2026.

The quota section has a dedicated **Refresh quota observations** button. It
rereads the local non-archived session list, provider configuration and retained
Claude/Codex session snapshots using GET requests with no-store caching. It
never reads the host/project catalog or rerenders those sections. Expanded
project details remain open. Refresh controls serialize reads to prevent an
older dashboard request from overwriting a newer manual quota refresh.

The UI labels local read time separately from provider observation freshness.
Replaying history never advances quota observation times. Each provider card
shows readable, unavailable and truncated history counts; partial history can
miss the latest report. Percentages remain top-level, selected per window and
never summed across sessions. Codex windows are ranked by the percentage cell's
adapter observation time before the session creation tie-breaker. Claude replay
has no stored source time and continues to show unknown freshness.

## Provider capability findings

Inspected the retained event consumer (`web/usage-status.js`), the tracked
Codex bundle and `dist/subscription-limits.js`, Claude ACP 0.93.0 installed
adapter source, and backend status/snapshot/runtime paths.

- Codex's native app-server supports `account/rateLimits/read` internally, but
  Agentworks has no standalone dashboard/backend read API for it. The adapter's
  `/status` route runs through the session prompt command path. This change
  invokes neither that path nor a new provider process. The retained version-1
  `_codex/subscriptionLimits` contract contains percentage, duration and reset
  cells, each with its own adapter observation time. It emits cached quota only
  alongside genuine parent usage events after provider pushes, so an idle
  account refresh is not promised.
- Native Codex `credits.unlimited` describes credits, not proof that all quota
  windows or account spending are uncapped. Its `individualLimit` display uses
  credits, not verified USD. Neither field is retained by the version-1 metadata
  extension. No dollar conversion or account-wide unlimited state is inferred.
- Claude's retained unified windows provide utilization fractions; replay has
  no source timestamp. SDK `total_cost_usd` is session cost, not account spend
  or an account spending limit. Scalar utilization has no verified scale.
- Backend `/api/status` reports adapter installation and authentication
  configuration only. It supplies no authoritative account plan, cap or spend.

Consequently current supported data cannot establish a first-class
**No usage cap** state or account **USD used / total**. The dashboard explicitly
shows **Reported quota windows**, **No cap reported** for readable complete
histories without windows, or **Quota unavailable** for missing/incomplete
observations without windows. Missing fields, enterprise plan names, session
cost and unlimited credits cannot establish an unmetered account plan. A future
provider contract must prove the scope, units and observation times before such
an account state or USD meter can be displayed.

No provider requests, prompts, account credential scraping, service restarts or
merges were performed. Tests use synthetic events and isolated local servers.
The carried Dashboard-first navigation, visible sidebar, active non-archived
turn lists and top-level quota improvements are included and preserved.

## Validation

All passed:

- `node web/dashboard-checks/model.test.mjs` (6 tests)
- `node tests/usage-status.test.mjs`
- `npm run test:codex-quota` (pinned adapter provenance and handler checks)
- `PLAYWRIGHT_BROWSERS_PATH=/home/captain/projects/agentworks/.cache/browsers node node_modules/@playwright/test/cli.js test tests/codex-subscription.spec.mjs`
- `PLAYWRIGHT_BROWSERS_PATH=/home/captain/projects/agentworks/.cache/browsers node web/dashboard-checks/browser.mjs`
- `CARGO_HOME=/home/captain/projects/agentworks/.cache/cargo CARGO_TARGET_DIR=.cache/b009-u10-target cargo build --locked --offline`
- Same Cargo environment: `cargo test --locked --offline runtime::tests::codex_subscription_metadata_survives_typed_acp_notification`
- `cargo fmt --check`, JavaScript syntax checks and `git diff --check`

Browser checks cover isolated quota reads, no host/project DOM replacement,
partial coverage, quota failure/recovery, no account spend inference, original
navigation/activity behavior, HTTPS proxy mounts, keyboard access and widths
320–1280 px. They assert only GET requests, no streams/account-service requests,
no credential/transcript rendering and no browser errors. Chromium initially
failed in the sandbox with `Operation not permitted`; both browser checks passed
with approved sandbox escalation. The Rust test emits an existing unused
`axum::Json` warning in unchanged `src/inbox_delivery.rs:1582`.
