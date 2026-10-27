# Project directories and the folder picker

Projects are logical groups. A project can also have one optional **project directory**, a host and a path, where new sessions in that project start. Grouping never depends on it: sessions in one project can still use different directories and hosts, and moving a session between projects never changes its directory.

## Using it

- **New project / Project settings → Project directory.** Choose the host, then either type an absolute path or use **Browse…**. The field is optional. Saving other project settings leaves the directory as it is; **Clear** removes it. Existing projects have no directory until you set one, and nothing sets one for them automatically.
- **Browse…** opens the folder picker. It starts at the current path if that path is inside an allowed root, and otherwise at the list of allowed roots. Open folders to go deeper, and use the breadcrumbs or **↑ Up** to go back. Up stops at the root. **Create folder** makes one new folder inside the folder you are viewing and opens it. **Use this folder** selects the folder you are viewing. Breadcrumbs are built from the server's canonical paths using the host's own separator: `/` on Linux and macOS, and `\` on Windows (including `C:\` and `\\?\C:\` roots). The browser never joins a Windows path with `/`.
- **New session → Working directory** has the same **Browse…** button. It uses the host selected in the dialog.

### Which directory a new session starts in

The new-session dialog fills **Working directory** using the first of these that applies:

1. An explicit context, such as a fork's directory.
2. The project's directory, when it is on the selected host.
3. On Local only: the directory open in the workspace view.
4. The last source directory this browser used for that project on that host, then (on Local) the open session's directory.
5. The first allowed root, or the host's first working directory.

The browser-remembered value is the **source** directory you chose, not a worktree created from it. This means a project that defaults to new worktrees does not start the next session inside the previous session's worktree.

An API request to `POST /api/sessions` with an empty `cwd` uses the project's directory on the requested host. If none is set, it returns `400`. A non-empty `cwd` is always used as given and validated as before.

## API

| Method | Path | Body / query | Result |
| --- | --- | --- | --- |
| `GET` | `/api/directories` | `hostId` (default `local`), `path` (absolute; omit for the roots), `hidden` | `{path, root, parent, entries:[{name, path}], truncated}`. With no `path`, `entries` are the canonical roots that exist. |
| `POST` | `/api/directories` | `{hostId?, parent, name}` | `{path}`: the new folder's canonical path |
| `POST` | `/api/projects` | `{name, defaultLocation?: {hostId, path}}` | Project, with `defaultLocationId` when set |
| `PATCH` | `/api/projects/{id}` | `{name, defaultLocation?: {hostId, path} \| null}` | Omitted keeps the directory; `null` clears it |

The project directory is stored as a catalog location. `defaultLocationId` points into `GET /api/catalog` `locations` and is also added to the project's `locationIds`. The directory is validated on its host before the project is created or renamed. A rejected directory saves nothing. Removing a host clears project directories on that host.

## Security model

Every check happens on the machine that owns the path (the server for Local, the worker for a remote host), at the time of the operation. Browser strings are never trusted:

- The configured roots are canonicalized on every request. The requested path must be absolute, must canonicalize to an existing directory, and must start with a canonical root. Missing, outside-root and non-directory paths all get the same `404` answer, so the API cannot be used to probe what exists outside the roots.
- Listings contain only directories. A symlink is listed under its canonical target only when that target stays inside the same root; symlinks that escape are left out. The parent link never goes above the root. A listing scans at most 5,000 entries.
- New folder names must be a single plain component: no `/`, `\`, NUL or control characters, no leading dot (so `.` and `..` are excluded), no leading or trailing spaces, and at most 255 bytes. An existing file, folder or symlink with that name returns `409`; nothing is reused or overwritten.
- On Linux, creation opens the checked parent with `O_DIRECTORY | O_NOFOLLOW` and re-checks the opened directory's real path (`/proc/self/fd`) against the root. It then creates the folder with `mkdirat` relative to that handle. A path component swapped for a symlink between the check and the creation therefore cannot redirect the new folder. On other platforms, creation uses `create_dir`, which does not follow a final symlink, and re-checks containment afterwards. That is best effort, not race-free.
- Folder creation is serialized with session creation. It never starts an agent.

## Remote hosts

Remote browsing and creation use the existing worker `workspace` request with the operations `browseDirectories` (`path`, `hidden`) and `createDirectory` (`path` as the parent, `name`). The server attaches the host's configured working directories as `roots`. The worker expands `~`, canonicalizes, and applies the same rules as above. Workers advertise support with `directories: true` in `hello`. For older workers the picker shows *"Update the worker on this host to browse and create folders there."* A disconnected host, or one without working directories, gets the existing diagnostics. Remote project directories are validated with the same root checks as remote sessions.

## Checks

- `cargo test --locked -p agentworks-workspace directories`: containment, symlink escape, name rules, no reuse.
- `cargo test --locked -p agentworks-runtime --lib directory_operations`: worker operations against host roots.
- `cargo test --locked -p agentworks --bin agentworks default_location`: catalog persistence and explicit updates.
- `npm run test:directories`: an isolated server (port 4353) with a local and a fake-SSH remote host covering the API, project directories and empty-`cwd` sessions, plus the browser spec `tests/directory-picker.spec.mjs` (port 4364). The API smoke also runs in `npm run verify`.
