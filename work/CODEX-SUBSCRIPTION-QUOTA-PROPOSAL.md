# Codex subscription quota: implemented local contract

Status: implemented for review; no merge or deployment. The original stop
condition was revised to authorize a pinned local npm fork. This document
supersedes the blocked proposal committed as `cd79c67`. The branch was first
fast-forwarded cleanly from `f5f2c2a` to main `5f44324`.

## Reproducible adapter path

`package.json` and package-lock.json select `file:vendor/codex-acp`, local version
`2.2.2-agentworks.1`. The fork contains the existing upstream 2.2.2 packaged
bundle, LICENSE, README and manifest, a small helper, the exact bundle patch and
upstream SHA-256. There are no startup downloads, postinstall scripts or ignored
node_modules-only changes. `npm ci --ignore-scripts` installs the local CLI through
the existing adapter_path; offline install works with the existing npm cache.
No runtime dependencies were upgraded.

The bundled `CodexEventHandler` observes `account/rateLimits/updated` using
`observeSubscriptionLimits` alongside its unchanged upstream cache. Because the
upstream cache has no timestamps and does not retain omitted window fields, a
parallel session-state cache retains each reported field with its observation.
`createUsageUpdate` attaches that cache only to existing genuine parent context
updates; used/size calculation, child handling and null-context suppression are
unchanged. No quota-only context values are fabricated.

This addresses the documented upstream gap:
https://github.com/agentclientprotocol/codex-acp/issues/227

## Implemented metadata contract (version 1)

A genuine `usage_update` retains its real `used` and `size` and may include:

```json
{
  "_meta": {
    "_codex/subscriptionLimits": {
      "version": 1,
      "limits": [{
        "limitId": "codex",
        "limitName": null,
        "primary": {
          "usedPercent": {"value": 15, "observedAtMs": 1792376797970},
          "windowDurationMins": {"value": 300, "observedAtMs": 1792376797970},
          "resetsAt": {"value": 1792385436, "observedAtMs": 1792376797970}
        },
        "secondary": {
          "usedPercent": {"value": 67, "observedAtMs": 1792376797970},
          "windowDurationMins": {"value": 10080, "observedAtMs": 1792376797970}
        }
      }]
    }
  }
}
```

- A limit retains its ID and optional name. Primary and secondary stay distinct;
  the UI labels the actual duration, including nonstandard 90-minute windows.
  Different limit IDs cannot overwrite one another.
- `usedPercent` retains Codex's 0..100 scale (unlike Claude fractions). The UI
  validates finite values in that range, displays the complement as remaining,
  and preserves 0 and 100. Resets are native epoch seconds converted once to ms.
- Omitted fields/windows/limits retain their older values **and their own older
  timestamps**. Explicit null clears a field to unknown or removes a window.
  Missing/invalid percentages, resets and durations are never inferred as zero.
- Each scalar is stamped when a native push reports it. Later token updates
  repeat the cache without restamping. A sparse reset update cannot freshen a
  cached percentage or another window. A new provider observation can refresh an
  unchanged percentage; equal values alone are not used as observation identity.
- The emitted metadata is a deep copy, so future pushes cannot mutate history.
  Parent quotas are not attached to child or unrelated context updates.
- UI context processing and Claude quota behavior remain intact. The Codex UI
  keeps independent field times, labels them adapter-observed, and marks stale
  percentages independently of live context. Reset/duration freshness appears
  separately in details.
- Replay retains historical source timestamps and labels replay; receipt of a
  snapshot does not become a new quota observation. A replay can be recent if
  the original timestamp is recent; missing timestamps remain unknown. Session
  switching and snapshot replacement reset the selected session's display state.

This is a local versioned ACP `_meta` extension, not an upstream standardized
quota field. The existing typed Rust ACP receive/serialization boundary retains
it; no alternate transport is needed. `_meta.quota` from upstream is prompt
**token usage** and is deliberately not interpreted as subscription limits.

## Checks

Run from the worktree:

```sh
npm ci --offline --ignore-scripts --no-audit --no-fund
npm run test:codex-quota
node tests/usage-status.test.mjs
PLAYWRIGHT_BROWSERS_PATH=/home/captain/projects/agentworks/.cache/browsers node node_modules/@playwright/test/cli.js test tests/codex-subscription.spec.mjs
CARGO_HOME=/home/captain/projects/agentworks/.cache/cargo CARGO_TARGET_DIR=target cargo test --locked --offline runtime::tests::codex_subscription_metadata_survives_typed_acp_notification
cargo fmt --check
git diff --check
```

The adapter test executes the actual shipped event-handler class in isolation
without starting its CLI, stubbing only unrelated startup dependencies and time.
It checks native push handling, unchanged genuine context, no child quota leak,
cache immutability and sparse observations. It also verifies the bundle delta
against its pinned upstream hash. Tracked native fixtures cover primary,
secondary, standard/nonstandard windows, limit names/IDs, resets, nulls and sparse
updates. UI tests cover native percent scale, missing/invalid fields, independent
freshness, replay, reset-only changes and the real session-reset controller.
Chromium uses route fixtures only: no Agentworks server, provider or sessions.
The Rust test exercises the actual typed notification metadata boundary.

## Limitations and historical evidence

This passive path emits quotas on the next **genuine parent usage event** after
a native push. It does not promise idle refresh or emit quota-only notifications.
No `/status` prose parsing, log scraping, account queries, polling or inference
were added. Existing upstream commands and error-only continuation quota queries
were not changed or invoked by this extension.

The quota observation time is the adapter host's clock, not a provider-signed
measurement timestamp. A restarted adapter starts with an empty push cache;
persisted history can display historical quota until another push and real
usage event arrive. Older event history lacking this metadata stays unavailable.
The extension has deterministic handler/typed transport/UI coverage, but has
not been run against a live subscription or deployed to the running service.

Diagnosis evidence was recorded, not fetched afresh: manager rollout
`/home/captain/.codex/sessions/2026/10/18/rollout-2026-10-18T10-51-28-01a14fb5-f8ee-7ac0-9ba0-956079843d48.jsonl:2831`
at `2026-10-19T02:26:37.970Z` contained 15% used for a 300-minute primary and
67% for a 10080-minute secondary. All 801 Codex ACP usage events examined then
contained only `sessionUpdate`, `used`, and `size`. Those are historical values,
not current account balances. No services/settings changed, sessions started,
changes merged or deployment performed for this implementation.
