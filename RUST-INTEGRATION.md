# Rust and ACP integration assessment

Updated October 17, 2026. The prototype compiles with ACP Rust SDK `3.4.0`. Real adapter initialization and demo end-to-end behavior have been tested; no paid model inference has been tested.

## Recommendation

Use Rust + Tokio + Axum with the ACP project's official Rust client SDK for chat. Run the actual provider CLIs in PTYs for terminal mode. Keep React/TypeScript, SQLite, and xterm.js.

ACP supersedes the earlier proposal to evaluate separate community Claude Rust SDKs. It gives the application a shared protocol boundary while published adapters handle provider-specific integration. This makes Rust a stronger fit for the persistent remote host service, subject to integration and packaging checks.

## Shared protocol and adapters

- [Agent Client Protocol](https://agentclientprotocol.com/get-started/introduction) standardizes client-to-coding-agent communication. Local integration uses JSON-RPC over stdio. Native remote support is still evolving. Our remote hosts are independent of it: ACP stdio is relayed over the worker link.
- [Official ACP Rust SDK](https://github.com/agentclientprotocol/rust-sdk): `agent-client-protocol` supplies client roles, connection support, and protocol types. “Official” here means maintained by the ACP project, not a provider-specific OpenAI or Anthropic SDK.
- [Claude adapter](https://github.com/agentclientprotocol/claude-agent-acp): wraps the official Claude Agent SDK and documents images, tool calls, and permission requests.
- [Codex adapter](https://github.com/agentclientprotocol/codex-acp): wraps Codex app-server and documents API-key authentication, images, resource links, tool events, and permission handling.

Both providers are accessible through ACP adapters. Do not assume their unwrapped CLIs natively expose the same protocol or that all advertised extensions are universally supported.

## Architecture

```text
Browser chat
    -> Agentworks Rust service
        -> shared ACP client over stdio
            -> Claude ACP adapter -> Claude Agent SDK/harness
            -> Codex ACP adapter  -> Codex app-server

Browser terminal (xterm.js)
    -> Agentworks Rust service
        -> PTY -> actual claude / codex CLI
```

On a remote host, adapters, PTYs, credentials, and authorized file access belong to the worker that the server starts over SSH. The server keeps the ACP client role and all session state, and relays ACP stdio and file operations over a WebSocket on ssh's stdin and stdout. A dropped link ends that host's processes and is recovered like a server restart, through native ACP resume.

ACP terminal-tool events are useful for displaying an agent's command activity in chat; they do not provide the full interactive CLI screen required by terminal mode.

## Remaining responsibilities and limitations

- Negotiate protocol versions and capabilities; begin with a mutually supported stable version. Keep draft and vendor extensions optional.
- Verify questions, cancel, session loading/resume, image inputs, usage, and additional-directory behavior per adapter/version.
- Implement only the filesystem/terminal callbacks we intentionally advertise, with authorization on the execution host.
- Keep our own durable event history, browser replay, idempotent commands, pending approvals, file previews, and runtime supervision. Protocol compatibility alone does not provide durability.
- Pin SDK, adapter, and provider versions together. Preserve safe rendering of unknown events and clearly report unavailable capabilities.
- Review adapter distribution: npm/TypeScript packaging can require Node; Codex documents a standalone binary build. Validate released artifacts and transitive dependencies rather than promising a single dependency-free installation. Agentworks itself can still be a native binary.
- Direct Codex app-server or a provider-specific SDK remains a fallback for a proven required gap, not a parallel implementation.

## Phase 0 compatibility spike

1. Inspect released Rust SDK and adapter versions, licenses, dependencies, and install/binary artifacts. Pin candidates; avoid preview packages for the baseline.
2. Compile a small Rust ACP client and launch both adapters using the existing host CLI sign-ins in explicit project working directories; test optional API-key mode separately.
3. Demonstrate initialization/capability negotiation, multiple turns, streamed text/tool activity, approval allow/deny, questions, cancellation, and session load/continuation where supported.
4. Verify project configuration loading, additional directories, image inputs, unavailable capabilities, errors, and clean process-tree shutdown.
5. Defer terminal validation to Phase 3, after working chat: use morsh’s portable-pty/Ghostty patterns, keep the host PTY and emulator alive, and redraw into a fresh browser terminal on attachment. Test resize, Unicode, interactive prompts, terminal replies, and disconnect/reattach. Historical scrollback restoration is outside MVP.
6. Record the tested capability matrix and packaging requirements. Address gaps explicitly before committing the full frontend to an assumed behavior.

Live provider tests consume usage through the selected sign-in or explicit API-key mode. The later prototype evidence below records actual model checks.

## Prototype evidence

- Claude adapter `0.93.0` and Codex adapter `2.2.2` both negotiated ACP v1 and advertised image prompts and session loading.
- Codex advertised the `api-key` authentication method. Claude consumes its API key from the environment; its probe advertised no interactive authentication methods.
- The Rust SDK's subprocess integration owns the adapter process and includes Unix process-group cleanup. Agentworks uses it rather than adding its own stdio/process wrapper.
- An explicitly labelled local demo ACP subprocess passed multi-turn streaming, permission allow/deny, cancellation, browser reload with a pending permission, and disconnect-survival checks.
- Demo tests exercise the shared client and browser lifecycle, not real provider behavior. Provider inference and resume have separate opt-in checks using the existing host sign-ins; API keys are not required. SQLite now preserves transcripts and provider IDs through restarts, with native ACP resume/load, no duplicate replay, and interrupted-turn recovery.

## OpenCode prototype extension

OpenCode `2.0.26` is installed on the host and exposes native `opencode acp`, without a provider-specific adapter. A real ACP session advertised free model options, accepted `session/set_config_option` for model selection, and streamed `ACP_OK` from Big Pickle without an API key. The app exposes its advertised free entries through a per-session picker and rejects arbitrary/paid IDs in that endpoint.

OpenCode uses isolated XDG directories under `.agentworks/opencode`; existing user accounts/configuration are not imported. Native OpenCode ACP starts its own local backend, so it requires permission to bind locally. This extends the same Rust ACP client; no additional Rust SDK or npm adapter was added.

References: [native ACP](https://docs.opencode.ai/docs/acp/), [free models and availability](https://opencode.ai/v2/docs/console/models/#free-models).

## Existing-sign-in verification

Claude and Codex now default to inheriting the host CLI credential locations. No tokens are copied, no new browser/device login is started, and API-key mode is an explicit opt-in with separate stores. On this host, both providers streamed `SIGNIN_OK` through ACP using the existing claude.ai / ChatGPT sign-in. The browser no longer disables these providers when API-key variables are absent.
