# Sample E-01 parser: Bazel phase and reported action/test counts.
# Mirrors the built-in U-05 rules in web/app.js. The host strips ANSI/OSC,
# splits lines on CR/LF, handles snapshots versus deltas and bounds line size.
load("@agentworks//progress.star", "Counts", "LineContext", "Progress")

_NUMBER = "(\\d{1,3}(?:,\\d{3})+|\\d+)"
_PHASE = "(?i)^(?:INFO:\\s*)?(Loading|Analyzing|Fetching|Building|Testing|Computing (?:main repo|repository) mapping):"
_ACTIONS = "^\\s*\\[" + _NUMBER + "\\s*/\\s*" + _NUMBER + "\\](.*)$"
_TESTS = "(?i)(?:^|[;\\]\\s])" + _NUMBER + "\\s*/\\s*" + _NUMBER + "\\s+tests\\b"
_UNITS = ["actions", "tests"]

# Largest integer the browser can display exactly (Number.MAX_SAFE_INTEGER).
_MAX_COUNT = 9007199254740991

def _counts(done: str, total: str) -> dict[str, int] | None:
    d = int(done.replace(",", ""))
    t = int(total.replace(",", ""))
    if d < 0 or t < d or t > _MAX_COUNT:
        return None
    return {"done": d, "total": t}

def on_line(ctx: LineContext, line: str) -> Progress | None:
    state = ctx.state
    label = None
    m = regex_match(_PHASE, line)
    if m != None:
        state["phase"] = m[0]
    m = regex_match(_ACTIONS, line)
    if m != None:
        value = _counts(m[0], m[1])
        if value != None:
            state["actions"] = value
            state["phase"] = "Executing"
            label = m[2].strip()[:200] or None
    m = regex_match(_TESTS, line)
    if m != None:
        value = _counts(m[0], m[1])
        if value != None:
            state["tests"] = value
            state["phase"] = "Testing"
    phase = state.get("phase")
    if phase == None:
        return None
    counts = [Counts(done = state[u]["done"], total = state[u]["total"], unit = u) for u in _UNITS if u in state]
    return Progress(phase = phase, label = label, counts = counts)
