# Batch 010 — Starlark parser integration

The user asked for Starlark integration with type hints enabled. The Claude
worker delivered `feature/starlark-build-type-hints` at `07e9e98`, rebased onto
current main `3e6ab46`. The coordinator reviewed the diff and fast-forwarded
main to `07e9e98` on 2026-10-22. No deployment or service restart was requested
or performed.

The merge adds a typed Starlark crate and host prelude, moves Bazel progress
interpretation into a server-side parser thread, stores explicit parser
installations in SQLite, and adds Settings → Progress parsers for adding,
editing, enabling, disabling and restoring parsers. Type hints are enabled at
parse, install and call time, and entry points must match the typed host
signature. Workflow interpretation remains a prototype rather than an enabled
workflow runner.

Coordinator checks:

- `cargo fmt --all --check` passed.
- `mise exec -- cargo test --locked --offline --workspace` passed: 135 service
  tests and 32 Starlark tests.
- `node --test tests/bazel-progress.test.mjs`, `node --check
  web/parser-scripts.js` and `git diff --check` passed.
- The coordinator's `npm run verify` attempt could not start the prototype
  service in this sandbox (`Operation not permitted`). The worker reports a
  successful full verify at port 4377: 209 Playwright tests passed, 1 skipped,
  with progress smoke checks and graceful-shutdown coverage.
- A clean isolated Cargo target could not rebuild Ghostty in this environment:
  first the sandbox denied fetching its pinned source; reusing the already
  fetched source then hit a Zig 0.15.2 standard-library cache error. Tests
  passed in the branch worktree's existing target. The worker reports a clean
  `cargo build --locked` and format check.

The experiment record reports 106 added transitive crates and release binary
growth from 22.0 MB to 36.4 MB. Scripts installed in Settings execute in-process
under the single-user trusted-host assumption, with interpreter limits; they
are not an OS sandbox. Live streaming cadence across every harness remains
unverified. See [the experiment record](../experiments/001-starlark.md).
