# Session secrets (proposal)

Status: storage, Settings → Secrets, per-session assignment, `environment` delivery (local and remote, with automatic restart), transcript scrubbing, discovery (`list_session_secrets`) and `run_with_secrets` are implemented. Delegation and project default assignments are not.

Users keep credentials that agents and tools need (API tokens, registry tokens) without leaving them exported in their shell. Agentworks stores these secrets encrypted on the server host and makes them available only to the sessions a human assigns them to, either as harness environment variables or through a templated MCP tool.

Code: `crates/agentworks-secrets` holds the key file and value sealing (`vault`), names and assignment rules (`model`), redaction (`redact`) and command execution for `run_with_secrets` (`exec`). Its `vault` feature (on by default) covers the key file; the worker's MCP facade builds it without that feature. The tables and their atomic, audited operations are in `crates/agentworks-database/src/secrets.rs`, which only ever sees sealed values. `crates/agentworks/src/secrets.rs` adapts it to the service: HTTP routes, harness delivery, the bridge half of `run_with_secrets` with approvals, and per-session scrubbing state.

Prior art: 1Password `op run` (values resolved per process, output masking), [fnox](https://fnox.jdx.dev/) (MCP `get_secret`/`exec` with redaction), Infisical Agent Vault (credential proxy).

## Storage

Secrets live on the server host. Remote workers receive a value only while running a session assigned that secret, over the existing authenticated link; workers never persist values.

### Keys

Envelope encryption, so a password can be added later without re-encrypting stored secrets:

- **Data key**: random 256-bit key, generated once. Encrypts every secret value.
- **Key-encryption key**: `argon2id(password, salt)`. Wraps the data key. Until passwords exist, the password is empty.
- **Key file**: beside the database as `<database>.secrets.key` (for example `sessions.secrets.key`), or `AGENTWORKS_SECRETS_KEY_PATH`. Mode 0600, created on first use:

  ```json
  {"version": 1, "kdf": "argon2id", "params": {"m": 19456, "t": 2, "p": 1},
   "salt": "<base64 16 bytes>", "wrappedKey": "<base64 nonce‖ciphertext‖tag>",
   "password": false}
  ```

  The file never holds a usable key. A wrong password or mismatched file fails the AES-256-GCM tag check on `wrappedKey`; no separate password hash is stored.

- **Values**: one SQLite row per secret, `AES-256-GCM(dataKey, nonce, value)` with the secret's ID and name as associated data, so ciphertexts cannot be swapped between rows.

The server unwraps the data key at startup (or at unlock, once passwords exist) and holds it in memory, zeroized on drop. Encryption uses `ring`, already in the build through rustls; `argon2` is the only new library.

Without a password this keeps values out of database snapshots, recovery backups and exports, which are useless without the key file. It does not protect against a process running as the same user that reads both files, including agents. The password step below closes that.

Losing the key file makes stored values unrecoverable: a new key file is created, the secrets stay listed as **Needs re-entry**, and each value must be entered again. A damaged key file is never replaced; secrets stay unavailable until it is repaired or removed, and the rest of the server runs normally. Recovery backups copy only named files and do not include the key file.

### Later: password

- Setting a password derives a new key-encryption key with a new salt, re-wraps the data key and sets `"password": true`. Changing or removing it is the same operation.
- With a password, the server starts locked. A browser banner (**Unlock secrets**) prompts for it; the data key is held for a configurable idle timeout, then zeroized.
- While locked, sessions start without their secrets and `run_with_secrets` waits for unlock up to its timeout, as described below.

## Model

### Secret definitions (global)

Created in **Settings → Secrets** by a human:

| Field | Meaning |
| --- | --- |
| `name` | Environment-style name, `[A-Z_][A-Z0-9_]*`, unique. |
| `value` | Write-only. Never returned to browsers, MCP callers or logs. The UI shows length and last-updated time, with **Replace** and **Delete**. |
| `description` | Optional, shown in pickers. |

Names Agentworks manages are rejected: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`, `CODEX_API_KEY`, `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `GOOSE_*`, `AGENTWORKS_*`, `PATH`, `HOME`, and the other variables set in `crates/agentworks-runtime/src/provider.rs` and `model_providers.rs`.

Deleting a definition removes every assignment. Names cannot be changed after creation.

### Assignments (per session)

Session settings list the session's secrets. Each assignment has:

| Field | Values | Default |
| --- | --- | --- |
| `delivery` | `environment` (also usable by the tool) or `tool` (tool only) | `tool` |
| `delegation` | `restricted` or `passable` | `restricted` |
| `approval` | `none` or `eachUse` (tool calls only) | `none` |

Assignment is the authorization: a session can never name, use or discover a secret it is not assigned. `list_session_secrets` shows only assigned secrets, never values (see [Discovery](#discovery)).

Only humans create or widen assignments. Project settings may hold a default assignment set applied to new human-created sessions in that project.

`tool` delivery is the default because environment variables reach everything in the harness process, including harness-owned subagents and every shell command, and harnesses may persist them (Codex writes shell snapshots under `$CODEX_HOME/shell_snapshots/`). Claude Code also passes its environment to every MCP server in the user's configuration, not only Agentworks; Codex does not pass it to MCP servers (both checked with Claude Code 2.1.319 and Codex 0.165).

### Delegation

A session that creates another session (`start_session`, forks) may pass on only its own `passable` assignments, and only the ones it names:

```
start_session { ..., secrets: ["GITHUB_TOKEN"] }
```

- Nothing passes implicitly, consistent with grants ("started sessions inherit no grants").
- A passed assignment keeps the parent's `delivery` and `approval`, or narrower values if the caller asks (`environment` → `tool`, `none` → `eachUse`). It can never widen.
- The child's assignment is `restricted` unless the parent's is `passable` and the caller sets `passable: true`, so transitive delegation is an explicit choice at each hop.
- Naming a secret the caller lacks or holds as `restricted` fails the whole call; nothing is created.
- Human forks offer the parent's assignments preselected; the human can change them.
- Revoking an assignment does not cascade automatically, but the UI shows which sessions received it from this one and offers to revoke those too.

Harness subagents (Claude tasks, Codex subagents) share the harness process. They see `environment` secrets and can call the tool; delegation rules cannot restrict them. This is another reason `tool` is the default.

## Discovery

Agents learn about secrets lazily, so the system prompt and tool list never depend on assignments:

- The shared Agentworks system prompt carries one fixed sentence for every session: if a task needs credentials such as API tokens, call `list_session_secrets` to see what this session may use and how, rather than asking the user to paste one into the chat.
- `list_session_secrets` and `run_with_secrets` are listed in every session, and never for external controllers. With no assignments, `list_session_secrets` returns an empty list and `run_with_secrets` refuses.
- `list_session_secrets` returns, for each assigned secret: its name, its description (written for agents, so Settings labels it **Description for agents** and warns that assigned agents can read it), and usage instructions generated from the assignment:
  - `environment`: available as `$NAME` in commands the agent runs, or, if the harness started before the assignment, not yet present and usable through `run_with_secrets` until the session restarts.
  - `tool`: use `run_with_secrets` with `{{secret:NAME}}` in `argv`, `env`, `stdin` or `files`.
  - `approval: eachUse`: the user is asked before each use.
  - Always: do not print, log, commit or send the value anywhere it is not needed.

Because nothing in the prompt or tool list changes, assigning or editing a `tool` secret takes effect on the next tool call. Only `environment` changes need the harness restarted.

## Delivery 1: environment, with restart

At harness start, `environment` assignments are decrypted and added to the harness process environment after the mise environment and before the provider configuration, so provider-managed variables always win. Values that cannot be decrypted are left out and the transcript notes which (**Not available to this harness**); the harness still starts. Remote workers advertise `secretEnvironment`; a session with environment secrets refuses to start on an older worker rather than starting without them. Replacing a stored value reaches a session at its next harness start. Local sessions add them where the server spawns the harness; remote sessions receive them in the worker start request and apply them through the same `.envs()` path in `crates/agentworks-runtime/src/worker.rs`. Start requests carrying values are never logged or persisted, on either side.

Changing a session's `environment` assignments restarts an idle harness automatically, as changing its Agentworks access already does; a busy one restarts when its turn ends. The UI also offers **Restart with secrets**. It stops the harness process and starts a new one with the current assignments, resuming the native conversation through the existing resume/load path in `session_runtime.rs`. Harnesses that support neither cannot use this; the UI offers a new session instead. Once passwords exist, the same action applies to sessions that started while secrets were locked.

Codex writes no rollout until a thread's first turn, so resuming a session that was never prompted (for example after an environment change) fails with `no rollout found`. The Rust Codex adapter reports that as `ResourceNotFound`, and Agentworks then starts a new harness session, as it already did for Claude. Codex passes the harness environment to its commands by default (checked with Codex 0.165 via `codex sandbox`). If a user's Codex configuration sets `shell_environment_policy.ignore_default_excludes = false`, Codex drops variables whose names contain `KEY`, `SECRET` or `TOKEN`, including assigned secrets; use `tool` delivery for those. Agentworks does not override the policy: `include_only` would remove every other variable, and `set` would put values on the command line.

## Delivery 2: templated tool

A new Agentworks MCP tool, listed for every session (see [Discovery](#discovery)):

```
run_with_secrets {
  argv: ["curl", "-sH", "Authorization: Bearer {{secret:GITHUB_TOKEN}}", "https://api.github.com/user"],
  env?:   { "NPM_TOKEN": "{{secret:NPM_TOKEN}}" },
  files?: { "NPMRC": "//registry.npmjs.org/:_authToken={{secret:NPM_TOKEN}}\n" },
  stdin?: "…",
  cwd?: "…", timeoutSeconds?: 300
}
```

- Placeholders `{{secret:NAME}}` are allowed in `argv`, `env` values, `stdin` and `files` contents. `files` entries are written to private temporary files (mode 0600) that are deleted after the call. Their paths are available to `argv` and `env` as `{{file:NPMRC}}`.
- The command runs without a shell. The agent can call `sh -c` itself; placeholders are still replaced only after the command is fixed, so the model never sees the value.
- The command always runs on the session's host, never on the server for a remote session. See [Where the tool runs](#where-the-tool-runs).
- Output (stdout and stderr, 1 MiB cap each) is returned with every value used replaced by `[REDACTED:NAME]`, along with its base64, base64url, URL-encoded and JSON-escaped forms. This prevents accidental disclosure; it is not a boundary against a command that deliberately transforms the value.
- `approval: eachUse` assignments raise a normal approval card during the turn, showing the templated command (never values). Answers: allow once, allow for this session, or deny. "Allow for this session" lasts until the session's assignments change or the server restarts. An unanswered approval is withdrawn after 10 minutes. The tool result tells the agent when the user approved a use. Codex cancels an MCP call after 300 seconds by default (Codex 0.165), so the Codex adapter gives the MCP servers Agentworks passes in a 4260-second limit (approval plus the longest command). Claude Code 2.1.319 waited the full 10 minutes without a setting.
- Once passwords exist, a call while secrets are locked shows the **Unlock secrets** banner and waits instead of failing.
- The tool runs outside the harness's own sandbox and permission system. Assignments and `approval` are its only controls, which is why assignment is human-only.

### Where the tool runs

Every other Agentworks MCP tool executes on the server: the `agentworks mcp` facade (`crates/agentworks-worker/src/mcp_proxy.rs`) runs next to the harness and translates each call into a bridge operation, relayed over the worker link for remote sessions (see [the relay contract](REMOTE-HOSTS.md#agentworks-mcp-messaging-relay)). `run_with_secrets` splits instead:

1. The facade parses the call and extracts the referenced names. It sends one bridge operation, `resolve_secrets {names, argv, envKeys, fileKeys, cwd}`, containing the templated request but no values.
2. The server authenticates the session credential, checks every name against the session's assignments and audits the release. If approval is needed it publishes the approval card and returns `{pending: id}` at once; the facade polls with `{approval: id}`. Every bridge request stays short because the bridge holds the communication lock and has a 15-second limit. Assignments are checked again when the human answers. Nothing runs on the server.
3. The facade substitutes the values, runs the command on its own host in the session directory, redacts the output, and returns the result to the harness. The bridge credentials are removed from the command's environment.

The same path serves local sessions (facade on the server host) and remote ones (facade on the remote host), so the command always runs where the harness and its workspace are.

Relay and facade rules:

- `resolve_secrets` and `list_session_secrets` are allowed through `crates/agentworks-runtime/src/mcp_relay.rs` on both ends. `run_with_secrets` is listed but its call never crosses the link.
- The facade refuses `resolve_secrets` as an MCP tool call, so a value can never be returned to the model as a tool result.
- Responses carrying values are relayed opaquely: the worker does not parse, log or retain them. A lost response fails the call; it is not retried.
- If the link drops after the server returns values, the call may or may not have run. The audit records the release of values, not the command outcome.
- This keeps values out of the conversation; it is not isolation from a process running as the same user, which could read the facade's bridge credential and request a value itself. Output redaction likewise stops accidental disclosure only.

## Transcript protection

Values delivered to a session are registered with that session's scrubber when its harness starts. Every session event (messages, tool calls, tool output, prompts) has each value and its unpadded base64, base64url, percent-encoded and JSON-escaped forms replaced by `[redacted:NAME]` before it is stored or sent to browsers, the same path that already redacts the session's MCP credential. Completion reports and messages the session sends through Agentworks MCP are scrubbed the same way. Values shorter than 4 bytes are not replaced. The session's interactive terminal is the user's own shell and receives no secrets. Events stored before a value was delivered, and other sessions' events, are not rewritten.

## API (sketch)

- `GET/POST /api/secrets`, `PUT/DELETE /api/secrets/{id}`: definitions. `PUT` takes `description` and/or `value`. Responses never contain values.
- `GET/PUT /api/sessions/{id}/secrets`: assignments, each with its origin (human or parent session ID). `PUT` replaces the whole list; retained secrets keep their origin. Changes are audited against the session and shown in its transcript by name and settings, never value.
- `POST /api/sessions/{id}/secrets/restart`: restart with current assignments.
- Later: `POST /api/secrets/unlock`, `POST /api/secrets/lock`, `PUT /api/secrets/password`.
- MCP: `list_session_secrets`, `run_with_secrets` (executed by the facade on the session's host), and a `secrets` argument on `start_session`.
- Bridge: `resolve_secrets`, the only operation that returns values, relayed for remote sessions.

Every value change, delivery, tool call, delegation and assignment change is audited by name, session, host and outcome.

## Delivery order

1. Key file, envelope encryption and storage; definitions and the Settings UI; assignments in session settings.
2. Environment delivery for local and remote sessions, Restart with secrets, Codex include override, transcript scrubbing.
3. `run_with_secrets` with facade-side execution and `resolve_secrets`, locally, then through the remote relay.
4. Delegation through `start_session` and forks.
5. Password, unlock prompt and idle timeout.

## Not planned

- Reading secrets from external stores (`pass`, `op`). Could be added later as an alternative to a stored value, without changing assignments or delivery.
- A credential proxy that keeps values out of the agent entirely (Agent Vault style). Revisit if `tool` delivery proves insufficient.
