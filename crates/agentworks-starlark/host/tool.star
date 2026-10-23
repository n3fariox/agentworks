# Host API v1 tool display prelude, loaded as "@agentworks//tool.star".
# Header hooks only choose the one-line text shown for a tool card. Raw tool
# events, status and the expandable details are unchanged.

# `provider` is the harness ID (claude, codex, opencode, ...); `cwd` is the
# session's working directory, without a trailing slash. `state` is unused
# and always {}.
ToolContext = record(provider = str, cwd = str, state = dict[str, typing.Any])

# A summary of the tool's latest ACP fields. Strings are bounded by the host.
# `command` is the shell command (argv lists are joined), `description` is
# the harness's own short explanation when one is given, and `input` holds
# the remaining top-level scalar `rawInput` fields.
ToolCall = record(
    kind = str,
    title = str,
    command = str | None,
    description = str | None,
    input = dict[str, typing.Any],
)

def tool_from_json(data: dict[str, typing.Any]) -> ToolCall:
    return ToolCall(
        kind = data["kind"],
        title = data["title"],
        command = data.get("command"),
        description = data.get("description"),
        input = data.get("input", {}),
    )
