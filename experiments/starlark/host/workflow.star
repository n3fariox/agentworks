# E-01 host prelude for workflow scripts, host API version 1.
# Host-owned and loaded as "@agentworks//workflow.star". Constructors only
# return data: Rust persists, authorizes and dispatches every proposed action.

# An event delivered by the host: "start", "action_completed" or "action_denied".
# `outcome` is "succeeded", "failed", "cancelled" or "interrupted" for completions.
Event = record(
    id = str,
    kind = str,
    action_key = str | None,
    outcome = str | None,
    data = dict[str, typing.Any],
)

# A proposed effect. `key` is unique within one workflow run and becomes part
# of the stable request ID, so proposing the same key twice cannot repeat it.
Action = record(kind = str, key = str, project = str, prompt = str)

# Return value of `on_event(state: dict[str, typing.Any], event: Event) -> Decision`.
# A non-None outcome finishes the workflow.
Decision = record(
    state = dict[str, typing.Any],
    actions = list[Action],
    outcome = str | None,
)

def start_session(key: str, project: str, prompt: str) -> Action:
    return Action(kind = "start_session", key = key, project = project, prompt = prompt)

def decide(state: dict[str, typing.Any], actions: list[Action] = [], outcome: str | None = None) -> Decision:
    return Decision(state = state, actions = actions, outcome = outcome)

def event_from_json(data: dict[str, typing.Any]) -> Event:
    """Rebuild a persisted event; used by the host before each call."""
    return Event(
        id = data["id"],
        kind = data["kind"],
        action_key = data.get("action_key"),
        outcome = data.get("outcome"),
        data = data.get("data", {}),
    )
