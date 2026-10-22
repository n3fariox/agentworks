# Agentworks local Codex ACP extension

This is the Apache-2.0 packaged `@agentclientprotocol/codex-acp` 2.2.2 bundle,
with upstream LICENSE and README retained, pinned as local version
`2.2.2-agentworks.1`. Root package.json and package-lock.json select
`file:vendor/codex-acp`. Runtime dependencies retain their existing lockfile
versions. Install with `npm ci --ignore-scripts` (or `--offline` with a populated
npm cache); no download, patching or build happens when a session starts.

Upstream gap: https://github.com/agentclientprotocol/codex-acp/issues/227

`AGENTWORKS.patch` is the complete bundle delta: import/modification notice,
observe native subscription push notifications, attach cached metadata to genuine
parent usage events, and label the local version. The small new implementation
is `dist/subscription-limits.js`. `upstream.sha256` identifies the original
packaged bundle; the adapter test reverses those four changes and verifies that
hash. No other bundled code is changed. There is no postinstall script.

The existing upstream rate-limit cache and `/status` behavior are unchanged.
The helper maintains a parallel cache of just the subscription window fields
with observation times, because the upstream cache has no field timestamps and
its merge loses omitted primary/secondary fields. No account methods are called.
The tracked contract and limitations are documented in
`../../docs/CODEX-SUBSCRIPTION-QUOTA.md`.

After changing this fork, run `npm run test:codex-quota`,
`node tests/usage-status.test.mjs`, and the focused browser and Rust checks from
that document. On an upstream version change, replace the original package,
rebase this small patch, update the hash and lockfile, and rerun the checks.
Do not silently apply the patch to a different upstream bundle. Remove this fork
when an upstream supported metadata contract replaces it, updating the UI and
fixtures together. Upstream README version/release guidance is retained for
attribution; production installation uses the root lockfile, not its npx examples.
