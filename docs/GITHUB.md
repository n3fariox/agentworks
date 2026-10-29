# GitHub context

Agentworks reads GitHub context using **GitHub CLI accounts on the main server**. Remote workers supply Git metadata only: they never run `gh`, inspect GitHub credentials or make GitHub API calls for this integration.

## Setup

1. Install a recent GitHub CLI on the main server and run `gh auth login` as the OS user running Agentworks. Sign in to each public/Enterprise host and account you need. The CLI must support `gh auth status --json hosts`, `gh auth status --active`, and `gh auth token --hostname HOST --user LOGIN`.
2. Open **Settings → GitHub**, rediscover accounts, and enable the accounts Agentworks should use. Connections save immediately. **Verify identity** checks the selected credential against that host's API.
3. Open a session in a Git repository with an HTTPS or SSH `origin`. The GitHub/PR status button beside Workspace in the existing session toolbar opens repository context, associated PRs, checks and review summaries. On phones, it is in the session options (⋯) menu.

An absent CLI, no accounts, authentication failures and inaccessible GitHub hosts have setup/error messages. You can finish configuring authentication later without recreating sessions. Account discovery does not enable accounts automatically.

`gh` uses its normal main-host configuration and keychain. Agentworks removes environment token overrides (`GH_TOKEN`, `GITHUB_TOKEN` and Enterprise equivalents), explicit-host overrides and CLI debugging from credential subprocesses, so a connection consistently resolves its saved host/account. For the currently active account, fresh successful CLI metadata must match the saved login before Agentworks uses the host-level token lookup; other accounts use explicit `--user`. This supports legacy host-level tokens. In both cases, the API identity is checked against the saved login before repository reads, including if the active account switches during lookup. Tokens are captured privately, used in memory, and never returned to the browser or stored in the catalog. API calls use the main process's proxy environment and system certificate trust; custom `gh` extensions, aliases and insecure TLS settings are not imported.

## Origin matching and accounts

Supported examples:

- `https://github.com/owner/repo.git`
- `git@ghe.example.com:owner/repo.git`
- `ssh://git@ghe.example.com/owner/repo.git`

The origin's literal hostname selects enabled connections. SSH aliases, explicit SSH port parsing, local/file remotes and alternate remote names are not supported in this release. Public GitHub and Enterprise Cloud `*.ghe.com` use their API hosts; other authenticated hostnames use GitHub Enterprise Server REST `/api/v3` and GraphQL `/api/graphql`.

If one account matches, Agentworks saves that repository's selection. If multiple accounts match, select one in the GitHub panel. Bindings use GitHub host + owner/repository and are shared by ordinary checkouts and linked worktrees. They are independent of the checkout's execution host. Disabling the saved account requires reconnecting it or explicitly choosing another; Agentworks never falls back to a different account or public GitHub. You can change accounts in the panel.

Remote Git context requires a worker advertising `gitContext`; update/reconnect older workers. The worker still enforces its configured directory roots. Main-host Git inspection uses the same root and subprocess bounds as the workspace dialog. Network origins are returned without URL credentials, queries or fragments.

## Display and attachment

The sessions browser shows `PR#1234` badges on a shared third line below each session's title and provider/model. Empty badge rows collapse completely. Clicking a PR badge opens that PR in a new tab without switching sessions. Normal GitHub context collection updates these badges in the shared session badge store. Existing Starlark hooks can call `agent.set_badge` and `agent.clear_badge` to contribute to the same row. See [Session badges](SESSION-BADGES.md) for setters, persistence, bounds and composition rules.

Green means approved; red means not approved and CI is failing; yellow means awaiting review or changes requested. Approval has priority over failed CI. Drafts, closed/merged PRs and unknown/stale states are neutral. Repositories without review rules use latest opinionated reviews, and fallback approvals must refer to the current PR head. Incomplete review data stays unknown.

The session GitHub panel lists PRs whose head branch and head repository match the origin repository, ordered by recent update, up to 20 candidates. It displays all candidates rather than silently choosing among several. A PR from a fork with the same branch name is not inferred as this checkout's PR. Fork PRs into another repository need broader repository association support in a later release.

Attach an issue or PR by type and number from the origin repository (up to 10 per session). Attachments persist through restart and are session-specific. Use **Detach** to remove an unavailable or incorrectly numbered attachment as well. Links never change the repository or credential host. Changing the origin does not apply an old link to the new repository.

PR details show state, draft status, base branch, current head SHA, review decision and up to 50 check/status details. Every PR is marked according to whether its head SHA matches the checkout commit. Checks are explicitly labeled as belonging to the PR head; old-commit results are never labeled as current checkout results. Missing checks or review decisions are unknown. Pagination limits are visible; check success is not a claim that repository merge rules are satisfied.

Snapshots include a fetched timestamp. Active visible sessions refresh approximately once a minute; manual refresh has a 10-second minimum cache interval. Sessions share a bounded in-memory cache scoped by connection, repository, branch, commit and attachments, with serialized requests. Errors show unavailable state rather than an empty result. API failures back off; rate-limit errors honor the reported reset time or numeric Retry-After, with a default pause. A partial GraphQL error does not become a successful empty snapshot. Restart clears snapshots and rediscovers credentials when needed.

The public UI/API currently supports read-only context and local connection/link management. GitHub event triggers, native GitHub write actions and dedicated agent tools remain future work. The [design proposal](GITHUB-INTEGRATION.md) describes those extensions.

## API and verification

- `GET /api/github/connections[?refresh=true]`: sanitized discovered/saved account identities, enabled state, API address and main-host setup message.
- `POST /api/github/connections`: `{host, login, enabled}`; enabling requires a discovered account.
- `POST /api/github/verify`: same identity fields; verify an enabled account against `/user`.
- `GET /api/sessions/{id}/github[?refresh=true]`: origin resolution and normalized snapshot or a setup/error state.
- `POST /api/sessions/{id}/github`: `{login? , links?}`; select an enabled account and/or replace session attachments. Links use `{host,owner,repo,kind:"issue"|"pullRequest",number}`.

These are browser APIs under existing origin and trusted-user controls. They do not add agent grants or OS isolation. Connection changes invalidate cached snapshots. Existing repository-level bindings remain saved when an account is disabled.

Checks: `mise exec -- cargo test --locked -p agentworks github`, workspace origin sanitization tests, runtime worker Git-context tests, and `npm run test:github` after building. Browser/API fixtures use a fake main-host CLI, disposable repositories/catalog/database, and mock display responses; Rust tests exercise public and Enterprise GraphQL routing through a local HTTP fixture. They make no live GitHub or paid model calls.
