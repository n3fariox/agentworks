# Agent-proposed Starlark scripts (design, not implemented)

Status: **proposal**. Agents cannot write server scripts today. This records the
rules to decide before any write path is enabled (E-01 backlog). The routing order
these rules depend on is implemented; see
[Routing order](experiments/001-starlark.md#routing-order).

## Why there is no write API yet

The existing authorization model does not cover script writes:

- **Scope mismatch.** Every Agentworks capability (`communicate`, `startSessions`,
  `manageReviewQueue`, ...) is checked against the caller's own project. Scripts are
  host-global: an enabled hook runs on the tool output and inbox deliveries of every
  session in every project. No grant expresses "may change behaviour for other
  projects".
- **Settings are human-only.** [Agent communication](AGENT-COMMUNICATION.md#permissions)
  lists "modifying application settings" as a distinct capability that no preset
  includes, and [controllers](CONTROLLERS.md#scope-and-grants) exclude all
  human-only operations. Installing, enabling, ordering or deleting a script is that
  kind of operation.
- **Presentation is a trust surface.** A formatter rewrites how peer messages read to
  the receiving agent (Rust still adds the attribution and blockquote). A tool-header
  hook relabels the tool cards the human reads when following or reviewing work, so a
  hostile header could present `rm -rf` as "List files". A progress hook only adds a
  derived view and cannot change status, exit codes or raw output.
- **In-process trust.** Installed scripts run in the service process, trusted, with
  per-call limits but no OS sandbox (the supervised worker exists but is unused).

So a write tool would need a new, human-reviewed path rather than a new grant on the
existing one.

## Same-user caveat

As documented in [Agent communication](AGENT-COMMUNICATION.md#trust-boundary),
agents running as the service's OS user can already call `PUT /api/scripts/...` or
edit the SQLite file. Everything below is an application-level control: it defines
the sanctioned path, attribution and review. It is not a security boundary until R-03
separates agent processes from service credentials and the human API.

## Proposed path: propose, then human review

Agents never install. They file a **proposal**; a human approves it in Settings.

### Capability

- New capability `proposeScripts`. Off by default, never part of the Participant or
  Coordinator presets, never inherited by child sessions, only changed through the
  human grant editor. Checked live on every call, including retries.
- External controllers: not available in v1 (add only with an explicit
  `proposeScripts` controller grant if a real need appears).

### MCP tools (all require `proposeScripts`)

| Tool | Input | Effect |
| --- | --- | --- |
| `propose_script` | `{requestId, name, source, rationale, baseHash?}` | Validates and stores a pending proposal. `baseHash` is required when `name` already exists and pins the version being replaced. |
| `list_script_proposals` | `{after?}` | The caller's own proposals and their status. |
| `withdraw_script_proposal` | `{requestId, id}` | Withdraws the caller's own pending proposal. |

There is deliberately no tool to install, enable, disable, reorder, delete, restore
or approve. Actor and project come from the credential, never from input.

### Validation at proposal time

All checks reuse the existing install path, so a proposal that passes would also pass
a human save:

1. `valid_name`, source within the engine's source-size limit, `rationale` 1–4096
   bytes, request capped at the existing MCP payload limit.
2. `Engine::compile_registered`: syntax, load allow-list, static type check, entry
   signatures, pattern compilation and size, at most 32 registrations. The resulting
   hook list is stored with the proposal; the diagnostic is returned on failure and
   nothing is stored.
3. **Hook allow-list for agent proposals:** v1 accepts `register_progress` only.
   Proposals registering formatters or tool headers are rejected with a named
   diagnostic until a separate decision covers those surfaces.
4. Shipped built-in names may be proposed only as replacements with a `baseHash`.
5. Bounds: at most 5 pending proposals per actor and 20 host-wide; `(actor, requestId)`
   is idempotent over the normalized payload (same rules as the review queue: exact
   retries return the original response, changed payloads conflict).

Proposals never run user-visible hooks. Compilation runs the module body once with
parser limits, as Settings saves already do.

### Storage

Additive tables, created by the normal startup schema block; if a future
implementation needs a one-time data migration, increment the schema version:

- `script_proposals(id, actor, actor_title, project_id, request_id, name, source,
  hash, base_hash, hooks, rationale, status, created, decided, decided_note,
  UNIQUE(actor, request_id))`, with `status` one of `pending`, `approved`,
  `rejected`, `withdrawn`, `stale`.
- `script_provenance(name, hash, proposal_id, approved)`: links an installed version
  to the proposal it came from. Settings shows "Proposed by @session" on such scripts
  until a human edits them.
- Every proposal, withdrawal, denial and decision writes an `agent_audit` row (actor,
  action, target script/proposal, outcome, request ID). Proposals and audit survive
  session and project deletion, for provenance.

### Human review (HTTP only, existing Origin checks)

- `GET /api/script-proposals` lists pending and recent proposals.
- `POST /api/script-proposals/{id}/decision {decision: "approve"|"reject", hash, note, enable?}`.

Settings → Starlark scripts gains a **Proposals** list showing the proposer (session
link), rationale, registered hooks and filters, a diff against the current version,
and the current compile result. Approval rules:

- `hash` must equal the proposal's hash, so the human approves exactly what they read.
- The current script hash must still equal `baseHash` (or the name must still be
  free); otherwise the proposal becomes `stale` and the agent must re-propose.
- The source is recompiled against the current engine at approval time.
- Install, status change, provenance and audit commit in one transaction, then the
  hub reloads. The script is installed **disabled** at priority 0 unless the reviewer
  sets `enable: true`; agents can never set priority.
- A proposer whose grant was revoked is shown as such; approval remains a human
  decision and is still allowed.

### Runtime

No change to evaluation: approved scripts run in-process with the existing limits.
If unapproved code ever needs to run (for example, a dry run against a recorded
fixture before approval), run it in the supervised `agentworks-starlark-worker`
process, never in-process.

## Alternatives considered

- **Direct write tool behind a grant.** Rejected: a project-scoped grant would change
  host-global behaviour with no review, and it would let an agent alter the text other
  agents and the human read.
- **Project-scoped scripts.** Scripts that route only for sessions in the proposer's
  project would match the existing grant scope and could later allow progress-only
  writes without review. It needs a per-script scope column, routing by session
  project and a decision on how project scripts order against global ones (project
  scripts first, then the global order). Worth revisiting after proposals exist; it
  does not remove the need to review formatter and header hooks.

## Decisions needed before implementation

1. Approve `proposeScripts` as a new human-only grant, and whether controllers ever get it.
2. Confirm v1 limits agent proposals to progress hooks.
3. Confirm approved scripts install disabled by default.

## Tests to add with the implementation

Grant denial and live revocation; idempotent retries and conflicting payloads;
rejected formatter/header proposals; compile diagnostics without storage;
pending-count bounds; hash-pinned approval, stale `baseHash`, recompile failure at
approval; disabled-by-default install with provenance and audit; agents unable to
reach install, enable, priority or delete through MCP; Settings review on desktop and
phone.
