# Embedded Goose harness

Choose **Goose · embedded** to run the linked Rust Goose runtime. Agentworks launches its own executable as `agentworks goose-acp` and connects through ACP stdio. Goose owns model requests, tools, approvals, and native conversation storage; Agentworks owns the interface, transcript, and subprocess supervision. No separate Goose executable, Node adapter, or desktop shell is required for its built-in coding tools.

## Configure a provider

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

By default, application-launched Goose uses `.agentworks/goose` beneath the runtime directory, with `config`, `data`, and `state` subdirectories. Set `GOOSE_PATH_ROOT` to an absolute directory to choose a different store, including an existing Goose profile. Retain this store with the Agentworks database so saved conversations can resume. Direct `agentworks goose-acp` invocation follows Goose's own path defaults unless `GOOSE_PATH_ROOT` is set.

Remote workers include Goose in their available harnesses because it is linked into the worker binary. Configure provider credentials and endpoints on that remote host. Server environment variables are not forwarded to workers.

## Dependency choices

Goose is pinned to revision `2b17adc33a7176e701c2fef9578a5fc5834ea89a`. Only the `rustls-tls` feature is enabled; optional telemetry, GPU inference, voice, scheduler, and desktop tooling are disabled. Rust 1.94.1 or newer is required.

The full Goose runtime has no stable public Rust API. The entry point deliberately uses its small ACP server surface. Agentworks retains ACP SDK 3.4.0 and schema 1.12.0 through local source copies; Goose uses its original SDK 2.2.0 and schema 1.9.1. The local schema source allows Cargo to resolve both exact schema pins without changing either protocol implementation. See [vendoring notes](../vendor/ACP-SOURCES.md).

Both runtimes link SQLite. Agentworks uses rusqlite 0.39.0 to share `libsqlite3-sys` 0.37 with Goose's SQLx 0.9 and avoid conflicting native SQLite libraries. Database formats and application schemas are unchanged.

Goose capabilities determine the available lifecycle and model controls. Provider-specific subscription quotas, native CLI history imports, and Agentworks role injection are not implemented for Goose.

## Verification

```bash
npm run build
target/debug/agentworks --probe goose
npm run test:goose
```

The smoke test uses a temporary profile, project, database, and loopback OpenAI-compatible fixture. It exercises model discovery without inference, streamed replies, a built-in shell permission request and execution, process restart with native conversation loading, and cancellation. It does not use a paid provider. Set `AGENTWORKS_GOOSE_TEST_PORT` to change its default port 4497, or `AGENTWORKS_TEST_BINARY` to select another built binary.
