# Embedded Goose harness

Choose **Goose · embedded** to run the linked Rust Goose runtime. Agentworks launches its own executable as `agentworks goose-acp` and connects through ACP stdio. Goose owns model requests, tools, approvals, and native conversation storage; Agentworks owns the interface, transcript, and subprocess supervision. No separate Goose executable, Node adapter, or desktop shell is required for its built-in coding tools.

## Configure a provider in Settings

Open **Settings → Model providers** and add a provider with a name, OpenAI-compatible base URL, optional API key, and default model. In its **Available on hosts** checkboxes, choose Local and any registered remote hosts that can use it. This is one provider record and one saved key, shared by those hosts. Offline hosts can be enabled before they reconnect; unchecking every host disables the provider for session use.

**Load models** and **Test connection** use the unsaved fields without saving or sending an inference request. The editor states which connected, enabled host runs the check. They query `/models`, confirming connectivity/model-list access rather than inference or tool-calling capability. Services without `/models` can still be saved with a manually entered model ID.

Click **Save provider** to save immediately. An empty API-key field keeps the saved key; **Remove saved API key** removes it. Keys are never returned in settings responses. New Goose sessions show only providers enabled for their execution host and use the selected provider's default model. **Host environment configuration** retains the original environment-based setup.

The profile ID is saved with each session and reused for discovery, execution, resume, same-host forks, and provider-history deletion. Disabling a host blocks new sessions, prompts, and resumes there; a turn already running can finish. Re-enabling it restores access to its saved conversations. A profile referenced by saved sessions cannot be deleted or have its URL changed. Names, default models, host assignments, and keys can be updated; existing sessions keep their selected model, and rotated keys apply when their harness next starts.

Profiles use OpenAI-compatible **Chat Completions** with an explicit request path, so Goose does not infer Responses routing from an OpenAI model name. Other native protocols remain available through host environment configuration.

The Agentworks server stores the canonical profiles and keys in `.agentworks/model-providers` beneath its runtime directory, overridden with `AGENTWORKS_MODEL_PROVIDERS_DIR`. `profiles.json` holds connection details and `hostIds`; `credentials.json` holds keys separately in an owner-only plaintext file (`0600`, directory `0700` on Unix). Authorized remote launches receive connection details and credentials over the SSH worker link for that process only; workers do not save the credentials. Workers must advertise `sharedModelProviders` support.

Each execution host keeps its native Goose state under `goose/<profile-id>` in that host's model-provider directory. Back up the server's provider store and session database, plus the native Goose state on execution hosts, to preserve resumes.

The settings API is `GET /api/model-providers` for all providers; an optional `hostId` query filters the list by availability. `POST` accepts `operation` equal to `save`, `delete`, `test`, or `models`. Save/test/models accept `profile` with optional `id`, `name`, `baseUrl`, `defaultModel`, `hostIds`, optional `apiKey`, and optional `removeKey`. Omitted `hostIds` defaults to Local for compatibility; an empty array disables all hosts. Test/models can instead take a saved `id`, with a `hostId` query selecting the enabled execution host. Delete takes `id`. Session creation and `/api/models` accept `modelProvider` with the shared profile ID. Responses return `keyConfigured` instead of a credential. Provider changes save independently of the global Settings button.

Goose automations can optionally select a profile (`modelProvider` on `POST`/`PUT /api/automations`). It must exist and be enabled on the definition's host when saving, and is saved in the definition. Omitting it, including on `PUT`, keeps host environment configuration exactly as before. Each run, manual or scheduled, re-checks that the profile still exists and is enabled on the host. If not, the run is refused with a 409 (scheduled runs are recorded as `skipped`) and never falls back to the host environment. `GET /api/automations` adds `modelProviderStatus` for bound definitions: `available`, the profile `name`, and `keyConfigured` when available, or a `detail` reason. `keyConfigured` only means a key is stored; whether the provider accepts it is known only when a run contacts it. A profile referenced by an automation cannot be deleted. Native-history imports continue using host environment configuration.

## Configure a provider through the environment

Set Goose's provider configuration on the machine that executes the harness. For an OpenAI-compatible Chat Completions service:

```bash
GOOSE_PROVIDER=openai \
GOOSE_MODEL=your-model-id \
OPENAI_BASE_URL=https://your-provider.example/v1 \
OPENAI_API_KEY=your-api-key \
npm start
```

The service must support the model's tool calls and streaming format. Responses-only providers need corresponding Goose endpoint configuration. Other providers can use Goose's provider-specific environment variables or configuration file.

Agentworks preserves Goose's provider environment regardless of `AGENTWORKS_AUTH_MODE`. That switch continues to control Claude/Codex authentication. Keys are inherited by the child and are not stored in session records or sent to the browser. Provider setup errors appear when discovering models or starting a session.

With host environment configuration, application-launched Goose defaults to `.agentworks/goose` beneath the runtime directory, with `config`, `data`, and `state` subdirectories. Set `GOOSE_PATH_ROOT` to an absolute directory to choose a different store, including an existing Goose profile. Retain this store with the Agentworks database so saved conversations can resume. Direct `agentworks goose-acp` invocation follows Goose's own path defaults unless `GOOSE_PATH_ROOT` is set.

Remote workers include Goose in their available harnesses because it is linked into the worker binary. Managed profiles use the provider configured in Agentworks and must enable that host. Host environment configuration uses credentials and endpoints on the remote host; server environment variables are not forwarded.

## Dependency choices

Goose is pinned to revision `2b17adc33a7176e701c2fef9578a5fc5834ea89a`. Only the `rustls-tls` feature is enabled; optional telemetry, GPU inference, voice, scheduler, and desktop tooling are disabled. Rust 1.94.1 or newer is required.

The full Goose runtime has no stable public Rust API. The entry point uses a small ACP compatibility bridge around its server and public session APIs. Agentworks retains ACP SDK 3.4.0 and schema 1.12.0 through local source copies; Goose uses its original SDK 2.2.0 and schema 1.9.1. The local schema source allows Cargo to resolve both exact schema pins without changing either protocol implementation. See [vendoring notes](../vendor/ACP-SOURCES.md).

Both runtimes link SQLite. Agentworks uses rusqlite 0.39.0 to share `libsqlite3-sys` 0.37 with Goose's SQLx 0.9 and avoid conflicting native SQLite libraries. Database formats and application schemas are unchanged.

Agentworks advertises ACP system prompts in append mode and native forks through its compatibility bridge. Role instructions and shared Agentworks context occupy one managed entry in Goose's system prompt extras, replaced on each new/load/fork so reconnects do not duplicate instructions or retain a parent's session identity. Non-text content and unsupported prompt modes fail before changing native state; override mode is not advertised.

Same-host, same-profile automatic forks copy Goose's native session, retain its configuration and conversation metadata, and trim the child through the selected message ID before activation. Exact IDs avoid timestamp collisions. Missing or invalid boundaries fail rather than including newer history. The parent is unchanged; the child retains its role snapshot and resumes independently. Cross-host/profile forks and explicit transcript/summary forks continue to use carried context.

Goose capabilities determine the remaining lifecycle and model controls. Provider-specific subscription quotas and native CLI history imports are not implemented for Goose.

## Verification

```bash
npm run build
target/debug/agentworks --probe goose
npm run test:goose
```

The smoke test uses a temporary server provider store and remote conversation store, a project, a database, and a loopback OpenAI-compatible fixture. It exercises shared provider/key management, host assignment and revocation, keyless isolation, model discovery without inference, streamed replies, a built-in shell permission request and execution, process restart with native conversation loading, cancellation, role persistence, exact native forks and profile-bound remote execution through a fake SSH worker. It does not use a paid provider. Set `AGENTWORKS_GOOSE_TEST_PORT` to change its default port 4497, or `AGENTWORKS_TEST_BINARY` to select another built binary.

Provider settings and session selection have desktop/mobile browser checks: `npx playwright test tests/model-providers.spec.mjs`.
