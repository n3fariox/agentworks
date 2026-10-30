# PR tracking and observation hooks

Agentworks tracks PR status on the main host independently of the browser. Once GitHub connections are configured, a scheduler checks non-archived sessions about once a minute using their origin, branch and explicitly attached PRs. Archived sessions are excluded from background reads. Remote workers provide Git metadata only; their GitHub credentials/configuration are never used.

The scheduler wakes every five seconds to find due sessions. Actual GitHub reads share the existing serialized transport, one-minute cache and account backoff. Sessions with the same repository/account/branch/attachments share a snapshot even when their checkout SHAs differ; checkout matching is calculated separately. Slow hosts and rate-limit backoff can delay refreshes. Browser reads use the same path and a per-session refresh lock, so they cannot race the background baseline.

GraphQL remains the transport for the combined PR, review and CI snapshot. This release does not implement REST ETags. Approval and CI remain scoped to the PR head SHA, and ambiguous branch matches remain visible instead of silently choosing one PR. Existing discovery limits apply: up to 20 branch candidates and 10 explicit attachments. Incomplete candidate lists do not prove that an older PR disappeared. Explicitly attach a PR to keep its identity pinned when branch inference is ambiguous.

Normal tracking updates native `PR#1234` badges and pushes the current GitHub context to connected session browsers. It also records meaningful differences: review state, CI rollup, draft/open/closed/merged state, PR head SHA, checkout matching and completeness flags. Changes to fetch timestamps alone do not create PR status events.

## Starlark hooks

Use **Settings → Starlark scripts → New script → GitHub PR events**, or register a callback in an existing script:

```python
load("@agentworks//github.star", "GithubContext")

def changed(ctx: GithubContext, event: dict[str, typing.Any]) -> None:
    pr = event["after"]
    if pr["review"] == "CHANGES_REQUESTED":
        agent.set_badge("review", "Changes requested", tone = "warning", url = pr["url"])
    else:
        agent.clear_badge("review")
    return None

agent.register_github(changed, events = ["pr.status_changed"])
```

`events` is an optional list of exact event names. Omit it (or pass `None`) to receive all observations. An explicit list must contain 1–6 supported names from the table below; unknown names, empty lists and non-string entries are rejected when installing the script. The host filters events before evaluating the callback. This saves callback execution; it does not change the shared GitHub polling cadence. Include `pr.tracked` and `pr.snapshot` if a handler also needs initial state.

Matching enabled GitHub callbacks run by priority descending, filename, then declaration order. One failing callback is logged and isolated from other callbacks, tracking and session operation. The callback must return `None`. `ctx` contains the session's `provider`, `cwd` without a trailing slash and a fresh empty `state`. Callbacks use the existing presentation-hook runtime limits.

The callbacks can call [badge setters](SESSION-BADGES.md), with the session and script ownership supplied by the host. `agent` is the host API namespace, not the selected browser session or the signed-in GitHub account. Before evaluating a callback, the host binds the session whose observation is being delivered. A successful setter commits to `(that session, installed script filename/hash, badge ID)`. Event contents cannot override this target. If two sessions track the same PR, the host delivers a separate observation to each session; each invocation updates its own badge state. They have no filesystem/network access, workflow action constructors or authority to start agent turns or write to GitHub. This phase exposes PR observations to hooks; automation dispatch and follow-up turns are later extensions.

## Event contract

| Kind | Meaning |
| --- | --- |
| `pr.tracked` | First successful observation of an associated PR; `initial: true` |
| `pr.snapshot` | Initial state for newly enabled/updated hooks, or refreshed state after source recovery; `initial: true` |
| `pr.status_changed` | A difference from the previous known PR status; `initial: false` |
| `pr.unavailable` | The source cannot currently supply status for a previously observed PR; `before` holds its last known state and `after` is null |
| `pr.untracked` | The source repository/account/branch changed, or a complete snapshot no longer associates the PR; `after` is null |
| `github.availability_changed` | Source availability changed; `before`/`after` are availability strings rather than PR objects |

A PR event includes a stable integer `id`, `repository` (`host`, `owner`, `name`), `connection` (`host`, `login`), `observedAt`, and `before`/`after` as applicable. PR snapshots and status changes also include `fetchedAt` and `changed`, an array of differing field names. PR status objects contain:

`number`, `url`, `state`, `draft`, `headSha`, `headMatches`, `review`, `reviewComplete`, `ci`, `checksComplete`.

`pr.untracked` and `pr.unavailable` retain the previous PR's repository/account identity even if the session has moved to another repository. Availability failures never become a closed PR, an empty authoritative list, or failing CI. Handle `pr.unavailable` when maintaining custom PR badges so stale custom status can be cleared. The editor's starter demonstrates this.

Initial snapshots establish current state; do not treat them as evidence that approval or another condition just occurred. A new script version or enabled script set can produce another initial snapshot. Hooks that should react only to later changes should check `kind` and `initial`.

These are **observed state differences**, not exact GitHub webhook events. A review/check transition that happens and reverses between polls may be missed. `observedAt` is local observation time, not GitHub event time; no labeling/review actor provenance is asserted. Unknown and incomplete data remain explicit. GitHub bodies, comments, credentials and raw API payloads are not included in event records.

## Persistence and recovery

The normalized baseline and new events commit in one SQLite transaction. Unchanged refreshes do not repeat status changes. Restarts preserve the baseline and pending delivery; an outage preserves the last known PR state and recovery supplies a fresh snapshot.

Delivery to Starlark is at least once. A crash after a hook runs but before its delivery acknowledgement can replay the same event ID. Badge setters are idempotent. A timed-out callback delivery reuses its existing interpreter-queue entry rather than enqueueing copies on every retry. Runtime/type errors in a callback are logged and acknowledged, avoiding a permanent retry loop.

The latest 100 journal records per session are retained after delivery, together with pending records. Deleting a session cascades its tracking state and journal. Schema v13 introduces `github_pr_tracking` and `github_pr_events`; existing backup/recovery preserves them, and older binaries refuse a v13 store.

## APIs and browser delivery

- `GET /api/sessions/{id}/github`: existing context response, now using the shared tracking path. Includes `tracking` with transport, interval and observation time. Explicit refresh remains supported.
- `GET /api/sessions/{id}/github/events?after=<id>`: up to 100 retained observations in ID order, with IDs suitable for deduplication/cursoring. This reads the journal without fetching GitHub or executing hooks.
- The session WebSocket carries unsequenced `github_context` updates and `github_event` observations separately from transcript events. The browser updates the PR panel and badges and emits a `github-event` DOM event containing `sessionId` and the observation. These pushes are transient; consumers that need recovery should read the journal with their last ID.

This adds no GitHub write API, agent grants, issue-label polling or automatic session starts. The [integration proposal](GITHUB-INTEGRATION.md) describes later event-driven automations, webhook ingestion and typed actions.
