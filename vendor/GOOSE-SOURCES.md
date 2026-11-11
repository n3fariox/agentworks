# Embedded Goose source

`goose-1.56.0` is the Goose crate from https://github.com/aaif-goose/goose at revision `2b17adc33a7176e701c2fef9578a5fc5834ea89a`. Its Apache 2.0 license is included. The rest of the Goose crates remain pinned Git dependencies.

The manifest resolves upstream workspace package/dependency defaults explicitly and points sibling crates at the same Git revision, so this snapshot builds independently of the upstream workspace.

The Codex provider includes embedding hooks for retrieving a Codex access token per request, an optional host-supplied inference endpoint, and observing response headers. Without these hooks, Goose retains its upstream authentication and response handling. Agentworks registers the hooks before constructing its ACP server. The callback and refresh-token store are owned by Agentworks, and workers obtain only access tokens over their existing ACP transport. No worker OAuth credential file is required.

The response observer exposes subscription quota headers, including model-specific limit families, to the Agentworks ACP bridge. The bridge attaches `_codex/subscriptionLimits` metadata to Codex session usage updates using the same versioned format as the other Codex harnesses. Missing headers retain the original field values and observation times; invalid values clear the affected fields. No extra usage request is made. The existing Goose context/token reporting remains unchanged.

The Gemini OAuth provider supports structured form events via `AGENTWORKS_GEMINI_AUTH_JSON=1` in a dedicated sign-in child with piped stdio, and an opt-in terminal pasted callback URL via `AGENTWORKS_GEMINI_CALLBACK_INPUT=1`, set only for Agentworks sign-in children. The settings form receives authorization URLs and status as JSON and submits callback URLs through the existing SSH worker request channel; credentials remain on the execution host. It validates the callback origin, path, and OAuth state, then uses the existing PKCE token exchange. Direct callbacks and the five-minute timeout remain available. Ordinary ACP processes never read terminal input for OAuth.

Gemini onboarding retains `ineligibleTiers[].reasonMessage` from Google instead of discarding it and attempting unsupported onboarding. Missing-project failures explain the legacy client restriction for individual accounts. See upstream issue #11045 for the confirmed server-side rejection.
