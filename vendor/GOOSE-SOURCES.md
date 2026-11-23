# Embedded Goose source

`goose-1.56.0` is the Goose crate from https://github.com/aaif-goose/goose at revision `2b17adc33a7176e701c2fef9578a5fc5834ea89a`. Its Apache 2.0 license is included. The rest of the Goose crates remain pinned Git dependencies.

The manifest resolves upstream workspace package/dependency defaults explicitly and points sibling crates at the same Git revision, so this snapshot builds independently of the upstream workspace.

The Codex provider includes embedding hooks for retrieving a Codex access token per request, an optional host-supplied inference endpoint, and observing response headers. Without these hooks, Goose retains its upstream authentication and response handling. Agentworks registers the hooks before constructing its ACP server. The callback and refresh-token store are owned by Agentworks, and workers obtain only access tokens over their existing ACP transport. No worker OAuth credential file is required.

The response observer exposes subscription quota headers, including model-specific limit families, to the Agentworks ACP bridge. The bridge attaches `_codex/subscriptionLimits` metadata to Codex session usage updates using the same versioned format as the other Codex harnesses. Missing headers retain the original field values and observation times; invalid values clear the affected fields. No extra usage request is made. The existing Goose context/token reporting remains unchanged.

## Local patches

Carry these forward when the snapshot is updated, or drop them once upstream covers the behavior.

### ACP compaction updates

Upstream Goose reports compaction only as Goose custom status notifications ("goose is compacting the conversation...", "Compaction complete"), sent only to clients that opt into `_meta.goose.customNotifications`. Those notices carry no summary. The patch makes Goose send ACP `compaction_update` notifications, so Agentworks can show each compaction and its summary in the session log. No upstream issue or PR covered this when it was written (checked 2026-11-02).

- `src/context_mgmt/mod.rs`: `CompactionResult` gains `summary`, the text of the agent-only summary message that replaces the history.
- `src/agents/state_machine/ops_compaction.rs`: automatic and manual (`/compact`) compaction attach `{"compaction": {id, status, trigger, preTokens, postTokens, summary, error}}` data to their system notifications. Automatic compaction reuses its existing notices and adds one for failures. Manual compaction adds progress notices for start, completion and failure.
- `src/acp/server.rs`: notifications carrying that data are also sent as a `session/update` `compaction_update`, with token counts and trigger under `_meta["_goose/compaction"]`. The update is built as JSON because Goose's ACP schema (1.9.1) predates compaction types. It is sent only when `clientCapabilities._meta["agentworks/compaction"]` is `true`. The Agentworks bridge (`crates/agentworks-goose-acp/src/lib.rs`) sets that flag when the client advertises `session.compaction`, because the older schema drops that field.

Not covered: loading an existing session does not replay earlier compactions, and tool-pair summarization (aaif-goose/goose#12690) still runs without any notice. Upstream PR aaif-goose/goose#13263 rewrites `ops_compaction.rs`, so expect a conflict when updating past it.

### Model inventory refresh at session start

Upstream Goose builds a session's model options from a cached provider inventory. For a provider with no cache, it uses Goose's bundled model catalog. It refreshes the cache only in a detached task after `session/new`, `session/load` or `session/fork`, and only when the cache is more than 24 hours old. For an OpenAI-compatible endpoint, the session therefore listed Goose's OpenAI catalog or an old copy of the endpoint's models. A short-lived process, such as model discovery, exits before the refresh can store anything, so the cache never updated.

- `src/acp/server.rs`: when `AGENTWORKS_GOOSE_SESSION_INVENTORY=1`, every session start refreshes the inventory, whatever its age. `prepare_acp_session_agent` waits up to 5 seconds (`SESSION_INVENTORY_REFRESH_WAIT`) for the refresh before it builds the session's model options. If the wait times out, the session uses the cached list, and the refresh keeps running and stores its result for the next session. Without the variable, upstream behavior is unchanged.
- Agentworks sets the variable only for OpenAI-compatible profiles (`configure_connection` in `crates/agentworks-runtime/src/model_providers.rs`). Model discovery for those profiles reads the endpoint's `/models` directly, without starting Goose.
