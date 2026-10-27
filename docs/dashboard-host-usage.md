# Dashboard host usage

The operations dashboard opens `/api/dashboard/host-usage/events`, a same-origin
WebSocket under the existing host/origin guard. Reverse proxy mount paths use
`socketUrl`. The server owns sampling; browsers never request individual samples.
Session/project/quota snapshots retain their existing manual refresh behavior.

## Lifecycle and overhead

- The first visible dashboard subscribes to a shared, reference-counted collector
  on the Agentworks server. Additional dashboards subscribe to the same current
  value and do not start additional reads. The first read runs immediately; the
  next reads run every five seconds, with missed ticks skipped and no queued reads.
- Only the current value and one CPU baseline exist in memory. There is no database
  write, sample history, host shell command, new SSH connection, or external probe.
- Browser `alive` messages every ten seconds renew a thirty-second lease per
  connection. Expiry is independent of sample delivery. Sends have a five-second
  deadline; input frames are capped at 128 bytes. Unrecognized input closes the
  subscription. A stalled or disconnected browser cannot keep collection alive
  indefinitely.
- `visibilitychange` to hidden and `pagehide` close the stream and clear browser
  timers/data. Visible/restored pages reconnect; network failure retries after five
  seconds. Hidden dashboards therefore do not count as viewers.
- The last subscription's drop aborts the collector and drops its sample and CPU
  baseline. A new viewer starts with CPU warmup. A blocking OS read already in
  progress may finish after cancellation, but its result is discarded and no more
  reads are scheduled. Abrupt network loss without a close frame can continue
  sampling until lease expiry (up to thirty seconds).

## Supported metrics and limits

Only the local Linux host is collected. Non-Linux servers explicitly report
unsupported; remote hosts explicitly report that their worker lacks metrics
capability, even when connected to Linux. No privilege or remote protocol changes
are included.

- CPU is aggregate utilization from consecutive `/proc/stat` counters, with guest
  counters excluded from the sum because user/nice already include them. Idle and
  I/O wait are excluded from used time. First sample, resets, zero deltas, and read
  errors report reasons instead of percentages. A read failure clears the baseline.
- Memory is `MemTotal - MemAvailable`, with both used and total in bytes. There is
  no guessed fallback when `MemAvailable` is absent. Swap is `SwapTotal - SwapFree`;
  zero total means no configured swap, not a read failure.
- Disk is `statvfs("/")`: total blocks and used blocks (`blocks - bfree`) times
  fragment size. Reserved but free blocks are not counted as used. This covers
  the root filesystem only, not every mount, remote workspace, or physical device.
- The timestamp records completion of the read, not browser delivery. The UI labels
  samples older than fifteen seconds stale and identifies an ahead source clock.
  Individual failures carry reasons. Stream disconnection clears the displayed
  value. Sampling is sequential, so a blocked OS read delays updates and freshness
  becomes stale. There is no command timeout that can cancel a kernel filesystem
  read; `/` is deliberately the only statvfs target.
- Values reflect the server process's Linux proc/mount namespaces. They are not
  cgroup/container quota measurements or per-session resource attribution.

## Remote proposal (deferred)

Remote execution already uses one persistent SSH-supervised, framed WebSocket to
an `agentworks worker` (protocol 3). Worker hello reports platform/providers and
negotiated features for some existing operations, but no metrics feature exists.
Unknown message types are ignored; sending a metrics message to an old worker
would hang until an RPC timeout. Local server `/proc` must never be labeled as a
remote host's usage.

A follow-up should add a Linux-only `hostMetrics: 1` hello capability, preserved by
`check_hello`, plus a bounded `hostMetrics` request/reply using existing request IDs
and deadlines. Keep it read-only, accept no caller-controlled filesystem path or
shell command, use `/proc` and `statvfs("/")` without sudo, and cap reply size.
The shared server collector should issue at most one request per connected capable
host per interval while viewers exist; disconnected/old/unsupported workers get
explicit unavailable reasons. Do not create a worker-side timer: return raw CPU
counters and compute interval deltas centrally, resetting on connection generation
changes. Dropping the last viewer cancels pending waits and clears all baselines;
workers perform no ongoing work. Tests must cover mixed worker versions, timeout,
reconnect, late replies, and closing every viewer while remote reads are pending.

This is an additive negotiated protocol extension, rather than a need for broader
SSH access or privileges. It needs coordinated server/worker implementation and
compatibility checks and is intentionally outside this local slice.
