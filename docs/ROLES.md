# Roles

A role is a named set of instructions that Agentworks adds to an agent's system or developer instructions. Choose one under **Role** in New session. Roles shape how an agent works; they never grant Agentworks access. Starting sessions, prompting peers and other MCP controls are still granted only through **Agentworks access** and **Agents & inbox**.

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

Agentworks does not rely on harness persistence of client instructions. It sends the stored role with every ACP `session/new`, `session/load`, `session/resume` and `session/fork`, including after a server restart. The prompt uses ACP's `append` mode, after the harness's own instructions. A small shared block identifies the current session and directs the agent to Agentworks MCP tools for peers, inbox, updates and completion, with stable request IDs for retries. This block is also sent for sessions without a role when the harness advertises system prompt support; MCP server instructions and tool descriptions carry the detailed guidance and live access.

A fork always keeps its parent's stored role. Forking to an agent that cannot accept that role is rejected before preparing a workspace. Child sessions started over MCP and automation runs have no role.

## Agent support

Roles use the unstable ACP `sessionCapabilities.systemPrompt` capability (`unstable_session_system_prompt` in the Rust SDK).

| Agent | Roles |
| --- | --- |
| Claude (`claude-agent-acp`) | Supported. Appended to the `claude_code` preset prompt. |
| Claude · Rust ACP (`claude-rust`) | Supported. Passed to Claude Code as `--append-system-prompt`. |
| Goose · embedded | Supported. Appended through a managed Goose system-prompt entry, replaced on new/load/fork. |
| Demo agent | Supported. `role?` replies with the received system prompt. |
| Codex · Rust ACP (`codex-rust`) | Supported. ACP append text is passed as Codex app-server `developerInstructions`, preserving its base instructions. |
| Codex (`codex-acp`) | Not yet. The vendored Node adapter does not accept `systemPrompt`. |
| OpenCode | Not yet. |

`/api/status` reports `roles` per provider, and New session disables the picker for unsupported agents. If a harness with a stored role stops advertising the capability, opening the session fails with an error instead of continuing without the role. The transcript is kept.

## Rust Codex mapping and constraints

The Rust bridge advertises append-only `systemPrompt` and `session/resume`.
New maps to `thread/start`; load and resume map to `thread/resume`; fork maps to
`thread/fork`. Load replays history; resume does not. Only text blocks are accepted,
joined in order with blank lines. Unsupported modes (including override), content
types and malformed prompts fail before any native lifecycle request.

ACP permits developer-instruction precedence for append mode. The bridge never
sets `baseInstructions`: Codex retains its own base/tool guidance. Codex's
`developerInstructions` field **replaces**, rather than appends to, configured
`developer_instructions`. The bridge reads effective configuration with
`config/read` for the session directory and prepends that guidance (or an explicit
`CODEX_CONFIG.developer_instructions` override) to the ACP text. Failure to read
configuration fails the open. Other Codex-managed instruction sources stay under
Codex's control. Arbitrary `CODEX_CONFIG` overrides that change configuration
resolution, such as selecting a different profile, are not resolved by this read;
provide the intended `developer_instructions` explicitly in that case.

Codex can ignore overrides when rejoining an already subscribed thread. The
bridge rejects load/resume with a system prompt for a session it already has
open; close it first. Agentworks normally opens a new bridge process when
resuming a suspended session. This does not provide live role editing. A fork
receives the parent's stored role and the **child's** Agentworks session ID;
inherited native history can still contain older context.

The mapping was checked against Codex CLI 0.163.0 generated app-server schemas and
[source configuration precedence](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/core/src/config/mod.rs),
plus [native lifecycle handling](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/src/request_processors/thread_processor.rs).
Offline wire/application tests verify delivery, not model compliance or behavior
of every installed Codex version. Run `python3 tests/codex-acp.test.py` and
`python3 tests/codex-rust-harness.test.py` after building both binaries.

## Not yet included

Project and global default roles, a role editor in Settings, roles for MCP-started children, and Codex support through the vendored Node adapter patch.

Run `npm run test:roles` after `cargo build --locked`. It is also part of `npm run verify`.
