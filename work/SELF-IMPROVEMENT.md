# Running Agentworks development inside Agentworks

The coordinator can develop Agentworks through ordinary saved chat sessions and its existing project-scoped MCP controls. This is a manual, bounded workflow, not an installed autonomous service. Importing this conversation preserves its native context; running it in Agentworks makes its recognized quota failures eligible for the opt-in continuation timer.

## Transfer and prepare

After the external conversation finishes, use **Resume existing session**, choose Codex, and select native session `01a14a2c-ee30-71a2-bf54-e551e9ec6d76` in `/home/captain/projects/agentworks`. Assign it to **Agentworks · overnight backlog**, project `bd61c375-1916-4e60-a2d9-8fc7614e68d0`, so it can see the saved workers and inbox. Do not run the same native session in two harnesses simultaneously.

The preparation helper operates on the imported Agentworks session ID, not the native Codex ID. First preview, then apply:

```sh
node scripts/prepare-self-improvement.mjs <agentworks-session-id> \
  --project bd61c375-1916-4e60-a2d9-8fc7614e68d0 \
  --native 01a14a2c-ee30-71a2-bf54-e551e9ec6d76 --service-restart
# Add --apply to make the displayed changes.
```

It verifies that the selected idle/suspended session has the expected native identity and repository directory. It sets mandatory new-worktree launches for this project and grants only this coordinator communication/start/prompt/cancel controls. The explicit `--service-restart` flag additionally grants global supervised-restart control; omit it to leave that capability off. Coordinator presets and child sessions never imply this grant. Worker sessions use the configured project/global communication role, without copying the coordinator's grants; automation runs remain Off. It does not send a prompt, enable a timer or deploy anything. The same changes can be made in project settings and **Agents & inbox**. MCP tools refresh on native reconnect; explicitly suspended sessions require Resume.

When ready to begin a run, human-enable **Usage-limit continuation**, choose an absolute deadline and attempt bound, then send a bounded assignment. Start with three attempts and a deadline matching the authorized work window; adjust deliberately. The old overnight deadline has expired. Use `inheritResumePolicy:true` when creating a worker only if it should continue through the same quota window. This passes remaining attempt/deadline bounds, never controller grants. With automatic inbox delivery off, ordinary turn completion does not poll inbox or start more work; the coordinator checks workers at active bounded checkpoints. Batch 007 adds a separate human-enabled [bounded inbox policy](INBOX-DELIVERY.md) that can submit new peer messages into an idle saved native context. It has independent turn/deadline/cooldown bounds, respects explicit input and quota pauses, and requires reauthorization after a service restart. It grants no new backlog scope. Unknown reset data or an uncertain crash dispatch pauses for inspection.

## Coordinator loop

1. Read `work/BACKLOG.md`, the latest batch record and Git status. Check saved sessions/claims/inbox before creating anything. Native completion claims are evidence to inspect, not proof that code is merged or correct.
2. Record a batch with exact scope, absolute stop time, ownership, branch names, base commit and test ports. Defer major product decisions. Start two or three independent workers with stable request IDs and mandatory new worktrees from a committed main revision. Prefer Codex for Rust/runtime work; Claude or OpenCode/free models can handle independent UI slices. Avoid overlapping shared-file ownership and shared builds/data/dependency edits.
3. Send one concrete task to each worker. Require checkpoints, clean commits, checks/limitations, a completion claim and a final inbox handoff. Read inbox using only its returned `nextCursor`, never outgoing message IDs. Messages start no turns unless the recipient has a separately human-enabled bounded inbox policy. Active workers still need explicit inbox checkpoints. Monitor approvals and quota states; do not automatically approve unrelated/destructive operations or replay ambiguous tool calls.
4. Review actual diffs, request fixes and inspect their commits. Merge one branch at a time into main. Resolve integration deliberately, then run the appropriate combined Rust/API/MCP/browser checks using isolated data and ports. Keep immutable live executable paths separate from Cargo output. Never ask a worker to merge its sibling's work or rebuild over the live binary.
5. Stage only the reviewed immutable executable, then use the supervised self-update sequence below. Preserve native identities and transcripts. Unrelated projects do not gate updates; a shared-process restart can still suspend their harnesses. Update the batch/backlog with actual delivered behavior, checks, limitations and pending decisions.
6. Before the deadline, stop launching work, finish or checkpoint outstanding tasks and leave a handoff. Explicitly report completed/blocked and disable coordinator continuation when its assignment ends. No new run is implied by an old timer or by an empty inbox.

## Supervised self-updates

[R-06](SERVICE-RESTART.md) supplies a service restart API, Settings controls and three separately granted MCP tools: `get_service_status`, `restart_service` and `cancel_service_restart`. The external supervisor survives the requesting harness and checks the successor's PID, instance, release and readiness. Direct unsupervised invocations refuse restart. Linux uses the supplied user-service unit; the deployment record is in [batch 005](batches/005-service-restart.md).

After reviewing, merging and testing an update:

1. Copy the finished executable into the configured private release directory using a new immutable basename (this host uses `.agentworks/bin/agentworks-<commit>`). Never build over the running executable. Verify its `--service-protocol` metadata. Publishing a prepared release is a deliberate coordinator shell action; the restart tool does not build, merge, download or install code.
2. Call `get_service_status`; require `restartSupported:true`, no manager fault and the expected current release/instance. Check the global activity counts. Stop or defer if the current assignment's authorization has ended.
3. Call `restart_service` with a stable request ID, `mode:"wait"`, a bounded `expiresInSeconds`, the prepared release basename, `handoff:true`, and an absolute `handoffDeadline` within 30 minutes. This authorizes one continuation in your own saved native context. Finish the current turn after acceptance so the drain can complete. Do not keep the turn alive polling for its own restart.
4. The service blocks new work, waits for existing activity, verifies private catalog/SQLite backups, then transfers a durable restart ticket to the supervisor. After the supervisor confirms the new instance and persists its successful release selection, Agentworks opens your original native context and submits one attributed instruction to inspect results and continue unfinished authorized work. It never repeats the previous tool or prompt and never starts other sessions through this handoff.
5. In the continuation turn, call `get_service_status` and verify the request, new instance and release. Inspect the saved conversation/tool results before proceeding. A submitted handoff is not proof that its task completed. Stop/suspend, cancellation, revocation, expiry or ambiguous dispatch prevents automatic retry; inspect and recover manually.

`mode:"interrupt"` is an explicit global interruption and cannot request native handoff. Prefer idle drain for self-updates. If a successor cannot become ready, the bounded supervisor stops with an external failure receipt/journal; repair/select a compatible release and start the service manually. There is no automatic database rollback or repeated restart loop. Successful release selection persists through cold starts; backups currently require manual retention management. See [operator recovery instructions](SERVICE-RESTART.md).

The setup helper does not start any turn, enable a quota policy or request a restart. After transfer, choose a new bounded backlog assignment and quota-continuation authorization. Stronger named external controller credentials, a persistent review queue and scheduled automation remain backlog work.
