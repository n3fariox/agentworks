# Review and merge queue (C-05)

The queue is a manual coordination ledger. Completion reports remain agent claims; reporting completion never proposes work, approves a review, records independent tests or reserves an integration. Open **Agents & inbox → Review and merge queue** on any project session to propose commits, inspect history and record decisions.

A proposal pins its source branch, ordered full commit IDs, target branch and full reviewed target base. These fields are immutable. Corrected code or a moved target base needs a new proposal and new review/test evidence. The service stores operator-supplied evidence; it does not verify Git objects, execute tests or attest that a recorded merge happened.

The human reviews the actual diff and records `approved` or `changesRequested`, and independently records `passed` or `failed` test results with evidence (commands, outcomes and scope). Only approved review plus passed tests permits **begin**. Begin durably reserves the single service-wide integration slot before any manual Git work. Follow the project’s manual merge policy: verify the pinned source commits and target base, inspect the destination worktree, then integrate deliberately. Finish records the actual resulting commit and releases the slot. A changed target base requires rejecting this proposal and proposing/reviewing against the new base before merging.

No queue operation runs Git, starts an agent, approves tools, deploys or restarts the service. The slot serializes cooperating queue users, including projects sharing a repository; it cannot lock out independent shell Git commands. There is no automatic queue drain. Order is proposal ID order, but the operator chooses the next eligible item.

If integration fails, first ensure the manual Git operation has stopped and inspect/resolve retained conflicts. Then finish as `failed`; fresh approval and passing tests are required to begin again. `cancelled`, `rejected` and `integrated` are terminal. While applying, evidence cannot change and the reservation cannot be rejected. An uncertain outcome must remain `applying` until inspected. A restart preserves that state and does not retry or expire the slot. Project deletion is refused while its reservation is active, so deletion cannot orphan the global slot. Another explicitly authorized operator may resolve an abandoned reservation after inspection, with a note identifying the decision.

## API and permissions

Human endpoints use normal API Origin checks:

- `GET /api/projects/{id}/review-queue?after=0`: current items plus up to 100 history events, `nextCursor`, `hasMore`. Use the returned cursor to page forward. Current items are always included. History includes actor, time, stable request ID, operation/evidence and immutable response.
- `POST /api/projects/{id}/review-queue`: one operation below, with `requestId` (1–100 bytes). Unknown fields are rejected.

MCP `get_review_queue` takes `{after?:number}` and `update_review_queue` takes the same operation body as POST. Both require the separately human-enabled `manageReviewQueue` capability, checked live on every call including retries. It defaults false for existing/new/child sessions and is excluded from Coordinator presets. The actor is the authenticated session and scope is its current project; callers cannot supply either. Moving project/revocation denies access to old scoped records. This does not grant shell merge privileges. Existing same-user/browser trust limitations in [AGENT-COMMUNICATION.md](AGENT-COMMUNICATION.md) still apply.

Examples (all IDs/commits below are placeholders):

```json
{"requestId":"proposal-1","action":"propose","title":"C-05","branch":"worker/c05","commits":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],"target":"main","base":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
{"requestId":"review-1","action":"review","id":1,"revision":1,"result":"approved","note":"Inspected pinned diff against base bbbb…; no findings"}
{"requestId":"tests-1","action":"test","id":1,"revision":2,"result":"passed","note":"cargo test --locked --offline; all tests passed at pinned commits/base"}
{"requestId":"begin-1","action":"begin","id":1,"revision":3,"note":"Destination clean and base verified; beginning manual integration"}
{"requestId":"finish-1","action":"finish","id":1,"revision":4,"outcome":"integrated","commit":"cccccccccccccccccccccccccccccccccccccccc","note":"Inspected resulting commit and combined checks"}
```

`review` results: `approved`, `changesRequested`. `test`: `passed`, `failed`. `finish` outcomes: `integrated` (requires full result `commit`), `failed`, `cancelled` (omit commit or use null). `reject` takes id/revision/note. All non-proposal actions require a nonblank note up to 4096 bytes. Proposals permit 1–32 distinct full 40/64-character hexadecimal commits, branch/title/target up to 256 bytes. Requests are capped at 24000 normalized bytes. Revision starts at 1 and increases for each successful operation; stale decisions conflict (409).

Each successful operation atomically saves current state, an append-only event, immutable retry response and audit row. `(actor,requestId)` binds the normalized payload including project. Exact retries return the original snapshot with `duplicate:true`, even after subsequent actions/restart; they never rewind current state. Changed payloads conflict. Rejected transactions consume no key. Browser retries unchanged submissions with the original key; after a revision conflict refresh and decide again. The SQLite partial unique index enforces the slot independently of UI/MCP concurrency. History and reservations survive session/project deletion; no cascading deletion or completion link can erase provenance. Schema version 8 migrates older databases without rewriting their claims/grants/history and rejects future versions.

## Checks

```sh
cargo test --locked --offline
cargo build --locked --offline
node scripts/review-queue-smoke.mjs
```

The smoke uses deterministic demo sessions, temporary isolated data and an overridable `AGENTWORKS_REVIEW_QUEUE_PORT` (default 4495). No real inference or repository mutation. Unit tests cover validation, independent evidence, revisions, global reservation, rollback, retry snapshots, project scope and restart recovery; smoke covers HTTP, native MCP grants/revocation, browser rendering and restart persistence.
