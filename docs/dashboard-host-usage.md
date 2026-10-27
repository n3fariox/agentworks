# Dashboard host usage

The operations dashboard opens `/api/dashboard/host-usage/events`, a same-origin
WebSocket under the existing host/origin guard. Reverse proxy mounts use
`socketUrl`. The server owns viewer subscriptions; browsers never request samples.
Session/project/quota snapshots retain manual refresh behavior.

## Collection and platforms

Local servers and connected capable workers use the same
`agentworks_runtime::metrics::Collector`, based on `sysinfo` **0.38.4** with only
`system` and `disk` features. It refreshes CPU usage, memory, swap, and selected disk
capacity. It never enumerates processes, network counters, sensors, or disk I/O.

The collector supports Linux, macOS, and Windows through sysinfo. Other OSes
explicitly report unsupported. This is collection support, not a claim that every
Agentworks remote-worker feature or SSH setup works on every platform. Native
verification is required on each target OS. The existing worker still contains
Unix-specific process/ownership code; this change does not port the full runtime
to Windows. The portable metrics module can be checked independently. Sysinfo's latest 0.39.6 requires Rust
1.95; 0.38.4 requires 1.88 and fits this workspace's declared 1.94.1 minimum.

Sources: [sysinfo 0.38.4](https://docs.rs/sysinfo/0.38.4/sysinfo/),
[upstream repository](https://github.com/GuillaumeGomez/sysinfo),
[latest version](https://docs.rs/sysinfo/0.39.6/sysinfo/).
Heim's own documentation describes partial platform support and MVP status, so it
was not selected: [heim documentation](https://docs.rs/heim/latest/heim/).

## Lifecycle and overhead

- The first visible dashboard starts one server collector. All other dashboards
  share its current per-host snapshot. The first local read runs immediately;
  subsequent ticks run every five seconds, skipping missed ticks and never
  queueing reads. Remote events may arrive between dashboard snapshot ticks, so
  delivery can lag a remote sample by an additional five seconds.
- Only the current snapshot, each remote connection's current event, and CPU
  baselines exist in memory. There are no history/database writes, shell probes,
  extra SSH connections, or external services.
- Browser `alive` messages every ten seconds renew a thirty-second viewer lease.
  Expiry is independent of sample delivery. Sends have a five-second deadline
  capped by lease expiry; incoming browser frames are capped at 128 bytes.
- Hidden/pagehide closes the stream and clears browser timers/data. Visible or
  restored pages reconnect; transport errors retry after five seconds. Hidden
  dashboards do not count as viewers.
- The service shutdown hook closes metrics viewers as well, so a dashboard cannot
  keep graceful shutdown alive by renewing its lease.
- The last viewer aborts local collection, explicitly stops all remote
  subscriptions, and clears samples/baselines. A new viewer starts with CPU warmup.
  Cleanup does not wait for an in-progress local OS read. A cancellation flag
  prevents a completing read from starting new remote subscriptions after stop.
- Abrupt browser loss without a close frame may keep sampling until its lease
  expires (at most thirty seconds). Already executing blocking OS reads cannot be
  force-cancelled: they may finish after stop, with their results discarded.

## Remote protocol

Protocol remains **3**; `hostMetrics: 1` in worker hello negotiates the additive
feature. Unsupported/old workers advertise no capability. Servers report an
update-needed reason and send no unknown message to them. A new worker remains
idle when connected to an old server that never requests metrics.

Messages over the existing SSH worker WebSocket:

```json
{"type":"metricsStart","subscription":7}
{"type":"metricsEvent","subscription":7,"sample":{"sampledAt":123,"cpu":{"percent":25},"memory":{"used":1,"total":2},"swap":{"used":0,"total":0},"disks":[{"path":"/build","used":1,"total":2}]}}
{"type":"metricsStop","subscription":7}
```

Subscription IDs are positive integers generated per connection. Repeating start
for the active ID renews a fixed thirty-second lease without resetting the CPU
baseline. A different start replaces the collector; stop only matches its ID.
There is one collector per worker, sampling every five seconds. No caller-supplied
path, interval, command, or privilege is accepted.

The shared server collector renews starts once per snapshot tick. Last-viewer
cleanup sends stop without awaiting the transport; a full queue or partition is
covered by worker lease expiry. Worker connection teardown drops the collector.
Events are at most 64 KiB, have at most 64 mounts, and undergo shape/value
validation. Outgoing queue saturation ends worker collection rather than building
an unbounded backlog. The next renewal can restart it. Events with old or unknown
subscription IDs are ignored, and reconnecting creates a fresh subscription and
baseline. Disconnected hosts get an explicit reason. Absent events remain waiting;
old timestamped events become stale rather than being labeled fresh by a renewal.

## Visual display

Host cards show CPU, memory, and swap as large percentages with accessible usage
bars. Bars turn amber at 80% and red at 95% (90% for storage). Unknown values use a
dash and a reason; absent swap is labeled not configured. A compact live/stale
indicator exposes the source timestamp in its tooltip.

The storage overview shows up to three mounts, prioritizing the system volume and
build/workspace/data paths. Other capacity summaries fill the overview; repeated
capacity values and unavailable mounts stay in an expandable list. This is display
selection only: equal counters do not establish that mounts share a filesystem,
and every reported mount remains accessible. Mount expansion and keyboard focus
survive sample updates. The layout adapts to phone widths.

## Values, freshness, and mounts

- CPU uses sysinfo's interval deltas, respecting `MINIMUM_CPU_UPDATE_INTERVAL`.
  The first read is warmup. Empty CPU lists and invalid percentages are unavailable.
  CPU busy-time semantics follow the platform's sysinfo backend.
- Memory and swap use sysinfo's OS definitions in bytes. These differ across OSes;
  this is not a cross-platform accounting identity. A zero swap total means no
  configured swap. Memory state is recreated per read so failed capacity refreshes
  cannot retain an old successful value; missing/invalid totals show a reason.
- Disk policy is automatic: local storage mount points/volumes, including separate
  build mounts and read-only system volumes. Boot-only mounts at `/boot`, `/efi`,
  and their descendants are skipped before capacity queries; `/` remains included.
  Removable media, tmpfs, common pseudo
  filesystems, network filesystems, and FUSE mounts are excluded. Sysinfo also
  applies backend-specific enumeration filters. Mounts are rediscovered per read,
  filtered before capacity queries, deduplicated by mount point, sorted, and
  limited to 64 and the event byte budget. Truncation is labeled. Bind mounts or
  APFS volumes can share underlying storage; figures must not be summed.
- Disk used is **total minus available**, labeled "used or reserved" because
  reserved blocks unavailable to the service user are included. Capacity snapshots
  are recreated each read, preventing stale cached capacity after refresh failure.
  An individual failed capacity read reports a reason and retains its mount label.
- The source timestamp records completion, not delivery or lease renewal. The UI
  labels samples older than fifteen seconds stale and identifies an ahead source
  clock. Disconnection clears browser values. There is no persisted history.
- Metrics reflect OS/process namespaces, not per-session usage or cgroup quotas.
  Sysinfo refresh methods do not expose every native OS failure as a `Result`;
  CPU read failures may be indistinguishable from a retained native counter value.
  Freshness is the sampler timestamp, not proof that each OS counter advanced.
- Network/FUSE/tmpfs build storage is intentionally excluded in this slice. There
  is no explicit mount allowlist UI. Kernel filesystem calls can block even on a
  selected local volume; updates then become stale. No userspace timeout can
  forcibly cancel a kernel read. Lease expiry still prevents ongoing remote work.

## Focused checks

```sh
mise exec -- cargo test --locked -p agentworks-runtime
mise exec -- cargo test --locked -p agentworks host_usage
mise exec -- cargo test --locked -p agentworks hosting::tests
node --test web/dashboard-checks/*.test.mjs
mise exec -- cargo check --locked -p agentworks-runtime --target x86_64-pc-windows-gnu
```
