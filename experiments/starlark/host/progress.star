# E-01 host prelude for parser scripts, host API version 1.
# Host-owned and loaded as "@agentworks//progress.star". Parser scripts receive
# only these types and the host `regex_match` helper; they cannot create actions.

# Reported counts. The host rejects done > total; no percentage field exists.
Counts = record(done = int, total = int, unit = str)

# Structured progress shown to the user.
Progress = record(phase = str, label = str | None, counts = list[Counts])

# Context for `on_line(ctx: LineContext, line: str) -> Progress | None`.
# `state` belongs to the script and is mutated in place. It may hold only
# plain data (str, int, float, bool, None, list, dict) and starts as {} for
# each output stream. Changes are kept only for committed lines: for the
# unterminated current line (`provisional`) they are discarded. State is
# temporary; after a reload it is rebuilt by replaying the raw output.
LineContext = record(
    command = str,
    stream = str,
    provisional = bool,
    state = dict[str, typing.Any],
)
