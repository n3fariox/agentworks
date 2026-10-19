# Codex subscription quota: blocked implementation proposal

Status: implementation blocked by the installed adapter's public interface.
This proposal changes no runtime behavior. The investigation branch was clean
and fast-forwarded from `f5f2c2a` to main `5f44324` before this assessment.

## Evidence and blocker

The installed, lockfile-pinned `@agentclientprotocol/codex-acp` is 2.2.2.
Its package manifest ships `dist/index.js`, README, LICENSE and package.json;
`main` and `bin` both point to the CLI bundle. That entry point unconditionally
starts the CLI/server; it does not export an embeddable adapter or plugin API.
The following line references are to that installed version's `dist/index.js`
under `node_modules/@agentclientprotocol/codex-acp/`, not tracked source:

- `30344`: `createRateLimitsMap` retains `rateLimitsByLimitId`, falling back to
  the legacy `rateLimits` response.
- `31941`: `handleRateLimitsUpdated` caches snapshots in
  `sessionState.rateLimits`, keyed by limit ID.
- `31393`: `account/rateLimits/updated` updates that cache and returns `null`;
  it emits no ACP event.
- `31919`: `createUsageUpdate` emits genuine context `used` and `size` only.
- `38884`, `39030`, `36738`: `/status` explicitly reads account limits and
  formats transcript text. It is not a structured cache-read interface.
- `42515`: extension dispatch exposes authentication, steering, async-task
  stop and goal control, but no subscription quota/cache method.
- `45365`: `_meta.quota` contains **prompt token counts**, not subscription
  rate limits. It cannot be used as a subscription quota source.
- `45632` onward: CLI startup and request registration expose no callback for
  transforming the cached quota into ACP metadata.

The documented `CODEX_PATH` selects a Codex executable; it does not expose the
adapter's cache. An ordinary ACP proxy sees only events after the adapter has
discarded quota notifications. Capturing native traffic through an executable
proxy would add a second protocol boundary and lifecycle/routing implementation;
it is not a supported cached-quota extension to this adapter.

Agentworks `src/runtime.rs:401` already serializes and forwards typed ACP updates.
The pinned Rust ACP schema supports `UsageUpdate._meta`; a new generic transport
is unnecessary. `web/usage-status.js` currently consumes `usage_update` and only
normalizes `_claude/rateLimit`. A UI-only parser or synthetic fixture cannot make
the absent Codex subscription data arrive.

Implementing the requested path now would require modifying bundle internals,
maintaining a private adapter fork/source build, or intercepting its native
transport. There is no supported tracked extension in the installed package.
Per the requested stop condition, no node_modules patch, bundle rewriting,
vendored adapter, speculative UI parser or new runtime path was introduced.

## Concrete minimal proposal

Have the adapter publish a supported namespaced metadata contract in a pinned
release (or explicitly authorize maintaining a small source fork separately).
Proposed key, **not implemented or currently supported**:

```json
{
  "_meta": {
    "_codex/subscriptionLimits": {
      "version": 1,
      "snapshots": [
        {
          "limitId": "codex",
          "limitName": null,
          "observedAtMs": 1792376797970,
          "snapshot": {
            "primary": {
              "usedPercent": 15,
              "windowDurationMins": 300,
              "resetsAt": 1792385436
            },
            "secondary": {
              "usedPercent": 67,
              "windowDurationMins": 10080,
              "resetsAt": 1792874966
            }
          }
        }
      ]
    }
  }
}
```

The adapter would attach its cached snapshots to existing genuine parent-session
`usage_update` events, retaining the actual context fields exactly as today. Set
each snapshot's observation time when its native rate-limit notification arrives,
not when a later token event repeats the cache. Repeated equal percentages may
still represent a new provider observation; compare observation identity/time,
not percentages alone. Never infer observations for untouched cache entries.
Do not send fake `used`/`size` for a quota-only notification. This minimal passive
path updates the display on real usage events; it does not promise idle refresh.

Agentworks can then normalize that documented metadata into its existing usage
display, without changing context processing or fetching any additional data:

- Keep account limit IDs/names and the `primary`/`secondary` identity. Retain
  actual `windowDurationMins`; do not assume every primary is 5h or secondary 7d.
- Codex `usedPercent` is already 0..100. Do not multiply by 100 as for Claude's
  utilization fractions. Validate finite values within bounds and preserve zero.
- Convert present valid `resetsAt` **seconds** to milliseconds once. Missing
  percent, reset, duration or window remains unknown/absent, never zero by default.
- Preserve unknown windows and distinct limits rather than overwriting a single
  hardcoded 5h/7d slot. Keep existing Claude behavior intact.
- Track quota source observations separately from context freshness. Context-only
  updates and repeated cached metadata must not advance quota freshness. A new
  observation of one limit must not freshen another limit or its missing fields.
- Replay must not treat browser receipt as a new provider observation. Preserve
  explicit historical source time if trusted; otherwise use unknown freshness.
  Clear per-session display state on switching sessions and snapshot replacement.
- Do not query account state, parse `/status`, scrape logs, poll, or add inference.

If idle updates become a requirement, agree on a separate supported structured
notification with the adapter rather than fabricating context values.

## Required acceptance checks after adapter support exists

Add tracked fixtures for real adapter metadata and test the complete adapter-to-
ACP-to-Rust-to-browser path with deterministic native notifications (no account
query or inference). Include:

1. Native 0/15/100 percent; primary and secondary; nonstandard durations; named
   and unnamed multiple limit IDs; reset seconds conversion; missing/null fields;
   malformed/out-of-range values. Preserve the established context output.
2. A quota observation followed by context-only and identical cached snapshots:
   context advances, quota time does not. A new source observation advances only
   its reported quota. Partial fields must not freshen retained older values.
3. Replay/reconnect does not freshen quota. Switching A to B clears A's quota;
   switching back replays A with historical/unknown freshness. Parent/subagent
   attribution must not let child data overwrite the selected session.
4. Reproducible clean installation of the pinned supported adapter, then focused
   unit/browser checks. No postinstall bundle surgery or node_modules-only fix.

These Codex tests were not added as passing implementation tests because no
supported producer exists; they would exercise a made-up contract only.
Existing `node tests/usage-status.test.mjs` is the relevant regression check for
the unchanged display. No browser/server smoke requiring new sessions is needed
for this proposal-only change.

## Live-data limitation

The diagnosis examined recorded native rollout observations, not a fresh account
query. Manager rollout
`/home/captain/.codex/sessions/2026/10/18/rollout-2026-10-18T10-51-28-01a14fb5-f8ee-7ac0-9ba0-956079843d48.jsonl:2831`
at `2026-10-19T02:26:37.970Z` recorded 15% used for a 300-minute primary and
67% for a 10080-minute secondary. All 801 Codex ACP usage events inspected at
that checkpoint contained only `sessionUpdate`, `used`, and `size`.
These are historical evidence, not current account balances. The proposed
extension has not been implemented, run against an account, or verified end to
end. No packages were installed, service settings changed, services restarted,
additional sessions started, or changes merged/deployed.
