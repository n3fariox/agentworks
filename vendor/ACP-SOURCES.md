# ACP source copies

These are source copies of the published crates.io packages:

- `agent-client-protocol-3.4.0`: ACP Rust SDK, version 3.4.0.
- `agent-client-protocol-schema-1.12.0`: the schema used by that SDK.

Both are Apache-2.0 licensed. The SDK includes the upstream license; a copy is also supplied with the schema. Original Cargo manifests and `.cargo_vcs_info.json` files record package provenance.

The Rust sources are unchanged. Packaging changes are limited to an empty `[workspace]` in each normalized manifest and a path dependency from SDK 3.4.0 to schema 1.12.0. This gives the newer schema its own Cargo source identity, allowing it to coexist with Goose's exact crates.io schema 1.9.1 dependency. A global crates.io patch would also affect Goose and would not isolate the sources.

Update the two packages together, retain their licenses/provenance, and validate both existing adapters and Goose's ACP wire behavior. Do not replace them with a global schema override.
