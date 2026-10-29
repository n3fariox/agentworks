# Session badges

The session browser reserves a third line below the title and provider/model for badges. Empty rows collapse. Optional HTTPS links open in a new tab without selecting the session.

## Setters from existing hooks

Existing progress, tool-header, inbox and chat hooks can update their session's badges:

```python
load("@agentworks//progress.star", "LineContext", "Progress")

def on_line(ctx: LineContext, line: str) -> Progress | None:
    if "FAILED" in line:
        agent.set_badge("build", "Build failed", tone = "danger")
    elif "SUCCESS" in line:
        agent.clear_badge("build")
    return None

agent.register_progress(on_line, command_pattern = "^make\\b")
```

`agent.set_badge(id, label, *, tone="neutral", tooltip="", url=None)` inserts or replaces that script's badge with this ID. `agent.clear_badge(id)` removes it. Omitting a badge from a later hook call does not clear it. There is no separate badge callback, and reading badges does not execute scripts.

Use **Settings → Starlark scripts → New script → Session badges** for this starter. Existing script editing, validation, enable/disable and priority controls apply. Repository scripts are never loaded implicitly.

The host supplies the target session from the running hook; scripts cannot choose another session. Effects are staged until the callback succeeds and its return value is accepted. Errors, invalid badge values and provisional progress results leave stored badges unchanged. Setters are unavailable during module registration. Startup replay of retained tool output does not change badges.

State is persisted per session and script and survives restart. A script can clear only its own badges. Disabled, deleted or changed script versions do not contribute their old badges. Among enabled scripts, priority descending then filename determines which duplicate ID is displayed; the native GitHub producer follows scripts. Up to 16 changes per invocation, 16 badges per source and 32 stored badges per session are allowed.

## Badge fields

| Field | Contract |
| --- | --- |
| `id` | Stable, 1–64 ASCII letters/digits, `.`, `_`, or `-` |
| `label` | Nonempty plain text, at most 32 characters, no control characters |
| `tone` | `neutral`, `success`, `warning`, `danger`, or `info` |
| `tooltip` | Plain text, at most 256 characters, no control characters |
| `url` | Optional absolute credential-free HTTPS URL, at most 2048 bytes, no whitespace/control characters |

The host adds `source` using the installed script filename. Badges contain plain text and links, without executable actions or HTML.

`GET /api/sessions/{id}/badges` reads stored state and returns `{badges:[...], badgesAt:<Unix ms>, badgeRevision:<number>}`. Successful hook changes push a `session_badges` update to the session's connected browser. Visible background rows refresh approximately once per minute with bounded concurrency; collapsed and offscreen rows are not polled. Script setting changes refresh visible rows. Setters do not add an independent timer or trigger.

## GitHub producer

Normal GitHub context collection writes native PR badges into the same store under the reserved source `@github`. These use IDs such as `github.pr.1234`, labels such as `PR#1234`, and the PR's HTTPS URL. Approval is green; no approval plus failed CI is red; awaiting review or changes requested is yellow. Draft, closed, unknown and stale states are neutral. Approval takes priority over CI failure.

GitHub collection, permissions, cache freshness and review-state calculation remain host responsibilities. Custom scripts can use the same ID at higher priority to override a native badge, or add unrelated badges without GitHub access.
