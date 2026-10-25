# Future work

Remaining candidates retained from the execution backlog. Check the implementation before selecting a task. Completed feature contracts are listed in the [documentation index](README.md).

Chat attachments (L-03) are implemented. L-06 is considered covered by the existing cross-harness fork flow, per the product decision; no separate session-switch action is planned. The manual ordered-recipe editor and weekly [scheduled triggers](SCHEDULED-AUTOMATIONS.md) (A-01) for automations are also implemented; only event triggers remain future work.

| Area | Remaining direction |
| --- | --- |
| Usage (L-04) | Richer provider metadata and supported account observations. Preserve source freshness and distinguish session cost from account spend; see [quota limitations](DASHBOARD-QUOTA-REFRESH.md). |
| Tool progress (U-05) | Extensible parsers only when a concrete need warrants them; built-in Bazel progress is implemented. See [tool activity](TOOL-ACTIVITY.md). |
| GitHub triggers (A-02) | A small supported event set, authenticated polling/webhooks, durable event IDs, and existing run/approval limits. |
| Retention and recovery (R-02) | Configurable retention, restorable exports/backups, and tested upgrades. Supervised restart already creates verified private backups; retention is manual. |
| Authentication and isolation (R-03) | Stronger human authentication and process boundaries beyond the existing trusted-user/private-network model. |
| Remote hosts (H-01/H-02) | Worker-side file browsing, Git views/diffs, and MCP relay. Remote chat, worktree launch/cleanup, queued inputs, continuation, and inbox turns are implemented; see [supported scope](REMOTE-HOSTS.md#supported-scope-and-verification). |
| Portability | Native Windows delivery and host-specific validation. Linux remains the supported deployment. |
| Experiment E-01 | Starlark was adopted for typed U-05 progress parsing. Workflow interpretation remains prototype-only; see the [experiment record](experiments/001-starlark.md). |

A frontend framework rewrite, managed public hosting, team accounts, a full browser IDE, and PDF rendering remain outside the initial Linux release requirements.
