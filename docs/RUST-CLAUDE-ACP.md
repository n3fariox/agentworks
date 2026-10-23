# Experimental Rust Claude ACP bridge

Choose **Claude · Rust ACP** (provider ID `claude-rust`) in the harness picker. The
**Claude** option keeps using the npm `claude-agent-acp` adapter. Both share the host
Claude sign-in, API-key store, default permission mode and limit-resume handling;
sessions keep the adapter they were created with. Unlike the npm Claude harness,
`claude-rust` forks natively at the recorded message boundary instead of carrying a
transcript into a fresh session.

The `agentworks-claude-acp` crate implements it; the Agentworks executable runs it
as `agentworks claude-acp`. Each ACP session starts one native `claude` process in
stream-json mode (`--input-format stream-json --output-format stream-json
--permission-prompt-tool stdio`) and translates its messages and control requests
into ACP v1. Set `CLAUDE_CODE_EXECUTABLE` to a native Claude Code executable, or put
`claude` on PATH. No npm package or Node interpreter is used. Host sign-in and
explicit API-key isolation follow the existing provider launch configuration.

The bridge handles session creation, resume, load with history replay, listing,
close, deletion, fork at a recorded message boundary, extra workspace directories,
stdio/HTTP/SSE MCP configuration, text/image/context input, streamed messages and
thinking, tool calls (Bash terminal output metadata, file diffs, search, fetch,
subagents), plans from TodoWrite and the task tools, permission approvals with
Claude's suggested rules, AskUserQuestion and MCP forms, ExitPlanMode mode choices,
slash commands, cancellation, and model, effort and permission-mode options
(`default`, `acceptEdits`, `plan`, `auto`, and `bypassPermissions` when allowed).
Usage updates carry context occupancy, cost, `_claude/model` and the raw
`_claude/rateLimit` information; failed turns report `errorKind` (for example
`rate_limit`) for limit-resume handling. Auto mode falls back to Accept edits for
models without it, as in the npm adapter.

Turns end when Claude Code reports idle (`CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS`),
not at its first `result`. Claude Code runs subagents in the background: the Agent
tool returns at once, Claude's own turn ends, and Claude continues by itself when the
subagent finishes. The prompt stays open through all of that, so subagent permission
requests and Claude's follow-up answer belong to the turn that started them. CLIs
without state events end the prompt at `result`, as before.

Subagents become [native ACP child sessions](SUBAGENTS.md) when the client
advertises the `subagents` capability, as Agentworks does. Each Claude task is
announced with `subagent_update` on the session that owns its Agent tool call (the
root, or a parent subagent for nested agents), using the task ID as the child session
ID, the task description as its title and the prompt as its description. The
subagent's text, thinking, tool calls, permission requests and questions then use the
child session, so they appear in its `Sub:` tab; the child is reported idle when the
task finishes, or cancelled when the turn ends first. The Agent tool call stays on
its owner, completing with the task summary.

Clients without the capability get the subagent's text (`forwardSubagentText`) in
the Agent tool card instead, and its tool calls as cards with
`_meta.claudeCode.parentToolUseId`. Either way it is kept out of the main
transcript, so fork boundaries stay on Claude's own messages.

Loading a saved conversation (as Agentworks does when importing one) replays
subagents too. Claude stores each in `<session>/subagents/agent-<id>.jsonl` with a
`meta.json` naming the Agent tool call that started it. Replay announces that child
where its Agent call appears, replays its transcript into it (descending into
nested subagents), then reports it idle. Child IDs match the live task IDs. A
subagent's opening prompt stays its description rather than a user message.
Agentworks' own resumes do not need this: they rebuild tabs from saved events.

History comes from Claude's own transcripts in `$CLAUDE_CONFIG_DIR/projects` (default
`~/.claude/projects`). Resume and load require the transcript in the project
directory of the requested cwd; a missing one is reported as not found. Claude writes
a forked transcript only after its first prompt, so the bridge writes the fork itself:
it copies the conversation up to the boundary message into the child cwd's project
with fresh message ids. Unknown boundaries fail rather than inheriting newer history.
Deletion removes the transcript and its sidecar directory.

This is intentionally smaller than the npm adapter. Not implemented: the AIR/JetBrains
extensions (steering, async tasks, goals, file-change
reports), fast mode, custom agents, provider routing/gateways, terminal login
methods, MCP OAuth, plan clear-context, prompt suggestions, and session titles beyond
those Claude stores.

If a session's CLI exits on its own, nothing is respawned or retried. A running turn
fails with `errorKind: transport_lost` and a message saying it was not retried, and
the bridge then exits, reporting the CLI's exit status on stderr. Agentworks shows
the session as failed; **Resume** reopens it with `session/resume`. Close, delete and
a CLI that fails while a session is opening are ordinary errors and leave the bridge
running.

A remote worker offers `claude-rust` when it finds a native `claude` (or
`CLAUDE_CODE_EXECUTABLE`), even without Node. Remote enrollment still installs the
existing npm dependencies.

Validation (no Node or inference needed):

```sh
cargo build -p agentworks-claude-acp
cargo test -p agentworks-claude-acp
python3 tests/claude-acp.test.py -v
node node_modules/@playwright/test/cli.js test tests/claude-rust-subagents.spec.mjs
```

The browser test runs Agentworks with the `claude-rust` harness and the fake CLI,
checking the `Sub:` tab, the child's approval, transcript separation and reload.

The Python test uses a fake stream-json CLI and the built standalone binary. The Rust
integration test uses the same typed ACP client as Agentworks, so every update kind
is checked against the schema.

The installed Claude Code 2.1.306 also passed live runs: model and mode selection,
Bash with terminal output, a rejected Write, cancellation with a pending approval,
`/context`, listing, close/load replay, fork at a message boundary (the fork did not
see later turns), and deletion. AskUserQuestion, ExitPlanMode, MCP servers and remote
execution have not yet been validated live.
