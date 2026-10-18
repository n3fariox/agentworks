# Agentworks execution backlog

This board distinguishes delivered behavior from the older architecture proposals in ROADMAP.md. The user has authorized Codex implementation sessions in separate worktrees, reviewed and merged sequentially by the coordinating conversation. Cross-host execution remains post-MVP; Linux is first and Windows portability remains a constraint.

## Current batch

See [batch 002](batches/002-worktree-policy.md), deployed after two Codex sessions implemented enforced worktree launches and their browser controls. [Batch 001](batches/001-parallel-coordination.md) is also deployed. The coordinator reviewed, integrated, validated and deployed both batches. Work stops after batch 002 at the user's request; no next feature batch is running.

| ID | Task | Status | Acceptance |
| --- | --- | --- | --- |
| C-02 | Read-only session Git/worktree context | Delivered in batch 001 | Main/linked/detached/non-Git states; branch/path/commit visible on desktop and phone; no repository mutation |
| C-03 | Durable completion claims | Delivered in batch 001 | Self-authored scoped MCP reports; idempotency; restart persistence; visible claims with commits, checks and blockers; no automatic work |
| C-04a | Enforced worktree launches | Delivered in batch 002 | Project defaults and mandatory isolation; new/existing worktree selection; common browser/MCP/automation checks; durable retry behavior; preserved history |

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
| L-01 | Rename, archive and suspend/stop controls | History deletion remains separate from stopping a harness; changes persist and activity state remains accurate |
| L-02 | Questions/elicitation | Render supported harness forms, submit validated answers, and recover safely across browser disconnect/cancellation |
| L-03 | Model attachments | Attach supported images/files to chat with explicit storage, size/type limits and harness capability checks |
| L-04 | Usage dashboard and harness configuration | Build on verified Claude/Codex usage commands; show supported data accurately without silent model work or credential exposure |

## Automations

| ID | Task | Acceptance |
| --- | --- | --- |
| A-01 | Scheduled runs | Durable schedules/time zones, pause state, deduplicated dispatch and explicit restart/missed-run policy |
| A-02 | GitHub triggers | Start with a small supported event set and polling or authenticated webhooks; persist event IDs, deduplicate, and apply existing run/approval limits |

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
| H-01 | Persistent execution service over SSH | Versioned capabilities, durable host/session IDs, reconnect without duplicate work and no dependence on SSH connection lifetime |
| H-02 | Cross-host projects, communication and files | Route operations to the owning host; distinguish unreachable hosts from failed agents; authorize remote file access there |

A frontend framework rewrite, public hosting, team accounts, full browser IDE, PDF rendering and native Windows delivery are not prerequisites for the agreed Linux/private-VPN release.
