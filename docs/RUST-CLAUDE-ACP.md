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

Subagent output (`forwardSubagentText`) goes into the Agent tool card: its prompt,
then the subagent's messages as they arrive, completed or failed on the task
notification. It is kept out of the main transcript, so fork boundaries stay on
Claude's own messages. The subagent's tool calls appear as their own cards with
`_meta.claudeCode.parentToolUseId`. History replay does not include subagent
transcripts.

History comes from Claude's own transcripts in `$CLAUDE_CONFIG_DIR/projects` (default
`~/.claude/projects`). Resume and load require the transcript in the project
directory of the requested cwd; a missing one is reported as not found. Claude writes
a forked transcript only after its first prompt, so the bridge writes the fork itself:
it copies the conversation up to the boundary message into the child cwd's project
with fresh message ids. Unknown boundaries fail rather than inheriting newer history.
Deletion removes the transcript and its sidecar directory.

This is intentionally smaller than the npm adapter. Not implemented: the AIR/JetBrains
extensions (native subagent child sessions, steering, async tasks, goals, file-change
reports), fast mode, custom agents, provider routing/gateways, terminal login
methods, MCP OAuth, plan clear-context, prompt suggestions, and session titles beyond
those Claude stores. If the CLI exits, the active turn fails with
`errorKind: transport_lost`; reopen the session to continue.

A remote worker offers `claude-rust` when it finds a native `claude` (or
`CLAUDE_CODE_EXECUTABLE`), even without Node. Remote enrollment still installs the
existing npm dependencies.

Validation (no Node or inference needed):

```sh
cargo build -p agentworks-claude-acp
cargo test -p agentworks-claude-acp
python3 tests/claude-acp.test.py -v
```

The Python test uses a fake stream-json CLI and the built standalone binary. The Rust
integration test uses the same typed ACP client as Agentworks, so every update kind
is checked against the schema.

The installed Claude Code 2.1.306 also passed live runs: model and mode selection,
Bash with terminal output, a rejected Write, cancellation with a pending approval,
`/context`, listing, close/load replay, fork at a message boundary (the fork did not
see later turns), and deletion. AskUserQuestion, ExitPlanMode, MCP servers and remote
execution have not yet been validated live.
