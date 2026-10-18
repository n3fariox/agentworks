# Traefik subpath hosting and optional PWA

Status: planned requirements; no deployment, implementation or agent batch started. Support the existing direct/VPN setup and the user's public Traefik hosting convention, for example `https://hexfall.home.oksmiths.com/a1b918dd-2b91-4fc2-8822-e6a559503040/`. Traefik terminates TLS and may strip the UUID prefix before forwarding to the local app.

## Reference implementation

Reviewed local xcom-game sources: `tools/web/publish.py` uses relative asset references and registers the worker relative to the public page URL; `game/branding/web/manifest.webmanifest` uses relative start URL/scope; `game/branding/web/service-worker.js` currently uses network-only navigation handling; `tools/web/app.py` reads forwarded host/prefix for deployment metadata. Adopt the mount-awareness patterns, with Agentworks-specific API, WebSocket and origin protections. Its D058 document contains a proposed offline-game design; that proposal is not Agentworks's cache policy.

[Traefik StripPrefix](https://doc.traefik.io/traefik/reference/routing-configuration/http/middlewares/stripprefix/) reports the removed prefix in `X-Forwarded-Prefix`. [Manifest scope](https://developer.mozilla.org/en-US/docs/Web/Progressive_web_apps/Manifest/Reference/scope) and [start URL](https://developer.mozilla.org/en-US/docs/Web/Progressive_web_apps/Manifest/Reference/start_url) must resolve inside the intended public mount.

## Mount and proxy contract — R-04

- Direct root hosting remains supported. Establish one canonical external app base, using explicit deployment configuration and, when enabled, validated headers from configured trusted proxy peers. Keep public mount/URL generation distinct from the backend's stripped routing path; apply a prefix exactly once.
- Use relative module imports/static links and a shared URL helper for all API calls, event WebSockets, file previews/images/downloads, redirects and navigation links. Preserve encoded paths and query parameters. Avoid relying on every helper accepting root-relative `/api/...` URLs. A browser URL helper can resolve against the module/app base without requiring an HTML `<base>` tag; the current CSP forbids that tag.
- Normalize the entry trailing slash so `/<uuid>` reaches `/<uuid>/` without losing the prefix or query. Decide whether the redirect belongs to Traefik or the app and document the proxy setup. Do not add UUID assumptions to ordinary API routing.
- Update host/origin validation for configured external HTTPS origins and WebSocket upgrades. The current guard compares Origin against `http://Host` and requires the backend listener port, so changing frontend paths alone is insufficient. Trust forwarded host/proto/prefix only in the configured proxy mode; preserve direct-request protections and the private internal MCP bridge.
- Namespace local/session storage by deployment identity/mount to prevent session selections, drafts, pending request IDs and settings colliding when multiple instances share an origin. Subpaths are deployment boundaries for routing/state, not browser origin security boundaries. Document how the existing hosting authentication layer or app credentials protect the public agent-control surface; a UUID path does not replace authentication.
- Provide a reviewed Traefik example covering Host + mount matching, StripPrefix, TLS, HTTP/WebSocket forwarding and external origin configuration. Reuse an existing authenticated hosting layer where configured rather than removing Origin checks to make proxying work.

## Installable app — R-05

- A deployment-time flag enables manifest links, app/touch icons, theme metadata, standalone display and service-worker registration. Keep these optional for ordinary browser/direct hosting. Name configuration flags during implementation; none are available yet.
- Resolve manifest `id`, `start_url`, `scope`, icons and worker URL/scope against the public UUID mount. The worker must never claim `/` or a sibling app. Serve its script directly without redirects; check CSP and service-worker headers for the selected layout.
- Start with network-only behavior or a narrowly versioned static shell. Do not cache transcripts, settings, file previews/downloads or control API responses. Do not enqueue/replay prompts, approvals, worktree creation or other mutations through offline/background sync. Offline mode explains the missing connection and reconnects without duplicating actions; offline agent execution is not part of installation support.
- Preserve existing turns, drafts and pending request IDs across app/browser updates. Prefer an explicit refresh opportunity over a worker forcing an active page reload. Namespace any caches by mount and version. Disabling PWA support must retire only this deployment's worker/caches, without touching another app on the origin or deleting chat state.
- Installation support does not itself provide background WebSocket execution or push notifications. Coordinate browser notification requirements U-02 separately, including support detection and any eventual push subscription mechanism.

## Acceptance checks

Use isolated data and a proxy fixture with a stripped, non-root UUID mount. Verify root and mounted entry points, trailing-slash redirects, modules/CSS, API mutations, WebSocket reconnect/cancellation/approvals, Markdown images and file downloads. Assert no accidental requests escape to origin-root app endpoints.

Test external HTTPS origin/host validation plus rejection of untrusted/spoofed forwarding headers. Exercise two mounts on one origin to check storage, manifest identity, worker scope and cache isolation. Verify PWA disabled/enabled, refresh after an upgrade, cleanup after disablement, offline failure/reconnect without mutation replay, and desktop/mobile standalone layouts. Record browser-specific installation limits rather than promising identical install UI everywhere. No real provider inference is required for the routing/PWA checks.
