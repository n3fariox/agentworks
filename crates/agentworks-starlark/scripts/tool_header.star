# Built-in tool card headers. Shipped with Agentworks and seeded into the
# scripts table. Claude describes each shell command in `description`; Codex
# sends only the command, which is reduced to its first meaningful step.
# Returning None keeps the harness title. Raw tool data is never changed.
load("@agentworks//tool.star", "ToolCall", "ToolContext")

_MAX = 100

# One shell step: quoted strings, $(...), escapes and redirections such as 2>&1 stay
# together; an unquoted ; & | or newline ends it. Groups: step, remainder.
# Unbalanced quotes match nothing.
_STEP = "(?s)^\\s*((?:\\$\\([^)]*\\)|[0-9]*>&[0-9]*|&>|\\\\.|\"(?:[^\"\\\\]|\\\\.)*\"|'[^']*'|[^;&|\"'\\\\\\n])*)(?:[;&|\\n][;&|\\n\\s]*(.*)|$)"

# Leading VAR=value assignments; a step of only assignments leaves "".
_ENV = "(?s)^(?:[A-Za-z_][A-Za-z0-9_]*=(?:\\$\\([^)]*\\)|\"(?:[^\"\\\\]|\\\\.)*\"|'[^']*'|[^\\s\"'$]|\\$)*(?:\\s+|$))+(.*)$"

# Wrappers that only change how the real command runs.
_WRAPPER = "(?s)^(?:(?:sudo|env|nohup|time|exec|command|nice)\\s+|timeout\\s+\\S+\\s+|mise\\s+exec\\s+(?:\\S+\\s+)*?--\\s+|(?:ba|z)?sh\\s+-l?c\\s+)(.*)$"

def _unquote(text: str) -> str:
    text = text.strip()
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "\"'" and text.count(text[0]) == 2:
        return text[1:-1].strip()
    return text

def _program(step: str) -> str:
    text = step.strip()
    for _ in range(4):
        env = regex_match(_ENV, text)
        if env != None:
            text = env[0]
        wrapper = regex_match(_WRAPPER, text)
        if wrapper == None:
            break
        text = _unquote(wrapper[0])
    return text

def _shorten(text: str) -> str:
    text = " ".join(text.split())
    if len(text) > _MAX:
        return text[:_MAX - 1].rstrip() + "…"
    return text

def _command(command: str) -> str | None:
    rest = _unquote(command)
    head = None
    more = 0
    for _ in range(24):
        if not rest.strip():
            break
        match = regex_match(_STEP, rest)
        if match == None:
            # Unbalanced quotes: show what is left as one step.
            step = rest.strip()
            rest = ""
        else:
            step = match[0].strip()
            rest = match[1]
        if step == "":
            continue
        if head == None:
            # `cd dir && cargo test` is about cargo, not cd or `PID=$(...)`.
            program = _program(step)
            if (program == "" or regex_match("^cd(?:\\s|$)", program) != None) and rest.strip():
                continue
            head = program if program else step
        else:
            more += 1
    if head == None or head == "":
        return None
    head = _shorten(head)
    return head + " +" + str(more) if more else head

def tool_header(ctx: ToolContext, tool: ToolCall) -> str | None:
    header = None
    if tool.description:
        header = _shorten(tool.description)
    elif tool.command:
        header = _command(tool.command)
    if header == None:
        header = _shorten(tool.title)
    if ctx.cwd:
        header = header.replace(ctx.cwd + "/", "")
    return header if header and header != tool.title else None

agent.register_tool_header(tool_header)
