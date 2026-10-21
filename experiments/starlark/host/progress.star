# E-01 host prelude for parser scripts, host API version 1.
# Host-owned and loaded as "@agentworks//progress.star". Parser scripts receive
# only these types and the host `regex_match` helper; they cannot create actions.

# Reported counts. The host rejects done > total; no percentage field exists.
Counts = record(done = int, total = int, unit = str)

# Serializable state carried between calls for one tool output stream.
ParserState = record(phase = str | None, counts = list[Counts])

# Structured progress shown to the user.
Progress = record(phase = str, label = str | None, counts = list[Counts])

# Return value of `on_line(state: ParserState, line: str) -> ParseResult`.
ParseResult = record(state = ParserState, progress = Progress | None)

def empty_state() -> ParserState:
    return ParserState(phase = None, counts = [])

def state_from_json(data: dict[str, typing.Any]) -> ParserState:
    """Rebuild persisted state; used by the host before each call."""
    counts = [Counts(done = c["done"], total = c["total"], unit = c["unit"]) for c in data.get("counts", [])]
    return ParserState(phase = data.get("phase"), counts = counts)
