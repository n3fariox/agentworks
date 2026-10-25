# Agent setup and configuration

[Documentation index](README.md) · [Quick start](../README.md)

Run commands from the repository root.

## Real agents

By default, Claude and Codex reuse the **existing agent sign-in on the host**. Run Agentworks as the same OS user that runs the CLIs. It inherits their normal credential locations (including any existing `CODEX_HOME` or `CLAUDE_CONFIG_DIR` override), without copying tokens or starting a new login flow.

If a sign-in has expired, refresh it through `codex login` or Claude's `/login` command on the host, then try a new session. The browser never asks for account credentials. Default mode clears API-key overrides in the child environment so an unrelated key does not change the intended account/billing path.

API keys are optional. To explicitly use them instead, provide `ANTHROPIC_API_KEY` for Claude or `CODEX_API_KEY` / `OPENAI_API_KEY` for Codex and start with:

```bash
AGENTWORKS_AUTH_MODE=api_key mise exec -- cargo run --locked
```

Only this explicit API-key mode isolates provider configuration/authentication under `.agentworks/claude` and `.agentworks/codex`, keeping it separate from the host CLI login store.

The app uses pinned local ACP adapters installed by `npm ci`, without downloading an adapter at session creation. Claude wraps its SDK; Codex wraps app-server. Project-local agent instructions may be loaded by the harness. Run `npm ci --ignore-scripts` on each execution machine; the Rust binary alone does not contain Node/adapters/provider dependencies.

Choose **Codex · Rust ACP** in the harness picker to use the experimental
[Rust Codex adapter](RUST-CODEX-ACP.md) included in the binary. Its provider ID is
`codex-rust`; it requires a native `codex` executable on PATH (or `CODEX_PATH`).
The existing **Codex** option uses the JavaScript adapter. Both share the host
Codex sign-in; model defaults and session adapter identity stay separate.
The Rust Codex execution path requires no Node or npm adapter. No adapter-selection
environment variable is needed.

Choose **Claude · Rust ACP** in the harness picker to use the experimental
[Rust Claude adapter](RUST-CLAUDE-ACP.md) included in the binary. Its provider ID is
`claude-rust`; it requires a native `claude` executable on PATH (or
`CLAUDE_CODE_EXECUTABLE`) and no Node or npm adapter. The existing **Claude** option
uses the JavaScript adapter. Both share the host Claude sign-in; model defaults and
session adapter identity stay separate. Existing enrollment/build tooling still uses npm.

Codex uses the tracked local `2.2.2-agentworks.1` fork in `vendor/codex-acp` to attach cached subscription windows to genuine context usage events. The usage display preserves each window's source observation time separately from context. No extra account queries or polling are performed; quotas appear after native push notifications and a genuine usage event. See the [implemented metadata contract and limitations](CODEX-SUBSCRIPTION-QUOTA.md).

To check the ACP handshake without making an inference request:

```bash
mise exec -- cargo run --locked -- --probe demo
mise exec -- cargo run --locked -- --probe claude
mise exec -- cargo run --locked -- --probe codex
```

Probes report negotiated capabilities/authentication methods only. A successful probe does not prove inference access or complete provider behavior. Actual prompts consume provider usage.

## Models, commands, skills, and settings

In **New session**, choose a directory and harness, then click **Load models**. This starts a temporary ACP session to discover the harness’s advertised models without sending a prompt. Select a model before creating the session; it is applied before the first turn. The session toolbar also allows changes while idle. OpenCode stays restricted to its advertised free models.

The sidebar’s **Settings** button saves default model IDs per harness and allowed root directories on this host. Each root has its own input row; use **Add root** or its remove button to edit the list. Each harness has a **Load models** button to populate model-ID suggestions using the first currently allowed root. Save root changes before discovering models in a newly added directory. Blank defaults use the harness configuration. A session can explicitly select **Harness default** to override a saved default. Missing/stale model choices fail visibly instead of silently choosing another model.

New Claude sessions default to the adapter's normal **Auto** permission mode. Settings → **Claude default permission mode**, beside the Claude model default, can save Auto, Default, Accept edits, or Plan (`claudeDefaultPermissionMode` in the catalog; an absent value means Auto). The choice is captured for new Claude sessions only, then requested after the adapter advertises it and confirmed before the initial prompt. If unavailable, the adapter's current mode is retained with an explanation in Session options. Rejected or unconfirmed selections stop initialization before any prompt. Existing sessions, native reconnects, Codex/OpenCode defaults, and controller grants retain their existing behavior.

Settings live in `.agentworks/catalog.json` and survive restart. Before roots have been saved, `AGENTWORKS_ROOTS` (or the startup directory) supplies the defaults; saved roots take precedence thereafter. Changes apply to new sessions and model discovery; existing sessions retain their cwd/model and file gateway. Roots control which directories can be selected, not the tool sandbox or agent permissions.

Settings → **Notifications** offers an immediate-save opt-in for turn completion, approvals/questions and failures across sessions. Click **Allow notifications in this browser** to request permission, then **Send a test notification** to check display. Permission is never requested automatically. Trusted HTTPS or HTTP localhost/loopback is required; ordinary HTTP LAN/VPN addresses cannot use this feature. Root-hosted TLS proxies can derive the public origin from overwritten `X-Forwarded-Proto` and `X-Forwarded-Host` headers; keep the backend private. See [setup, permission flow and limitations](NOTIFICATIONS.md). Delivery requires an open, connected tab; mobile/background push is not implemented.

Use **/ Commands & skills**, or type `/` or `$`, to browse the session’s ACP-advertised commands. Picking one inserts it into the composer; sending executes it through the harness. Claude skill/command names use `/name`; Codex advertises skills as `$name`. The harness discovers skills using its own host/project configuration and session directory. This list is retained across browser reconnects. Only advertised ACP commands are offered; the full terminal CLI command set is not guaranteed in chat mode.

Type `@` anywhere in the composer (at the start or after a space) to fuzzy-search other non-archived sessions in the same project; ↑/↓ moves, Enter or Tab inserts `@session-title`, Escape dismisses. When you send, each recognized mention becomes a link such as `[@session-title](agentworks://sessions/<id>)`. Agentworks MCP tools that take a session (`send_message`, `get_completion`, `prompt_session`, `cancel_session`) accept that URI or a bare ID, and `list_sessions` returns both, so agents can pass the URI through unchanged. Messages work the same way: `read_inbox` and `send_message` return `agentworks://messages/<n>` (plus sender/recipient session URIs), and `acknowledge_message` / `replyTo` accept either that URI or the number. The agent still needs Agentworks communication access to actually message the peer.

The **Usage** button runs Claude’s `/usage` or Codex’s `/status` in the current session and displays its output in the transcript without replacing a draft. Verified with existing personal sign-ins: both returned 5-hour and weekly quota percentages and reset times. Claude also returned request/session totals. These are account-level subscription limits shared with other clients, not exact per-session message counts; availability depends on the harness/account. No inference prompt is needed for these two installed adapter commands. The [quota dashboard](DASHBOARD-QUOTA-REFRESH.md) displays retained observations without background provider polling.

An opt-in check initializes both signed-in harnesses, selects advertised models, and runs their local usage commands:

```bash
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:capabilities
```

## OpenCode free-model testing

OpenCode has native ACP support, so no npm adapter is required. Install `opencode` on the host (the tested version here is `2.0.26`), or set `AGENTWORKS_OPENCODE_PATH` to its executable. Select **OpenCode · free models** when creating a session.

Agentworks reads the model options advertised by OpenCode and offers the `opencode/*-free` entries plus Big Pickle in the session's model picker. It explicitly selects one before accepting a prompt and restricts picker changes to that advertised free list. Available models, free status, and limits can change; this is not a permanent pricing guarantee. See [native ACP support](https://docs.opencode.ai/docs/acp/) and [current OpenCode free models](https://opencode.ai/v2/docs/console/models/#free-models).

OpenCode runs with isolated XDG data/config/cache/state under `.agentworks/opencode/`. The app does not import your existing OpenCode login/configuration. No API key was required for the tested Big Pickle prompt. The initial integration exposes free models for testing; paid providers and account management are outside this slice.

An opt-in real model check sends one short prompt in an empty test directory:

```bash
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:opencode
```

To include the native session/model-picker browser check without inference:

```bash
AGENTWORKS_TEST_OPENCODE=1 npm run test:browser
```

## Session permission modes

Session permission modes advertised by the native harness appear beside the model picker while idle. Selecting Auto uses the harness's own review policy, and confirmed selection persists across reconnects. Unsupported modes remain hidden; any model-dependent fallback is shown as reported by the adapter. `GET /api/sessions/{id}/permission-mode` reads the configuration; `POST` with `{"mode":"auto"}` requests an advertised mode only while idle. This does not change Agentworks controller grants or automatically retry interrupted turns. Isolated acceptance: `PLAYWRIGHT_BROWSERS_PATH=$PWD/.cache/browsers node scripts/permission-modes-smoke.mjs` (set `AGENTWORKS_TEST_BINARY` to a built executable if needed).

## Goose · embedded

Goose is linked into the Agentworks binary and runs through its `goose-acp` entry point. Use **Settings → Model providers** to add an OpenAI-compatible connection once and choose its **Available on hosts**, then select it when creating a Goose session. Host environment configuration remains available. See [embedded Goose setup](RUST-GOOSE-ACP.md) for OpenAI-compatible endpoints, storage, remote execution, and verification.
