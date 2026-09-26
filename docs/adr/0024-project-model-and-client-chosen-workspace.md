# Projects, and a workspace chosen by the client

Status: Accepted, 2026-09-26.

A session's working directory has always come from the server process.
`hya serve` resolves an unscoped request against its own cwd
(`agent_base_with_model` uses `workdir = "."`, and `scope_directory` falls
back to `"."`), and ADR-0023 records that a daemon's cwd is whatever its
starter's cwd happened to be. That was tolerable while every client ran on the
same machine and sent `x-hya-directory`. It stops working once a client can
reach the backend from another machine through the relay (ADR-0025): a remote
client's cwd means nothing on the backend, and the backend's cwd means nothing
to the user. The Project API in `hya.v1` has also been a stub
(`crates/hya-server/src/v1/project.rs`, `saved_permission.project_id` always
`"global"`), so there was no place to say "these directories belong
together".

## Decision

The client chooses where a session works. The unit it chooses is a
**Project**; `hya serve` has no working directory of its own.

- **Project.** A Project has an id (`ProjectId`), a name, and an ordered,
  non-empty list of **Project roots** (absolute directories on the backend
  machine). The first root is the *primary* root. Projects are ordinary
  mutable records, not events: SQLite tables `project(id, name, created_at,
  updated_at, archived)` and `project_root(project_id, path, ord)`, managed
  with CRUD like `saved_permission`. The `hya.v1` Project rpcs become real:
  `ListProjects`, `GetProject`, `CreateProject{name, roots}`,
  `UpdateProject{name?, roots?}`, `DeleteProject`, `ResolveProject{path}`.
- **Sessions record their project.** `Event::SessionCreated` gains
  `project: Option<ProjectId>` and `kind: SessionKind` (`project` |
  `temporary`), both `#[serde(default)]` so existing logs still decode (old
  sessions read as `kind = project` with no project). The store mirrors them
  in `session.project_id` and `session.kind`. A session's *roots* are not
  copied into the log: tools read the Project's current roots at the start of
  each turn, so editing a Project's roots applies to its running sessions from
  their next turn. Subagents inherit the parent's project and kind.
- **Local sessions: match the cwd.** A client on the backend's machine
  (bare `hya`, a local TUI) calls `ResolveProject{path: cwd}`. If the cwd is
  inside a root of an existing, non-archived Project, that Project is reused
  and the session's workdir stays the cwd (which may be a subdirectory of the
  root). Otherwise the client creates a Project with one root, the cwd, and
  the session's workdir is that root. When several Projects contain the cwd,
  the one with the longest matching root wins.
- **Remote and explicit sessions: first root.** A session created for a
  chosen Project (a remote client over the relay, the TUI Project view, or
  `CreateSession{project_id}`) uses the primary root as its workdir. A remote
  client must name a Project or ask for a temporary session; there is no
  implicit default.
- **Temporary sessions.** A `temporary` session belongs to no Project. Its
  workdir is a fresh directory `$XDG_CACHE_HOME/hya/scratch/<session_id>`
  (fallback `$HOME/.cache/hya/scratch/<session_id>`), created with the
  session, and its only root is that directory. The scratch directory lives
  below the cache root rather than being the cache root, because the cache
  root also holds `model_cache.db`. It is **never deleted** by hya, not even
  when the session is deleted: files a user produced there stay on disk
  until they remove them. Temporary sessions never join a Project
  automatically.
- **`hya serve` has no working directory.** Every `"."`/`current_dir()`
  fallback in `hya-server`, `hya-app`, and `hya-core` is removed. An rpc that
  needs a directory takes it from the session (its workdir and roots) or the
  request; without either it fails with `invalid_argument`. The daemon is
  spawned with `current_dir = $HOME` only so that it does not pin a random
  directory; nothing reads it.
- **Per-root context.** Bundles, skills, and `AGENTS.md` context under `.hya`
  are loaded from the session's workdir as before. Other roots contribute no
  context yet (follow-up).

## Why

- **The client knows the workspace; the server does not.** Only the user, at
  the client, knows which directory they meant. A server-side default is
  wrong for remote clients and surprising for local ones.
- **A Project groups what a task touches.** Real work often spans a repo and a
  sibling checkout, docs, or a data directory. Several roots let one session
  see all of them under one permission boundary (ADR-0026) without granting
  the whole disk.
- **Mutable tables, not events.** A Project's name and roots are
  configuration the user edits, like saved permissions. Event-sourcing them
  would add replay cost and a second projection for no audit value; sessions
  only need a stable `ProjectId` in their log.
- **Cwd matching keeps local use unchanged.** Running `hya` in a
  subdirectory of a repo that is already a Project lands in that Project, with
  the familiar workdir, instead of creating a new Project per directory.

## Consequences

- `CreateSessionRequest.workdir` becomes optional and is derived from the
  Project or the temporary scratch directory; `ListSessions` filters by
  `project_id`. This is a breaking `hya.v1` change for clients that relied on
  the server's cwd.
- A Project root that no longer exists makes tool calls under it fail like
  any missing path; hya does not validate roots continuously.
- Changing a Project's roots changes what its running sessions may touch from
  their next turn, including sessions another client is driving.
- Scratch directories accumulate under `$XDG_CACHE_HOME/hya/scratch/`. That is
  deliberate; cleaning them is the user's call.
- Supersedes in part ADR-0023: its consequence that "the daemon's working
  directory is the starter's" no longer holds. The daemon has no working
  directory that any request depends on; the rest of ADR-0023 stands.
