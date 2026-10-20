# HTTPS subpath hosting and PWA installation

Agentworks supports direct root hosting and TLS proxies that strip a public mount,
including `https://agents2.pve.n3far.io/a1b918dd-2b91-4fc2-8822-e6a559503040/`.
The backend can listen on `0.0.0.0:4317`; it serves HTTP, not TLS. Keep that port
private to the trusted proxy/VPN. Assets are compiled into the executable: rebuild
and release the executable to apply frontend changes.

## Public mount and proxy contract

The proxy owns the public mount and its trailing-slash redirect. It must strip
that prefix exactly once before forwarding. The browser derives its canonical
application base from the public `hosting.js` module URL; assets, imports, APIs,
WebSockets, previews/downloads and home links resolve against that base. No HTML
`<base>` is used (the CSP forbids it). A mount such as `/a1b918dd-.../` works without
putting a UUID into backend API routing or trusting `X-Forwarded-Prefix`.

The proxy must overwrite `X-Forwarded-Proto` and `X-Forwarded-Host` with singular
values and preserve the browser's `Origin`, including on WebSocket upgrades. The
backend validates writes and WebSocket origins against the resulting public
origin. It currently trusts well-formed forwarded headers without checking the
proxy peer against an allowlist, so backend isolation and header overwriting are
required. Origin checks and UUID paths are not authentication: retain the hosting
layer's authentication/access control. `AGENTWORKS_AUTH_MODE` controls provider
credentials; it does not authenticate public web clients. The internal MCP bridge
remains private and does not use the public mount.

Example Traefik **dynamic file configuration** for the supplied host/mount:

```yaml
http:
  routers:
    agentworks:
      rule: "Host(`agents2.pve.n3far.io`) && (Path(`/a1b918dd-2b91-4fc2-8822-e6a559503040`) || PathPrefix(`/a1b918dd-2b91-4fc2-8822-e6a559503040/`))"
      entryPoints: [websecure]
      tls: {} # use your existing certificate configuration
      # Keep your existing authentication middleware before these middlewares.
      middlewares: [agentworks-slash, agentworks-strip, agentworks-origin]
      service: agentworks
  middlewares:
    agentworks-slash:
      redirectRegex:
        regex: '^(https?://[^/]+/a1b918dd-2b91-4fc2-8822-e6a559503040)(\?.*)?$'
        replacement: '${1}/${2}'
        permanent: true
    agentworks-strip:
      stripPrefix:
        prefixes: [/a1b918dd-2b91-4fc2-8822-e6a559503040]
    agentworks-origin:
      headers:
        customRequestHeaders:
          X-Forwarded-Proto: https
          X-Forwarded-Host: agents2.pve.n3far.io
  services:
    agentworks:
      loadBalancer:
        # Use the backend address reachable from Traefik, not 0.0.0.0.
        servers:
          - url: http://127.0.0.1:4317
```

Configure the existing HTTP entry point to redirect to HTTPS. Use a reachable
private address instead of loopback if Traefik runs in another container/host.
Traefik forwards WebSocket upgrades without a separate WebSocket service. Use
this as an integration example, not as a replacement for existing TLS/auth rules.
No live proxy settings are changed by the source implementation.

Browser state is namespaced by the public mount: local/session storage and
notification IndexedDB claims cannot collide between mounts. Existing root-hosted
keys are preserved. A new mount intentionally starts with its own preferences and
selection; root state is not copied into it. Subpaths share a browser origin and
are not security boundaries: use separate origins for mutually untrusted apps.

## Installation support

PWA support is **enabled by default**: manifest discovery, app/touch icons, theme
metadata, standalone display and a service worker. No deployment flag is needed.
Set `AGENTWORKS_PWA=0` on the backend process to opt out, then restart it through
the normal release process. Only the value `0` disables support; leaving the
variable unset enables it. Ordinary browser/root/mounted operation also works
with PWA support disabled.
There is no in-app install button: use the browser's install/Add to Home Screen UI.

The manifest uses relative `start_url`, `scope` and icon paths. Its identity is the
resolved `start_url` (the standard fallback when `id` is absent), so each mount has
a stable, distinct identity without deriving URLs from forwarding headers. Keep
the mount and start URL stable across releases. A move to another mount/origin is
a new installation. The worker script is served directly as JavaScript and its
scope is exactly the public app directory. A mounted worker never claims `/` or
another deployment. Same-origin worker/manifest assets work with the existing CSP.

The worker is **network-only**: it has no fetch handler, Cache Storage, push or
background sync. Installation does not provide offline launch or agent execution.
Transcripts, settings, file downloads, approvals and control API responses are
not cached. Prompts and other mutations are never queued/replayed by the worker.
An open page retains its existing connection/reconnect behavior and session drafts;
a cold offline launch can show the browser's network error. Push delivery while
the app is closed is separate work; see [NOTIFICATIONS.md](NOTIFICATIONS.md).

Worker updates wait for the user to choose the refresh button; they never force
an active page reload. Browser session drafts and pending request IDs survive
refresh. Set `AGENTWORKS_PWA=0` to disable support. On the next online
visit the page removes only the exact Agentworks worker registration for that
mount; it leaves other workers, caches and browser state alone. Existing OS app
icons must be removed by the user. Existing open/offline pages cannot receive a
server-side disablement until they revisit the server.

HTTPS with a trusted certificate is required outside localhost/loopback. HTTP
LAN/VPN IPs are not secure contexts even over an encrypted VPN. Browser install
UI and capabilities differ: Chromium supports manifest-based installation,
Safari offers Add to Dock/Home Screen, and some browsers only create shortcuts.
iOS/iPadOS 26 also lets users open any Home Screen site as an app. Do not interpret
successful installation as evidence that mobile/background notifications work.

## Verification

`tests/hosting.spec.mjs` starts its own backend on 4423, an HTTPS StripPrefix proxy
on 4424, and temporary catalog/database/demo state. It exercises root and two
mounts with the PWA flag unset (the default), scoped manifest/icons/worker,
origin-preserving mutations, WebSockets,
encoded file downloads, query-preserving slash redirects, state isolation,
network recovery, explicit upgrades and selective disablement. It uses demo
sessions and no paid provider inference. `openssl` and Playwright Chromium are
required. Its self-signed fixture certificate is ignored only by the test browser.

```sh
cargo test --locked --offline
cargo build --locked --offline
node node_modules/@playwright/test/cli.js test tests/hosting.spec.mjs tests/notifications.spec.mjs
```

Before release, manually install on the intended desktop/mobile browsers using
the trusted public HTTPS origin. Check launch name/icon/start URL, standalone
layout, keyboard/safe areas, session selection, draft preservation, proxy auth,
previews/downloads, and reconnect/approvals. Confirm requests stay inside the
public mount. Check two mounts' identities and worker scopes in developer tools.
Validate proxy header overwriting and backend isolation separately. Native install
UI, certificate trust and OS notification display are not proven by headless tests.

Validation for this implementation (2026-10-19): locked offline build and all 90
Rust tests passed. The full isolated verification on port 4495 passed its smoke
checks, six supervisor tests, restart fixture, 121 browser tests (including eight
hosting and 25 notification cases; one opt-in OpenCode case skipped), and graceful
shutdown with an open WebSocket. Chromium's processed manifest confirms distinct
mount-scoped identity, start URL and scope. Formatting, JavaScript syntax and
whitespace checks passed. No live release, proxy settings or service data changed;
real-device installation and the production slash redirect remain deployment checks.

After switching to PWA-on-by-default, the locked offline build and all 33 hosting
and notification browser tests passed. The hosting fixture explicitly unsets
`AGENTWORKS_PWA` for enabled checks and sets it to `0` for opt-out/worker cleanup.
