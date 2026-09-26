# Per-Project catalog tier

Status: Accepted, 2026-09-26.

`hya serve` has no working directory of its own (ADR-0024), but until now two
catalog tiers still read one: project bundles (`./.hya/bundles`, loaded once
at startup as the highest-priority bundle tier) and project plugins
(`./.hya/plugins`, connected once, process-wide, at startup). ADR-0024's own
consequences section named this a known gap: "the only readers left are
project-tier plugin and bundle discovery … which therefore resolve to
`~/.hya/` for a daemon". A daemon almost never has `.hya/bundles` or
`.hya/plugins` under `$HOME`, so in practice these tiers stopped working the
moment ADR-0023's daemon started running from `$HOME` instead of a caller's
cwd, and a relay-connected remote client has no "start directory" on the
backend machine to point them at in the first place. `hya bundle install
--project` and `hya plugin`-style manifests kept writing into a directory's
`.hya/`, but nothing loaded them unless that directory happened to be the
server's start directory.

## Decision

Project bundles and project plugins become **scoped catalog tiers of a
Project**, resolved the same way tools already resolve a Project's roots
(ADR-0024) and its path-permission boundary (ADR-0026): lazily, per Project,
from every one of its roots.

- **Scoped overlays, not a global load.** `RuntimeRegistry` keeps one base
  snapshot (installed + first-party bundles, config-declared plugins, builtin
  agents/permission-modes/bundle-APIs) plus a map of scope overlays keyed by
  `CatalogScope`: `Global`, `Directory(path)`, or `Project{id, roots}`. A
  scope overlay adds Bundle-kind sources, Plugin-kind sources with their
  hooks, and project-bundle model config on top of the base snapshot; it is
  built lazily the first time something binds that scope, and rebuilt when
  the base changes or the overlay's own fingerprint (a digest of its roots'
  directory listings, `plugin.toml` bytes, and bundle `config.yml` bytes)
  changes.
- **Keying.** A session with a live, non-archived Project binds
  `Project{id, roots}`. A session without one (a temporary session, or a
  legacy project-kind session whose Project was deleted) binds
  `Directory(workdir)`. An unscoped request (no session, no directory) binds
  `Global`. A scoped request (a `directory` field or header) resolves through
  the Projects the same way a catalog lookup already does — the registered,
  unarchived Project whose root contains it, longest root wins — and falls
  back to `Directory(dir)` when none does. Subagents and resident sessions
  inherit their root's scope.
- **All roots, first wins.** A Project's bundle and plugin tiers are loaded
  from every one of its roots, in root order; an id or namespace collision
  keeps the first root's definition and warns about the rest (matching how
  `ProjectScope`/tool discovery already treat multiple roots in ADR-0024/
  ADR-0026). Command, skill, and AGENTS.md discovery, which were already
  per-workdir, are now per-Project: the requested directory (or session
  workdir) first, then each root in order, deduped the same way.
- **Implicit trust, registered Projects only.** A Project's bundle and
  plugin code runs with the same implicit trust a local session's tools
  already have inside its roots (ADR-0026): no separate consent prompt. A
  `Directory` or `Global` scope — including any temporary session — never
  loads bundle or plugin *code* from disk, only the inert catalog tiers
  (commands, skills, AGENTS.md) it already had. This closes the gap ADR-0024
  left open without adding a new trust surface: only directories a user (or
  an in-process `exec`/`run`/`-p`/`loop`/workflow invocation, which now
  ensures a Project for its cwd before binding) has actually registered as a
  Project root can run bundle or plugin code.
- **Hot respawn.** Editing a Project bundle's or plugin's manifest respawns
  only that Project's affected process at its next bind; other Projects and
  the base tier are unaffected. A plugin process is owned by the scope
  overlay (`kill_on_drop`) and stops once its overlay is dropped and no
  binding still holds it.
- **Cache.** Scope overlays are cached with an LRU (default 32 scopes) and an
  idle TTL (default 30 minutes), both configurable under `config.yaml`'s
  `catalog_scopes:`. Eviction runs on every bind and a periodic sweep; the
  scope just bound and any scope a live binding still references are never
  evicted.
- **The start-dir tier is removed, with no compatibility flag.** `hya serve`
  reads nothing from its own process directory. In-process, single-shot
  entry points that used to rely on that implicit tier — `hya exec`/`run`,
  `-p` goal mode, `loop`, and standalone Workflow runs — now call
  `ensure_project_for_path` for their working directory before binding, so a
  cwd that was never explicitly registered still gets a Project (reusing a
  containing one if it already exists), matching how a routed session
  already behaves.
- **Project-plugin workspace adapters are unsupported.** A project plugin
  that declares a workspace adapter has it ignored, with a warning; workspace
  adapters remain a config-plugin (process-wide) feature only, since a
  per-scope adapter would have to be wired into every session regardless of
  which Project it belongs to.
- **Project bundle APIs are session-scoped only.** `invoke_bundle_api`,
  `ListPermissionModes`, and `ListBundleApis` gain a directory/session scope
  and answer a Project's bundle APIs and permission modes there; the base
  (global) view stays installed/first-party only, as it already was Q7 in
  the accompanying design notes. `CatalogUpdated` gains an additive
  `project_id` field (empty for the base provider/model catalog, set for one
  Project's catalog tier) so a client knows which scope to re-read.

## Why

- **Consistency with ADR-0024/ADR-0026.** Bundles and plugins are the last
  two catalog surfaces still keyed by "the server's directory" instead of
  "the client's chosen Project"; every other per-directory surface (tools,
  path permissions, commands, skills, Workflows) already moved to per-root,
  per-Project resolution. Leaving bundles and plugins on the old model meant
  a daemon or a relay-connected backend could never serve them at all.
- **Lazy, scoped overlays avoid a global reload.** Composing one overlay per
  Project (instead of reloading the base catalog whenever any Project
  changes) keeps a large multi-tenant daemon's cost proportional to the
  Projects actually in use, bounded by the LRU/TTL cache.
- **No new trust surface.** Restricting bundle/plugin *code* execution to
  registered Project roots — never Directory or Global scope — keeps the
  existing "a Project root is implicitly trusted" boundary from ADR-0024/
  ADR-0026 as the only place code execution is implied by a directory choice.

## Consequences

- **Upgrade effect.** A daemon that used to load `~/.hya/bundles` or
  `~/.hya/plugins` as its "project" tier (because it ran with cwd `$HOME`, an
  artifact of ADR-0023) no longer does: those directories are never a
  Project's root unless a user actually registers `$HOME` as one. Anyone
  relying on that accidental behavior must register the intended directory
  as a Project.
- Users must have a session inside a Project for `.hya/bundles` or
  `.hya/plugins` in that Project's roots to load; a Directory-scoped session
  (no containing Project) or a temporary session never loads them, even if
  the files are present.
- `CatalogUpdated.project_id` is additive on the wire; existing clients that
  ignore it keep re-reading everything on any notice, as before.
- `ListPermissionModes` and `ListBundleApis` now take an optional
  directory/session scope; existing unscoped callers see the same
  installed/first-party-only view as before.
- The start-dir tier's removal is a breaking behavior change for `hya serve`
  invocations that depended on their launch directory holding `.hya/bundles`
  or `.hya/plugins` (see CHANGELOG 0.42.0, Breaking changes).

## Rejected alternatives

- **Per-Project registries** (one independent `RuntimeRegistry` per Project)
  — would duplicate the base catalog (installed/first-party bundles, builtin
  agents) per Project for no benefit, since only the bundle/plugin tiers
  differ; the base snapshot is already shared read-only state.
- **Namespacing every Project's ids into one catalog** — would make bundle
  and agent ids ambiguous across Projects and complicate the existing
  first-wins shadowing rules for no gain; a scoped overlay already isolates
  one Project's ids from another's.
- **A per-Project trust prompt** before running its bundle/plugin code —
  deferred. Implicit trust for a registered Project's roots matches the
  existing tool/path-permission boundary (ADR-0026); a separate prompt would
  be a second, inconsistent trust model. Revisit if Projects are ever shared
  across users or machines.
