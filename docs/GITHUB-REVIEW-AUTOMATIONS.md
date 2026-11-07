# Script-owned GitHub automations

Triggers and routing policy live in Starlark. Rust owns authenticated polling, transport facts, authorization, workspace/session operations, durable decisions and idempotent dispatch. Automation files are managed entirely in **Automations**, with compiler-enforced separation from Starlark hook files.

## Ownership and script kinds

Automation files and hook files are separate persisted kinds. **Automations** owns automation import, source editing, enable/disable, export and deletion. **Settings → Starlark hooks** owns progress, formatter, badge and PR-observation hooks. Automation files are excluded from the hook list, and the hook APIs reject editing, disabling, reprioritizing or deleting an automation filename.

The compiler enforces the distinction. An automation file may only register automation handlers; a hook file cannot register automation handlers. Mixed modules are rejected atomically. Ordinary functions and pure helpers remain reusable. The same bounded Starlark engine runs both kinds, but no registration can silently cross into the other lifecycle.

One file owns one automation entry. It can register multiple uniquely named triggers/handlers. Each trigger has its own durable polling baseline, while identical repository/account subscriptions share fetched data. Triggers within a file must declare the same project and execution host, which form its authority boundary; instructions and model settings can differ by handler.

The Automations page discovers registered automation files and derives names, settings, instructions and trigger descriptions from current source. Its database record is an operational index, retaining identity, authorship/permissions, revisions and history associations. Neither browser form state nor stale indexed display fields override source. Source edits refresh the entry; a missing file removes its entry on refresh/poll and cancels pending work. Deleting through Automations removes the file and entry together. Existing sessions and run history survive.

Use **Import .star** for a handwritten file. Import validates its automation-only registrations and in-file configuration, opens the source editor and defaults to paused. The file appears immediately after saving. A script requiring repairs exposes its diagnostic and remains editable from Automations. There is no separate binding-creation step.

## Recipe primitives

The form uses the bundled `github_review.star` template as source generation code. That template is no longer installed as a global script in Settings. Generated or imported files contain the callback and concrete registration, such as:

```python
load("@agentworks//workflow.star", "WorkflowContext", "Event", "Action", "sessions_for_pr", "queue_input", "start_session", "pr_worktree", "review_prompt", "skip", "instruction_steps")

SETTINGS = {"provider": "goose", "cwd": "/projects/service", "projectId": "project-id", "hostId": "local", "title": "Review incoming PRs"}
STEPS = ["Review correctness and tests; leave draft comments."]

def review_requested(ctx: WorkflowContext, event: Event) -> list[Action]:
    pr = event.data["pr"]
    matches = sessions_for_pr(ctx.sessions, pr, archived = False)
    if len(matches) > 1:
        return [skip("Multiple matching sessions")]
    prompt = review_prompt(pr, ctx.instructions)
    if matches:
        return [queue_input(matches[0], prompt)]
    return [start_session(settings = ctx.settings, workspace = pr_worktree(pr), prompt = prompt)]

agent.register_github_trigger(
    review_requested,
    host = "ghe.example.com",
    repo = "team/service",
    account = "reviewer",
    events = ["pr.assigned_to_me", "pr.review_requested"],
    settings = SETTINGS,
    instructions = instruction_steps(STEPS),
    steps = STEPS,
)
```

`name` defaults to `default`; a second registration must use a different name. `include_existing` defaults to false. Event selection belongs to the source registration. Constructors return data; Rust validates, authorizes and durably dispatches proposed effects. Source cannot perform I/O or register hooks from inside a running callback.

## Form → .star file

In Automations choose **A PR needs my review**. Fill in host, `owner/repo`, token account, assignment/review-request event choices, initial-backlog choice, instructions and session settings. **Preview .star** generates a complete ordinary script, and Save stores that file as an automation script under a unique `automation_<id>.star` name. **Download .star** exports the exact source. No script-name field or JSON parameters are needed.

The file contains literal subscription arguments, `SETTINGS`, ordered `STEPS`, the callback's routing policy, and the registration:

```python
SETTINGS = {"provider": "goose", "cwd": "/projects/service", "projectId": "project-id", "hostId": "local", "model": None}
STEPS = ["Review correctness and tests; leave draft comments."]

# review_requested is defined above this registration in the generated file.
agent.register_github_trigger(
    review_requested,
    host = "ghe.example.com",
    repo = "team/service",
    account = "reviewer",
    events = ["pr.assigned_to_me", "pr.review_requested"],
    include_existing = False,
    settings = SETTINGS,
    instructions = instruction_steps(STEPS),
    steps = STEPS,
)
```

The script is the program and owns its execution configuration. The automation catalog keeps its identity, enabled state, author, revision and an execution/authority index derived from the script. It does not supply a second user-authored JSON recipe to the generated program. Source and its index save in one transaction. Settings and instructions for dispatch come from the pinned script registration.

The form reopens from the saved file's registration metadata. Ordered instruction steps round-trip through `STEPS` / `instruction_steps`. Generated source remains editable in the automation editor. Once source is customized, Save preserves it verbatim, even if form fields change. **Replace custom script from form** is the explicit action that discards custom code and regenerates it. Syntax/type errors leave the previous saved file intact. Custom policies that cannot be represented by this form stay in source; the form never silently rewrites them.

Source imports and edits use the Automations API (`source`, optional `scriptName`, and `enabled`); separate user-supplied JSON script bindings are rejected. The prototype `github_review.star` is bundled generation code rather than an independently enabled global script. Each saved file has one enable state, managed from Automations.

Choose a project, working directory, execution host, harness and model/provider as usual. A script's settings may also specify a role library ID. Existing sessions retain their own harness/model/settings. Current action authority remains scoped to the saved automation's project and execution host; editing source cannot silently grant access to a different project or add agent-control grants.

GitHub API requests and `gh` credential lookup stay on the main host. The account must be enabled there, and the verified token identity determines “me.” Worker Git operations use the execution host's own Git authentication.

## Primitives and event input

| API | Behavior |
| --- | --- |
| `parameter(name)` | Low-level template helper retained for compatibility; managed automation files declare concrete configuration. |
| `instruction_steps(steps)` | Compile the file’s ordered instruction strings into a prompt. |
| `sessions_for_pr(ctx.sessions, pr, archived=False)` | Filter the supplied session snapshot by full host/owner/repository/PR identity and archive state. |
| `queue_input(session, prompt, key="queue")` | Propose a durable queued input to a same-project session. |
| `start_session(settings=..., workspace=..., prompt=..., key="start")` | Propose session creation through the normal validated creation path. `workspace` is optional. |
| `pr_worktree(pr)` | Build workspace data pinned to the observed SHA. The host supplies a stable unique branch if absent. Origin identity and PR-head fetch use the existing worktree machinery. |
| `review_prompt(pr, instructions)` | Build a review prompt containing the canonical PR URL and exact SHA, preserving local changes and reporting blockers without interactive questions. Optional helper, not a host-injected prompt. |
| `skip(reason)` | Finish the occurrence as skipped, with a visible reason. Must be the only action. |

Callbacks receive the existing typed `WorkflowContext` and `Event`. `event.kind` is a selected observed transition. `event.data` contains `pr`, `assigned`, `reviewRequested`, `initial`, and `subscription`. If both selected transitions happen together they coalesce into one occurrence (`event.kind` is `pr.review_requested`, with both flags true). `pr` includes host/owner/repo/number, normalized PR status, URL and head SHA.

`ctx.sessions` is a read-only input snapshot of known sessions associated with this event's PR in the bound project, including archived sessions. Associations come from explicit PR attachments and last-observed tracking snapshots, with the same freshness limitations as the GitHub panel's session chooser. Deleted sessions are absent. Effect targets are rechecked against live project/archive state before dispatch. The lookup primitive does not perform synchronous database or network I/O in the interpreter.

`ctx.parameters` is empty for managed files. `ctx.settings` and `ctx.instructions` come from the specific trigger’s pinned source registration. `ctx.state` starts empty for each occurrence; mutations are saved with that occurrence's decision, not shared across PRs. A callback returns up to four actions with unique keys of 1–32 letters, digits, `_` or `-`. Prompt limits are 32 KB; interpreter source/input/output/time/heap bounds still apply. An empty action list completes without effects. The prototype workflow engine's older `finish` and positional `start_session(project=...)` forms remain available to its own host; production event automations use the action forms above.

## Observation and cadence

An enabled binding polls while the dashboard is closed, using the host's GitHub polling interval (default five minutes). Identical repository/account subscriptions share membership fetches and the normalized PR status pool used by badges. Dashboard discovery remains a separate visibility-driven query. Requests share the main-host request serialization and rate-limit backoff.

Complete repository-scoped GraphQL searches establish a baseline of open PRs assigned to the token identity and requesting its review. Existing requests are ignored unless the registration opts into `include_existing`. Later false-to-true transitions in a selected category produce occurrences. A push alone does not trigger another review. Changes entirely between polls may be missed. Team review requests follow GitHub's `review-requested:login` search semantics.

Failed/partial queries, identity mismatches, bad pagination and truncated lists never replace the baseline with an empty list. Each category is bounded to five pages / 500 PRs; an oversized subscription reports an error. Subscription errors appear on the automation. Pausing discards its baseline; resuming or changing subscription fields establishes a new one with the script's initial-backlog policy. Deleting the project pauses its script automations instead of falling back to projectless routing.

## Decisions, replay and completion

A baseline and its occurrences commit atomically. Each occurrence pins the script source/hash and handler that observed it. Decisions already accepted for dispatch replay their persisted actions with stable request IDs. Saving a changed file cancels undispatched occurrences from older revisions, and pausing/deleting also cancels pending work.

The interpreter runs a callback with no I/O, then Rust validates its complete action list and durably saves the decision and action keys before any effects. Each action has an independent stable request ID. A crash after acceptance but before acknowledgement reuses queue bindings or session-creation reservations. Already completed actions are not repeated when later actions resume. Interrupted Git mutations fail closed under existing creation reservations. Transient storage failures retry the same accepted effect instead of inventing a new request.

The run finishes once its proposed actions are dispatched. A review session's lifetime does not keep the automation active. Completed means dispatch succeeded, not that the review finished. New requests for other PRs can dispatch while earlier reviews work. Queued inputs preserve busy-session ordering and all suspension, pause, availability and usage-limit gates; the automation never clears those gates or alters an existing checkout.

Script errors and permanent dispatch errors are recorded as skipped with a reason. Unchanged assignment/review-request membership does not retry a skipped occurrence; an observed removal/re-addition is new work. There is no context-free **Run now** for a script event trigger.

Human definitions authorize effects in their configured project. Agent-created definitions recheck their author's live project and `manageAutomations`, `startSessions` and `promptSessions` grants. New review sessions inherit no control grants. PR content remains untrusted data. Polling itself only reads GitHub; the recipe's prompt determines any work the review agent may subsequently perform.

Schema v14 introduced the GitHub event/action ledgers. Schema v15 persists the automation script kind and adds `automation_trigger_baselines` keyed by automation and trigger name. Existing v14 default baselines migrate once, without replacing later observations. Older auto-only sources are classified without dropping source edits or enable state; mixed modules fail validation. An earlier bound template lacking in-file settings is reported for repair instead of executing cached form configuration. The unused global review template is removed. Existing scheduled/manual behavior is unchanged. Run and skipped-occurrence history remains available through the normal automation runs endpoint.
