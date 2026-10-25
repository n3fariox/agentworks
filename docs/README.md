# Documentation

Start with the [quick start](../README.md). Commands in these documents run from the repository root. Design references describe constraints and rationale; historical assessments may include proposals that have since shipped.

## Guides

- [Embedded Goose harness](RUST-GOOSE-ACP.md): provider configuration and the linked ACP runtime.
- [Agent setup](AGENT-SETUP.md): credentials, models, permission modes, commands, skills, and OpenCode.
- [User guide](USER-GUIDE.md): sessions, terminals, files, automations, workspaces, and communication.
- [Operations](OPERATIONS.md): local/remote hosts, directories, network access, persistence, and restarts.
- [Development and testing](TESTING.md): isolated verification, browser checks, and optional provider checks.
- [HTTPS hosting and PWA installation](HOSTING.md).
- [Supervised service restart and recovery](SERVICE-RESTART.md).

## Feature contracts

| Area | References |
| --- | --- |
| Sessions | [Lifecycle](LIFECYCLE.md), [roles](ROLES.md), [forks](FORKING.md), [subagents](SUBAGENTS.md), [queued inputs](INPUT-QUEUE.md), [composer recovery](COMPOSER-RECOVERY.md) |
| Runtime | [Scheduled automations](SCHEDULED-AUTOMATIONS.md), [timeouts](TIMEOUTS.md), [usage-limit continuation](LIMIT-RESUME.md), [questions and elicitation](QUESTIONS.md) |
| Workspaces | [Git context](WORKTREES.md), [worktree policy/API](WORKTREE-POLICY.md), [launch controls](WORKTREE-UI.md), [archive cleanup](WORKTREE-CLEANUP.md), [diff viewer](DIFF.md) |
| Communication | [MCP and permissions](AGENT-COMMUNICATION.md), [external controllers](CONTROLLERS.md), [completion reports](COMPLETION-REPORTS.md), [review queue](REVIEW-QUEUE.md), [inbox delivery](INBOX-DELIVERY.md) |
| Display | [Tool activity and Bazel progress](TOOL-ACTIVITY.md), [source highlighting](SOURCE-HIGHLIGHTING.md), [Mermaid rendering](MERMAID-MARKDOWN.md), [browser notifications](NOTIFICATIONS.md) |
| Quotas | [Codex adapter metadata](CODEX-SUBSCRIPTION-QUOTA.md), [dashboard observations and limitations](DASHBOARD-QUOTA-REFRESH.md) |
| Remote execution | [Worker protocol and supported scope](REMOTE-HOSTS.md) |
| Development workflow | [Bounded coordination and self-updates](SELF-IMPROVEMENT.md) |

## Design and future work

- [Future work](BACKLOG.md): remaining candidates without completed batch history.
- [Rust workspace architecture](architecture.md): crate ownership, dependencies, and application adapters.
- [Original architecture and phased roadmap](ROADMAP.md): historical planning reference.
- [Rust/ACP integration assessment](RUST-INTEGRATION.md): original integration rationale and prototype evidence.
- [Morsh terminal reuse assessment](MORSH-REUSE.md): original source review and reuse rationale.
- [Retention and staged recovery (R-02)](RETENTION-RECOVERY.md) — explicit keep-count pruning, validated restore/upgrade rehearsal, and operator promotion boundaries.
- [Starlark experiment](experiments/001-starlark.md): evaluation and adoption for typed progress parsing; workflow scripts remain a prototype.
- [Agent-proposed scripts](SCRIPT-PROPOSALS.md): proposed review and authorization rules for agent script writes; not implemented.

Keep durable usage instructions, contracts, and design rationale here. Session IDs, local rollout observations, batch journals, completion handoffs, and deployment receipts belong outside tracked documentation. Earlier working notes remain available in Git history.
