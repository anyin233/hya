# Path permissions bounded by the Project's roots

Status: Accepted, 2026-09-26.

Each file tool checks its own path boundary today, against one directory: the
session's workdir. `read` (`read.rs`), `write`, `edit`, `glob`/`ls`
(`fs_tools.rs`), and `grep` each carry a `starts_with(workdir)` helper over a
lexically normalized path. Nothing canonicalizes, so a symlink inside the
workdir that points outside it passes the check. `bash` also checks that its
`cwd` is inside the workdir. When a path is outside, the tool asks
`Action::ExternalDirectory`; "allow always" on that ask stores a rule with the
resource pattern `*` (`RememberScope::LegacyAction`, around
`crates/hya-tool/src/permission.rs:911`), so one approval for one directory
silently approves every directory on the disk for the rest of the process.

ADR-0024 gives a session a Project with several roots and makes it the unit a
user chooses. The permission boundary has to follow.

## Decision

A session may touch files inside its Project's roots without asking. Anything
outside is an explicit, narrowly remembered escalation. Shell commands have no
path boundary.

- **One boundary helper.** `hya-tool` gets `ProjectScope { roots }` with
  `contains(path) -> bool`, built at turn start from the session's current
  roots (ADR-0024; a temporary session's single root is its scratch
  directory). It canonicalizes both roots and the candidate path, which
  closes the symlink escape. A path that does not exist yet (a `write` target)
  is judged by canonicalizing its nearest existing ancestor and re-appending
  the rest, after rejecting `..` in the remainder. Nested and overlapping roots
  are fine: a path is inside if any root contains it.
- **Tools that use it.** `read`, `write`, `edit`, `glob`, `ls`, `grep`,
  `find`, `lsp`, and `apply_patch` replace their per-tool helpers with
  `ProjectScope`. `apply_patch` accepts absolute paths and paths inside any
  root instead of only relative, workdir-local paths; a patch path outside
  every root is an input error (`apply_patch path is outside the Project
  roots`), not an ask. Relative paths still resolve against
  the session's workdir.
- **Outside is an ask.** A path outside every root raises the existing
  `Action::ExternalDirectory` ask for the directory in question. It goes
  through the normal `PermissionPlane` order: saved rules first, then the
  permission mode. In **yolo** mode it is auto-approved. In **bundle** mode the
  ask reaches the bundle's `permission.approve` interceptor like any other
  ask; ExternalDirectory is not special-cased or hidden from it. A call-level
  grant still does not cover ExternalDirectory, as today.
- **Allow always is concrete and per-Project.** Approving an
  ExternalDirectory ask with "allow always" stores the concrete rule
  `<dir>/*` for that action, with `saved_permission.project_id` set to the
  session's Project. The plane loads the global rules plus the rules of the
  session's Project. The `*` rule is no longer written for
  ExternalDirectory. A temporary session has no Project, so its "allow
  always" lasts for the session only.
- **A grant is exactly one canonical directory.** (Amended 2026-09-26.) The
  ask names the canonical directory: the path is resolved like a
  containment check, so a symlinked directory or file names where it really
  lives. A remembered ExternalDirectory grant (scoped, saved, or plane-wide)
  is matched by directory equality, not as a glob: `<dir>/*` covers the
  files and entries of `<dir>` only, never its subdirectories, and glob
  characters in a path are never interpreted. The literal `*` pattern still
  means every directory (legacy global rows); any other stored pattern,
  including a `<dir>/*` row saved before this amendment or a bare `<dir>`,
  grants exactly `<dir>`. Rules a user writes in configuration (the plane's
  snapshot, and a turn's attached directories) keep glob semantics.
- **Bash has no path control.** `bash` drops its external-`cwd` check
  (`assert_external_workdir`): its `cwd` may be any directory, and its
  command's file effects are not inspected. It keeps its `Action::Bash`
  invocation rules (allow/ask/deny by command pattern). Its output artifacts
  stay under `<workdir>/.hya/tool-output`.

## Why

- **One helper, one set of edge cases.** Six hand-written prefix checks
  drifted apart and all missed symlinks. A single canonicalizing helper is
  testable once (symlink escape, `..`, missing targets, nested roots) and then
  shared.
- **Remember what was approved.** The user approved one directory, not the
  disk. Storing `<dir>/*` scoped to the Project makes the remembered grant say
  exactly that, and keeps it from leaking into other Projects. Matching it as
  a glob did not: `*` crosses `/`, so approving `~/notes.txt` (`~/*`)
  silently approved `~/.ssh`, and a lexical directory could be a symlink to
  anywhere. Exact comparison of canonical directories keeps the grant to
  what the user saw.
- **Bash cannot be bounded by paths.** A shell command can reach any path
  through variables, `cd`, subshells, or programs it runs. A path check on
  bash would suggest a guarantee it cannot give. Invocation rules on the
  command are the honest control; sandboxing is a separate question.

## Rejected alternatives

- **Parsing bash commands for paths.** Unsound for the reasons above, and a
  source of false asks.
- **Keeping lexical checks.** Cheaper, but leaves the symlink escape open
  exactly where a Project root contains a link to the rest of the disk.
- **Per-root permission modes.** No request for it; one boundary per Project
  keeps the model simple.

## Consequences

- Sessions of a multi-root Project read and edit all its roots without asks;
  a user who adds a root widens the boundary for every session of that
  Project.
- Existing saved `*` ExternalDirectory rules keep working (they are global
  and broad). New approvals no longer create them; users can remove old ones
  with the rules view.
- Existing saved `<dir>/*` rows now grant exactly `<dir>`, no longer its
  subtree; reaching a subdirectory asks once more for it. A row saved with a
  non-canonical spelling (for example `/tmp/...` on macOS, where asks now
  name `/private/tmp/...`) no longer matches and asks again once.
- `bash` is now the one way to touch files outside the roots without an ask.
  Users who need that closed should restrict `bash` with invocation rules or a
  permission bundle.
- Canonicalization costs a few `stat`/`realpath` calls per tool call; paths on
  unreadable directories are treated as outside and ask.
