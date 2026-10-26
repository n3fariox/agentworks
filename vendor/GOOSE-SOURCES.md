# Embedded Goose source

`goose-1.56.0` is the Goose crate from https://github.com/aaif-goose/goose at revision `2b17adc33a7176e701c2fef9578a5fc5834ea89a`. Its Apache 2.0 license is included. The rest of the Goose crates remain pinned Git dependencies.

The manifest resolves upstream workspace package/dependency defaults explicitly and points sibling crates at the same Git revision, so this snapshot builds independently of the upstream workspace.

`agentworks-managed-auth.patch` records the only Rust source change: an embedding hook for retrieving a Codex access token per request, plus an optional host-supplied inference endpoint. Without that hook, Goose retains its upstream file-based authentication. Agentworks registers the hook before constructing its ACP server. The callback and refresh-token store are owned by Agentworks, and workers obtain only access tokens over their existing ACP transport. No worker OAuth credential file is required.
