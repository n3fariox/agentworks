# Project tasks

Tasks are small, persistent project tickets, separate from sessions, completion claims and the review queue. Open **Tasks…** from a project's editor or a project session's options. Create a ticket with a title, optional Markdown body and Open/Done status. Select an existing ticket to edit or delete it; **Refresh tickets** discards unsaved edits and loads the current revision. Archived projects allow reads only. Lists and the `#` picker show open tickets by default. Enable **Show done** in the task dialog to include completed tickets; direct links still open done tickets.

Each project has a unique ticket prefix (slug): 2–20 uppercase ASCII letters/digits, starting with a letter. Set it when creating/editing a project or leave it blank on creation for a generated prefix. Renaming a project does not change its generated prefix. Explicitly changing the prefix changes displayed keys, not ticket identity.

Ticket numbers start at 1 independently in each project and are never reused after deletion. `ABC-1` is a display key, not an API identifier. The canonical identity is:

```
agentworks://projects/<project-uuid>/tasks/<positive-number>
```

In a project session's composer, type `#` to fuzzy-pick tickets by key or title. Enter/Tab inserts the key; sending expands known plain-text keys into canonical links. Unknown keys and Markdown links/code are not expanded. Clicking a ticket link opens its project ticket; persisted links continue to work after project name/prefix changes. Markdown previews are sanitized, and titles are rendered as text.

## HTTP API

Human requests use the normal same-origin API protections. All successful operations return JSON with HTTP 200.

| Method | Path | JSON body | Response |
| --- | --- | --- | --- |
| GET | `/api/projects/{projectId}/tasks` | none | `{tasks: [...], project: {...}}` |
| POST | `/api/projects/{projectId}/tasks` | `{title, body?, status?, requestId}` | `{task: {...}}` |
| GET | `/api/projects/{projectId}/tasks/{number}` | none | `{task: {...}}` |
| PUT | `/api/projects/{projectId}/tasks/{number}` | `{revision, title?, body?, status?, requestId}` | `{task: {...}}` |
| DELETE | `/api/projects/{projectId}/tasks/{number}` | `{revision, requestId}` | `{ok: true}` |

The list endpoint accepts `?includeDone=true` to include done tickets. Single-ticket reads are never filtered by status.

Use the number in the URL, not a `taskId` body field. Project creation (`POST /api/projects`) and editing (`PATCH /api/projects/{id}`) accept optional `slug`.

## Native MCP tools and permissions

- `list_tasks {includeDone?: boolean}` returns open tasks by default and project metadata. Set `includeDone: true` to include done tasks.
- `get_task {taskId}` reads one task.
- `create_task {title, body?, status?, requestId}` creates a task.
- `update_task {taskId, revision, title?, body?, status?, requestId}` updates a task.
- `delete_task {taskId, revision, requestId}` deletes a task.

`taskId` is a positive integer or a canonical URI belonging to the caller's project. Numeric strings, display keys and other-project URIs are rejected. Callers cannot supply a project override or impersonate an actor.

A project participant with communication access may read tasks. A coordinator session may read and write (the coordinator capability is any of start sessions, prompt sessions, cancel sessions or run automations). Ungrouped sessions have no task scope. External controllers with communication/coordinator access may read only: even controllers with orchestration grants cannot mutate tasks. Tools are filtered by grants, and authorization is checked again on every call, including retries; revocation applies to already-connected clients. Archive status is checked before mutation receipts are replayed.

## Concurrency, retries and limits

Task responses include `id`, `key`, `uri`, `title`, `body`, `status`, `revision`, `created`, `updated` and `createdBy`. Times are Unix milliseconds; creators are `human` or the session ID. New tasks start at revision 1; updates increment it. Update/delete must send the current revision. A stale revision or changed payload using an existing request ID returns 409; refresh before retrying edits. Missing tasks return 404, invalid fields return 400, and denied capabilities return 403.

Use a stable mutation `requestId` (1–100 bytes) when retrying exactly the same operation and arguments. Receipts are scoped by actor and project and retain the original response, even if a later edit or prefix change makes that snapshot old. Only the most recent 256 successful mutation receipts per project are retained; older retries are not guaranteed idempotent. Rejected mutations do not consume request IDs. A title must be nonblank and at most 160 characters; the body is at most 16,384 bytes. Status is `open` (default) or `done`. Unknown fields are rejected.

Tasks, next numbers and retry receipts are stored in the catalog JSON at `AGENTWORKS_CATALOG_PATH` (default `.agentworks/catalog.json` under the build's repository root), not in the session transcript database. They survive a normal server restart. Catalog updates are serialized inside one server process and saved atomically. **Multiple server processes writing the same catalog are not supported**; use one writer and back up the catalog together with the rest of the service state.

## Verification

After building the binary and installing Node dependencies:

```sh
npm run test:tasks
```

The command runs `scripts/tasks-smoke.mjs` (HTTP and real native MCP/demo harness checks, including restart) and `tests/tasks.spec.mjs` (isolated Playwright CRUD, Markdown safety, picker, stable links, conflict and mobile checks). Neither uses a model or the live service. Each owns a temporary catalog, database and demo state. Ports default to 4591/4592; override with `AGENTWORKS_TASKS_PORT` / `AGENTWORKS_TASKS_BROWSER_PORT`. Set `AGENTWORKS_BINARY` for a different executable, or `AGENTWORKS_KEEP_TEST_FIXTURES=1` to retain fixtures for diagnostics. Port ownership is verified before mutations.
