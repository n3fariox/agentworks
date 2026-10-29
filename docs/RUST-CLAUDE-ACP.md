# Rust Claude ACP bridge

Choose **Claude** (provider ID `claude-rust`) in the harness picker. It replaced the
npm `claude-agent-acp` adapter; sessions saved under that adapter's ID, `claude`, now
run on this bridge too, resuming from the same Claude transcripts. It forks natively
at the recorded message boundary instead of carrying a transcript into a fresh
session.

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
models without it, as the former npm adapter did.

Agentworks [roles](ROLES.md) and its shared session context arrive as an ACP
`systemPrompt` on every new, resume, load and fork request; Claude Code does not store
them. `append` becomes `--append-system-prompt` and `override` becomes
`--system-prompt`, so `claude-rust` accepts roles.

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

Claude's other background tasks become [background jobs](BACKGROUND-JOBS.md) when
the client advertises the AIR `asyncTasks` capability, as Agentworks does; the bridge
then advertises it back. A Bash command run in the background (or moved there later)
is announced with `async_task_spawned` on the session of its tool call, with the
task's name, the tool call ID and the output file Claude names in the result. Its
`task_progress`, `task_updated` and `task_notification` events become progress and
state updates, and a task missing from `background_tasks_changed` is reported
stopped. The launching Bash card completes with
`_meta.jetbrains.air.asyncTasks.backgrounded`. `_session/async_task/stop` sends
Claude's `stop_task` control request for that one task; it answers
`{"stopped":false}` for a task that is unknown, already ending or already ended.
Closing or deleting the session reports its running jobs stopped. Subagents stay
child sessions, and Monitor tasks, which only feed the model, are never shown. Load
replay does not rebuild jobs; Agentworks keeps its own record.

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

This is intentionally smaller than the former npm adapter. Not implemented: the AIR/JetBrains
extensions other than async tasks (steering, goals, file-change
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
`CLAUDE_CODE_EXECUTABLE`), even without Node.

Validation (no Node or inference needed):

```sh
cargo build -p agentworks-claude-acp
cargo test -p agentworks-claude-acp
python3 tests/claude-acp.test.py -v
node node_modules/@playwright/test/cli.js test tests/claude-rust-subagents.spec.mjs
```

The browser test runs Agentworks with the `claude-rust` harness and the fake CLI,
checking the `Sub:` tab, the child's approval, transcript separation and reload, and
background jobs: the panel, the backgrounded tool card and stopping one job.

The Python test uses a fake stream-json CLI and the built standalone binary. The Rust
integration test uses the same typed ACP client as Agentworks, so every update kind
is checked against the schema.

The installed Claude Code 2.1.306 also passed live runs: model and mode selection,
Bash with terminal output, a rejected Write, cancellation with a pending approval,
`/context`, listing, close/load replay, fork at a message boundary (the fork did not
see later turns), deletion, and background jobs (two backgrounded Bash commands
announced with their output files, one completing after the turn, the other stopped
individually). AskUserQuestion, ExitPlanMode, MCP servers and remote
execution have not yet been validated live.
