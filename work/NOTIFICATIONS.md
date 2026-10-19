# U-02 browser notifications

Opt in under Settings → Notifications, then use **Allow notifications in this
browser** to request browser permission. The request runs directly in that click,
before any asynchronous work. Loading the app, opening Settings, receiving an
event and changing the saved policy never ask for permission. The policy toggle
saves immediately; the general Save settings button is not required. Both saved
policy and permission for this browser profile/origin must allow notifications.
A denied permission is not requested again: change this site's notification
permission in browser settings, then reopen Settings. A dismissed prompt can
remain ungranted; the app cannot distinguish that from never having asked.
The **Send a test notification** button attempts direct browser display even if
the saved event policy is off. Constructor exceptions and asynchronous display
errors are reported, but browser/OS Do Not Disturb or notification settings can
still hide a notice. Settings also reports API failures and stream connection
state. Opening Settings again retries loading policy; reloading reconnects setup.

## Secure contexts and HTTPS hosting

The browser must report `window.isSecureContext === true` and expose the
Notification API. A trusted HTTPS page normally qualifies. HTTP loopback URLs
such as `http://localhost:4317` and `http://127.0.0.1:4317` also normally qualify.
An HTTP LAN/VPN IP or ordinary hostname does not qualify, even over an encrypted
VPN. Localhost refers to the browser's own machine; use local port forwarding
when the app runs elsewhere. Permissions are separate across origins, ports,
browser profiles and devices. See [secure contexts](https://developer.mozilla.org/en-US/docs/Web/Security/Defenses/Secure_Contexts)
and [permission requests](https://developer.mozilla.org/en-US/docs/Web/API/Notification/requestPermission_static).

The backend serves HTTP, not TLS. For **root-hosted** HTTPS through a TLS proxy,
forward requests and WebSocket upgrades with the browser's `Origin` unchanged,
and set singular `X-Forwarded-Proto` and `X-Forwarded-Host` headers. The backend
derives the public origin from those headers and requires an exact match with
`Origin`; changing the proxy's public host or port needs no app configuration
change. The proxy must overwrite these headers rather than append client-supplied
values. The client uses `wss:` automatically from an HTTPS page.

Forwarded headers are a deployment trust boundary: keep the backend private so
untrusted clients cannot send requests directly with forged forwarding headers.
Origin validation is not authentication; put access control at the proxy or use
the app's authentication mode. Direct HTTP Host/Origin checks remain in force.
This supports root hosting, not arbitrary subpaths or PWA hosting; UUID subpath
hosting remains planned in [HOSTING.md](HOSTING.md).

## Delivery and limitations

The server sends turn completion, human approval/question requests and failures
(including timeout and interruption) through a separate same-origin WebSocket,
covering sessions other than the selected session. Streaming content, starts and
completion claims do not produce extra notices. The selected session is quiet
while its tab has focus. Clicking a notice focuses the tab and selects its source
session, fetching it if the sidebar has not refreshed yet.

Saved settings default off. The retained API supports category filters, project
overrides and muted sessions; the settings UI offers the global switch. Dedicated
notification saves preserve roots/models/timeouts, and ordinary settings saves
preserve the notification policy. Existing catalog files need no migration.

Tabs atomically claim stable session/event IDs in IndexedDB before showing a
notice. This works without BroadcastChannel or Web Locks and retains the newest
1000 claims through reloads. Separate browser profiles/devices have independent
claims. If browser storage is unavailable, notices are suppressed to avoid
cross-tab duplicates. A crash after claiming can lose a notice. The stream has no
historical replay: disconnected browsers do not receive missed notices, and
closing every tab stops delivery. There is no push service or service worker.
Most mobile browsers require service-worker notifications and may reject direct display; the
app reports that failure when it occurs. Browser/OS delivery remains best effort.
HTTPS and permission alone do not provide mobile/background push support. See
the [Notification constructor documentation](https://developer.mozilla.org/en-US/docs/Web/API/Notification/Notification).

## Recovery and verification

This implementation recovers U-02 from `fca994b` onto main base `a3c90d8`, retaining
the newer Claude permission-mode defaults. The old implementation had never been
merged or deployed: the investigated live release `agentworks-054714a` returned
404 for the notification module/API. That release is not changed by this work.
Assets are embedded in the executable, so copying a JS file into a source tree
does not update a running release.

Deterministic coverage lives in `src/notifications.rs` and
`src/browser_origin.rs` and `tests/notifications.spec.mjs`. The browser suite owns ports 4413 (HTTP) and 4414 (TLS proxy) with temporary
catalog/database/demo state, fakes the OS Notification surface, and exercises real
cross-tab IndexedDB claims, concurrent and replayed events, permission gestures,
unsupported/insecure contexts, actual demo approvals/failures, source navigation,
project/session rules and process-restart persistence. The full Settings flow is
tested in the embedded app over localhost and HTTPS, including a real TLS proxy
preserving Host/Origin, denied permission, display/API failures and forged origins.
TLS fixtures require `openssl`; only their temporary self-signed certificate is
ignored by the test browser. OS display is mocked, so a real desktop browser must
still verify Allow → test notification → background demo approval/turn completion
on the intended origin. Keep another session selected or blur the tab during that
check. No provider inference is used. Run `cargo test --locked`, `cargo build --locked`, then
`npm run test:notifications`; use `scripts/verify.mjs` on an isolated port for the
combined regression suite. Clear inherited service supervisor/release variables
when running standalone test processes.

Validation on the recovered branch: `cargo test --locked --offline` passed all 85
Rust tests; `cargo build --locked --offline`, `cargo fmt --check`, JavaScript syntax
and whitespace checks passed. The isolated `scripts/verify.mjs` run on port 4498
passed API/MCP/settings/import/workspace/completion/worktree/continuation/lifecycle
checks, six supervisor tests, fixture restart checks, all 25 notification browser
tests within the full 101-passed browser suite (one opt-in OpenCode test skipped),
and graceful WebSocket shutdown. Test data/builds stayed in this worktree or
temporary directories; no live release, service configuration or data was changed.
