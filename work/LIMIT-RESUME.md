# L-09 — Resume when usage limits reset

Status: planned; highest-priority next feature requested by the user. No policy is enabled and no implementation batch is started by this planning update.

User intent: “work on the backlog, and keep going when limits reset.” A session can be explicitly authorized to continue unfinished work across quota windows, and an authorized agent can carry that policy into sub-sessions it creates through MCP. This is a session continuation timer, not a restart of the Agentworks server.

## Policy and controls

- Off by default for existing/new sessions and child sessions unless inheritance is explicitly requested from an enabled parent. Provide an obvious human session toggle and show its effective policy. Any future project/host default also starts disabled.
- Show “Paused by usage limit — resumes at …”, a countdown, the blocking quota/window, reset-data source/freshness and next attempt. Provide Disable/Cancel timer and an explicit manual resume option. Integrate compact information into U-07's prompt statusline; notifications can use U-02 when available.
- Retain the assigned scope and stop conditions, including “stop after this round”, plus any configured deadline, attempt/spend bounds and concurrency limits. Completion, explicit Stop/Pause, deletion, disabling the policy or reaching a stop condition cancels pending continuation. Server/browser reconnect must not undo that decision.

## Recognizing a limit

Use structured harness errors, quota events or a provider-specific trusted adapter interpretation to distinguish exhausted usage windows from tool failures, expired authentication, network failures, permission waits and context exhaustion. Arbitrary tool/inbox text cannot schedule work. Record whether the exhausted window is account-wide or specific to a model/session.

Prefer an authoritative reset timestamp or retry-after value, store an absolute due time and render it in the user's timezone. Consider every blocking window: a short-window reset must not wake work while a longer window still blocks it. If the reset cannot be established, expose that uncertainty and an explicit scheduling choice rather than displaying a guessed exact countdown. Repeated limits update the due time with bounded backoff; avoid busy polling or model calls merely to probe whether quota returned.

## Durable continuation

Persist policy, reason, due time, originating limit/event ID, task scope, generation/cancellation marker and a stable continuation request ID. Scheduler state must survive app updates/restarts. Claim each due attempt durably and reconcile request IDs after a crash; do not dispatch the same continuation twice.

At dispatch, recheck that the policy is enabled, the assignment remains unfinished, stop conditions permit work, the session still exists, no competing turn/approval is active, and the native harness context is resumable. Keep the original directory/worktree and normal root/access rules. If the native context is missing, pause visibly rather than silently creating a fresh context.

Resume the harness if needed, then submit a recorded continuation turn directing it to inspect current task/tool results and continue the saved assignment. Do not blindly resend the original user turn or last command: earlier tool effects may already have happened. Generic failures/cancellations remain stopped; L-09 handles recognized usage-limit pauses only. Coordinate with queued inputs so a scheduled continuation and a queued user turn cannot race; explicit newer user direction takes precedence.

Coordinate wake-ups sharing the same host/harness/account quota bucket: stagger or bound concurrent resumes and respect provider retry guidance. Use opaque account identity where available, without copying credentials into timer records. Expose scheduled continuations separately from active execution in activity/status APIs so updates can preserve timers without claiming that unfinished work is complete.

## MCP child policy propagation

Extend authorized `start_session` with an explicit inherit-resume-policy option. A child may inherit only an already enabled effective parent policy, including its task stop/deadline/budget constraints; it can opt out or narrow those constraints. It cannot enable automatic continuation from a disabled parent, broaden inherited authorization or obtain coordinator grants through this option. Existing child grants remain off by default, and existing project/worktree enforcement is unchanged.

Persist and display the policy's origin and parent linkage. Retry of the same MCP creation request returns the same child/policy rather than another timer/session; changed request payload conflicts normally. Honor revocation and explicit human stops at dispatch. An autonomous parent can therefore create isolated workers that independently wait for quota recovery while respecting the original assignment's bounds.

## Acceptance checks

Use deterministic fake quota events/clocks and demo sessions; no paid inference is required. Verify default-off behavior; opt-in short/long-window resets; unknown/stale reset data; repeated limits/backoff; visible countdown/cancellation; no wake for arbitrary errors or injected text; shared-account concurrency; native resume with preserved context/worktree; and no last-tool replay.

Exercise crashes before/after timer claim and continuation dispatch, app restart with overdue timers, duplicate limit events, user prompt versus timer races, active approvals, completion and explicit Stop/Pause. Verify one effective continuation per attempt, retained cancellation markers and explicit stop-after-round behavior. Test MCP inherit/opt-out/narrowing, disabled-parent rejection, grant revocation, no grant escalation, child creation idempotency and persisted policy lineage.

L-09 can use a small Rust-backed durable scheduler before any general workflow language exists. Reuse its state/dispatch primitives later for automations or the Starlark experiment; E-01 is not a prerequisite for this priority feature.
