# Development and verification

[Documentation index](README.md) · [Quick start](../README.md)

Run commands from the repository root.

## Checks

Use `mise exec --` before direct Cargo commands to select the pinned Rust and Zig toolchains. For example, `mise exec -- cargo test --locked`.

With the server running:

```bash
cargo test --locked
npm run test:smoke
```

Remote host links have a separate check that starts its own server and uses `scripts/fake-ssh.sh` in place of ssh, so it needs no SSH server or keys:

```bash
cargo build --locked && npm run test:remote-hosts
npx playwright test tests/terminal.spec.mjs
npm run test:remote-partitions # ~50s: silent transport and replacement ownership
```

Guided enrollment has an isolated real-SSH check (requires `sshd`, `ssh`, and `ssh-keygen`). It creates temporary host keys, a passphrase-protected bootstrap identity, an isolated remote home and authorization file, and a loopback SSH server. The remote home has no mise or Node, so the check proves the worker installs and runs the demo agent without them; nothing is downloaded and no provider is called:

```bash
npm run test:host-enrollment
# Also launch the worker through a real mise in that temporary home:
AGENTWORKS_ENROLL_REAL_MISE=1 npm run test:host-enrollment
```

The browser flow is covered by `npx playwright test tests/hosts.spec.mjs`, including mobile layout, terminal input, setup errors/retries, cancellation, and directory selection.

The smoke test uses the demo agent to exercise ACP streaming, multiple turns, disconnect survival, idempotency, approval allow/stale/invalid responses, cancellation, Markdown sanitization, and directory/origin checks. Workspace checks also cover file boundaries, manual automation runs, approval/concurrency handling, and retained run history. They make no paid model requests.

Compact tool snapshots, tool bodies on demand, reconnect resume and tool headers: `npm run test:tool-bodies` (smoke against `AGENTWORKS_URL`, then an isolated browser check on port 4458).

For browser checks:

```bash
npx playwright install chromium
npx playwright test
```

Browser checks cover desktop/mobile layout, session switching, permission requests across reload, cancellation, images, and Markdown file preview.

After `cargo build --locked`, `npm run verify` starts an isolated server on port 4318, runs the API, remote-host relay, project, settings/model, import, workspace/automation, completion-report, worktree-policy, Git-context and browser suites, checks shutdown with an open browser connection, and stops its server. Use `AGENTWORKS_VERIFY_PORT` to change the main test port; worktree policy owns a separate server on port 4351.

Architecture and follow-up work: [ROADMAP.md](ROADMAP.md), [Rust/ACP assessment](RUST-INTEGRATION.md), and [morsh reuse notes](MORSH-REUSE.md).

For an opt-in verification of existing CLI sign-ins (one short model prompt per provider):

```bash
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:signin
```

To verify persistence with an isolated database and real process restarts/crashes:

```bash
npm run test:persistence
# Opt-in: prove Claude/Codex/OpenCode context retention with two short model prompts each:
AGENTWORKS_KEEP_TEST_FIXTURES=1 AGENTWORKS_TEST_HARNESS_RESUME=1 npm run test:persistence
```

Isolated smoke fixtures (catalogs, databases, demo state and Git worktrees) are removed after server shutdown, including on assertion failures. Set `AGENTWORKS_KEEP_TEST_FIXTURES=1` to retain them for inspection; remove the printed fixture directory when finished. Verification also uses a fresh fixture root on every run. Browser screenshots and failure reports remain in `.cache`. Run full verification serially: several suites use fixed ports. Cleanup cannot run if the test runner is killed with SIGKILL.

To check native history import without new inference, reuse the populated test fixtures from a prior native persistence run:

```bash
AGENTWORKS_IMPORT_FIXTURE_DB=.cache/persistence-<run>/sessions.sqlite3 \
AGENTWORKS_URL=http://127.0.0.1:4318 npm run test:import-capabilities
```

## README media

`npm run build && npm run readme:media` regenerates the screenshots and walkthrough GIF in `docs/media` (requires `ffmpeg`). It starts an isolated server on port 4391 (`AGENTWORKS_MEDIA_PORT`), builds throwaway sample repos under `/tmp/agentworks-readme`, and replays the scripted conversation in `scripts/readme-media/conversation.mjs` through the demo agent's `AGENTWORKS_DEMO_SCRIPT` hook; no model is called. Pass names to refresh only some outputs, for example `npm run readme:media -- walkthrough diff`.

## Embedded Goose

After building, `npm run test:goose` runs the linked Goose harness against a loopback OpenAI-compatible fixture with temporary stores. It covers shared profiles, host assignments/revocation, ephemeral remote credentials, private keys, discovery, streaming, shell approvals, native resume after restart, and cancellation without a paid model call. `npx playwright test tests/model-providers.spec.mjs` checks the Settings editor and session picker on desktop and mobile. See [Goose setup](RUST-GOOSE-ACP.md).
