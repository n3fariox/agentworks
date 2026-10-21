# Sample E-01 parser: Bazel phase and reported action/test counts.
# Mirrors the built-in U-05 rules in web/app.js. The host strips ANSI/OSC,
# splits lines on CR/LF, handles snapshots versus deltas and bounds line size.
load("@agentworks//progress.star", "Counts", "ParseResult", "ParserState", "Progress")

_NUMBER = "(\\d{1,3}(?:,\\d{3})+|\\d+)"
_PHASE = "(?i)^(?:INFO:\\s*)?(Loading|Analyzing|Fetching|Building|Testing|Computing (?:main repo|repository) mapping):"
_ACTIONS = "^\\s*\\[" + _NUMBER + "\\s*/\\s*" + _NUMBER + "\\](.*)$"
_TESTS = "(?i)(?:^|[;\\]\\s])" + _NUMBER + "\\s*/\\s*" + _NUMBER + "\\s+tests\\b"

# Largest integer the browser can display exactly (Number.MAX_SAFE_INTEGER).
_MAX_COUNT = 9007199254740991

def _counts(done: str, total: str, unit: str) -> Counts | None:
    d = int(done.replace(",", ""))
    t = int(total.replace(",", ""))
    if d < 0 or t < d or t > _MAX_COUNT:
        return None
    return Counts(done = d, total = t, unit = unit)

def _with(counts: list[Counts], value: Counts) -> list[Counts]:
    return [c for c in counts if c.unit != value.unit] + [value]

def on_line(state: ParserState, line: str) -> ParseResult:
    phase = state.phase
    counts = state.counts
    label = None
    m = regex_match(_PHASE, line)
    if m != None:
        phase = m[0]
    m = regex_match(_ACTIONS, line)
    if m != None:
        value = _counts(m[0], m[1], "actions")
        if value != None:
            counts = _with(counts, value)
            phase = "Executing"
            label = m[2].strip()[:200] or None
    m = regex_match(_TESTS, line)
    if m != None:
        value = _counts(m[0], m[1], "tests")
        if value != None:
            counts = _with(counts, value)
            phase = "Testing"
    next = ParserState(phase = phase, counts = counts)
    if phase == None:
        return ParseResult(state = next, progress = None)
    return ParseResult(state = next, progress = Progress(phase = phase, label = label, counts = counts))
