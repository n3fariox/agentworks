# Codex default permission mode

Settings → Models includes a Codex sandbox / permission default beside its model.
The built-in default is Full access. The API/catalog key is
`codexDefaultPermissionMode`; omitted settings updates preserve its saved value.
Changing it affects only newly created native Codex sessions, including automation
runs. Imports and existing sessions do not acquire the current global default.

The Rust Codex bridge also receives the captured default during thread creation,
before native initialization. On resume it receives the session's saved mode
instead. Rust model discovery uses native model listing without opening a thread.
These capabilities are negotiated with the adapter, including on remote workers;
older adapters retain their existing initialization behavior. See
[the Rust bridge protocol](RUST-CODEX-ACP.md).

The mode IDs match those of the former npm `codex-acp` 2.2.2 adapter, so sessions
saved under it keep their mode on the Rust bridge:

| ACP mode ID | Adapter label | Sandbox | Approval policy |
| --- | --- | --- | --- |
| `read-only` | Read-only | `read-only` | `on-request` |
| `workspace-write` | Workspace access | `workspace-write` | `on-request` |
| `agent` | Auto review | `workspace-write` | `on-request`, auto reviewer |
| `agent-full-access` | Full access | `danger-full-access` | `never` |

`danger-full-access` is a raw sandbox name, **not** a selectable ACP mode ID.
Agentworks explicitly selects the advertised ACP mode via
`session/set_config_option`, then requires the returned `configOptions` to
confirm the requested value before publishing readiness or dispatching the
queued initial prompt. The session picker uses that confirmed configuration.
If the adapter does not advertise the default, its current mode is retained
and Session options displays a diagnostic. A rejected, timed-out, or unconfirmed
selection fails initialization without dispatching the initial prompt.

The captured default and confirmed session configuration are persisted separately.
On reconnect, saved per-session configuration takes precedence; a saved mode that
is no longer advertised fails with a diagnostic. Old database rows without a
captured default never infer one from today's settings.

This changes only the Codex harness preset. Agentworks MCP/controller grants,
communication roles, workspace policy, and host controls remain independent.
The adapter applies its preset to native turn requests; Agentworks does not
independently inspect operating-system enforcement.

Validation: focused `permission` tests in `agentworks`, persistence tests in
`agentworks-session`, and Playwright `claude-permission-default.spec.mjs` plus
`codex-permission-default.spec.mjs`. The latter uses an offline ACP fixture and
isolated server/database, never a real Codex account or model.
