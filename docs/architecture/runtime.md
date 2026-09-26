# Runtime

The runtime lives in [`../../crates/hya-core`](../../crates/hya-core). Its
central type is `SessionEngine` in
[`engine.rs`](../../crates/hya-core/src/engine.rs).

## `SessionEngine`

`SessionEngine` owns:

- `SessionStore` for persistence.
- `ProviderRouter` for model streaming.
- `RuntimeRegistry` for one atomically published immutable tool/skill/MCP and
  Agent catalog snapshot, including source ownership metadata for Bundle,
  MCP, and plugin contributions. `ToolRegistry` is only an offline candidate
  builder.
- `PermissionPlane` for allow/ask/deny decisions.
- `InteractionPlane`, `BoundSpawnSender`, `BoundWorkflowSender`,
  `MailboxPlane`, `TodoPlane`, `WebSearchPlane`, `LspPlane`, and
  `FormatterPlane` for cross-cutting tool services.
- `EventBus` for live subscribers.
- optional hook dispatcher for plugins.

### Publish seams

Runtime events do **not** all pass through a single `emit` path. There are
three write/publish seams:

1. **`emit`** — appends the event to SQLite, takes the returned sequence
   number, **then** publishes an `Envelope` with that seq. A live observer
   never sees a non-durable event on this path.
2. **`publish_live`** — publishes an `Envelope` at `seq: 0` with **no** store
   write. Used only for high-frequency **text** streaming
   (`TextStart` / `TextDelta` / `TextEnd`, and live `TextReplace` from the
   `text_complete` hook). At round end those text parts are re-emitted
   **durably** as a `TextStart` / `TextReplace` / `TextEnd` triple. Reasoning
   and other non-text stream events are **not** live-only: they go straight to
   `emit_for_actor` and are durable on first emit (no reasoning re-emission
   loop).
3. **`emit_for_actor`** — the fencing seam for resident work: when given
   `Some(&ActorClaim)` it routes through `commit_resident_mutation` (fenced
   SQLite commit, publish only after commit); when `None` it falls through to
   plain `emit`. Every resident-originated event goes through this method;
   transient turns pass `None` and never look up an actor claim.

All three eventually call **`publish_envelope`**, the single publish seam: it
dispatches to global hooks (`HookDispatcher::dispatch_event`), then activation
(sidecar) hooks for the envelope's session, then the `EventBus`.

### `AgentSpec`

`AgentSpec` is the resolved agent for one turn
([`crates/hya-core/src/engine.rs`](../../crates/hya-core/src/engine.rs)). It is
what a `TurnBinding`'s agent resolution produces and what the server holds as
its process-level default for new sessions:

| Field | Type | Role |
| --- | --- | --- |
| `name` | `AgentName` | Agent display / catalog name |
| `model` | `ModelRef` | Model route for completions |
| `system_prompt` | `String` | System prompt base before guidance/skills composition |
| `workdir` | `PathBuf` | Filesystem workdir for tools and path resolution |
| `reasoning` | `Option<ReasoningEffort>` | Optional reasoning effort for capable models |

Note: `AgentSpec.workdir` is a **`PathBuf`**. Event payloads that record the
session workdir (`SessionCreated`, `SessionMoved`) use **`String`** on the wire
— same concept, different type at the two seams.

The server's process-level `AgentSpec` (`hya_app::agent_base_with_model`) has
an **empty** workdir: `hya serve` has no working directory (ADR-0024). Each
turn's `AgentSpec` takes its workdir from the session's recorded workdir, and
so do workflow members and recovered residents. The client-side direct
commands (`hya exec`/`run`/`-p`/`loop`, `agent_with_model`) name the caller's
cwd as an absolute workdir instead.

### No process working directory

Nothing in `hya-server`, `hya-app`, or `hya-core` resolves a request against
the server process's cwd:

- Every session records its workdir in `SessionCreated`, so turn admission,
  resident revival, fork, command expansion, and workflow runs read it from
  the projection; a projection without one is a missing session
  (`CoreError::Invalid` / `session_not_found`), never a fallback to `.`.
- An rpc that works on a directory takes it from the request (`x-hya-directory`
  or `directory`, absolute) or from the session it names; without either it
  fails with `invalid_argument` (per-rpc table in
  [the protocol guide](../protocol/README.md#base-url-and-scoping)).
- Catalog listings that only prefer a directory (agents, commands, skills,
  bootstrap, agent models) bind the project-less **global** view without one:
  `SessionEngine::bind_global_runtime` (`RuntimeRegistry::bind_global`)
  discovers user skills and builtins only, keyed under the empty path, and
  commands skip `.hya/commands` and leave `${path}` unexpanded.
- Project bundles (`<root>/.hya/bundles`) and project plugins
  (`<root>/.hya/plugins`) are not process-level: they are the per-Project
  catalog tier, loaded lazily at a registered Project's first bind from every
  root (first root wins by id) and published only in that Project's scope
  overlay (`hya-app` `ProjectScopeRefresh`). Nothing is read from the
  directory the process started in. Plugins declared in `config.yaml` stay
  process-wide and win over a project manifest with the same id.

### `RuntimeCatalogRefresh`

Optional app-owned hook that `SessionEngine::bind_scope_runtime` (and so every
root, session, and catalog bind) calls **before** binding a snapshot. Child/bound
turns reuse the parent's pinned `TurnBinding` and never consult the registry.
`hya-app` implements it so the installed bundle catalog can refresh when the
registry generation changed.

```rust
#[async_trait]
pub trait RuntimeCatalogRefresh: Send + Sync {
    async fn refresh_if_changed(
        &self,
        runtime: &RuntimeRegistry,
    ) -> Result<bool, CoreError>;

    /// Default: `Ok(false)`.
    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError>;
}
```

**Return contract** (rustdoc at `engine.rs`):

| Result | Meaning |
| --- | --- |
| `Ok(true)` | A new generation (or scope overlay) was published |
| `Ok(false)` | Nothing changed |
| `Err(_)` | Discovery/publication failed; **a turn bind aborts** and does not bind (catalog reads only log it) |

The engine discards the `bool` after success (`let _ = refresh…await?`), calls
`refresh_if_changed` then `refresh_scope` for the scope being bound, and binds
that scope (see "Scope resolution and the scope cache" below). Implementors own
MCP/plugin discovery I/O; the engine only rebinds after a successful refresh.

## Session Creation

`create` mints a `SessionId` and emits `Event::SessionCreated` with:

- optional parent session
- agent name
- model reference
- workdir
- Project and kind (ADR-0024)

`CreateSession.project` / `CreateSession.kind` apply to a **root** session
only. A subagent session always records its parent's Project and kind,
whatever its spec says. A temporary root session naming a Project is refused
(`CoreError::Invalid`). A fork (`parent: None`) passes its source's Project
and kind, so it stays in the same Project.

`create_with_id(id, spec)` is **idempotent**: if the supplied id already has
events in the log it returns immediately without re-emitting `SessionCreated`.
That makes resume, fork, and recovery paths safe to call unconditionally.

`create_for_actor` is the `ActorClaim`-fenced variant: it commits
`SessionCreated` through `commit_resident_mutation` under the claim.

Parent sessions are used by goal, loop, and team-related helpers to keep child
runs connected to a lead session.

### Placement over v1 (`CreateSession`)

The v1 `CreateSession` handler ([`v1/session.rs`](../../crates/hya-server/src/v1/session.rs))
decides the workdir, Project, and kind before calling
`create_with_id(Some(id), spec)` with a pre-minted id:

| Request | Workdir | Project / kind |
| --- | --- | --- |
| `kind: temporary` (no `projectId`, no `workdir`) | a fresh `0700` directory `<scratch root>/<session id>`, created before `SessionCreated` | none / `temporary` |
| `projectId` | `workdir` if it lies inside a root (component-wise), else `invalid_argument`; unset → primary root | that Project / `project` |
| `workdir` only | `workdir` | `EnsureProjectForPath(workdir)`: the Project containing it (longest root wins), else a new one rooted at it / `project` |
| `parent` | `workdir`, else the parent's | inherited by the engine (`projectId`/`kind` must be unset) |

The scratch root is `$XDG_CACHE_HOME/hya/scratch` (fallback
`$HOME/.cache/hya/scratch`, `hya_store::user_cache_dir`), overridable with
`AppState::with_scratch_root`. Deleting a session never removes its scratch
directory. `EnsureProjectForPath` runs under a server-wide lock so two local
clients starting in one directory share a Project.

### Workspace roots

Every turn resolves the session's workspace roots once, at turn start, and
hands them to each tool call as `ToolCtx::roots` (`SessionEngine::session_roots`,
[`engine/roots.rs`](../../crates/hya-core/src/engine/roots.rs)). Shell turns
resolve them the same way. They are read from the store fresh per turn, so a
Project rename or root replacement applies to the next turn, never mid-turn.

| Session | `roots` |
| --- | --- |
| `kind: project` with a Project | the Project's roots in order; the workdir is prepended when it lies inside none of them (component-wise containment) |
| `kind: project` whose Project was deleted (or cannot be read) | `[workdir]`, with a warning |
| `kind: project` with no Project, or `kind: temporary` | `[workdir]` |

A session created from a local cwd inside a root keeps `workdir = cwd`; the
roots stay the Project's. Subagent sessions inherit their parent's Project,
so they see the same roots. `ToolCtx::workdir` and `bind_session_runtime`
(bundles, skills, AGENTS context) keep using the workdir; path permission
checks do not consult `roots` yet.

## Prompt Admission

`admit_user_prompt` writes a complete user message as:

1. `MessageStarted`
2. `TextStart`
3. `TextDelta`
4. `TextEnd`
5. `MessageFinished`

The same shape is used by `inject_system_message` for system messages.
`admit_command_prompt` records command metadata while admitting a user message.

`record_user_prompt_context` emits `UserPromptContextRecorded { files, agents }`
when a caller records per-prompt context (files, `@mentions`). It
**short-circuits** to `Ok(())` and emits **nothing** when both vectors are
empty — a prompt with no `@mentions` leaves no context event in the log, and
consumers must not expect one per user message. When present, that metadata is
replayed through the projection and provider request builder.

`admit_user_prompt_with_attachments(session, text, attachments)` admits a
prompt with images ([`attachments.rs`](../../crates/hya-core/src/attachments.rs)):
it validates them (PNG/JPEG/GIF/WebP by signature, 10 MiB each, 20 MiB per
turn), then in **one** store transaction
(`SessionStore::append_events_with_blobs`) writes each image to the session's
`file_blob` table under its sha256 and appends `message_started`, the text
part, `user_prompt_context_recorded` (one `image_attachment` entry per image,
naming the blob) and `message_finished`. A crash or error leaves either the
whole prompt or nothing. When a round builds its request, the engine loads the
blobs of the images in the messages it sends (after the latest compaction)
and turns each entry into a `Part::Media` `data:` URL, which the provider
routes encode as their native image blocks. A missing blob drops that image
from the request (logged) rather than failing the turn. A fork copies the
blobs of the copied messages into the new session. The server checks the
turn's model first (`root_turn_model` + `Capabilities::image_input`) and
refuses images for a model that declares no image input.

## Session-state mutators

These methods are thin single-event emitters (append + publish) with no other
side effects:

| Method | Event |
| --- | --- |
| `switch_agent` | `AgentSwitched` |
| `switch_model` | `ModelSwitched` |
| `set_agent_model_override` | `SessionAgentModelOverrideSet` |
| `set_title` | `SessionTitled` |
| `set_workdir` | `SessionMoved` |
| `set_metadata` | `SessionMetadataSet` |
| `set_permission` | `SessionPermissionSet` |
| `set_share` | `SessionShareSet` |
| `clear_share` | `SessionShareCleared` |
| `delete_message` | `MessageDeleted` |
| `delete_part` | `PartDeleted` |
| `replace_text_part` | `TextReplace` |
| `replace_reasoning_part` | `ReasoningReplace` |
| `update_tool_part` | `ToolPartUpdated` |

`archive_session` / `unarchive_session` append `SessionArchived` (stamped
with the current time) / `SessionUnarchived` on a root session only when the
archived state changes (both return whether they appended), and notify the
`session.end` / `session.start` hooks. Archiving a subagent child session is
`CoreError::Invalid`. Archiving a session whose turn is still running fires
`session.end` but keeps the captured bundle hooks and channel policy the turn
reads; the turn finishes normally.

## Durable Workflow Control

Workflow state remains inside the Session event log and shared Projection. The
durable sequence is `WorkflowSelected`, `WorkflowRunStarted`, Stage start/member
link events, one bounded `WorkflowStageRouteOutcome` per explicit provider stream
group, Stage finish events, then `WorkflowRunFinished`. Selection changes only
the Workflow projection and preserves every transcript message. Stage output and
directives remain in child Sessions rather than the owning root event payloads;
route outcomes carry model/effort/failure metadata, not prompts or responses.

`hya-app::WorkflowControl` is the one orchestration adapter for the CLI, Agent
tool, native `/workflow` command, HTTP routes, SDK, and in-process transport. It
builds a catalog from the pinned `TurnBinding`, preflights the complete graph,
and delegates transient, loop, and resident work to existing core execution
paths. The server and SDK decorate replayed state with runtime-only source
availability and Agent activity; neither owns another reducer or scheduler.

Run admission and Workflow selection use actor-fenced store transactions. A
file-backed runtime holds an exclusive owner claim before recovery. Startup
appends one `Interrupted` terminal event for each prior nonterminal run and
never replays a Stage side effect. A selected source is executable only when
its exact source identity and revision still exist; changed and missing sources
remain visible as stale or unavailable.

## Assistant Turn Loop

### Turn activation modes

`TurnActivation` selects how a turn obtains its runtime binding:

| Mode | Behavior |
| --- | --- |
| `Root` | Re-binds the runtime (may start a sidecar) |
| `Bound` | Reuses a `TurnBinding` captured by the parent turn |
| `Resolved` | Reuses pre-resolved agents, resource policy, and sidecar tools |

### Entry points

| Entry point | Activation | Notes |
| --- | --- | --- |
| `run_turn` | `Root` | Interactive / top-level path |
| `run_turn_with_external_dirs` | `Root` | Adds external-directory permission rules |
| `run_turn_with_external_dirs_and_guidance` | `Root` | Also attaches request-scoped guidance |
| `run_bound_turn` | `Bound` | Child path reusing parent binding |
| `run_bound_turn_for_actor` | `Bound` | Resident path with `ActorClaim` |
| `run_resolved_turn_with_sidecar_tools` | `Resolved` | Child with pre-resolved sidecar tools |
| `run_resolved_turn_with_sidecar_tools_for_actor` | `Resolved` | Resident resolved path |

The `_for_actor` variants carry an `ActorClaim` and are the resident path.

### Single active turn per session

A session is one agent, and **an agent runs at most one turn at any time**.
This is an engine invariant, not a convention: every entry point above, shell
turns (`run_shell`), and resident actor wakes funnel through the engine's
per-session turn gate
([`engine/turn_gate.rs`](../../crates/hya-core/src/engine/turn_gate.rs))
and hold its `TurnLease` for the whole turn — from before the first
`MessageStarted` until the turn's `MessageFinished`, sidecar cleanup, and
root-admission finalization. Separate sessions hold separate leases, so team
members and subagents (each its own session) keep streaming concurrently with
each other and with their lead.

| Path | Claim | When the session already has a turn |
| --- | --- | --- |
| `run_turn*`, `run_bound_turn*`, `run_resolved_turn*` (exec, serve, goal/loop drivers, Workflow stages, subagents) | waiting | queues behind the active turn; a cancel while queued returns `FinishReason::Cancelled` with no turn |
| `run_shell` | non-blocking | `CoreError::TurnAlreadyActive` (`session_busy` on `/v1`) |
| resident wake (mail, quiescence synthesis) | non-blocking, taken under the team lock | the wake stays queued on the slot and is delivered at the turn boundary |
| nested same-session `run_turn*` from inside the holding task | — | `CoreError::TurnAlreadyActive` instead of self-deadlock |
| any claim after a drain began | — | refused: `CoreError::Cancelled` (non-blocking) or `Ok(Cancelled)` with no turn (queued) |

Interface (all on `SessionEngine`, exported from `hya_core`):

```rust
pub fn try_begin_turn(&self, session: SessionId) -> Result<TurnLease, CoreError>;
pub fn turn_active(&self, session: SessionId) -> bool;
pub fn set_turn_observer(&self, observer: Weak<dyn TurnBoundaryObserver>);

pub trait TurnBoundaryObserver: Send + Sync {
    fn turn_released(&self, session: SessionId); // runs on a fresh task
    fn turn_started(&self, _session: SessionId) {} // sync, after the claim
    fn turn_failed(&self, _session: SessionId) {}  // sync, before the lease is released
}
// CoreError::TurnAlreadyActive { session } — "TURN_ALREADY_ACTIVE: …"
```

Dropping a `TurnLease` releases the claim, wakes queued `run_turn*` callers,
and notifies the observer. The `ResidentSupervisor` installs itself as the
observer: a wake that arrived while the session's turn was running (child mail
to the lead, the `TEAM QUIESCED` synthesis notice) is re-armed at that
boundary. Team quiescence is declared only when no team member — the lead
included, even when its turn was started by `hya exec`/`serve` rather than the
supervisor — has an active turn. Mail the running turn already consumed
through in-turn steering is not replayed as a separate lead turn.

After prompt admission succeeds, a root turn resolves the session workdir,
refreshes its skill candidate if the logical view changed, and captures one
`TurnBinding`. It then records `MessageStarted` and
`TurnBindingRecorded { generation }`. The binding retains an `Arc` to the
complete immutable runtime snapshot for the entire assistant turn.

### Per-round sequence

Each round runs **in this order** (see `run_turn_rounds` in
[`engine/turn.rs`](../../crates/hya-core/src/engine/turn.rs)):

1. Validate the actor claim (if any).
2. Check activation-hook health (`is_healthy`); unhealthy → `CoreError::Cancelled`.
3. Check the cancel token; if cancelled, emit `MessageFinished` with
   `FinishReason::Cancelled` and return.
4. Read the current projection from the store.
5. Maybe compact context (see [Compaction and Summaries](#compaction-and-summaries)).
6. Run the `chat_params` hook (may rewrite the `CompletionRequest`).
7. Acquire a governor stream permit (reserved or general by depth), then open
   the provider stream: walk the configured cross-model chain, then ask
   `model_fallback` while no stream exists (Workflow-routed turns use their
   declared route instead). The opener returns the stream together with the
   model that serves it.
8. Emit `StepStarted`.
9. Stream the provider round (`collect_stream_round`) — live **text** via
   `publish_live` (then durable text triple at round end); reasoning, tool
   calls, and other events via durable `emit_for_actor` immediately. When the
   round reported usage, append `UsageRecorded { purpose: turn }` with the
   serving model — also when the stream then failed (see
   [Usage attribution](#usage-attribution)).
10. Emit `StepFinished`.
11. **Drop the stream permit** before any tool work.
12. If the round produced no tool calls, emit `MessageFinished` and end the turn.
13. Otherwise run the tool-dispatch pipeline (below), then repeat.

If a provider round produces tool calls, the engine starts another round with
the updated projection. The turn continues until the provider finishes,
cancellation is observed, or execution returns an error.

> **Stream permit lifetime (deadlock invariant)**  
> The governor stream permit is held **only** around provider streaming and is
> dropped **before** tool execution. A member blocked inside the `task` tool
> waiting on its children holds no permit, so nested fan-out cannot deadlock
> the semaphore. Moving tool dispatch inside the permit scope reintroduces
> that deadlock.

### Stream permit class by session depth

Depth is derived from the session parent chain:

- **Depth 0** (root / interactive) takes a **reserved** stream permit
  (`acquire_reserved_stream`).
- **Depth > 0** (subagent) takes a **general** stream permit
  (`acquire_general_stream`).

General work cannot borrow from the reserved pool, so root progress never
queues behind background subagent work.

Defaults (see [`orchestrator.rs`](../../crates/hya-core/src/orchestrator.rs)):

- `DEFAULT_GENERAL_STREAM_PERMITS = 100` (`max_concurrency` normalized to
  `1..=100`)
- `RESERVED_STREAM_PERMITS = 28` (fixed)

Together that is a 128-permit live stream budget.

### Tool dispatch pipeline

For each `ToolCallRequested` collected in the round, in order:

1. Re-validate the actor claim.
2. **`tool.execute.before` hooks** — global plugin host first, then activation
   (sidecar). A `Veto { reason }` produces `ToolError` with message
   `blocked by plugin: <reason>` and the tool never runs.
3. After the before-hook batch, re-check activation-hook health (unhealthy →
   cancel the turn).
4. `resolve_tool` against the bound runtime resource view.
5. Permission authorize.
6. **Re-validate the actor claim at the dispatch boundary** — a stale resident
   cannot dispatch a tool even if it passed validation at round start.
7. Execute the tool.
8. **`tool.execute.after` hooks** — may rewrite the outcome (unless the error
   was a permission denial, which is preserved).
9. Re-check activation-hook health after the after-hook batch.
10. `cap_tool_output` on success so one oversized result cannot blow the next
    model context window.
11. Emit `ToolResult` or `ToolError`, then `FilesChanged` when the call
    changed files ([File snapshots and revert](#file-snapshots-and-revert);
    the prior state is captured after the before-hooks, ahead of the
    permission check).

Formatter/LSP post-edit work for file mutations runs through the tool planes
when configured (inside tool execution, not as a separate round step).

### Root-turn admission cleanup

A completed depth-0 turn on a **governor-backed** engine calls
`finalize_root_spawn_admissions(root)`. The call site is guarded
([`turn.rs`](../../crates/hya-core/src/engine/turn.rs)):

```text
if self.governor.is_some()
    && let Ok((root, 0)) = self.session_lineage(session).await
{
    self.finalize_root_spawn_admissions(root).await?;
}
```

So cleanup runs only when a `SubagentGovernor` is installed **and** the finishing
session is depth-0. When the governor is `None`, this cleanup is **skipped
entirely**, even though durable admission rows may still have been written
(`claim_admission` / `start_admission` still run without a governor).

When it does run, `finalize_root_spawn_admissions(root)`:

- cancels every live governor operation for that root
- cancel-finalizes every nonterminal admission journal row
- releases the root's per-run subagent budget entry

Without this cleanup on a governor-backed engine, a long-lived root session
leaks budget and never recovers spawn capacity.

### Runtime registry publication

Refresh builds and validates a complete candidate while the active snapshot
remains readable, then replaces the active `Arc` once. Only a changed,
successful candidate advances `ConfigGeneration`; failure and logical no-op
leave both generation and effective view unchanged. New turns bind the
published snapshot, while in-flight turns continue on their retained snapshot
without a dispatch-path registry lock. Direct shell turns use the same binding
and audit event.

#### Catalog scopes (internal)

`RuntimeRegistry` keeps one base snapshot plus, per published overlay, one
scope snapshot (`hya_core::catalog_scope`). A `CatalogScope` names what a
turn binds:

| Scope | Key (`ScopeKey`) | Binds |
| --- | --- | --- |
| `Global` | `Global` | the base (user skills only, empty workdir) |
| `Directory(path)` | `Directory(path)` | the base; skills keyed by the workdir |
| `Project { id, roots }` | `Project(id)` (roots are not part of the key) | the base plus the Project's overlay |

A `ScopeOverlay` carries the scope's complete `AgentCatalog`, every
Bundle-kind source (installed plus scope bundles), extra Plugin-kind sources
(project plugins, which may carry hooks), the scope bundles' model leaves
(`bundle_models`), `project_bundle_dirs` (bundle id to source directory), and
a caller-defined `fingerprint` the registry never interprets.

- `publish_scope(key, overlay) -> Result<ConfigGeneration>` composes the
  overlay over the current base at once: `RuntimeCandidate::from_snapshot(base)`,
  `replace_catalog`, `replace_sources_of_kind(Bundle)`, then an upsert of the
  overlay's Plugin sources that the base does not already publish (configured
  plugins beat project manifests). Validation is the base publication's. On
  failure it returns `RuntimeRefreshError::ScopeCompose { scope, source }`,
  consumes no generation, and leaves the base and the scope's previous overlay
  and snapshot unchanged. A publish always replaces the previous overlay;
  callers compare `scope_overlay(key)?.fingerprint` to skip a rebuild.
- `bind_scoped(scope, workdir)` (and `bind_scoped_with_skills` for a
  caller-supplied discovery) first publishes the workdir's skills into the
  base exactly as `bind_turn` does, then retains the scope snapshot. The scope
  snapshot is recomposed lazily at bind when the base generation moved since
  it was built (an MCP reconcile, a skill change, a base catalog publish); a
  failing recompose returns `ScopeCompose` and keeps the previous snapshot. A
  scope without an overlay binds the base. `bind_turn(workdir)` is
  `bind_scoped(Directory(workdir), workdir)`; `bind_global()` is
  `bind_scoped(Global, "")`.
- `drop_scope(key)` forgets the overlay; `scope_keys()` lists the published
  ones. Source owners (plugin and bundle processes) live as long as the
  overlay or any binding retaining the scope snapshot.
- Base and scope publications share one registry-wide generation counter
  (held by the publication lock), so every snapshot's `ConfigGeneration` is
  unique and increasing across scopes. Base-only use numbers generations
  exactly as before.
- `TurnBinding::scope()` and `TurnBinding::project_bundle_dirs()` expose the
  bound scope. They share one `Arc` with the workdir: bindings move by value
  through deep async frames, and a larger binding has overflowed the stack.
  In a scope binding, scope bundle ids drop their user-scope model leaves and
  the overlay's `bundle_models` apply, so `configured_agent_model` reads the
  project bundle's own `config.yml`.
- `bundle_hooks_for_agent` dispatches Plugin-kind bundle hooks and then every
  hook-carrying Plugin source's hooks in source-id order, so a scope's project
  plugin hooks reach only bindings of that scope.

Which scope a session binds, and the overlay builders for project bundles and
plugins, sit above this mechanism in `SessionEngine` and `hya-app`.

#### Scope resolution and the scope cache (`SessionEngine`)

Every bind of a session (create-time `session.start` hooks, admission, each
turn and round rebind, direct shell, title, summary, residents, loop mode)
goes through `bind_session_runtime`, which binds the session's scope
(`catalog_scope_for_session`):

| Session | Scope |
| --- | --- |
| Project kind with a live Project (archived included) | `Project { id, roots }`, roots read from the store at every bind, so a roots edit applies to the next turn |
| Temporary, Project kind without a Project (legacy), or its Project deleted | `Directory(workdir)` (inert tiers only) |
| Child / resident | the parent's: a child copies its parent's Project and kind at creation |

Catalog reads use `catalog_scope_for_directory(dir)`: the unarchived Project
with a root containing `dir` (`resolve_project_by_path`), else
`Directory(dir)` (also for a path the store rejects, such as a relative one);
no directory is `Global`. `bind_root_runtime(dir)` binds that scope and
`bind_global_runtime()` binds `Global`.

`bind_scope_runtime(scope, workdir)` runs `refresh_if_changed` (base), then
`refresh_scope(runtime, scope)` for every scope, `Global` included (the app
publishes or drops the scope's overlay there), then `bind_scoped_with_skills`
with skills discovered for the workdir plus, for a Project, every root in
order (`discover_skills_for_roots_with_builtins`: first root wins a name).

Bundle APIs and permission modes: session-scoped calls (`invoke_bundle_api`
with a session, `session_bundle_apis`, `session_permission_modes`, and the
mode check in `set_permission_mode`) resolve in the session's scope, so a
Project session reaches its Project bundles; their refresh failures are only
logged. The global variants (`bundle_apis`, `permission_modes`,
`invoke_bundle_api` without a session) stay base-only.

`invalidate_catalog_scope(project_id)` drops the Project's overlay and sends
`ScopeKey::Project(id)` to every `subscribe_catalog_scope_invalidations()`
receiver (the server turns it into a catalog-updated notice). Call it when a
Project's roots change, or it is deleted or archived. Existing bindings keep
their snapshot; the next bind rebuilds the overlay.

Scope cache: every non-global bind records the scope's last-bind time. On
each bind and on `sweep_catalog_scopes()` the engine drops (`drop_scope`)
scopes idle longer than `CatalogScopeCacheConfig::idle_ttl` (default 30
minutes), then the least recently bound beyond `max_scopes` (default 32).
The scope just bound and any scope a live `TurnBinding` still retains
(`RuntimeRegistry::scope_in_use`, a turn in flight) are never dropped; turns
rebind every round, which keeps their scope recent. A dropped scope is
rebuilt by the next bind's `refresh_scope`, and bindings keep their snapshot
and its sources alive regardless. Set the limits with
`with_catalog_scope_cache` or `set_catalog_scope_cache_config`
(evictions are not reported to invalidation subscribers: the catalog does not
change).

Captured session hooks follow the scope. Outside a turn, a session's bundle
and scope-plugin hooks (`session.start`, `session.end`, live events) go to the
chain captured from its last bind; inside a turn, the turn's own activation
chain handles events. The captured chain never outlives the scope snapshot
it came from:

- **Own bind swaps.** Every `bind_session_runtime` (so admission, each turn
  and round rebind, shell, title, summary, residents) and every explicit
  capture (create, subagent/resident/loop spawn) replaces the session's chain
  with the new binding's chain for the session's agent, in one map write, at
  bind time, before anything bound to it is published. An event published
  before the swap goes to the old chain, after it to the new one, never to
  both or neither.
- **Identity decides.** Entries are compared by their retained source
  dispatcher (the process) and the owner-bundle hook filter, not by
  generation. A rebind with unchanged entries (a base-only publish that kept
  the processes, a bundle edit that kept the project plugins) changes
  nothing. Otherwise dispatchers new to the session get `session.start` right
  after the swap, kept ones get nothing, and dropped ones get no
  `session.end` (the session did not end); they are released, so a retired
  process exits once no binding or overlay holds it.
- **Idle sessions never pin a retired process.** When any bind of a scope
  yields a new generation, other sessions' chains from that scope whose
  entries the new snapshot no longer has are released at once. Each
  recaptures at its own next bind, where only processes new to it get
  `session.start`.
- **Invalidation and eviction release at once.** `invalidate_catalog_scope`
  and cache eviction release every captured chain from that scope, so its
  processes exit even if the sessions stay idle (a turn in flight keeps its
  binding, and so its processes, until it ends).
- A released session's out-of-turn events and lifecycle hooks reach no
  bundle or scope hooks until its next bind. Process-wide (config) plugin
  hooks are separate and unaffected.

#### `ToolRegistrySnapshot` and dispatch identity

A turn takes an immutable, lock-free `ToolRegistrySnapshot` of the tool
registry so tool resolution cannot change mid-turn. `ToolRegistry` remains
the offline candidate builder; only the snapshot is live.

Each builtin entry carries a SHA-256 **dispatch identity** computed over:

- the domain string `hya.tool.builtin-dispatch/v1`
- the `hya-tool` crate version (`CARGO_PKG_VERSION`)
- the canonical tool name

MCP and plugin tools receive a **per-source** identity instead
(`runtime_source_dispatch_identity` over source kind, configured id,
declaration digest, resources, and export names).

`ToolRegistry::logically_matches` compares a candidate builder against a
published snapshot using tool maps, alias maps, and those dispatch identities.
That is how a reconciliation can tell a no-op refresh from a real change
without diffing full tool schemas.

#### Permission policy semantic identity

`PermissionPlane::semantic_identity_v1` produces a SHA-256 fingerprint over:

- the domain string `hya.permission.semantic-identity/v1`
- the snapshot resource rules
- the invocation model and compiled invocation rule selectors
- the installed interceptor's own identity (if any)

Any change to the effective policy — including swapping the interceptor —
changes the fingerprint, so a reload can detect that permissions actually
changed rather than re-deriving the rule set.

`hya-app::RuntimeReconciler` owns only desired/observed coordination for MCP
and startup plugin declarations. It has no resolve or dispatch API and caches
no effective tool set. Stable source IDs are `(mcp|plugin, configured_id)`;
the effective source manifest, declaration digest, client/child owner, and
exports live only in `RuntimeSnapshot`. Startup, deferred MCP, and Compat MCP
control all submit to this reconciler. Complete current-revision candidates
publish through `RuntimeRegistry`; stale successes are dropped, failures leave
the prior generation unchanged, and explicit removals publish before unrelated
additions. The publication closure always starts from the current snapshot, so
it cannot overwrite a concurrent skill refresh with an older candidate.

#### Deferred MCP startup

When sideplane deferral is enabled (`HYA_DEFER_SIDEPLANES` defaults to on;
set to `0`/`false`/`off`/`no` to disable) **and** MCP servers are configured,
plugins are reconciled synchronously while MCP connection is moved to a
background task. A slow or hanging MCP server cannot block hya from starting.
The user-visible consequence is that MCP tools may not be present for the very
first turn. A refresh rejected by the reconciler is non-fatal and only prints
`hya: MCP runtime refresh rejected` to stderr.

#### MCP resources

Beyond tools, hya performs a best-effort `resources/list` per connected MCP
server and exposes the result through `McpManager::resources()`. Each entry is
keyed `<sanitized server>:<sanitized resource name>`, where sanitizing keeps
ASCII alphanumerics, `_`, and `-`, and replaces every other character with
`_`. Each value carries a `client` field naming the owning server.

Resources are **not** registered as tools and are **not** reachable by the
model through the tool registry today.

Configured Plugin reconciliation covers tool exports and their RPC binding;
startup callbacks remain owned by the configured `PluginHost`. A respawn must
reproduce the complete canonical initialize declaration or calls fail closed.

`RuntimeSnapshot` owns exactly one `BundleCatalog`. At root admission, new turn
binding, subsequent root model-round boundaries, and catalog refresh, `hya-app` merges project, installed, and
first-party payloads. It prepares static Skills, native/Bun/Claude process
contributions, and bundled MCP servers before publishing catalog and Bundle
sources atomically. Initialization failure preserves the previous generation.
Unchanged sources reuse their process owners; retained bindings keep those
owners and staged files alive across replacement and uninstall.

Full-plane agents see agentless Plugin exports. Agent-bearing bundles retain
private tool/MCP resource selection and scoped `hook_refs`. Hooks and permission
interceptors follow captured bindings; they do not consult a second live catalog.
See [Bundle Runtime](../bundle-runtime.md) for names, process environment, exact
contribution checks, and lifecycle contracts. Root round rebinding replaces tools,
prompts, and hooks only after successful preparation; a failed rebind retains
the current snapshot. Bound child/Workflow activations do not rebind. There is
no bundle watcher or per-tool-call database check.

Core Agent definitions come from the trusted, runtime-loaded
[`hya/core-agents` preset](../core-agents.md), and native tool visibility/aliases/
permission posture come from the [five tool-family presets](../base-tools.md). Public packages
cannot claim their trusted origin. [`hya/subagents`](../subagent-bundles.md)
supplies the ordinary `hya-worker` definition (every spawned agent is a
resident actor); [channel policy bundles](../agent-channels.md)
restrict engine-minted communication topology without owning channel identities
or introducing separate event/replay state.

Core preserves that child pinning with a typed `BoundSpawnRequest` carrying the
parent `TurnBinding` through the application supervisor into resident
execution (and, for Workflow Stages without an `actor` key, into the one-shot
Stage runner). The child path does not query the bundle registry database
for a generation or bind whichever runtime is current when a queued request
runs.

## AgentBundle activation sidecars (0.34.11)

`RuntimeSnapshot` retains the sole `Arc<BundleCatalog>` authority and the
shipped `hya-core -> hya-bundle` dependency. For an executable public Bundle,
`hya-app` resolves and materializes the validated resources from the captured
snapshot/`TurnBinding` and constructs the activation-bound factory. The
core-facing start request carries only `activation_id` and `lifecycle`; the
factory returns an opaque lifecycle handle. It introduces no Bundle, package,
path, digest, or `hya-plugin` types. `hya-plugin` owns the child, stdio,
bounded stderr, shutdown, termination, and reap. `hya-app` depends on and
coordinates `hya-core`, `hya-plugin`, and existing `hya-bundle` catalog types.
Dependency directions remain `hya-core -> hya-bundle`, `hya-plugin -> hya-core`,
`hya-app -> hya-core`, and `hya-app -> hya-plugin`; `hya-bundle` remains
independent and no `hya-core -> hya-plugin` edge is added.

Prepared canonical hook IDs are limited to `event`, `tool.execute.before`, and
`tool.execute.after`. Selected Tool/Hook resources exact-path join to exactly
one JS Extension in the referenced resource's owning bundle; the captured
`TurnBinding` determines a deduplicated canonical entrypoint list, and
staged-but-unselected Extensions never activate. Independently initialized
Tool and Hook sets must each exactly equal the selected expected sets before
model polling. Selected Skill declarations must also exactly match the prepared
local ids, bytes, and content digests. Prepared static Skills are published as
source-owned parsed entries without a sidecar process. Old bindings remain
generation-pinned. There is no second resolver, catalog, DTO, or import scan.

Harness remains the sole agent/model/task/mailbox/event/`MemberOutcome` and
recovery runtime. The sidecar wire is newline-delimited JSON-RPC 2.0 using hya
plugin protocol version 1. Initialize remains request/reply: initialize retains
existing `protocol_version` and `host` fields, and the only activation-specific
metadata is `{ activation_id, lifecycle }`. Declaration drift fails before
Running or model polling. `tool/call` and `hook/*` are request/reply and `event` is a
one-way notification without an id or result.

### Sidecar lifecycle cleanup

A turn ending with `FinishReason::Stop` or `FinishReason::Length` calls
`SidecarHandle::shutdown()` (graceful). **Every other outcome** — error,
cancellation, or a round that ended in tool calls without completing — calls
`terminate()`. Sidecar authors must assume `terminate()` is the common path on
abnormal exit and must not rely on shutdown-time flushing for durability.

A root agent's turn activation owns one child through its whole activation
and then shuts down/reaps it (a Workflow Stage without an `actor` key does the
same). A healthy resident reuses one child across mailbox
messages; idle loss lazily creates a fresh child, running loss aborts and
fences the current item without replay, and queued-after work resumes with a
fresh ACK on the same pinned binding. Explicit stop is final and idempotent,
canceling queued work, removing the resident, and releasing its claim. There
is no TTL, heartbeat, reclaim, second runtime, or persisted process state.

### Activation-hook health gate

A task-local activation hook dispatcher is scoped to one session and runs
**in addition to** (not instead of) the global plugin host for
`tool.execute.before` / `tool.execute.after` (and event dispatch).

The turn loop checks the activation hook dispatcher's `is_healthy()`:

- at the top of every round
- again after each before/after hook batch

An unhealthy dispatcher aborts the turn with `CoreError::Cancelled` (not a
hard error). The tool call whose hook was lost never commits its own
`ToolResult`/`ToolError`; the turn then closes like any cancelled turn — the
open tool part gets a harness `ToolError { code: "CANCELLED" }` and the
message gets `MessageFinished { Cancelled }`. Sidecar **loss-token**
cancellation (separate from the health gate) closes the message the same way.

## Resident recovery and actor fencing

Resident subagents retain the immutable runtime `TurnBinding` behavior above,
but additionally carry an internal `ActorClaim` containing their stable session
identity, current monotonic epoch, and per-process owner identity. Actor epochs
and runtime configuration generations are independent: takeover does not
terminate old snapshot owners, and a runtime refresh does not take over an
actor.

Before runtime readiness, `hya-app` advances all active resident claims, aborts
non-actor admissions through the existing startup seam, folds the canonical
projections, terminalizes each old epoch's actor-bound admissions and running
work through the recovered claim, and recreates the existing
`ResidentSupervisor` slots.
Committed queued mail resumes under the new epoch; work that crossed the
durable start marker is aborted and never automatically retried.

The single fencing write seam is **`emit_for_actor`**: with `Some(claim)` it
routes to `commit_resident_mutation` (fenced, publish-after-commit); with
`None` it falls through to plain `emit`. Provider/tool events, mailbox writes,
spawn admissions, and child transitions use that claim-aware path. A stale
claim returns `StaleActorClaim` without appending or waking successful work.
Full-tuple claim release atomically aborts any still-bound admission before the
claim becomes reusable; only the first logical release refunds a live governor.

The claim is TTL-free and local to one harness process incarnation. There is no
heartbeat, wall-clock expiry, lease daemon, distributed coordination, HA, or
active-active behavior. Canonical-state fencing cannot promise exactly-once
filesystem/network/API side effects, and this release does not certify the
future 100/256 workload envelope.

## Turn termination guarantees

Invariant (see [Event Model — End-event invariant](event-model.md#end-event-invariant)):
every assistant message ends with exactly one `MessageFinished`, every
non-terminal tool part reaches a terminal state, and every member reaches a
terminal status. Implemented in
[`engine/turn_end.rs`](../../crates/hya-core/src/engine/turn_end.rs).

### A turn closes its own message

`run_turn*` (and every resident/member/subagent turn — they share the choke
point) runs under an effective cancel token: a child of the caller's token that
the engine can also cancel. However the turn ends, before anything fallible
(Workflow route finalization, sidecar cleanup) the turn closes its message
against the folded log:

| Outcome | Closing events | `cause` |
| --- | --- | --- |
| Model finished (`stop`, `length`, …) | the provider's `MessageFinished` | none |
| Cancelled (caller token, `cancel_turn`, drain, sidecar loss, activation-hook health loss) | `ToolError { code: "CANCELLED" }` per open tool part, `MemberFinished { cancelled }` per member row spawned by those tool calls, then `MessageFinished { cancelled }` | the engine-recorded cause (`user_cancel`, `shutdown`, `leader_failed`, `archived`), else none |
| Provider/runtime error after `MessageStarted` | same, with `code: "TURN_FAILED"`, then `MessageFinished { error }` | `provider_error` for `CoreError::Provider`, else none |

Each step is checked against the projection, so a part or message that already
reached a terminal state is never closed twice. Shell turns (`run_shell`) bind
the same cancel token and close their message the same way on error.

### Graceful drain

A graceful stop — SIGINT/SIGTERM on `exec`/`run`/`-p`/`loop`, the normal end
of those runs, `serve` shutdown (SIGTERM/SIGINT/SIGHUP), `BuiltSessionEngine::shutdown`
— drains every in-flight turn in **every** session (roots, members,
subagents, and a quiescence-synthesis turn that started on the lead during
shutdown):

1. `begin_drain(cause)`: the turn gate refuses every new claim from now on
   (`try_begin_turn` → `CoreError::Cancelled`; queued `run_turn*` →
   `Ok(Cancelled)` with no turn; resident wakes stay parked) and cancels each
   active turn, recording `cause` on it.
2. Wait up to **`DRAIN_DEADLINE` = 5 s** for the gate to go idle; each
   cancelled turn closes its own message (above).
3. A turn still running at the deadline is a straggler: its open messages are
   closed by the drain (`SessionStore::close_open_turns`, same events, same
   cause).
4. `ResidentSupervisor::drain` then stops every team and **archives** every
   member (deepest first): a degraded handoff, `MemberFinished { cancelled }`
   (summary `stopped: …`) on the parent log, claim released, then
   `AgentArchived { reason: shutdown }`. Archived members stay readable and a
   later run on the same database wakes one by mailing its handle (same handle
   and session, resumed from the handoff). Each lead is parked `idle` — the
   lead is never archived and its session stays resumable. (A member whose
   archive cannot commit falls back to roster `failed` with a `stopped: …`
   reason and a released claim, as before 0.41.0.)

Causes: SIGINT → `user_cancel`; SIGTERM, normal end, `serve` shutdown →
`shutdown`; end of a one-shot run whose lead turn failed → `leader_failed`.
The drain is idempotent (the first cause wins). A second SIGINT during a
one-shot run's drain exits at once (status 130); crash recovery closes whatever
it left open.

```rust
pub const DRAIN_DEADLINE: Duration; // 5 s
impl SessionEngine {
    pub fn cancel_turn(&self, session: SessionId, cause: FinishCause) -> bool;
    /// cancel_turn + wait up to `deadline`, then close a straggler's message
    pub async fn stop_turn(&self, session: SessionId, cause: FinishCause, deadline: Duration) -> bool;
    pub fn begin_drain(&self, cause: FinishCause) -> Vec<SessionId>;
    pub async fn drain_turns(&self, cause: FinishCause, deadline: Duration) -> TurnDrainReport;
    pub fn draining(&self) -> Option<FinishCause>;
}
pub struct TurnDrainReport { pub cancelled: Vec<SessionId>, pub stragglers: Vec<SessionId> }
impl ResidentSupervisor {
    pub async fn drain(&self, cause: FinishCause, deadline: Duration) -> TurnDrainReport;
}
impl BuiltSessionEngine { // hya-app
    pub async fn drain(&self, cause: FinishCause) -> TurnDrainReport; // DRAIN_DEADLINE
    pub async fn shutdown(&mut self) -> Result<(), CoreError>;         // drain(Shutdown) first
}
```

### Crash recovery

A process that dies mid-turn (SIGKILL, OOM, power loss) leaves assistant
messages open. When the next process claims the runtime-owner lock
(`.runtime-owner.lock` next to the SQLite file; every engine build claims it
before startup recovery), `build_session_engine` runs
`SessionStore::recover_interrupted_turns(owner)` **before** Workflow/resident
recovery and before any turn:

- The candidate set comes from the write-through `open_assistant_message`
  index (inserted on assistant `message_started`, deleted on
  `message_finished` / `message_deleted`, backfilled by migration `0010`), so
  the pass folds only sessions a crash left mid-turn — not every log.
- Per session, in one `BEGIN IMMEDIATE` writer transaction: `ToolError
  { code: "INTERRUPTED" }` per open tool part, `MemberFinished { cancelled }`
  per spawning/running member row, then `MessageFinished { cancelled, cause:
  interrupted }` per open assistant message; stale index rows are dropped.
- Idempotent: closing a message removes its index row, so a second pass (same
  or later owner) appends nothing. It requires the owner claim, so it can never
  close a live turn of another writer.

Resident actors that were mid-turn are then recovered as before
(`recover_resident_actor`); their messages are already closed.

### Leader failure

When a team lead's turn (the root session, handle `main`) fails with a
provider/runtime error — never a user cancel, a drain, or a SIGINT:

- the turn ends `finish: error` (`cause: provider_error` for provider
  failures); the lead is **never** reported, handed off, or archived — its
  session stays live and resumable and members keep their DM back to it;
- the harness mails every live resident member of the team (all depths) a
  wrap-up notice from `harness` (see
  [Subagent Orchestration — Leader failure](subagent-orchestration.md#34-leader-failure));
- the supervisor marks the lead failed: it starts **no** lead turn on its own
  — no `TEAM QUIESCED` synthesis, no mail wake — until a turn on the lead
  starts from outside the supervisor (the user resumes it). Members' reports
  stay in the lead's inbox and are surfaced to that resumed turn by in-turn
  steering. The next quiescence after the resumed turn synthesizes normally.

In a one-shot `exec`, the run ends right after, so the members are drained
with `cause: leader_failed` (the notice stays in the log).

## Usage attribution

Every provider call that reports usage is recorded once in the session's log
as `Event::UsageRecorded` (contract in
[event-model.md](event-model.md#token-accounting)):

| Call | `purpose` | `message` / `step` | `model` |
| --- | --- | --- | --- |
| Turn round | `turn` | assistant message / round index | model that opened the stream: after `chat.params`, the fallback chain or `model.fallback` hook, or the Workflow route candidate |
| `auto_title_session` | `title` | none | title model (definition model or caller fallback) |
| Ladder `summarize` / `handoff` rung, `summarize_session`, terminal handoff | `compaction` | none | summarizer request model (definition/preference model or summarizer fallback) |

- `usage` is the call's own normalized `TokenUsage`, not a message sum.
  `MessageFinished.tokens` keeps the legacy per-message sum; the projection
  fold never counts both.
- A round whose stream reported usage and then failed is still recorded
  (best-effort on the failure path; the round's error wins). A stream dropped
  before the decoder reported usage (mid-stream cancel, transport reset) has
  no usage to record.
- Side calls go through `SummarizeOptions.usage: Option<UsageCollector>` (the
  summarizer records `(model, usage)` per call) and
  `SessionEngine::record_side_call_usage`; recording failures are logged and
  never fail the call.
- Not attributed: provider-native `/responses/compact`, goal evaluators, and
  loop verifiers.

The fold `SessionProjection.usage` sums the records by serving model and by
purpose, per session log; read it with `read_projection(session)`. Example: a
turn whose first round ran on `openai/gpt-5` and whose second round failed
over to `anthropic/claude-sonnet` yields two `turn` records and two
`by_model` entries; the Anthropic entry's output lands in
`reasoning_unknown_output` because Anthropic does not report thinking.

The token ledger (`record_session_usage`, best-effort, one row per finished
assistant message) prefers the message's attributed rounds: the sum of its
records with the latest round's serving model, `prompt_tokens =
input + cache_read + cache_write`.

## Compaction and Summaries

Compaction lives in [`compaction.rs`](../../crates/hya-core/src/compaction.rs)
and is walked from the turn loop. The mechanism set, the configurable firing
order, thresholds, and wire records are documented canonically in
[Compaction](../compaction.md); this section records the runtime contracts.

`CompactionConfig` fields: `token_threshold` (default `100_000`),
`keep_recent` (default `6`), `context_fraction` (default `0.75`),
`reserve_tokens` (default `16_384`), `summary_max_tokens` (default `4_096`),
and `method_order` (a permutation of the five rungs; see below).

The trip threshold is `min(window * context_fraction, window -
reserve_tokens)` floored at `MIN_RESOLVED_THRESHOLD` (`1_000`) when the route
advertises a nonzero `max_context`; otherwise the flat `token_threshold`
applies. A fraction outside `(0.0, 1.0]` falls back to the flat threshold.
`keep_recent` is independent: compaction still requires
`messages.len() > keep_recent`.

`SummarizeOptions` fields for summarizer calls: `system`, `model`,
`reasoning`, `previous_summary` (the anchored summary this one updates),
`max_output_tokens`, and `handoff` (send the verbatim transcript plus one
trailing handoff prompt instead of the rendered serialization).

### The reduction ladder

When the window is over threshold, the turn walks
`CompactionConfig::method_order` — the five oh-my-pi mechanisms under their
omp wire names: `shake` (`SpillToolOutputs`), `remote` (`ProviderCompact`),
`soft` (`Summarize`), `snapcompact` (`SnapCompact`), `handoff` (`Handoff`).
Default order: `shake, remote, soft, snapcompact, handoff`.

1. Before each rung the loop re-checks the threshold; the walk stops at the
   first rung that fits the transcript back under it.
2. An unavailable rung advances: `remote` on a route without compact support
   (only `openai-response`, `openai-codex`, and `grok-build` advertise
   `/responses/compact`), `soft`/`handoff` when no summarizer is wired,
   `shake` when nothing is left to evict. A failed rung advances the same
   way, so a model-free order still folds.
3. `remote` resolves the fixed `compaction` system agent (missing definition
   fails closed with `AgentDefinitionMissing`), calls
   `ProviderRouter::compact_if_supported`, and persists the folded items
   behind `HYA_COMPACTED_CONTEXT` + `<<<RESPONSES_COMPACT_ITEMS>>>` via
   `format_responses_compact_system`; later `/responses` requests re-inject
   the items verbatim.
4. `soft` folds the prefix through `fold_prefix`/`ModelSummarizer` behind the
   same marker; `snapcompact` commits a local deterministic archive of the
   folded prefix (no model call); `handoff` commits a model-written handoff
   document over the whole transcript. All three emit `ContextCompacted`
   with their own strategy (`local_summarizer` / `snap_compact` / `handoff`).
5. `shake` evicts stale completed tool outputs to `artifact://` handles
   (request-local, idempotent, `keep_recent` protected; a 512-byte floor
   keeps bodies that cost more to reference than to keep) and emits
   `ContextEvicted` whenever it saved tokens — including when the saving
   alone did not suffice and the walk escalated.

Every fold is persisted behind the `HYA_COMPACTED_CONTEXT` marker, and
`compacted_messages` (`engine/turn/messages.rs`) drops pre-marker history on
later requests. `previous_summary` reads the marker-prefixed body so the
next fold anchors on it — including snapcompact archives and handoff
documents.

The CLI exposes local compact via `/compact` (`engine/summary.rs`, which
also writes the marker); the v1 `SummarizeSession` rpc persists the same
native summary shape.

## Session Titles

**Trigger.** The v1 server (`hya serve` and the TUI's backend; opt-in
`AppState::with_auto_title` for embedders) calls `auto_title_session` in a
background task right after it admits a turn's user prompt — for `prompt`
and `command` turns, not for shell turns, which admit no user prompt. The
turn never waits for it, and a failure (no title agent, provider error,
empty output) is logged at `warn` and dropped. Headless `hya exec` and the
RPC loop do not title.

`auto_title_session` issues a separate provider completion to generate a
session title. Guards:

1. **Root sessions only** — children (`parent.is_some()`) are skipped.
2. Skips any session that already has a non-default / non-fallback title
   (set at creation, by `UpdateSession`, or by an earlier auto title).
3. Requires **exactly one** user message in the projection (multiple user
   messages → no title), so only the first prompt titles a session.

Together these make titling idempotent: every later turn, a concurrent
admission, and a restart find either a title on the log or more than one
user message, and skip before any model call. A title set while the call is
in flight (a manual rename) wins — the generated one is dropped. A title
call that failed is not retried on later turns.

It resolves the fixed `title` system agent from the bound catalog and calls
the provider at `temperature: 0.0` with `max_output_tokens: 128`, honoring the
definition's model and reasoning effort (falling back to the caller's model
when the definition has none). Then it emits `SessionTitled`.

This is an extra billed provider call per title generation, on the route
resolved for that model; its usage is recorded as
`UsageRecorded { purpose: title }` on the session being titled. The title is
the first non-empty line of the output (`<think>` blocks stripped), cut at
100 characters. With the offline `hya/offline` echo model that line is the
first line of the prompt, so offline sessions get a deterministic title
without a special case.

### Fixed system agents

`FixedSystemAgent` is the closed set of Harness system operations:

| Id | Use |
| --- | --- |
| `compaction` | Over-threshold context compact |
| `title` | Auto session title |
| `summary` | Explicit summarize path |

Callers cannot pass an arbitrary agent id into these seams.

### Empty-session cleanup

`cleanup_empty_unnamed_session` deletes a session if and only if
`title::is_empty_unnamed_session` holds: the session exists, has **no** title,
and has **no** messages (no user content and no assigned title). It calls
through to `SessionStore::delete_session`, which removes `token_ledger` rows
then `event_log` rows in one transaction.

## Shell turns

`run_shell` admits a shell user message, binds the root runtime, then emits a
full synthetic assistant message around one `shell` tool call:

1. `MessageStarted`
2. `TurnBindingRecorded`
3. `ToolInputStart`
4. `ToolCallRequested` (after optional `tool.execute.before`; a veto emits
   `ToolError` and finishes with `Error`)
5. `ToolResult` or `ToolError`
6. `FilesChanged` when the command changed files in a git work tree
   ([File snapshots and revert](#file-snapshots-and-revert))
7. `MessageFinished`

There is **no** provider call, so no `StepStarted` / `StepFinished` and no
stream permit is taken.

## Forking a session

A fork is a new root session that starts with a copy of another session's
transcript. `POST /v1/sessions/{session}/fork` (`ForkSession`) resolves the
cut with `hya_core::fork_cut` over the source's **visible** messages
(messages hidden by a pending revert are never copied):

| Request | Cut | The fork holds |
| --- | --- | --- |
| `{}` (head) | none | every visible message, the last one included |
| `{"messageId": "<user message>"}` | that message | the messages strictly before it; `promptText` returns its text (the TUI puts it back in the composer) |
| `{"untilSeq": "<seq>"}` | first message started after `seq` | the messages whose `message_started` has `seq <= untilSeq` (in their final state) |

`messageId` must name a visible user message of the source (`invalid_argument`
for another role, `not_found` when absent). The server then creates the
session (source agent, model, workdir, Project, kind), records
`session_forked { source, before_message }` (the cut, `None` for a head
fork), titles it `<source title> (fork)` (the source id when the source has
no title or a default one; a fork of a fork keeps one suffix, so automatic
titling — which renames only default titles — never renames it), copies the
metadata, and calls
`copy_messages_to_session`, which replays the kept messages as **fresh**
events with newly minted `MessageId` / `PartId` per copy (ids are never
reused). `SessionInfo.forkedFrom` reports `{session, messageId}` from
`session_forked`.

Tool parts cannot be replayed as their original streaming events, so they are
recreated as `ToolInputStart` followed by `ToolPartUpdated` carrying the final
`ToolPartState`. File snapshots (`files_changed`) are not copied: reverting a
copied message in the fork restores no files.

Consequence: a forked session's event log is not byte-identical to the
source's, and its sequence numbering is independent.

Before 0.41.0 a head fork passed the last message as the cut and so dropped
it, and `untilSeq` was ignored.

## File snapshots and revert

`/undo` in a frontend is `RevertSession`: it hides the last user turn (and
every later message) from the transcript and puts the files that turn's tools
changed back the way they were. `/redo` (`undo: true`) brings both back,
until the next prompt commits the revert. This follows opencode's semantics,
with hya's event log in place of opencode's git snapshot directory.

### Snapshots (`files_changed`)

Before a tool call runs, the engine captures the prior state of the files it
may change ([`file_snapshot.rs`](../../crates/hya-core/src/engine/file_snapshot.rs));
after the call it keeps the prior content of each file that actually changed
as a per-session blob (`file_blob` table, keyed by sha256) and appends one
`files_changed { message, call, files: [{path, before}] }` after the call's
`tool_result` / `tool_error`. `before` is `absent`, `stored {hash, size}`, or
`omitted {size, reason}`.

| Tool | What is captured |
| --- | --- |
| `write`, `edit` | the `path` (or `file_path`) argument, resolved against the workdir like the tools do; `local://` handles are skipped |
| `apply_patch` | every `*** Add File:`, `*** Delete File:`, `*** Update File:`, and `*** Move to:` path of the patch |
| `bash` (model calls and the user's `!` commands) | only when the session workdir is inside a git work tree: `git status --porcelain -z --untracked-files=all` before and after the command (plus the `HEAD` tree diff if the command moved `HEAD`) names the touched files; a file that was clean before is read back from the old `HEAD` tree, a dirty or untracked one from a copy read before the command. Ignored files and the workdir's `.hya/` are not covered. Outside a git work tree bash changes are not captured. |

Why event + blobs and not a git shadow repository: the restore data lives in
the same database as the transcript, so it is replayed, deleted, and capped
with the session, and `write`/`edit`/`apply_patch` are covered in any
directory. Git is used only as a change detector for bash, where the touched
files cannot be known in advance.

Limits, so snapshots never grow without bound:

| Limit | Value | Beyond it |
| --- | --- | --- |
| One file's content | 2 MiB (`MAX_FILE_BYTES`) | `omitted` / `too_large` |
| All blobs of one session | 256 MiB (`MAX_SESSION_BLOB_BYTES`) | `omitted` / `session_cap` |
| bash pre-capture: dirty files read | 2,000 files (`MAX_DIRTY_FILES`) | `omitted` / `snapshot_budget` |
| bash pre-capture: bytes read | 16 MiB (`MAX_DIRTY_BYTES`) | `omitted` / `snapshot_budget` |
| each git call | 10 s | the bash call is not captured |

Blobs are deduplicated by hash within a session and deleted with it. Prompt
images (see [Prompt Admission](#prompt-admission)) share the table: they are always
stored (their own limits are 10 MiB each, 20 MiB per turn), and they count
toward the 256 MiB session total, so a session with many images keeps fewer
file snapshots. A
capture problem never fails the tool call; an `omitted` file is simply not
restored. The bash capture compares the tree before and after the command,
so a file some other process changed while the command ran is recorded too.

### Revert, unrevert, commit

`SessionEngine::revert_session(session, target)` takes the session's turn
lease (so no turn runs meanwhile; the server also holds the admission slot and
answers `session_busy` while a turn runs), then:

1. Picks the target: the last visible user message (`/undo`) or a given
   visible user message. Assistant/system messages and already hidden
   messages are refused.
2. For each path in the `file_changes` of the messages it will hide (then of
   the already hidden ones), takes the **earliest** recorded `before` state:
   the state before the first hidden change.
3. Keeps each file's current content as a blob (`saved`), then writes the
   `before` state: `stored` content is written back (parent directories
   created), `absent` removes the file, `omitted` leaves it alone.
4. Appends `session_reverted { message, files: [{path, restored, saved,
   error?}] }`. The reducer moves the message and every later one from
   `SessionProjection.messages` to `SessionProjection.revert.hidden`.

A revert while one is pending extends it further back; each file keeps the
`saved` state of the first revert. `unrevert_session` writes every `saved`
state back, appends `session_unreverted { files }`, and the reducer returns
the hidden messages to the transcript. The next `message_started` on the
session (a prompt, a shell turn, a compaction summary) **commits** the
revert: the reducer drops the hidden messages for good and `/redo` is no
longer possible. Because the hidden messages are no longer in
`SessionProjection.messages`, the model context of the next turn, titles,
handoffs, and `ListMessages` never see them.

Not restored: changes made by subagent sessions (their `files_changed` live
on the child logs), bash changes outside a git work tree, ignored files, and
`omitted` files. A file edited by hand after the reverted turn is still set
back to its recorded state.

## Hooks

[`hooks.rs`](../../crates/hya-core/src/hooks.rs) defines `HookDispatcher`, the
runtime hook boundary used by `hya-plugin` and activation sidecars.

### `HookDispatcher` methods

There is **no** permission-ask method on `HookDispatcher`. Permission
callbacks are owned by `PermissionPlane` and the `hya-plugin` permission
bridge (see below).

| Method | Input | Outcome |
| --- | --- | --- |
| `dispatch_event` | `&Envelope` | (void) — fires for **every** published envelope, including seq-0 live-only ones |
| `is_healthy` | — | `bool` (default `true`) |
| `command_execute_before` | `CommandExecuteBeforeInput { session, command, arguments, text }` | `Continue { text }` |
| `text_complete` | `TextCompleteInput { session, message, part, text }` | `Continue { text }` |
| `message_user_before` | `MessageUserBeforeInput { session, text }` | `Continue { text }` |
| `chat_params` | `ChatParamsInput { session, root_session, agent, message, request }` | `Continue { request }` |
| `model_fallback` | `ModelFallbackInput { session, root_session, agent, message, model, error_class, error_message, attempt, tried }` | `Retry { model }` or `GiveUp` (default); first `Retry` in chain order wins |
| `tool_execute_before` | `ToolExecuteBeforeInput { session, message, call, tool, input }` | `Continue { input }` or `Veto { reason }` |
| `tool_execute_after` | `ToolExecuteAfterInput { …, result: ToolOutcomeNative }` | `Continue { result }` where result is `Ok { output, time_ms }` or `Err { message }` |

The CLI installs a `PluginHost` when `plugins:` are configured.

### `tool.execute.before`

Runs before every model-issued tool call and also for direct shell turns. A
hook may return rewritten input JSON (replaces the model's arguments) or a
`Veto` with a reason. A veto skips execution entirely and emits a `ToolError`
whose message is `blocked by plugin: <reason>`. Both the global plugin host
and the per-session activation (sidecar) dispatcher run this hook (global
first).

### `text_complete`

The engine accumulates streaming text per `PartId` in a `TextPartAccumulator`.
On `TextEnd`, the `text_complete` hook is offered the accumulated text and may
return replacement text. A rewrite:

1. publishes a live `TextReplace` (`publish_live`, seq 0), and
2. changes what is persisted in the durable
   `TextStart` / `TextReplace` / `TextEnd` triple at round end

So this hook can alter the stored transcript, not just the display.

### `permission.ask` (plugin bridge, not `HookDispatcher`)

`PermissionBridge` implements `PermissionInterceptor` over the plugin host
(wired in `hya-app` when plugins are present). Every plugin that declared the
`permission.ask` hook is polled in declaration order; the **first non-`defer`
reply decides** and later plugins are not consulted.

Valid wire outcomes (`outcome` tag, snake_case):

- `allow_once`
- `allow_always`
- `reject` with optional `feedback` string (folded into
  `PermissionError::Denied` as `— user says: <feedback>` for the model)
- `defer` — try the next plugin

If every plugin replies `defer` (or none declare the hook), the request falls
through to the normal user prompt.

### Activation hooks

A task-local activation hook dispatcher is scoped to one session and runs
alongside the global host for the same tool before/after hooks and event
dispatch. If it reports unhealthy while a tool call is in flight, the whole
turn is cancelled rather than silently continuing without the sidecar's hooks
(see [Activation-hook health gate](#activation-hook-health-gate)).

## Goal Mode

Goal mode lives in [`completion.rs`](../../crates/hya-core/src/completion.rs).
It uses three pieces:

- `IterationDriver`: generic loop runner with safety caps.
- `LeadTurnExecutor`: admits the next directive into the lead session and runs a
  turn.
- `GoalGate`: asks a `GoalEvaluator` whether the transcript satisfies the goal.

`ModelGoalEvaluator` issues a tool-free `CompletionRequest` at
`temperature: 0.0` and `max_output_tokens: 256`, asking the model for a strict
JSON verdict:

```json
{"met": true, "reason": "..."}
```

This is a **separate** provider call per gate evaluation and therefore bills
against the same route as the evaluator's configured model (not free).
Malformed evaluator output is treated as not met, so it counts toward caps
rather than causing an unbounded loop.

## Loop Mode

Loop mode lives in [`loop_mode.rs`](../../crates/hya-core/src/loop_mode.rs).
It is a lower-level planner/verifier loop:

- `LoopVerifier` grades transcript evidence.
- `LoopPlanner` proposes the next directive.
- `LoopGate` is the only component allowed to stop for success.
- `cost_preflight` rejects budgets outside the hard ceiling before workers run.

Current guards include:

- explicit budget
- satisfaction threshold
- evidence-quality requirement
- no-progress detection
- repeated-directive detection unless the planner marks a strategy change

The current CLI exposes goal mode directly; loop mode is available as core
runtime API.

## Teams, Members, and Workspaces

Team-related code is split across:

- [`subagent.rs`](../../crates/hya-core/src/subagent.rs)
- [`workspace.rs`](../../crates/hya-core/src/workspace.rs)
- [`category.rs`](../../crates/hya-core/src/category.rs)

There is no `team.rs` and no in-memory `TeamControlPlane`. Inter-agent mail and
channels are event-sourced (`MailSent`, `ChannelJoined`, `ChannelLeft`) and
folded by `hya-proto::Projection` (see
[ADR 0001](../adr/0001-event-sourced-mailbox-and-channels.md)).

Reach is **hierarchy-scoped**: an agent addresses only its parent, its
same-parent siblings, and its direct reports. Handles are canonical paths
(`main/hya-planner-amiya/hya-worker-texas`; leaves are `<subagent_type>-<operator>`, see
[subagent-orchestration.md §2.1](subagent-orchestration.md#21-handles-agent-type--operator-name-0410))
and channels belong to one unit (`main/hya-planner-amiya#build`). The rule itself is pure path arithmetic in
`hya-proto::scope`, enforced at a write gate in `hya-store` and a read filter in
`hya-core` (see
[ADR 0011](../adr/0011-hierarchy-scoped-mailbox.md)).

`run_team` runs member specs in child sessions and returns bounded evidence
summaries. It intentionally does not project full child transcripts into the
lead session.

`WorktreeManager` allocates git worktrees under `.hya/worktrees` and only cleans
up paths it recorded as owned.

These primitives are present in `hya-core`; the shipped CLI exposes
single-turn/run aliases, goal, server, replay, sessions, catalog/auth, and
JSONL RPC surfaces (bare startup prints a guidance banner).

## Errors

`CoreError` variants
([`error.rs`](../../crates/hya-core/src/error.rs)):

| Variant | Meaning |
| --- | --- |
| `Bundle` | Bundle catalog / prepare failure |
| `Provider` | Provider stream or compact failure |
| `Tool` | Tool execution failure |
| `Store` | SQLite / session store failure |
| `RuntimeRefresh` | Runtime candidate publication rejected |
| `Cancelled` | Turn cancelled (including activation-hook health loss) |
| `AgentDefinitionMissing { agent_id }` | Fixed system agent not in catalog |
| `Invalid(String)` | Other invalid runtime state |

`hya-server` maps every `CoreError` to the `internal` error code (HTTP 500 /
gRPC `Internal`) via the v1 error table (see
[Server and Client](server-client.md)); only Workflow control keeps its own
structured failure codes. `Cancelled` and `AgentDefinitionMissing` are
therefore **not** distinguishable through the generic mapping today.
