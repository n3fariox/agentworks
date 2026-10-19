# Batch 008 — Codex subscription quota reporting

The user asked for a Codex investigator to fix missing subscription usage and
submit the change for review, then explicitly authorized deployment after the
review and merge. The issue was that `codex-acp` cached native rate-limit push
notifications but did not include those values in structured `usage_update`
events; Agentworks displayed context occupancy correctly and had no Codex
subscription quota parser.

## Review and implementation

The investigator's initial supported-extension investigation was blocked. The
user's revised authorization allowed a tracked, pinned local npm fork. The
reviewed implementation is commit `a9db12527c9dbdd147b6b5386b2f16139ac2974e`
on main, based on `5f44324`. It vendors the upstream `codex-acp` 2.2.2 package
and license, records the exact bundle patch and upstream hash, and provides a
parallel per-session cache of native quota fields. The fork attaches namespaced
quota metadata only to genuine parent context events. The UI keeps native
percentages, IDs, primary/secondary windows, durations, reset times and per-field
observation freshness distinct from context. Sparse updates do not freshen
unrelated quota values. No quota-only context event, polling, `/status` parsing,
account query or inference was added.

Coordinator review passed the focused quota adapter/UI test, existing usage
test, Rust tests (68 passed), `cargo fmt --check` and whitespace checks. The
investigator reports a successful focused Chromium run. Coordinator's first
browser attempt could not launch Chromium because the sandbox blocked its
sandbox host; this was an environment limitation before test execution.
`npm ci --offline --ignore-scripts --no-audit --no-fund` installed 128 packages
from cache. The implementation and review are documented in
`../CODEX-SUBSCRIPTION-QUOTA-PROPOSAL.md`.

## Deployment

After the user authorized deployment, `cargo build --locked --offline` succeeded
and npm installed the pinned local adapter from cache. The immutable executable
`.agentworks/bin/agentworks-a9db125` was prepared with mode 0700 and its
`--service-protocol` probe returned protocol 1. The deployed adapter reports
version `2.2.2-agentworks.1`. Focused Codex quota and existing usage tests passed
after installation.

Supervised wait-mode request `codex-quota-a9db125-deploy-20261019` selected the
prepared release. The successor reported ready with no manager error and
dispatched this coordinator's saved native handoff. Verified live instance:
`515b540d-9bb6-4531-953e-c689d5d5d73d`, PID `1952486`, executable
`.agentworks/bin/agentworks-a9db125`. The service is not draining. No service
settings, grants or account state were changed. No backlog work was started.
