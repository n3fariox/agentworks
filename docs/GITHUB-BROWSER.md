# GitHub PR browser

Open **GitHub** in the left navigation to see open PRs created by, assigned to, or requesting review from any enabled GitHub account. Each account's credential is resolved and identity-checked on the main host. “Assigned” means GitHub assignee, rather than a review request. Authored, assigned and review-requested searches are merged, so a PR matching several categories appears once with each inclusion reason. Review requests use GitHub’s [`review-requested:USERNAME` search qualifier](https://docs.github.com/en/pull-requests/how-tos/review-pull-requests/viewing-a-pull-request-review). Multiple enabled accounts on the same host also share one row per repository/PR.

Each collapsible host contains a **Review requested** section first, followed by **Created or assigned**. A PR matching both appears once in Review requested, keeping all inclusion reasons. Tables default to **Updated, descending** so the most recently updated PR is first within each section. **Sort by** and clickable column headings support title, repository, author, inclusion reason, review, CI, updated/created dates and PR number. Unknown dates stay last in either direction. Sort choices are remembered in browser storage.

**Column filters** apply locally to the loaded PRs across all hosts and sections. Text filters match title/number/branch, repository and author without case sensitivity; selection filters cover inclusion, review and CI. Created/updated date bounds are inclusive UTC calendar dates. Filters combine with AND and can be cleared together. Filtering and sorting make no GitHub requests. Filters remain while moving between views but reset on page reload.

**Group by repository** restores collapsible repository groups. Groups follow their first row in the current sort, and rows remain sorted inside each group. Flat tables are the default to maintain date order across repositories. Group choices are remembered in browser storage. PR links and the context menu’s **Open PR in new tab** action open GitHub in a separate tab. Titles and branch names are rendered as plain text, and destinations are restricted to credential-free HTTPS URLs.

PR badges use the same Rust presentation function as session badges: green for approved; red for unapproved with failing CI; yellow for review required or changes requested; neutral for drafts, unknown or unavailable status. Approval has priority over CI failure. The table also shows review and CI independently. Account failures retain cached rows with neutral status and an explanation, rather than reporting a definitive empty result. Search is bounded to five pages per category (authored, assigned and review requested); incomplete results are labeled.

## Polling and shared data

**Settings → GitHub** includes a polling interval for each configured or discovered host. The default is **5 minutes**, adjustable from **1 to 60 minutes**. Public GitHub can use a longer interval while internal hosts use a shorter one. The setting is shared by session PR tracking, context-cache freshness, dashboard membership, and badge freshness. It is saved in the main-host catalog as `github.pollIntervals` (seconds by hostname).

The PR browser requests its data only while this workspace view is open and the browser tab is visible. Leaving the view or hiding the tab stops its timer. Opening another browser uses the same main-host cache and cannot force an extra membership search before that host's interval expires. Existing session PR tracking continues independently, using the same per-host cadence.

Dashboard and session context reads share normalized PR records keyed by host/account/repository/PR number. Dashboard results can update the records subsequently used by session badges; session status updates can update cached dashboard rows and remove closed PRs. Browser membership and branch/attachment discovery are separate queries with the shared interval; PR status is not a separate table-only implementation. Explicit session Refresh remains a deliberate user action with its existing short throttle. Account API backoff is shared and respected.

## Open in session

Right-click a PR row, use the keyboard context-menu key, or use **Open in session…**. On touch screens the same delegated long-press menu is available, along with the visible button.

**Open in session…** looks up sessions using the full GitHub host/repository/PR number. If there are matches, a chooser shows each session’s title, project, execution host, harness, status and directory. Choose one to switch to its conversation, or choose **New session…**. This navigation uses normal session-opening behavior, including reconnect/resume rules, without sending a prompt or changing its checkout. If there are no matches, it opens the New Session dialog directly. The context menu also offers **New session…** to go straight to creation.

Associations come from saved explicit PR attachments and the PR tracker’s last observed matches, across all credential accounts and execution hosts. Archived matches appear in a collapsed **Archived sessions** section below active matches. Opening one shows its saved conversation without unarchiving it or automatically resuming a suspended harness; normal session actions still provide explicit unarchive. When only archived matches exist, the chooser remains available with an empty active section and the archived group. Deleted sessions are excluded. Lookups happen on demand and do not poll GitHub or inspect workers. A branch change is reflected after the normal tracker refresh; during an outage, a last observed match may remain available. The session is rechecked before opening, so a deleted session produces an error. A session archived after being listed as active asks you to reopen the chooser and use its archived entry. No selection restores or unarchives a session.

New-session creation uses the existing **New Session** dialog. It prefills the title, new-worktree mode, a unique local branch based on the PR branch, and the observed PR head commit. Choose the project, execution host and harness using the normal dialog. The prefill survives project-policy loading and project changes. Nothing is created until you submit the dialog.

On submission:

1. The chosen source directory must have an origin matching the PR's GitHub host and repository.
2. The worktree starts at the pinned head SHA. If that object is missing, Git fetches `refs/pull/<number>/head` from the source repository's `origin` before resolving it. It does not change the source checkout's branch.
3. Normal workspace policy, allowed-root checks and durable creation reservations apply. The new branch and worktree are isolated. The session is created **without a prompt**.
4. The PR is explicitly attached to the new session so status tracking works even though its local branch has a different name. A failed attachment is reported separately from successful session creation.

The Git fetch uses the execution host's normal Git credentials. Integration tokens are not copied to workers; the integration never invokes remote `gh` to resolve credentials. Updated workers are required for the optional PR checkout metadata. A missing/stale head or inaccessible Git origin produces the normal creation error for inspection/retry; no branch/files are silently deleted.

## APIs

- `GET /api/github/pulls`: cached/due authored/assigned/review-requested PR groups by host, account messages, per-host interval and next polling eligibility. Calling this endpoint explicitly opts into the broad listing read; the background session tracker does not invoke it.
- `GET /api/github/sessions?host=…&owner=…&repo=…&number=…`: local associated-session lookup, with session metadata, project/host display names and attachment/observation provenance. No GitHub requests or persistent mutations.
- `PUT /api/github/polling`: `{host, intervalSeconds}`. The host must be configured or present in the latest discovered accounts. Reschedules its session watches. Does not enable an account.
- `GET /api/github/connections`: additionally returns per-host `polling` preferences.
- A new-worktree submission can include `workspace.pullRequest: {host,owner,repo,number,headSha}` with `baseRef` equal to `headSha`. This pins and validates the PR checkout; it grants no GitHub write authority.

The [PR tracking](GITHUB-PR-TRACKING.md) journal and hook semantics are unchanged. GitHub App installs, issue-label automation and GitHub writes remain separate extensions.
