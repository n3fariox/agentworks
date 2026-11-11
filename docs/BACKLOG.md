# Future work

Remaining candidates retained from the execution backlog. Check the implementation before selecting a task. Completed feature contracts are listed in the [documentation index](README.md).

Chat attachments (L-03) are implemented. L-06 is considered covered by the existing cross-harness fork flow, per the product decision; no separate session-switch action is planned. The manual ordered-recipe editor and weekly [scheduled triggers](SCHEDULED-AUTOMATIONS.md) (A-01) for automations are also implemented; only event triggers remain future work.

| Area | Remaining direction |
| --- | --- |
| Usage (L-04) | Richer provider metadata and supported account observations. Preserve source freshness and distinguish session cost from account spend; see [quota limitations](DASHBOARD-QUOTA-REFRESH.md). |
| Tool progress (U-05) | Extensible parsers only when a concrete need warrants them; built-in Bazel progress is implemented. See [tool activity](TOOL-ACTIVITY.md). |
| Background jobs | When adapter-owned jobs finish, fold their result/output into the normal work log and show only currently active jobs in the background-jobs panel. Keep completed history available in the conversation without leaving a persistent finished-job count in the composer. |
| GitHub triggers (A-02) | A small supported event set, authenticated polling/webhooks, durable event IDs, and existing run/approval limits. |
| Retention and recovery (R-02) | Keep-count retention and staged backup restore/upgrade are implemented; see [R-02](RETENTION-RECOVERY.md). Remaining: age/space/default policy, portable exports including attachments/native context, and promotion tooling. |
| Authentication and isolation (R-03) | Stronger human authentication and process boundaries beyond the existing trusted-user/private-network model. |
| Project directories | Add a host-scoped folder picker for existing working directories and an option to create a new project folder beneath a configured allowed root. Keep project grouping separate from filesystem paths; validate every navigation/create operation against that host's roots. |
| Remote hosts (H-01/H-02) | Worker-side file browsing, Git views/diffs, and MCP capabilities beyond messaging. Remote chat, worktree launch/cleanup, queued inputs, continuation, inbox turns, and permission-checked MCP peer discovery/read/send/acknowledgment are implemented; see [supported scope](REMOTE-HOSTS.md#supported-scope-and-verification). |
| Portability | Native Windows delivery and host-specific validation. Linux remains the supported deployment. |
| Starlark scripts (E-01 follow-up) | Explicit priority/filename/declaration routing order is implemented. Agent additions need decisions on authorization, validation and review before any write path is enabled; see the [proposal](SCRIPT-PROPOSALS.md). Starlark was adopted for typed U-05 progress parsing; workflow interpretation remains prototype-only. See the [experiment record](experiments/001-starlark.md). |

A frontend framework rewrite, managed public hosting, team accounts, a full browser IDE, and PDF rendering remain outside the initial Linux release requirements.
