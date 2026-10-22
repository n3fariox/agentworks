# Dashboard quota observation refresh

The quota section has a dedicated **Refresh quota observations** button. It
rereads the local non-archived session list, provider configuration and retained
Claude/Codex session snapshots using GET requests with no-store caching. It
never reads the host/project catalog or rerenders those sections. Expanded
project details remain open. Refresh controls serialize reads to prevent an
older dashboard request from overwriting a newer manual quota refresh.

Quota meters show the provider-reported reset date/time in the browser's local
timezone and the time remaining as of the latest dashboard or quota-only read.
Missing or invalid timestamps display **Reset unavailable**. Past reset times
remain labeled as passed, awaiting a new provider report; passing a reset never
fabricates a zero-usage reading. Codex reset freshness is independent of usage
percentage freshness. Tests cover Claude seconds/milliseconds, Codex per-field
freshness, missing/invalid/past/zero timestamps and both refresh controls.

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

## Verification

Run `node web/dashboard-checks/model.test.mjs`, `node tests/usage-status.test.mjs`, and `npm run test:codex-quota`. Browser coverage lives in `tests/codex-subscription.spec.mjs` and `web/dashboard-checks/browser.mjs`; it uses synthetic events and isolated local servers.

Checks cover quota-only reads, partial history, freshness/reset handling, keyboard access, proxy mounts, and mobile widths. They do not establish live provider/account behavior.
