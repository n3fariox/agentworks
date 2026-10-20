# Batch 009 — Bazel progress and operations dashboard

The user authorized two independent Codex work sessions for U-05 and U-10 on 2026-10-20. Both start from committed main `8299b8c` with mandatory new worktrees. Coordinator reviews and merges sequentially; workers do not merge, deploy or restart the service. First checkpoint is due within ten minutes of prompt; checkpoint or stop by **2026-10-20 18:00 UTC** and final stop is **20:00 UTC**. No quota continuation or service restart is authorized.

| Work | Branch | Ownership | Isolated test ports |
| --- | --- | --- | --- |
| U-05 Bazel build progress first slice | `agentworks/u05-bazel-progress` | Own runtime output handling, `web/app.js`, `web/style.css` and focused tests. Do not edit `src/main.rs`, `src/hosting.rs`, `web/index.html`, dashboard files, dependencies or shared build outputs. | 4521–4525 |
| U-10 operations dashboard | `agentworks/u10-operations-dashboard` | Own `web/index.html` navigation, dashboard page/assets, `src/hosting.rs`, and `src/main.rs` only if a read-only API addition is necessary. Do not edit `src/runtime.rs`, `web/app.js`, `web/style.css`, dependencies or shared build outputs. | 4531–4535 |

U-05 acceptance is the bounded first slice recorded in the backlog: built-in Bazel phase and reported action/test counts from live tool output; correct handling of snapshots/deltas, ANSI and carriage-return updates, split lines and changing totals; raw output retained; normal elapsed activity when output is absent/unusable; tool status remains authoritative. No public parser/plugin API or configurable matching system in this slice. Use deterministic recorded fixtures and isolated tests; do not depend on paid/live provider inference.

U-10 acceptance is a dedicated `/dashboard` page showing connected host status and session counts, project session counts and current session states, and connected Claude/Codex account status with supported usage meters. Reuse existing read APIs where possible. Clearly label source, freshness and unavailable/unknown account data; never infer account totals by summing per-session reports or expose credentials. Keep the page read-only and mount-aware. Do not add account polling or trigger model/harness commands.

Both workers must report an early checkpoint, commit only on their assigned branch, leave clean worktrees, and provide checks, limitations, a completion claim and inbox handoff. Use isolated Cargo targets and fixture state. After integration run combined acceptance; deployment remains a separate human authorization.

## U-10 review and merge

Codex completed U-10 at branch commit `7f4e539eab96d54d34d0aae482f02b279ab83e92`; merged to main as `b26b327`. The coordinator independently passed `cargo fmt --check`, all 109 Rust tests, locked/offline build, three dashboard model tests, existing usage-status tests and the isolated Chromium/TLS StripPrefix browser check. The browser coverage includes empty and unavailable APIs, read-only behavior, per-session source-specific meters without aggregation, freshness labels, navigation, keyboard access and 320–1280px layouts. One pre-existing unused-`Json` warning remains in `inbox_delivery.rs`. Account identity/connectivity cannot be verified by available APIs and remains explicitly unknown. The clean owned worktree was removed on archive; its branch remains. No deployment or service restart was performed.

## U-05 review and combined acceptance

Codex completed U-05 at branch commit `700504baaaa4bd4ad79161d462856f0cdead21b3`; merged to main as `bc9b3b7`. The coordinator independently passed formatting, locked/offline build, all 110 combined Rust tests, 12 deterministic parser tests, three dashboard model tests, existing usage-status checks, the dashboard Chromium/TLS proxy suite, and all 14 Bazel-progress/work-log browser checks on the combined mainline. One pre-existing unused-`Json` warning remains in `inbox_delivery.rs`. The clean owned worktree was removed on archive; its branch remains. Neither feature has been deployed and the service was not restarted.

## U-10 review and merge

Codex completed U-10 at branch commit `7f4e539eab96d54d34d0aae482f02b279ab83e92`; merged to main as `b26b327`. The coordinator independently passed `cargo fmt --check`, all 109 Rust tests, locked/offline build, three dashboard model tests, existing usage-status tests and the isolated Chromium/TLS StripPrefix browser check. The browser coverage includes empty and unavailable APIs, read-only behavior, per-session source-specific meters without aggregation, freshness labels, navigation, keyboard access and 320–1280px layouts. One pre-existing unused-`Json` warning remains in `inbox_delivery.rs`. Account identity/connectivity cannot be verified by available APIs and remains explicitly unknown. No deployment or service restart was performed. U-05 remains active under its original assignment and deadline.
