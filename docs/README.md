# Documentation

Start with the [quick start](../README.md). Commands in these documents run from the repository root. Design references describe constraints and rationale; historical assessments may include proposals that have since shipped.

## Guides

- [Agent setup](AGENT-SETUP.md): credentials, models, permission modes, commands, skills, and OpenCode.
- [User guide](USER-GUIDE.md): sessions, terminals, files, automations, workspaces, and communication.
- [Operations](OPERATIONS.md): local/remote hosts, directories, network access, persistence, and restarts.
- [Development and testing](TESTING.md): isolated verification, browser checks, and optional provider checks.
- [HTTPS hosting and PWA installation](HOSTING.md).
- [Supervised service restart and recovery](SERVICE-RESTART.md).

## Feature contracts

| Area | References |
| --- | --- |
| Sessions | [Lifecycle](LIFECYCLE.md), [forks](FORKING.md), [queued inputs](INPUT-QUEUE.md), [composer recovery](COMPOSER-RECOVERY.md) |
| Runtime | [Timeouts](TIMEOUTS.md), [usage-limit continuation](LIMIT-RESUME.md), [questions and elicitation](QUESTIONS.md) |
| Workspaces | [Git context](WORKTREES.md), [worktree policy/API](WORKTREE-POLICY.md), [launch controls](WORKTREE-UI.md), [archive cleanup](WORKTREE-CLEANUP.md), [diff viewer](DIFF.md) |
| Communication | [MCP and permissions](AGENT-COMMUNICATION.md), [external controllers](CONTROLLERS.md), [completion reports](COMPLETION-REPORTS.md), [review queue](REVIEW-QUEUE.md), [inbox delivery](INBOX-DELIVERY.md) |
| Display | [Tool activity and Bazel progress](TOOL-ACTIVITY.md), [source highlighting](SOURCE-HIGHLIGHTING.md), [Mermaid rendering](MERMAID-MARKDOWN.md), [browser notifications](NOTIFICATIONS.md) |
| Quotas | [Codex adapter metadata](CODEX-SUBSCRIPTION-QUOTA.md), [dashboard observations and limitations](DASHBOARD-QUOTA-REFRESH.md) |
| Remote execution | [Worker protocol and supported scope](REMOTE-HOSTS.md) |
| Development workflow | [Bounded coordination and self-updates](SELF-IMPROVEMENT.md) |

## Design and future work

- [Future work](BACKLOG.md): remaining candidates without completed batch history.
- [Original architecture and phased roadmap](ROADMAP.md): historical planning reference.
- [Rust/ACP integration assessment](RUST-INTEGRATION.md): original integration rationale and prototype evidence.
- [Morsh terminal reuse assessment](MORSH-REUSE.md): original source review and reuse rationale.
- [Starlark experiment](experiments/001-starlark.md): proposed evaluation; adoption remains undecided.

Keep durable usage instructions, contracts, and design rationale here. Session IDs, local rollout observations, batch journals, completion handoffs, and deployment receipts belong outside tracked documentation. Earlier working notes remain available in Git history.
