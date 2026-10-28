# Host API v1 chat display prelude, loaded as "@agentworks//chat.star".
# `chat.agent` and `chat.user` formatters rewrite how the session's agent and
# user messages are shown, one Markdown line at a time, in order. Lines inside fenced code blocks are
# never passed. Return the replacement, which may span several lines, or None
# to keep the line. Nothing the agent sees or Agentworks stores changes; the
# result is sanitized like any other Markdown. Use `map_prose(line, f)` to
# leave inline code and existing links untouched.

# `provider` is the harness ID (claude, codex, opencode, ...); `cwd` is the
# session's working directory, without a trailing slash. `state` starts empty
# for each message and carries this formatter's plain data from one line to the
# next. With a `text_pattern`, it sees only matching lines; without one, every line.
ChatContext = record(provider = str, cwd = str, state = dict[str, typing.Any])
