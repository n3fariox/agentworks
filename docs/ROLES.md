# Roles

A role is a named set of instructions that Agentworks adds to an agent's system prompt. Choose one under **Role** in New session. Roles shape how an agent works; they never grant Agentworks access. Starting sessions, prompting peers and other MCP controls are still granted only through **Agentworks access** and **Agents & inbox**.

## Library

Four built-in roles ship with Agentworks: Coordinator, Implementer, Reviewer and Researcher. Built-in roles are read-only and follow the shipped text. Custom roles are stored in the Agentworks SQLite database:

| Method | Path | Body |
| --- | --- | --- |
| `GET` | `/api/roles` | Built-in roles first, then custom roles by name. |
| `POST` | `/api/roles` | `{name, description?, instructions}`; returns the role with a generated `id`. |
| `PUT` | `/api/roles/{id}` | Same body. Built-in roles return 409. |
| `DELETE` | `/api/roles/{id}` | Built-in roles return 409. |

Names are 1–80 characters, descriptions up to 200 and instructions up to 16 KB. `POST /api/sessions` accepts `role` with a role ID. An unknown role, or a role for an agent that cannot accept one, fails before any workspace is prepared.

## Session snapshot

A session stores the role's name, instructions and content hash when it is created. Editing or deleting the library entry later does not change existing sessions. Session metadata and snapshots expose `role: {id, name, hash}`; the instructions stay on the server.

Harnesses do not persist a client system prompt, so Agentworks sends the stored role with every ACP `session/new`, `session/load`, `session/resume` and `session/fork`, including after a server restart. The prompt uses ACP's `append` mode, after the harness's own instructions. A small shared block identifies the current session and directs the agent to Agentworks MCP tools for peers, inbox, updates and completion, with stable request IDs for retries. This block is also sent for sessions without a role when the harness advertises system prompt support; MCP server instructions and tool descriptions carry the detailed guidance and live access.

A fork always keeps its parent's stored role. Forking to an agent that cannot accept that role is rejected before preparing a workspace. Child sessions started over MCP and automation runs have no role.

## Agent support

Roles use the unstable ACP `sessionCapabilities.systemPrompt` capability (`unstable_session_system_prompt` in the Rust SDK).

| Agent | Roles |
| --- | --- |
| Claude (`claude-agent-acp`) | Supported. Appended to the `claude_code` preset prompt. |
| Claude · Rust ACP (`claude-rust`) | Supported. Passed to Claude Code as `--append-system-prompt`. |
| Demo agent | Supported. `role?` replies with the received system prompt. |
| Codex (`codex-acp`) | Not yet. The adapter does not accept `systemPrompt`. |
| OpenCode | Not yet. |

`/api/status` reports `roles` per provider, and New session disables the picker for unsupported agents. If a harness with a stored role stops advertising the capability, opening the session fails with an error instead of continuing without the role. The transcript is kept.

## Not yet included

Project and global default roles, a role editor in Settings, roles for MCP-started children, and Codex support through the vendored adapter patch.

Run `npm run test:roles` after `cargo build --locked`. It is also part of `npm run verify`.
