# Embedded Goose source

`goose-1.56.0` is the Goose crate from https://github.com/aaif-goose/goose at revision `2b17adc33a7176e701c2fef9578a5fc5834ea89a`. Its Apache 2.0 license is included. The rest of the Goose crates remain pinned Git dependencies.

The manifest resolves upstream workspace package/dependency defaults explicitly and points sibling crates at the same Git revision, so this snapshot builds independently of the upstream workspace.

The Codex provider includes embedding hooks for retrieving a Codex access token per request, an optional host-supplied inference endpoint, and observing response headers. Without these hooks, Goose retains its upstream authentication and response handling. Agentworks registers the hooks before constructing its ACP server. The callback and refresh-token store are owned by Agentworks, and workers obtain only access tokens over their existing ACP transport. No worker OAuth credential file is required.

The response observer exposes subscription quota headers, including model-specific limit families, to the Agentworks ACP bridge. The bridge attaches `_codex/subscriptionLimits` metadata to Codex session usage updates using the same versioned format as the other Codex harnesses. Missing headers retain the original field values and observation times; invalid values clear the affected fields. No extra usage request is made. The existing Goose context/token reporting remains unchanged.
