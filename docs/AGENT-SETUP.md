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

**Claude** and **Codex** run the [Rust Claude](RUST-CLAUDE-ACP.md) and
[Rust Codex](RUST-CODEX-ACP.md) ACP bridges built into the binary (provider IDs
`claude-rust` and `codex-rust`). They need a native `claude` or `codex` executable on
PATH (or `CLAUDE_CODE_EXECUTABLE` / `CODEX_PATH`) and no Node or npm adapter. Each
shares the host CLI sign-in.

The npm adapters (`claude-agent-acp` and the vendored `codex-acp`) have been removed.
Sessions, automations and settings saved under their old provider IDs, `claude` and
`codex`, keep those IDs and run on the Rust bridges, which read the same native
history. Default models saved for the old IDs carry over to the Rust harnesses.
Forking an old session into a Rust harness hands over its transcript.

Settings → Harnesses groups each harness's defaults (model, permission mode) and a
**Show in the new session picker** switch. A hidden harness keeps its ID and its
existing sessions; it is only left out of the new-session and automation pickers. The
picker lists harnesses you used most recently first (remembered in this browser).

The Rust Codex bridge attaches cached subscription windows to genuine context usage events. The usage display preserves each window's source observation time separately from context. No extra account queries or polling are performed; quotas appear after native push notifications and a genuine usage event. See the [metadata contract](CODEX-SUBSCRIPTION-QUOTA.md).

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

Settings live in the catalog (`AGENTWORKS_CATALOG_PATH`, or `.agentworks/dev/catalog.json` for development runs) and survive restart. Before roots have been saved, `AGENTWORKS_ROOTS` (or the startup directory) supplies the defaults; saved roots take precedence thereafter. Changes apply to new sessions and model discovery; existing sessions retain their cwd/model and file gateway. Roots control which directories can be selected, not the tool sandbox or agent permissions.

Settings → **Notifications** offers an immediate-save opt-in for turn completion, approvals/questions and failures across sessions. Click **Allow notifications in this browser** to request permission, then **Send a test notification** to check display. Permission is never requested automatically. Trusted HTTPS or HTTP localhost/loopback is required; ordinary HTTP LAN/VPN addresses cannot use this feature. Root-hosted TLS proxies can derive the public origin from overwritten `X-Forwarded-Proto` and `X-Forwarded-Host` headers; keep the backend private. See [setup, permission flow and limitations](NOTIFICATIONS.md). Delivery requires an open, connected tab; mobile/background push is not implemented.

Use **/ Commands & skills**, or type `/` or `$`, to browse the session’s ACP-advertised commands. Picking one inserts it into the composer; sending executes it through the harness. Claude skill/command names use `/name`; Codex advertises skills as `$name`. The harness discovers skills using its own host/project configuration and session directory. This list is retained across browser reconnects. Only advertised ACP commands are offered; the full terminal CLI command set is not guaranteed in chat mode.

Type `@` anywhere in the composer (at the start or after a space) to fuzzy-search other non-archived sessions in the same project; ↑/↓ moves, Enter or Tab inserts `@session-title`, Escape dismisses. When you send, each recognized mention becomes a link such as `[@session-title](agentworks://sessions/<id>)`. Agentworks MCP tools that take a session (`send_message`, `get_completion`, `prompt_session`, `cancel_session`) accept that URI or a bare ID, and `list_sessions` returns both, so agents can pass the URI through unchanged. Messages work the same way: `read_inbox` and `send_message` return `agentworks://messages/<n>` (plus sender/recipient session URIs), and `read_message`, `acknowledge_message` and `replyTo` accept either that URI or the number. `read_message` returns one message in full and acknowledges it when you are the recipient. Set `expectReply` on `send_message` to be woken by the recipient's first reply. The agent still needs Agentworks communication access to actually message the peer.

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

## Antigravity (official ACP)

Install the official Antigravity CLI on each execution host and sign in once with `agy`. Select **Antigravity** when creating a session. Agentworks launches `agy acp`, reuses the native credential store, and saves the ACP session ID for resume. No conversation ID scraping is needed. Set `AGENTWORKS_ANTIGRAVITY_PATH` to select another CLI executable; remote hosts need an updated Agentworks worker.

This harness uses your cached Antigravity account even when Agentworks is configured for API-key authentication for Claude/Codex. Inherited `GEMINI_API_KEY` and `GOOGLE_API_KEY` values are cleared for this launch. Models and thinking levels come from the ACP server. Agentworks roles and shared session instructions use the advertised `_meta.antigravity.systemPrompt.append` extension on new, load, and resume requests. This appends text to Antigravity's instructions; it does not replace them. A saved role fails to open if the adapter no longer advertises append support.

Quota comes from the installed CLI's native `agy --print /usage --output-format json` command, which reports zero model turns. Agentworks reads its structured `command.data.groups[].buckets[]`, never the display text or model-generated prose. Remaining fractions, reset times, and query observation times appear in the composer and shared dashboard account card. Reads run on session start/resume, after completed turns, or **Refresh quota**, coalesced per host for 30 seconds; failures preserve the last report and its age. This path queries account quota and does not send an ACP prompt or estimate quota from token counts. It reports all quota groups even if some models are unavailable to this ACP client. Remote quota reads require an updated worker advertising `antigravityQuota`. Sign-in UI remains unwired; use the native CLI login.

Google's current [ACP setup guide](https://antigravity.google/docs/ide/extensions/acp/) recommends `agy acp`. The [ACP registry](https://github.com/agentclientprotocol/registry/blob/main/antigravity-acp/agent.json) also distributes a standalone server; this integration uses the installed CLI rather than downloading a separate server.

Opt-in integration check: `AGENTWORKS_TEST_BINARY=/path/to/agentworks node scripts/antigravity-acp-smoke.mjs`. Add `--role` to verify the system-prompt extension with real Gemini replies before and after resume, or `--prompt` to verify real Gemini replies and preserved conversation context after suspend/resume; this consumes account usage. The check uses isolated Agentworks state and port 4461.

Verified with installed Antigravity ACP/CLI 1.4.12. The system-prompt metadata and structured quota output are native extensions rather than stable ACP fields; capability checks and strict command validation fence unsupported versions. See Google’s [quota command guide](https://antigravity.google/docs/cli/commands/usage/). Automated coverage: `node --test tests/antigravity-usage.test.mjs` and `playwright test tests/antigravity.spec.mjs`.

Antigravity ACP 1.5.5 advertises **Default**, **Accept edits**, **Auto**, and **Plan** through its native session mode/config APIs. Choose an advertised value in **Session options → Session permission mode** while idle. The confirmed selection is saved and reapplied on resume; **Restart harness** picks up an upgraded CLI and newly available modes. Older CLI versions may advertise none. Auto approves edits and safe commands while retaining review for irreversible actions; it is distinct from bypassing permissions.

The composer quota display follows the selected model: Gemini 4 Argon uses the separate reported `gemini-argon-wtu-weekly` bucket, Gemini 3.8 Flash uses `gemini-weekly` and `gemini-5h`, and the current Claude Opus/Sonnet 5.5 and GPT-OSS 120B catalog uses `3p-weekly` and `3p-5h`. The native quota command does not expose model IDs per bucket, so these are explicit mappings for the verified catalog and Pro account rather than a generic protocol guarantee. Missing buckets or unknown model IDs show mapping unavailable instead of an unrelated quota. All account groups remain visible on the dashboard. No model request is needed to switch the display.

Antigravity's ACP server is part of the installed `agy` executable. Agentworks launches that executable from PATH (or `AGENTWORKS_ANTIGRAVITY_PATH`); it does not install or update a separate registry package. Manage the CLI on each execution host: `agy update` requests an update, and Google's CLI also has a background self-updater. After an update, **Restart harness** starts the new executable and resumes the saved conversation. Existing harness processes continue running their loaded version. To manage updates manually, Google documents `AGY_CLI_DISABLE_AUTO_UPDATE=true`; Agentworks does not set it. Agentworks's quota mapping and extension integration are maintained in this repository. See [Google's updater documentation](https://antigravity.google/docs/cli/troubleshooting/#resolve-self-updater-locks-and-failures).
