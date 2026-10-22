# Future work

Candidate work retained from the original execution backlog. This is a planning reference; check the implementation before selecting a task. Completed feature contracts are listed in the [documentation index](README.md).

| Area | Remaining direction |
| --- | --- |
| Attachments (L-03) | Image/file input with explicit storage, size/type limits, and harness capability checks. |
| Harness handoff (L-06) | Switch to another harness with an explicit context selection, linked lineage, and preserved original conversation. Existing [forks](FORKING.md) provide native and fresh-context branching. |
| Usage (L-04) | Richer provider metadata and supported account observations. Preserve source freshness and distinguish session cost from account spend; see [quota limitations](DASHBOARD-QUOTA-REFRESH.md). |
| Tool progress (U-05) | Extensible parsers only when a concrete need warrants them; built-in Bazel progress is implemented. See [tool activity](TOOL-ACTIVITY.md). |
| Scheduled automations (A-01) | Durable schedules/time zones, pause state, deduplicated dispatch, and explicit restart/missed-run behavior. |
| GitHub triggers (A-02) | A small supported event set, authenticated polling/webhooks, durable event IDs, and existing run/approval limits. |
| Retention and recovery (R-02) | Configurable retention, restorable exports/backups, and tested upgrades. Supervised restart already creates verified private backups; retention is manual. |
| Authentication and isolation (R-03) | Stronger human authentication and process boundaries beyond the existing trusted-user/private-network model. |
| Remote hosts (H-01/H-02) | Worker-side file browsing, Git views/diffs, and MCP relay. Remote chat, worktree launch/cleanup, queued inputs, continuation, and inbox turns are implemented; see [supported scope](REMOTE-HOSTS.md#supported-scope-and-verification). |
| Portability | Native Windows delivery and host-specific validation. Linux remains the supported deployment. |
| Experiment E-01 | Evaluate [embedded Starlark](experiments/001-starlark.md) before deciding whether to adopt an interpreter for progress parsers or workflows. No adoption decision has been made. |

A frontend framework rewrite, managed public hosting, team accounts, a full browser IDE, and PDF rendering remain outside the initial Linux release requirements.
