# Running Agentworks development inside Agentworks

The coordinator can develop Agentworks through ordinary saved chat sessions and its existing project-scoped MCP controls. This is a manual, bounded workflow, not an installed autonomous service. Importing this conversation preserves its native context; running it in Agentworks makes its recognized quota failures eligible for the opt-in continuation timer.

## Transfer and prepare

After the external conversation finishes, use **Resume existing session**, choose Codex, and select native session `01a14a2c-ee30-71a2-bf54-e551e9ec6d76` in `/home/captain/projects/agentworks`. Assign it to **Agentworks · overnight backlog**, project `bd61c375-1916-4e60-a2d9-8fc7614e68d0`, so it can see the saved workers and inbox. Do not run the same native session in two harnesses simultaneously.

The preparation helper operates on the imported Agentworks session ID, not the native Codex ID. First preview, then apply:

```sh
node scripts/prepare-self-improvement.mjs <agentworks-session-id> \
  --project bd61c375-1916-4e60-a2d9-8fc7614e68d0 \
  --native 01a14a2c-ee30-71a2-bf54-e551e9ec6d76
# Add --apply to make the displayed changes.
```

It verifies that the selected idle/suspended session has the expected native identity and repository directory. It sets mandatory new-worktree launches for this project and grants only this coordinator communication/start/prompt/cancel controls. Workers stay unprivileged by default. It does not send a prompt, enable a timer or deploy anything. The same changes can be made in project settings and **Agents & inbox**. MCP tools refresh on native reconnect; explicitly suspended sessions require Resume.

When ready to begin a run, human-enable **Usage-limit continuation**, choose an absolute deadline and attempt bound, then send a bounded assignment. Start with three attempts and a deadline matching the authorized work window; adjust deliberately. The old overnight deadline has expired. Use `inheritResumePolicy:true` when creating a worker only if it should continue through the same quota window. This passes remaining attempt/deadline bounds, never controller grants. Ordinary turn completion does not automatically poll the inbox or start more work; the coordinator must stay in its active turn and check workers at bounded checkpoints. Unknown reset data or an uncertain crash dispatch pauses for inspection.

## Coordinator loop

1. Read `work/BACKLOG.md`, the latest batch record and Git status. Check saved sessions/claims/inbox before creating anything. Native completion claims are evidence to inspect, not proof that code is merged or correct.
2. Record a batch with exact scope, absolute stop time, ownership, branch names, base commit and test ports. Defer major product decisions. Start two or three independent workers with stable request IDs and mandatory new worktrees from a committed main revision. Prefer Codex for Rust/runtime work; Claude or OpenCode/free models can handle independent UI slices. Avoid overlapping shared-file ownership and shared builds/data/dependency edits.
3. Send one concrete task to each worker. Require checkpoints, clean commits, checks/limitations, a completion claim and a final inbox handoff. Read inbox using only its returned `nextCursor`, never outgoing message IDs. Messages do not start turns. Monitor approvals and quota states; do not automatically approve unrelated/destructive operations or replay ambiguous tool calls.
4. Review actual diffs, request fixes and inspect their commits. Merge one branch at a time into main. Resolve integration deliberately, then run the appropriate combined Rust/API/MCP/browser checks using isolated data and ports. Keep immutable live executable paths separate from Cargo output. Never ask a worker to merge its sibling's work or rebuild over the live binary.
5. Check activity, back up catalog and SQLite, verify integrity/preservation, and deploy only the reviewed immutable executable. Preserve native identities and transcripts. Unrelated projects do not gate updates; a shared-process restart can still suspend their harnesses. Update the batch/backlog with actual delivered behavior, checks, limitations and pending decisions.
6. Before the deadline, stop launching work, finish or checkpoint outstanding tasks and leave a handoff. Explicitly report completed/blocked and disable coordinator continuation when its assignment ends. No new run is implied by an old timer or by an empty inbox.

## Next assignment, prepared but not started

Finish **R-06 supervised service restart**, with R-01 Linux user-service setup only as needed to support it. This is the first operational gap to close before relying on unattended self-updates. A process cannot provide reliable restart merely by killing itself; an external supervisor must own restart and readiness. Give a dedicated Codex worker an isolated worktree, require deterministic fake-supervisor tests and a real isolated restart fixture, and retain manual deployment until that path is reviewed.

Acceptance: human restart control plus separately granted coordinator capability; show active turns/approvals and offer idle drain or deliberate interruption; durable request IDs and restart intent; state backup/integrity and native/timer preservation; bounded shutdown/readiness with visible failure; browser reconnect; no repeated restart loops or automatic tool replay. The requesting harness must be allowed to disappear without losing the intent. Do not give every worker restart/merge privileges. Never replace the live process during implementation tests.

Current MCP has no service-restart tool and the server has no supervised restart endpoint. This document and preparation helper do not supply those features. Other follow-ups remain in the backlog, including stronger named external controller credentials, a persistent review queue and scheduled automation.
