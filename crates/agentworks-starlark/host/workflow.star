# E-01 host prelude for workflow scripts, host API version 1.
# Host-owned and loaded as "@agentworks//workflow.star". Constructors only
# return data: Rust persists, authorizes and dispatches every proposed action.

# Context for `on_event(ctx: WorkflowContext, event: Event) -> list[Action]`.
# `state` belongs to the script and is mutated in place. It may hold only
# plain data (str, int, float, bool, None, list, dict) and starts as {}.
# Unlike parser state it is durable: Rust commits it with the decision.
WorkflowContext = record(
    workflow = str,
    owner = str,
    state = dict[str, typing.Any],
)

# An event delivered by the host: "start", "action_completed" or "action_denied".
# `outcome` is "succeeded", "failed", "cancelled" or "interrupted" for completions.
Event = record(
    id = str,
    kind = str,
    action_key = str | None,
    outcome = str | None,
    data = dict[str, typing.Any],
)

# A proposed effect, built with the constructors below. A start_session `key`
# is unique within one run and becomes part of the stable request ID, so
# proposing the same key twice cannot repeat the effect.
Action = record(
    kind = str,
    key = str,
    project = field(str | None, None),
    prompt = field(str | None, None),
    outcome = field(str | None, None),
)

def start_session(key: str, project: str, prompt: str) -> Action:
    return Action(kind = "start_session", key = key, project = project, prompt = prompt)

def finish(outcome: str) -> Action:
    """Ends the run with `outcome`. Must be the only action returned."""
    return Action(kind = "finish", key = "finish", outcome = outcome)

def event_from_json(data: dict[str, typing.Any]) -> Event:
    """Rebuild a persisted event; used by the host before each call."""
    return Event(
        id = data["id"],
        kind = data["kind"],
        action_key = data.get("action_key"),
        outcome = data.get("outcome"),
        data = data.get("data", {}),
    )
