# Agentworks execution backlog

This board distinguishes delivered behavior from the older architecture proposals in ROADMAP.md. The user has authorized Claude, Codex and OpenCode implementation sessions in separate worktrees, reviewed and merged sequentially by the coordinating conversation. Cross-host execution remains post-MVP; Linux is first and Windows portability remains a constraint.

## Current batch

[Batch 007](batches/007-reliable-continuation.md) reviewed and merged B-02, C-06 optional inbox wake and L-05 activity/queued inputs. The combined mainline release is live as `agentworks-351eafe`; completed workers are archived/suspended. [Batch 006](batches/006-session-usability.md) is closed with U-06, U-07 and L-07 deployed as `agentworks-6d1740d`; completed workers are archived/suspended with histories retained. [Batch 005](batches/005-service-restart.md) delivered supervised restart, and [batch 004](batches/004-overnight.md) delivered L-01, U-01/B-01, D-01 and U-04. The [self-improvement workflow](SELF-IMPROVEMENT.md) grants service-restart control only to the selected coordinator.

| ID | Task | Status | Acceptance |
| --- | --- | --- | --- |
| C-02 | Read-only session Git/worktree context | Delivered in batch 001 | Main/linked/detached/non-Git states; branch/path/commit visible on desktop and phone; no repository mutation |
| C-03 | Durable completion claims | Delivered in batch 001 | Self-authored scoped MCP reports; idempotency; restart persistence; visible claims with commits, checks and blockers; no automatic work |
| C-04a | Enforced worktree launches | Delivered in batch 002 | Project defaults and mandatory isolation; new/existing worktree selection; common browser/MCP/automation checks; durable retry behavior; preserved history |
| C-01 | Named external controller identity | Delivered | Project-scoped, revocable controller credentials with attributed operations; see [CONTROLLERS.md](CONTROLLERS.md) |
| C-04b | Worktree ownership and cleanup | Delivered | Ownership/dirty state, clean removal on archive with branch-based restore, confirmed dirty removal, unsafe cleanup refused; see [WORKTREE-CLEANUP.md](WORKTREE-CLEANUP.md) |

## Completed priority feature

**L-09: Resume when usage limits reset** was the user's highest-priority feature and is now delivered in batch 003. It enables explicitly authorized long-running backlog work to continue across recognized provider limit windows. Policies remain off by default. Batch 004 adds the daily-use features recorded below.

## Next coordination work

| ID | Task | Acceptance |
| --- | --- | --- |
| C-01 | Named external controller identity | Delivered | Explicit, revocable project-scoped credentials for this conversation/external MCP clients, with attributed operations; no borrowing an existing session's identity |
| C-04b | Worktree ownership and cleanup | Delivered | Build on delivered creation/selection; show ownership and dirty state, refuse unsafe cleanup, and keep integration/merge privileges explicit |
| C-05 | Review and merge queue — delivered ([behavior](../REVIEW-QUEUE.md)) | Record proposed commits, review/test results and integration state separately from agent completion claims; apply one integration at a time |
| C-06 | Optional inbox delivery/wake-up — implemented and verified in batch 007; bounded coordinator policy enabled after deployment | Opt-in queued delivery with stable request IDs, loop/cost bounds, cancellation and crash recovery; messages alone continue not to start work |

## Daily-use sessions

| ID | Task | Acceptance |
| --- | --- | --- |
| L-01 | Rename, archive and suspend/stop controls — implemented batch 004 | History deletion remains separate from stopping a harness; changes persist and activity state remains accurate |
| L-02 | Questions/elicitation — implemented ([behavior](QUESTIONS.md)) | Render supported harness forms, submit validated answers, and recover safely across browser disconnect/cancellation |
| L-03 | Model attachments | Attach supported images/files to chat with explicit storage, size/type limits and harness capability checks |
| L-04 | Usage dashboard and harness configuration | Build on verified Claude/Codex usage commands; show supported data accurately without silent model work or credential exposure; supply the compact composer statusline in U-07 |
| L-05 | Running activity words and queued inputs — implemented and verified in batch 007; bounded coordinator policy enabled after deployment | Show an animated running/activity label and elapsed time, distinguishing running, tool activity and waiting for approval. Allow inputs to queue during a turn, with visible order, edit/remove controls and clear queued/sending/sent states. Dispatch after the current turn ends; persist request IDs, prevent duplicate sends, and define cancellation, errors and restart recovery without blindly replaying work |
| L-06 | Switch harness with fresh context | Let the user choose another harness and what context to carry over into a fresh provider session. Preserve the original session/history and link the successor; make the context reset explicit, apply normal model/workspace policy, and leave the original usable if startup fails |
| L-07 | Configurable execution timeouts — implemented batch 006 | Saved defaults and per-session overrides for optional total duration, inactivity warning/timeout and cancellation grace; meaningful output separated from heartbeats, approval-wait semantics and visible timeout outcomes without retry. See [behavior and checks](TIMEOUTS.md) |
| L-08 | Fork into a new session — implemented; anchored Codex native fork, explicit fresh context and durable lineage | Create an independent session from the latest completed turn or a selected earlier turn, preserving the original session and recording parent/branch-point lineage. Prefer a native harness fork when supported; otherwise explicitly offer a fresh session seeded with selected transcript/context or a summary, without claiming identical hidden provider state. Choose harness/model and reuse the workspace or create a new worktree under normal project policy; conversation forking and Git worktree creation are separate choices. Snapshot the chosen boundary without interrupting a running parent, preserve file-reference meaning when directories change, do not copy pending approvals/queued inputs or automatically grant coordinator privileges, and make fork creation retry-safe. Keep lineage/history across restarts; coordinate context handoff with L-06. See [behavior, guarantees and checks](../FORKING.md) |
| L-09 | Resume when usage limits reset — delivered batch 003 | Off by default; human-enabled session policy schedules a durable continuation at a recognized provider quota reset, with visible countdown, cancellation and reset-source/freshness details. Resume the saved native context and unfinished assignment without blindly replaying the last tool/turn. Authorized MCP child-session creation can explicitly inherit an enabled parent's bounds; inheritance grants no additional controls and cannot enable a disabled policy. Timers persist across restarts; ambiguous dispatch pauses for inspection. Live state/grants and configured bounds are checked before dispatch. See [behavior, provider support and checks](LIMIT-RESUME.md) |

## Chat interface and navigation

Batch 004 implements U-01 and U-04; the other additions below remain planned. Batch 003 delivered L-09 and its controls.

| ID | Task | Acceptance |
| --- | --- | --- |
| U-01 | Code/literal block wrapping and highlighting preferences — implemented batch 004 | Offer wrapping and syntax-highlighting controls, including plain rendering for prose placed in code blocks. Preserve whitespace, raw copy contents and readable code; remember preferences and handle long lines on phones |
| U-02 | Browser notifications — implemented and deployed (`cc17195`, reviewed in `8a83223`) | Opt-in notifications for turn completion, approvals/questions and failures; explicit user-action permission, secure-context/support handling, atomic cross-tab claims, source-session navigation and configured root HTTPS origin. See [behavior, setup and limitations](NOTIFICATIONS.md) |
| U-03 | Declutter mobile session chat — implemented (`c5bd3f6`, polished in `054714a`) | Prioritize transcript, composer and essential running/approval controls. Collapse secondary model/project/workspace/inbox/file actions into an accessible menu or sheet; preserve discoverability, draft state, touch targets and keyboard usability |
| U-04 | Fuzzy command and session palettes — implemented batch 004 | Provide a fuzzy action/command palette and a dedicated fuzzy session-activation palette. Support keyboard navigation and shortcuts plus a touch entry point; match titles, project, host and directory, show enough context to distinguish sessions, and preserve drafts/running turns when switching |
| U-05 | Bazel build progress — first slice implemented on main (`bc9b3b7`), not deployed | Built-in Bazel phase and reported action/test counts from available live tool output, with exact raw output retained; snapshots/deltas, ANSI, carriage returns, split lines and changing totals handled; tool status remains authoritative; ordinary elapsed activity is the fallback. Extensible parser APIs and configurable rules are deferred. See [design and current behavior](TOOL-ACTIVITY.md) |
| U-06 | Compact turn work logs — implemented batch 006 | Counts, outcomes and stable expansion across replay, with raw chronological details and actionable approvals/failures. Live-observed durations are omitted after replay; absent process metadata is labelled unresolved, not still running. U-05's Bazel first slice is implemented; extensibility remains deferred. See [batch review](batches/006-session-usability.md) |
| U-07 | Prompt-bar context and usage statusline — implemented batch 006 | Reported context/size/percentage, Claude's reported 5-hour/7-day used/remaining quotas, and separate cost details with conservative billing labels. Independent freshness, unknown timestamp-less history and explicit attribution filtering; no hidden queries. Additional account-quota retrieval and richer provider metadata remain separate work. See [batch review](batches/006-session-usability.md) |
| U-08 | Clickable workspace file and image references in chat — implemented on main (`9753380`), not deployed | Assistant messages support a workspace file link with a line suffix (for example `[BACKLOG.md](/home/captain/projects/agentworks/work/BACKLOG.md:24)`): open the source preview, highlight and scroll to the requested line. Resolve only inside the session's allowed workspace roots, handle paths with spaces, and preserve normal external-link behavior. Clicking an inline chat image opens it enlarged in an accessible viewer with fit/actual-size controls, keyboard and wheel zoom, drag panning and touch pinch for inspecting detail; preserve normal inline sizing and close/return to the same chat position. |
| U-09 | Syntax highlighting for source and diffs — implemented on main (`077dc3e`), not deployed | Apply readable language-aware syntax highlighting to source views and the diff viewer, preserving diff addition/deletion styling, line numbers and readable plain-text fallback for unknown languages or large/binary content. Keep copied content exact and free of presentation markup |
| U-10 | Operations dashboard — implemented on main (`b26b327`), not deployed | Dedicated dashboard shows catalog-reported hosts and session counts; project counts and session states; and Claude/Codex configuration with source-session usage meters. Identity/connectivity and account totals stay unknown when the available APIs cannot verify them; meters identify their source and freshness. No credentials, inferred totals or account polling. |

## Session changes and review

| ID | Task | Acceptance |
| --- | --- | --- |
| D-01 | Per-session diff viewer — implemented batch 004 | Read-only views for current unstaged changes, branch changes against a selected base using its merge base, and a specific commit. Show the actual session worktree and compared refs/commit; define staged/untracked handling, support file selection and mobile layout, and bound large/binary diffs with existing Git/root protections |

## Known interface bugs

| ID | Bug | Acceptance |
| --- | --- | --- |
| B-02 | Status-only commands preserve pending usage-limit recovery — implemented batch 007 | Read-only provider status checks preserve the unfinished assignment's authorized quota wake, deadline and attempt bound without replaying mutations. Explicit Stop/disable, revocation, completion and replacement-work cancellation remain authoritative. Covered by quota continuation smoke and Rust tests; see [batch 007](batches/007-reliable-continuation.md). |
| B-01 | Settings success toast appears red — implemented batch 004 | Successful saves use success/neutral styling; failures retain error styling. Feedback remains accessible and distinguishes saved settings from partial/failed saves |

## Automations

| ID | Task | Acceptance |
| --- | --- | --- |
| A-01 | Scheduled runs | Durable schedules/time zones, pause state, deduplicated dispatch and explicit restart/missed-run policy |
| A-02 | GitHub triggers | Start with a small supported event set and polling or authenticated webhooks; persist event IDs, deduplicate, and apply existing run/approval limits |

## Experiments

| ID | Experiment | Acceptance |
| --- | --- | --- |
| E-01 | Embedded Rust Starlark for progress parsers and workflow logic | Prototype a Bazel progress parser and a durable conditional two-step workflow using the same interpreter library with separate capabilities. Rust owns I/O, scheduling, permissions and persistence. Verify replay/restart behavior, bounded evaluation, parser isolation and dependency/runtime cost; record a go/no-go decision before adopting it for U-05 or a production workflow engine. See [experiment plan](experiments/001-starlark.md). Planned only; no implementation started |

## Terminal mode

| ID | Task | Acceptance |
| --- | --- | --- |
| T-01 | PTY and Ghostty runtime | Reuse reviewed morsh patterns; supervise actual harness CLIs, resize/input/output, final drain and process-group cleanup |
| T-02 | Browser terminal and reconnect | Fixed mode per session; retain host PTY/emulator while detached; fresh browser redraw with ordered updates and input ownership |
| T-03 | Phone interaction | Touch controls, keyboard behavior, background/resume and usable terminal sizing |

## Release and host work

| ID | Task | Acceptance |
| --- | --- | --- |
| R-01 | Install/startup and Linux user service | Reproducible setup, pinned harness dependencies, diagnostics and documented credential/data locations |
| R-02 | Retention, backups/export and recovery | Configurable bounds, restorable backups, accurate interrupted work and clean upgrades |
| R-03 | Human authentication/process isolation | Explicit stronger control boundary if required; existing VPN/same-user trust assumptions remain documented |
| R-04 | Traefik UUID-subpath hosting — browser routing implemented; proxy deployment validation pending | Support direct `/` and a configurable external mount such as `https://hexfall.home.oksmiths.com/<uuid>/` with Traefik StripPrefix. Make assets/imports, API calls, WebSockets, file previews/downloads and links mount-aware; derive mount URLs from the public module base; retain forwarded host/proto HTTPS origin checks with an isolated trusted proxy. Namespace browser state per deployment and verify trailing slashes/reconnect behind a real proxy fixture. See [hosting plan](HOSTING.md) |
| R-05 | PWA enabled by default — deployed; Android installation confirmed | Install manifest, icons, standalone display and mount-scoped service worker are enabled by default, with a deployment opt-out; HTTPS terminates at Traefik. Scope app identity (derived from start URL), worker and any caches to the public mount; avoid caching private data or replaying mutations offline, handle upgrades/disablement without losing turns/drafts, and test desktop/mobile installation behavior. See [hosting plan](HOSTING.md) |
| R-06 | Restart service from the web interface — delivered batch 005 | Explicit human control and separately granted coordinator capability request a supervised service restart; show active turns/approvals and allow an idle drain or deliberate interruption. Persist state and restart intent before shutdown, retain transcripts/native sessions/timers, deduplicate requests, report restart/reconnect outcome and never depend on the requesting harness staying alive. Linux supervisor first, portable restart contract for later hosts. A coordinator running outside Agentworks cannot use Agentworks quota timers to restart itself; make that boundary clear and support moving its native session into Agentworks |
| H-01 | Persistent execution service over SSH | Versioned capabilities, durable host/session IDs, reconnect without duplicate work and no dependence on SSH connection lifetime |
| H-02 | Cross-host projects, communication and files | Route operations to the owning host; distinguish unreachable hosts from failed agents; authorize remote file access there |

A frontend framework rewrite, managed public hosting, team accounts, full browser IDE, PDF rendering and native Windows delivery are not prerequisites for the agreed Linux release. Self-hosting behind the user's public Traefik HTTPS proxy at a UUID subpath is planned in R-04/R-05 alongside direct/VPN access.
