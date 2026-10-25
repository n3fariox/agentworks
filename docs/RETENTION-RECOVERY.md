# Retention and staged recovery (R-02)

The Rust `agentworks recovery` CLI manages supervised restart snapshots without opening the configured live catalog/database, binding a server port, or starting any harness. It does not stop/start services, select releases, replay work, or replace a live database. Run as the same trusted operator who owns the private service directory. Use trusted local paths; hostile concurrent mutation by that Unix user or of ancestor directories is outside the service's existing trust boundary.

## Retention

Each successful preflight/final restart snapshot now finishes with an fsynced `manifest.json` (format 1, directory identity and SHA-256 of catalog, database and restart request). Snapshot creation compares schema and ordered contents of **all** SQLite tables, including additive tables and SQLite sequences, and checks integrity and foreign keys. Directory mode is 0700 and files are 0600. A manifest is a completion/checksum record, not a signature or independent proof of authenticity.

Choose how many completed snapshots to retain per backup root. This counts snapshots, not restart requests: one restart normally creates two. There is no implicit policy or scheduled deletion.

```sh
/absolute/agentworks recovery retain /absolute/private/service/backups --keep 10
# Review the JSON remove/protected lists, then explicitly apply:
/absolute/agentworks recovery retain /absolute/private/service/backups --keep 10 --apply
```

`--keep` must be at least 1. Ordering uses numeric creation timestamps in directory names, with UUID lexical order to break ties; file modification times do not affect it. A backward clock can affect chronological ordering. Only timestamp-UUID directories with valid manifests qualify. Legacy snapshots, partial snapshots, symlinks and unknown entries are protected. Corruption in any eligible backup aborts before deletion; unexpected contents in a deletion candidate also abort. Keep your separately known-good/off-host copies outside this managed root.

Retention, restore and restart snapshot creation share a nonblocking `.maintenance.lock`. A conflicting operation fails visibly; retry deliberately when maintenance completes. Applying retention first removes and fsyncs the completion marker, then removes only the named snapshot files and empty directory. An interrupted deletion leaves protected remnants for inspection, rather than a misleading completed backup. Filesystem failures can leave a partially applied batch; there is no transactional undelete. No recursive pruning or age/size/transcript/attachment retention is performed.

## Restore and upgrade rehearsal

Choose a compatible trusted executable and a **nonexistent** destination whose parent already exists. Restoring an old snapshot rehearses the same additive migrations and session/automation deserializers used at startup, entirely in the new directory:

```sh
/absolute/agentworks recovery restore \
  /absolute/private/service/backups/TIMESTAMP-UUID \
  /absolute/private/recovery-candidate
```

Restore checks regular source files, catalog/request JSON, supported schema version, SQLite integrity/foreign keys, and checksums when a manifest exists. Legacy R-06 snapshots without a manifest remain restorable but have no checksum guarantee. Standalone snapshots with SQLite `-wal`, `-shm` or `-journal` sidecars are refused. Never feed a live database file copy to this command.

The new directory is created exclusively with mode 0700. Files use 0600. Restore copies the catalog, database and request, opens/migrates **only the copy**, validates readable saved sessions/history and automation definitions, checkpoints/closes SQLite, and fsyncs the result. It writes `restore-ready.json` last and returns paths/session count/schema version as JSON. Existing destinations (including symlinks) are refused. Ordinary validation failures remove only the new directory created by this invocation. A crash can leave an incomplete directory; without `restore-ready.json` it must not be promoted. Inspect/remove it and rerun with a fresh destination. The source snapshot is never migrated or deleted by restore.

The readiness receipt means the supported database and catalog checks passed. It is not a full startup/inference test, nor does it validate every provider-specific opaque payload. The combined A-01/R-02 database schema is v11. Schema v10 snapshots from scheduled-automation releases are accepted and upgraded in the staged copy, preserving definitions, schedule cursors, occurrence outcomes and run request IDs. This version marker adds no tables beyond v10. Newer schema versions are refused; downgrades are unsupported. Rehearse recovery before a release upgrade, retaining the original snapshot and executable. Automated Rust tests cover current-schema roundtrip, v3/v9-to-current staged upgrade/reopen, v10-to-v11 restore with populated scheduled-automation ledgers and rejected invalid scheduled definitions, preserved history/native ID/grants/completions, corrupt/unsupported input, failed migration validation cleanup, private permissions, occupied destinations, retention previews and fail-closed deletion.

## Deliberate operator promotion

1. Inspect the failed supervisor receipt/journal and choose a compatible executable. Stage and validate the desired snapshot using the CLI above; inspect its catalog and saved session history.
2. Before promotion, stop the user service/supervisor and verify no direct instance remains. Preserve the current catalog, database **and any WAL/SHM companions** separately for investigation. Do not overwrite or mix SQLite files from different generations.
3. Deliberately set `AGENTWORKS_CATALOG_PATH` and `AGENTWORKS_DB_PATH` to the staged pair, or perform a separately reviewed offline replacement of both. Review the release selector and supervisor control receipts/tickets as described in [service recovery](SERVICE-RESTART.md). The CLI does not alter them.
4. Before starting, review recovered grants and saved continuation/automation policies: a historical restore revives historical authorization and request/deduplication ledgers. Normal startup can act on still-authorized policies. Reconcile external effects and disable/revoke stale policies deliberately. The restore CLI itself runs no scheduler and never replays a prompt/tool.
5. Start the selected compatible service manually and inspect readiness/history. Keep both the original live files and source snapshot until operator acceptance.

Snapshots cover the catalog and SQLite application state, including transcript events, grants and durable ledgers. They do **not** include attachments, provider-native context files, credentials, Git repositories/worktrees, remote-host state, release executables or supervisor control state. Preserve those independently; provider resume depends on its native context still existing. There is no portable all-data export/import or automatic live rollback in this slice.

Remaining product decisions: default/age/space retention policy and scheduling; attachment/transcript deletion semantics; portable archive coverage and encryption/off-host transport; promotion tooling and authorization reconciliation. These remain explicit follow-ups, outside automation-trigger work.

Run deterministic tests with the pinned toolchain and existing cached dependencies:

```sh
mise exec -- cargo test --locked --offline -p agentworks recovery
mise exec -- cargo test --locked --offline -p agentworks store::tests
mise exec -- cargo test --locked --offline -p agentworks service_restart
mise exec -- cargo build --locked --offline -p agentworks
node scripts/service-restart-smoke.mjs
```
