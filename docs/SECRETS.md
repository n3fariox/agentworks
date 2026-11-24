# Session secrets (proposal)

Status: storage, Settings → Secrets, per-session assignment, `environment` delivery (local and remote, with automatic restart), transcript scrubbing, discovery (`list_session_secrets`), `run_with_secrets`, and password or passkey unlock with an idle lock are implemented. Delegation, project default assignments and a manual **Restart with secrets** action are not.

Users keep credentials that agents and tools need (API tokens, registry tokens) without leaving them exported in their shell. Agentworks stores these secrets encrypted on the server host and makes them available only to the sessions a human assigns them to, either as harness environment variables or through a templated MCP tool.

Code: `crates/agentworks-secrets` holds the key file and value sealing (`vault`), names and assignment rules (`model`), redaction (`redact`) and command execution for `run_with_secrets` (`exec`). Its `vault` feature (on by default) covers the key file; the worker's MCP facade builds it without that feature. The tables and their atomic, audited operations are in `crates/agentworks-database/src/secrets.rs`, which only ever sees sealed values. `crates/agentworks/src/secrets.rs` adapts it to the service: HTTP routes, harness delivery, the bridge half of `run_with_secrets` with approvals, and per-session scrubbing state.

Prior art: 1Password `op run` (values resolved per process, output masking), [fnox](https://fnox.jdx.dev/) (MCP `get_secret`/`exec` with redaction), Infisical Agent Vault (credential proxy).

## Storage

Secrets live on the server host. Remote workers receive a value only while running a session assigned that secret, over the existing authenticated link; workers never persist values.

### Keys

Envelope encryption, so a password can be added later without re-encrypting stored secrets:

- **Data key**: random 256-bit key, generated once. Encrypts every secret value.
- **Key-encryption key**: `argon2id(password, salt)`. Wraps the data key. Until passwords exist, the password is empty.
- **Key file**: beside the database as `<database>.secrets.key` (for example `sessions.secrets.key`), or `AGENTWORKS_SECRETS_KEY_PATH`. Mode 0600, created on first use. It holds *slots*, each the same data key wrapped under a different unlock secret: one password slot and any number of passkey slots.

  ```json
  {"version": 2, "slots": [
    {"kind": "password", "kdf": "argon2id", "params": {"m": 19456, "t": 2, "p": 1},
     "salt": "<base64 16 bytes>", "wrappedKey": "<base64 nonce‖ciphertext‖tag>", "password": false},
    {"kind": "passkey", "id": "<uuid>", "label": "Passkey on Mac", "credentialId": "<base64url>",
     "rpId": "localhost", "prfSalt": "<base64url 32 bytes>", "wrappedKey": "<base64>", "created": 1795400000000}
  ]}
  ```

  The file never holds a usable key. A wrong password, a wrong passkey secret or a mismatched file fails the AES-256-GCM tag check on `wrappedKey`; no separate password hash is stored. Version 1 files (the password slot alone, with `version: 1`) are still read and are written as version 2 the first time they change. During a key replacement the file also has `previousKey`: the old data key, wrapped under the new one.

- **Values**: one SQLite row per secret, `AES-256-GCM(dataKey, nonce, value)` with the secret's ID and name as associated data, so ciphertexts cannot be swapped between rows.

The server unwraps the data key at startup, or at unlock when there is a password, and holds it in memory, zeroized on drop. Encryption uses `ring`, already in the build through rustls; `argon2` is the only new library.

Without a password this keeps values out of database snapshots, recovery backups and exports, which are useless without the key file. It does not protect against a process running as the same user that reads both files, including agents. A password closes that while secrets are locked.

Losing the key file makes stored values unrecoverable: a new key file is created, the secrets stay listed as **Needs re-entry**, and each value must be entered again. A damaged key file is never replaced; secrets stay unavailable until it is repaired or removed, and the rest of the server runs normally. Recovery backups copy only named files and do not include the key file.

### Password

Settings → Secrets → **Password** sets, changes or removes it.

- Setting the **first** password also replaces the data key and re-encrypts every value (see [Replacing the key](#replacing-the-key)): a copy of the key file taken while it had no password would otherwise still open every value. The new key is wrapped under argon2id (64 MiB, 3 passes) with a new salt and `"password": true`. Changing the password needs the current one and re-wraps the same data key; removing it re-wraps under the empty password. Neither re-encrypts values. The key file is replaced atomically (a private temporary file beside it, synced and renamed), and never with a file for a different key. Passwords need at least 8 characters. The password is never stored or logged.
- With a password the server starts **locked**. Every page shows an **Unlock secrets** banner; unlocking reads the key file again and derives the key off the async workers. A wrong password fails the AES-GCM tag check; failed attempts wait 1, 2, 4 … up to 60 seconds before the next one. Unlocks, failures, locks and password changes are audited (`secrets.unlock`, `secrets.lock`, `secrets.password`).
- **Lock after inactivity** (15 minutes to 24 hours, or never; default 1 hour) counts from the last time a value was sealed or opened: a harness start with environment secrets, a `run_with_secrets` release, or saving a value. Listing secrets does not count. **Lock now** locks at once. Locking drops the server's reference to the data key; a release already in progress finishes. Without a password there is nothing to lock, so the setting has no effect.
- While locked, secrets are listed but cannot be added, changed or read (`423 Locked`). Sessions start without their `environment` secrets; the transcript says which, and unlocking restarts those sessions with them (now when idle, after their turn when busy). `run_with_secrets` waits for the unlock, as described below.
- A forgotten password cannot be recovered. Remove the key file (Agentworks creates a new one) and enter the values again.

### Replacing the key

**Replace encryption key** in Settings → Secrets (`POST /api/secrets/rotate`, with the current password when there is one), and the first password, generate a new data key and re-encrypt every stored value under it. Use it if an older copy of the key file may have been taken. The key file and the database cannot change together, so it runs in three steps:

1. Write a key file with the new key in its password slot and the old key wrapped under the new one (`previousKey`).
2. Re-encrypt every value in one database transaction. Values that open under neither key (**Needs re-entry**) are left as they are.
3. Rewrite the key file without `previousKey`.

A crash at any point leaves a key file that opens every value: until step 3, values open under either key. Startup, or the next unlock when there is a password, finishes an interrupted replacement. Values are sealed under the store lock, so one saved during a replacement is either re-encrypted by step 2 or sealed with the new key. Key-file changes (password, passkeys, replacement, and finishing an interrupted replacement at startup or unlock) run one at a time under one lock; unlocking holds it from reading the key file until any interrupted replacement is finished. Every read-modify-replace of the key file also holds an exclusive OS lock on a sidecar `.<key file>.lock` (the key file itself is replaced by rename), so no change can overwrite another's slots, even from another process. Passkeys are removed, because their secrets never reach the server and cannot be re-wrapped; add them again afterwards. Each replacement is audited (`secrets.rotate`: `started`, then `finished:<values re-encrypted>`).

### Passkeys

Settings → Secrets → **Passkeys** adds passkeys (Touch ID, Windows Hello, a phone or a security key) that can unlock secrets as well as the password. They use the WebAuthn PRF extension: for a given passkey and 32-byte salt, the authenticator returns a fixed 32-byte secret, which never leaves the browser except to unlock.

- Adding one needs secrets unlocked and the current password, so an open browser tab cannot add a way in. The browser creates the passkey with a random salt and evaluates it; the server derives a key with HKDF-SHA256 (salt: the slot's PRF salt, info: the slot ID) and wraps the data key under it. Some authenticators return the secret only when a passkey is used, so the browser then asks for it once more.
- Unlocking: the banner's dialog offers **Unlock with a passkey** when this page's hostname has one. The browser sends the slot ID and secret, and the server unwraps that slot. Nothing is verified besides the unwrap: a wrong secret fails the tag check, counts as a failed unlock and is audited (`secrets.unlock` with `ok:passkey` or `failed:passkey`).
- A password is required as the fallback. Passkeys need one to exist; the password cannot be removed while passkeys remain; changing it keeps them. Removing a passkey needs no key and works even while locked. At most 16.
- Passkeys need HTTPS or `localhost`, and a hostname: a page opened by IP address cannot use them. Each passkey belongs to the hostname it was created on (for example `localhost` and a remote hostname need one each), and is offered only there. Browsers without PRF support (`PublicKeyCredential.getClientCapabilities()` reports it) do not offer them.
- No third-party library is involved: the browser's WebAuthn API on one side, `ring` (HKDF, AES-GCM) on the other.
- Not done: replacing the stored secret on every unlock. The browser could request the current and a new secret in one prompt so that each unlock re-wraps the slot under a fresh salt; until then a captured passkey secret keeps working, like a captured password.

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

Changing a session's `environment` assignments restarts an idle harness automatically, as changing its Agentworks access already does; a busy one restarts when its turn ends. Sessions that started while secrets were locked restart the same way once they are unlocked. Planned: a manual **Restart with secrets** action that stops the harness process and starts a new one with the current assignments, resuming the native conversation through the existing resume/load path in `session_runtime.rs`.

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
- A call while secrets are locked waits instead of failing: the transcript says it is waiting, the **Unlock secrets** banner appears, and the facade polls as it does for an approval. Assignments are checked first, so an unassigned name is refused at once. After the unlock the call continues and may then ask for approval; an approval given before the vault locked is kept. A call still waiting after 10 minutes fails and tells the agent to ask the user to unlock.
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
- `GET /api/secrets/status`: `{available, locked, password, passkeys, lockMinutes, keyFile, error}`, also included in `GET /api/secrets`.
- `POST /api/secrets/unlock {password}` or `{passkey: {id, prf}}`, `POST /api/secrets/passkeys {current, label, credentialId, rpId, prfSalt, prf}`, `DELETE /api/secrets/passkeys/{id}`, `POST /api/secrets/rotate {current}`, `POST /api/secrets/lock`, `PUT /api/secrets/password {current, password}` (empty `password` removes it), `PUT /api/secrets/lock-timeout {minutes}` (0 never, otherwise 5–1440).
- MCP: `list_session_secrets`, `run_with_secrets` (executed by the facade on the session's host), and a `secrets` argument on `start_session`.
- Bridge: `resolve_secrets`, the only operation that returns values, relayed for remote sessions.

Every value change, delivery, tool call, delegation and assignment change is audited by name, session, host and outcome.

## Delivery order

1. Key file, envelope encryption and storage; definitions and the Settings UI; assignments in session settings.
2. Environment delivery for local and remote sessions, Restart with secrets, Codex include override, transcript scrubbing.
3. `run_with_secrets` with facade-side execution and `resolve_secrets`, locally, then through the remote relay.
4. Delegation through `start_session` and forks.
5. Password, unlock prompt and idle timeout (done).

## Not planned

- Reading secrets from external stores (`pass`, `op`). Could be added later as an alternative to a stored value, without changing assignments or delivery.
- A credential proxy that keeps values out of the agent entirely (Agent Vault style). Revisit if `tool` delivery proves insufficient.
