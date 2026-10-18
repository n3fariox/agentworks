# Agentworks execution backlog

This board distinguishes delivered behavior from the older architecture proposals in ROADMAP.md. The user has authorized Claude, Codex and OpenCode implementation sessions in separate worktrees, reviewed and merged sequentially by the coordinating conversation. Cross-host execution remains post-MVP; Linux is first and Windows portability remains a constraint.

## Current batch

[Batch 006](batches/006-session-usability.md) starts three Claude workers for U-06 compact work logs, U-07 composer usage status and L-07 configurable timeouts, with isolated worktrees and a 2026-10-18 17:15 UTC worker deadline. No integration or deployment yet. [Batch 005](batches/005-service-restart.md) delivered supervised restart, and [batch 004](batches/004-overnight.md) delivered L-01, U-01/B-01, D-01 and U-04. The [self-improvement workflow](SELF-IMPROVEMENT.md) grants service-restart control only to the selected coordinator.

| ID | Task | Status | Acceptance |
| --- | --- | --- | --- |
| C-02 | Read-only session Git/worktree context | Delivered in batch 001 | Main/linked/detached/non-Git states; branch/path/commit visible on desktop and phone; no repository mutation |
| C-03 | Durable completion claims | Delivered in batch 001 | Self-authored scoped MCP reports; idempotency; restart persistence; visible claims with commits, checks and blockers; no automatic work |
| C-04a | Enforced worktree launches | Delivered in batch 002 | Project defaults and mandatory isolation; new/existing worktree selection; common browser/MCP/automation checks; durable retry behavior; preserved history |

## Completed priority feature

**L-09: Resume when usage limits reset** was the user's highest-priority feature and is now delivered in batch 003. It enables explicitly authorized long-running backlog work to continue across recognized provider limit windows. Policies remain off by default. Batch 004 adds the daily-use features recorded below.

## Next coordination work

| ID | Task | Acceptance |
| --- | --- | --- |
| C-01 | Named external controller identity | Explicit, revocable project-scoped credentials for this conversation/external MCP clients, with attributed operations; no borrowing an existing session's identity |
| C-04b | Worktree ownership and cleanup | Build on delivered creation/selection; show ownership and dirty state, refuse unsafe cleanup, and keep integration/merge privileges explicit |
| C-05 | Review and merge queue | Record proposed commits, review/test results and integration state separately from agent completion claims; apply one integration at a time |
| C-06 | Optional inbox delivery/wake-up | Opt-in queued delivery with stable request IDs, loop/cost bounds, cancellation and crash recovery; messages alone continue not to start work |

## Daily-use sessions

| ID | Task | Acceptance |
| --- | --- | --- |
| L-01 | Rename, archive and suspend/stop controls — implemented batch 004 | History deletion remains separate from stopping a harness; changes persist and activity state remains accurate |
| L-02 | Questions/elicitation | Render supported harness forms, submit validated answers, and recover safely across browser disconnect/cancellation |
| L-03 | Model attachments | Attach supported images/files to chat with explicit storage, size/type limits and harness capability checks |
| L-04 | Usage dashboard and harness configuration | Build on verified Claude/Codex usage commands; show supported data accurately without silent model work or credential exposure; supply the compact composer statusline in U-07 |
| L-05 | Running activity words and queued inputs | Show an animated running/activity label and elapsed time, distinguishing running, tool activity and waiting for approval. Allow inputs to queue during a turn, with visible order, edit/remove controls and clear queued/sending/sent states. Dispatch after the current turn ends; persist request IDs, prevent duplicate sends, and define cancellation, errors and restart recovery without blindly replaying work |
| L-06 | Switch harness with fresh context | Let the user choose another harness and what context to carry over into a fresh provider session. Preserve the original session/history and link the successor; make the context reset explicit, apply normal model/workspace policy, and leave the original usable if startup fails |
| L-07 | Configurable execution timeouts | Replace the fixed 15-minute turn deadline with saved defaults and per-session overrides for optional total duration, inactivity warning/timeout and cancellation grace. Distinguish meaningful output from transport heartbeats; pause inactivity timing during human approval waits, show approaching limits, and preserve accurate interrupted/recovery state. See [tool activity design](TOOL-ACTIVITY.md) |
| L-08 | Fork into a new session | Create an independent session from the latest completed turn or a selected earlier turn, preserving the original session and recording parent/branch-point lineage. Prefer a native harness fork when supported; otherwise explicitly offer a fresh session seeded with selected transcript/context or a summary, without claiming identical hidden provider state. Choose harness/model and reuse the workspace or create a new worktree under normal project policy; conversation forking and Git worktree creation are separate choices. Snapshot the chosen boundary without interrupting a running parent, preserve file-reference meaning when directories change, do not copy pending approvals/queued inputs or automatically grant coordinator privileges, and make fork creation retry-safe. Keep lineage/history across restarts; coordinate context handoff with L-06 |
| L-09 | Resume when usage limits reset — delivered batch 003 | Off by default; human-enabled session policy schedules a durable continuation at a recognized provider quota reset, with visible countdown, cancellation and reset-source/freshness details. Resume the saved native context and unfinished assignment without blindly replaying the last tool/turn. Authorized MCP child-session creation can explicitly inherit an enabled parent's bounds; inheritance grants no additional controls and cannot enable a disabled policy. Timers persist across restarts; ambiguous dispatch pauses for inspection. Live state/grants and configured bounds are checked before dispatch. See [behavior, provider support and checks](LIMIT-RESUME.md) |

## Chat interface and navigation

Batch 004 implements U-01 and U-04; the other additions below remain planned. Batch 003 delivered L-09 and its controls.

| ID | Task | Acceptance |
| --- | --- | --- |
| U-01 | Code/literal block wrapping and highlighting preferences — implemented batch 004 | Offer wrapping and syntax-highlighting controls, including plain rendering for prose placed in code blocks. Preserve whitespace, raw copy contents and readable code; remember preferences and handle long lines on phones |
| U-02 | Browser notifications | Opt-in notifications for turn completion, approvals/questions and failures; request permission through a user action, detect browser/origin support, avoid duplicate notices across reconnects/tabs, and open the relevant session when selected |
| U-03 | Declutter mobile session chat | Prioritize transcript, composer and essential running/approval controls. Collapse secondary model/project/workspace/inbox/file actions into an accessible menu or sheet; preserve discoverability, draft state, touch targets and keyboard usability |
| U-04 | Fuzzy command and session palettes — implemented batch 004 | Provide a fuzzy action/command palette and a dedicated fuzzy session-activation palette. Support keyboard navigation and shortcuts plus a touch entry point; match titles, project, host and directory, show enough context to distinguish sessions, and preserve drafts/running turns when switching |
| U-05 | Extensible tool progress rendering | Normalize tool output across harnesses, correctly distinguish snapshots from deltas, and feed a versioned parser interface. Start with Bazel phases/action counts and configurable match rules; support richer user-installed parsers without changing the chat renderer. Preserve raw output, handle ANSI/carriage returns and dynamic totals, and fall back to elapsed activity when no live output is available. See [design](TOOL-ACTIVITY.md) |
| U-06 | Compact turn work logs | Group chained tool activity into a turn-level work log that collapses after completion, with counts, duration, outcome and progress summaries. Preserve chronology and access to raw details; keep approvals, failures and continuing background work visible, respect manual expansion, and reproduce grouping after reconnect/replay. Offer compact/expanded preferences; see [design](TOOL-ACTIVITY.md) |
| U-07 | Prompt-bar context and usage statusline | Show reported context used/size and percentage beside the composer; include available 5-hour/7-day subscription quota values with explicit used/remaining labels, and optional reported session cost labeled appropriately for the account type. Tap/click opens token counts, reset times, data source and freshness. Use persisted ACP usage events where available; keep context occupancy, cumulative consumption/cost and account quota distinct, handle compaction/model changes and parent/subagent attribution, mark stale/unknown data, and never query usage by silently submitting turns. Compact mobile layout exposes details on demand. Existing stored events confirm usage updates from Claude, Codex and OpenCode; account quota retrieval remains a separate capability investigation |

## Session changes and review

| ID | Task | Acceptance |
| --- | --- | --- |
| D-01 | Per-session diff viewer — implemented batch 004 | Read-only views for current unstaged changes, branch changes against a selected base using its merge base, and a specific commit. Show the actual session worktree and compared refs/commit; define staged/untracked handling, support file selection and mobile layout, and bound large/binary diffs with existing Git/root protections |

## Known interface bugs

| ID | Bug | Acceptance |
| --- | --- | --- |
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
| R-04 | Traefik UUID-subpath hosting | Support direct `/` and a configurable external mount such as `https://hexfall.home.oksmiths.com/<uuid>/` with Traefik StripPrefix. Make assets/imports, API calls, WebSockets, file previews/downloads and links mount-aware; support trusted forwarded prefix/host/proto with correct HTTPS origin checks. Namespace browser state per deployment and verify trailing slashes/reconnect behind a real proxy fixture. See [hosting plan](HOSTING.md) |
| R-05 | Optional PWA at hosting time | Deployment flag enables install manifest, icons, standalone display and mount-scoped service worker; HTTPS terminates at Traefik. Scope app identity/start URL, worker and any caches to the UUID mount; avoid caching private data or replaying mutations offline, handle upgrades/disablement without losing turns/drafts, and test desktop/mobile installation behavior. See [hosting plan](HOSTING.md) |
| R-06 | Restart service from the web interface — delivered batch 005 | Explicit human control and separately granted coordinator capability request a supervised service restart; show active turns/approvals and allow an idle drain or deliberate interruption. Persist state and restart intent before shutdown, retain transcripts/native sessions/timers, deduplicate requests, report restart/reconnect outcome and never depend on the requesting harness staying alive. Linux supervisor first, portable restart contract for later hosts. A coordinator running outside Agentworks cannot use Agentworks quota timers to restart itself; make that boundary clear and support moving its native session into Agentworks |
| H-01 | Persistent execution service over SSH | Versioned capabilities, durable host/session IDs, reconnect without duplicate work and no dependence on SSH connection lifetime |
| H-02 | Cross-host projects, communication and files | Route operations to the owning host; distinguish unreachable hosts from failed agents; authorize remote file access there |

A frontend framework rewrite, managed public hosting, team accounts, full browser IDE, PDF rendering and native Windows delivery are not prerequisites for the agreed Linux release. Self-hosting behind the user's public Traefik HTTPS proxy at a UUID subpath is planned in R-04/R-05 alongside direct/VPN access.
