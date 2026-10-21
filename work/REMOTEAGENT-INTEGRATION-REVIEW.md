# RemoteAgent integration review

Review branch: `review/remoteagent-integration`, based on main `b936b0e`.
Source: `agentworks/session-cce33b75-274a-4348-a430-f44d0458f8fd` at
`79846d3`, six commits after merge-base `b8845a7`.

The exact source tip is merged, without rebasing or modifying its branch/worktree.
This review does not deploy builds or restart the live service.

## Assignment and source evidence

The session lookup returned “Session not found” for the worktree UUID. Local
Claude and Codex conversation records for the source worktree were accessible
and reviewed, including relevant test results. The human approved SSH launch,
WebSocket over stdin/stdout, server-owned state, per-host working directories,
worker-side mise environment discovery, and chat support first. The source
records include fake-SSH and real OpenSSH checks, but their real-host/provider
claims are historical evidence, not fresh verification by this review.

Source records also identified communication-default and side-panel test failures
on their old main. Current main includes the subsequent expectation, question-form,
side-panel test, and fixture-cleanup changes; those were retained.

## Findings addressed during integration

1. **Remote sessions reached local background machinery.** Source documentation
   said queued inputs, quota continuation and inbox wake-up skip remote sessions,
   but their admission/dispatch paths had no host guard. A remote path that also
   exists locally could pass those local checks. Admission now rejects unsupported
   remote activation and dispatch explicitly skips remote sessions. Disabling
   policies remains possible. Remote Codex errors also skip local account probes.
2. **Relay and process teardown needed stronger ownership.** Aborting a host
   supervisor could detach its socket writer. An explicit connection guard now
   aborts it and closes pending channels/requests on every exit. Closed links reject
   new spawn/directory requests even through handles retained across reconnects.
   Worker channels now own stop requests to child supervisors, rather than retaining raw process
   group IDs after children are reaped. Stop escalates before reaping the leader;
   finished channels are removed. Link teardown stops the writer before waiting
   for child cleanup, so a full outgoing queue cannot block exit reports forever.
   A full server writer queue no longer silently discards a channel kill.
   Review also found that only the server detected silent links: the worker now
   independently expires after 45 seconds without server frames. Protocol **3**
   adds a ready acknowledgment after an exclusive per-host worker lock is held.
   The lock is retained until adapter teardown finishes, so a replacement cannot
   become usable while a half-open old worker still owns its adapters. The kernel
   lock uses the same standard File locking API as the existing database lock.
   Zero-byte files remain under the remote user's cache to avoid unlink/inode races;
   no conversation state is moved to the worker. Older protocol-2 workers require
   updating alongside the server. The silent-transport test also exposed Tokio
   stdin's uncancellable blocking read keeping the CLI process alive after cleanup.
   Worker mode now exits explicitly after `agent::run` has completed teardown;
   ordinary server shutdown retains its existing graceful path.
3. **mise discovery serialized the worker's receive loop.** Adapter startup now
   runs inside each channel task; pings, other channels and cancellation remain
   responsive. Cancelled/timed-out mise children are killed on drop. The server's
   hello deadline now allows the worker's bounded global environment discovery.
4. **An empty working-directory entry silently became `/`.** Empty entries are
   rejected; an explicitly supplied `/` remains valid operator configuration.
   Remote validation has an overall 12-second deadline so an unresponsive remote
   filesystem cannot hold the shared workspace lock indefinitely. Cancelled directory
   requests unregister themselves. Directory comparison uses canonical remote paths
   and path components; sibling, traversal and symlink escapes are covered by the
   remote smoke.
5. **Creation controls still assumed local execution.** Remote availability uses
   the worker's providers and sign-ins, independent of local installation/API-key
   status. Directory memory is keyed by host and project; switching back to Local
   restores local roots. Remote worktree controls and automatic continuation are
   disabled. Projects requiring a worktree remain blocked remotely, and switching
   back restores their local policy. Existing model selection survives a project
   change.
6. **Host removal could race session creation.** Removal now serializes with
   session/workspace mutations before checking and deleting a host.
7. **Fixture cleanup diverged from main.** Main's shared cleanup helper and
   owned-server readiness checks are retained and applied to the remote smoke.
   Remote smoke teardown is bounded, and the smoke is included in `npm run verify`.
   The standalone inbox smoke also assumed a standing policy while reusing a saved
   deadline from its earlier cases; its UI step now explicitly clears that deadline
   before asserting standing authorization. The existing main test and UI code had
   this same mismatch. Its final schema assertion also still expected v7 although
   current main uses v8; that assertion was updated. No product behavior or schema
   was changed to satisfy these assertions.

## Merge resolutions and compatibility

- `src/runtime.rs`: keep main's question handler and use the source's dynamic
  local/remote transport. Both permission and question requests traverse the relay.
- `web/style.css`: retain both question and host-directory styles.
- `scripts/service-restart-smoke.mjs`: retain main's shared cleanup helper.
- Existing local API defaults and catalog deserialization remain compatible;
  absent hosts deserialize to an empty list and `local` remains synthesized.
- SSH credentials and host-key policy remain OpenSSH's responsibility. The worker
  has no network listener or enrollment endpoint. Browser host mutations remain
  behind the existing same-origin guard. Remote commands are trusted operator
  shell configuration, not a sandboxed command field.
- Remote file/workspace/diff requests cannot fall through to this server's disk.
  Remote imports, forks, isolated worktrees, discovery and Agentworks MCP remain
  unavailable. The worker still requires a matching checkout and Node adapters;
  this is not a standalone portable binary distribution.

## Validation

All checks below passed. Test servers used isolated catalogs, databases and demo
state, with inherited `AGENTWORKS_*` settings cleared. Main verification used ports
4618 and 4633 because the default verification port was already occupied; the
owned-process readiness guard refused that port before any mutations.

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` and Git whitespace/conflict checks | Passed |
| `cargo build --locked` | Passed |
| `cargo test --locked` | 121 passed |
| `node --test tests/*.test.mjs web/dashboard-checks/model.test.mjs` | 28 passed |
| Full `npm run verify` on the final protocol-3 build | Passed: API/lifecycle smokes, remote-host smoke, silent-partition/ownership smoke, supervisor checks, 180 browser tests and graceful shutdown; one existing opt-in skip |
| Focused host/worktree browser run | 17 passed, also covered by the full browser run |
| Additional standalone suites | Communication, controllers, forks, worktree cleanup, inbox delivery, permission modes, quota status, composer recovery, continuation scheduling, persistence, timeout acceptance and dashboard all passed |

The new partition smoke keeps stdin and the transport open while withholding
server traffic. It verifies independent heartbeat expiry, adapter termination,
exclusive replacement readiness and worker process exit. The remote-host smoke
also covers two hosts with different roots, symlink/traversal boundaries, slow mise
cancellation without blocking another channel, permissions, question forms,
suspend/resume, interrupted approval cleanup, restart, host removal and no replay.

The inbox suite passed after its two stale test expectations were corrected. No
application/schema behavior was changed for those assertions. Detailed local run
logs remain in `.cache/verify-final.log`, `.cache/cargo-test.log`,
`.cache/node-tests.log`, `.cache/extra-checks.log` and `.cache/inbox-smoke.log`;
the latter records the successful rerun of the initially failing inbox suite.

## Residual limits

- Fresh execution checks use deterministic demo ACP and an SSH stand-in. This
  review does not provision a remote host, run paid Claude/Codex prompts, or alter
  the existing remote/demo service. Real OpenSSH, network-partition timing and
  provider-specific resume behavior still need deployment-environment validation.
- Drops interrupt work and require deliberate resume; an already executed remote
  tool side effect is not rolled back. Check its last result before continuing.
- Process cleanup covers ordinary process groups during graceful link loss,
  suspend, deletion and reconnect. It is not an OS containment boundary for
  descendants that detach themselves, a forcibly killed worker, or host failure.
- This is the Linux implementation. No Windows worker validation or sustained
  output/load stress test was performed.
