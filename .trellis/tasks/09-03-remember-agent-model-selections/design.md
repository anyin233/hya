# Technical Design: Remember Per-Agent Model Selections

## Problem and invariants

Hya has three distinct model sources that must not be conflated:

1. Bound Agent policy (`model_policy.model` or `model_policy.category`) is explicit configuration.
2. A remembered TUI choice is a user preference only for an Agent without explicit model-routing policy.
3. A request-, Session-, spawn-, or Workflow Stage-local model is an explicit invocation override.

The preference must affect every catalog Agent, including ordinary subagents and fixed hidden system agents. A client-local TUI file cannot satisfy this: model-authored subagent spawns do not pass through the TUI, and a remotely attached TUI does not share the backend filesystem. The backend runtime therefore owns persistence and publishes one low-precedence in-memory view.

The Session Event log remains authoritative for Session behavior. Agent model preferences are global control-plane state, not Session events and not a second Session projection.

## Domain contract

### Identity

- Preference key: exact `AgentDefinition.stable_id`, serialized as the catalog Agent id.
- Preference value: one base `ModelRef`; no reasoning variant, credentials, headers, prompt data, or provider response.
- Explicit Agent policy means either `model_policy.model.is_some()` or `model_policy.category.is_some()`. A reasoning-only policy remains preference-eligible.
- A preference is usable only when the current provider router resolves its exact model. Unavailable preferences remain stored but are dormant.

### Precedence

From lowest to highest for normal spawn resolution:

1. Process/base model.
2. Exact-catalog remembered model, only while the bound Agent has neither a
   direct model nor a category policy.
3. Bound Agent category.
4. Request inline-Agent category.
5. Bound Agent direct model.
6. Request inline-Agent direct model.
7. Spawn/request category.
8. Spawn/request direct model.

Root creation uses the applicable subset: base < remembered < Agent category <
Agent direct model < explicit request category/model. An explicit Workflow Stage
route is applied after Agent-default resolution and remains highest for that
Stage role.

The presence of a direct model or category suppresses remembered state even if
the configured route is temporarily unresolvable; Hya must not turn stale
configuration into an implicit preference override. A reasoning-only policy
remains preference-eligible. A configured Agent can retain a dormant row. If
the explicit policy is later removed, the prior valid preference becomes
active again instead of being silently deleted.

Only deliberate TUI model changes update preferences. CLI/request overrides,
Session hydration, reasoning variants, spawn overrides, and Workflow Stage
routes remain invocation state.

## Persistence and memory boundary

### SQLite ownership

Add migration `crates/hya-store/migrations/0009_agent_model_preference.sql`:

```sql
CREATE TABLE agent_model_preference (
    agent_id    TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    CHECK(length(agent_id) BETWEEN 1 AND 1024),
    CHECK(length(provider_id) BETWEEN 1 AND 1024),
    CHECK(length(model_id) BETWEEN 1 AND 4096)
);
```

Separate provider/model columns avoid an ambiguous parser when model ids contain
slashes. `hya-store` exposes a typed `AgentModelPreference` plus deterministic
list, owner-fenced upsert, and owner-fenced idempotent delete methods on
`SessionStore`. Each mutation checks the current runtime owner in the same
`BEGIN IMMEDIATE` transaction as the row change. The table is an auxiliary
control plane: it never emits public Events and never stores Session state.

The active backend database owns the preferences. The normal persistent
Session database survives restart; an explicit alternate `--db` is an
independent preference scope; intentional in-memory mode is non-durable. This
reuses SQLite migration, atomicity, busy handling, and runtime-owner fencing
instead of adding JSON locking, fsync, corruption, and remote-client policy.

### Core immutable view

Add `AgentModelPreferences` and `AgentModelPreferenceSnapshot` in `hya-core`.
The publisher retains a Tokio watch value and uses infallible `send_replace`;
readers clone one `Arc<BTreeMap<AgentName, ModelRef>>`, not the map.

`RuntimeRegistry` owns the publisher and every new `TurnBinding` captures one
immutable preference snapshot beside the existing runtime snapshot. Runtime
candidate refresh does not copy or overwrite this separate control-plane state.
Old bindings remain pinned; new bindings see the latest successfully published
map.

One pure model resolver accepts the bound `AgentDefinition`, base model,
captured preference, existing category registry/router predicate, and existing
inline/request overrides. Spawn, root, Workflow, and fixed-system paths reuse
this resolver. Stale remembered models are retained on disk but are eligible
only when they exact-match the current provider catalog/router.

Do not fingerprint the full global map. Spawn and Workflow admission compute a
canonical digest only for referenced Agent ids whose remembered value can
change that request. An unrelated Agent preference must not invalidate an
idempotent admission.

### App-owned durable control

`hya-app` owns `PersistentAgentModelControl`:

- startup claims the runtime owner, lists stored rows, publishes the initial
  immutable map, and fails with context on migration/query corruption;
- `list(binding)` exact-projects every `AgentCatalog::all()` row, including
  hidden system Agents, against one bound catalog/provider snapshot;
- `set(binding, agent, Some(model))` exact-resolves both identities and rejects
  explicitly configured or unserved targets;
- `set(binding, agent, None)` clears idempotently, including dormant/stale rows;
- one async mutex serializes commit/publication for attached clients;
- SQLite commits first; only success performs infallible in-memory publication,
  so a failed store mutation leaves the live snapshot unchanged.

`BuiltSessionEngine` exposes the same control handle used by backend execution
and the server adapter. Filesystem/SQLite policy stays out of `hya-core`; HTTP
types stay out of `hya-app`.

## Runtime application

### Root Session creation

Resolve the Agent default before writing `SessionCreated`. Apply the same
bind-once helper to native, legacy, and V2 server creation plus backend
exec/RPC/goal/Workflow CLI entry points. An explicit request/CLI model or
category remains higher and is never persisted. Forked, synchronized,
recovered, resumed, and already-created Sessions retain their recorded models;
preference lookup must not rewrite replay.

### Spawn admission

`AdmissionResolutionContext` captures one preference snapshot. For each target,
apply its eligible remembered model to the base `AgentSpec` immediately before
the existing `apply_spawn_model_policy` call. Existing Agent, inline, and spawn
category/direct precedence then overwrites it naturally.

Include only relevant target preferences in the durable admission binding
fingerprint. Accepted work resolves from the captured snapshot, never a later
live map. A running resident keeps the model captured at activation; a change
affects later activations only.

### Workflow execution

Capture one preference snapshot before preflight. Worker and verifier resolution
applies eligible remembered defaults before existing Agent policy and before an
optional explicit Stage route. Include only referenced, semantically eligible
preferences in the Workflow request hash. Do not change Workflow authoring
syntax, compiled plans, Stage route Events, reasoning, or fallback behavior.

### Fixed system Agents

`title`, `summary`, and `compaction` bind their fixed catalog definition and
preference snapshot through the same resolver. Direct/category system-Agent
policy suppresses memory.

Resolve the `compaction` model before choosing the existing provider-native or
local fallback branch. Use that resolved model for both `compact_if_supported`
and the local summarizer request. Keep the active Session model for normal turns
and existing compaction-threshold calculations. Preference state changes the
model, not the native-versus-local mechanism.

## Server control seam

Add a dependency-inverted `AgentModelControl` port in `hya-server`, following
the Workflow/MCP control pattern. It exposes `list(binding)` and async
`set(binding, agent, Option<ModelIdentity>)`; the empty implementation advertises
unavailable and rejects mutation. `AppState` installs the app-owned adapter.

### Dedicated state contract

Do not overload existing `/agent` or `/api/agent` model fields. Add a dedicated
normalized row to `/tui/bootstrap.agentModels[]` and `GET /tui/agent-models`:

```json
{
  "agentID": "general",
  "description": "General-purpose agent",
  "mode": "subagent",
  "hidden": false,
  "settable": true,
  "configured": false,
  "preference": {"providerID": "openai", "modelID": "gpt-5.6-sol"},
  "preferenceAvailable": true,
  "effective": {
    "providerID": "openai",
    "modelID": "gpt-5.6-sol",
    "source": "remembered"
  }
}
```

`preference` is null when absent. A retained stale row has
`preferenceAvailable: false` and falls through to the existing effective model.
`configured` is true for direct model or category policy; such a row is visible
but `settable: false`. `effective.source` is one of `configured`, `remembered`,
or `default`. Old clients ignore the additive bootstrap field.

### Mutation route

Add `PUT /tui/agent-models/:agent_id` with the current directory header:

```json
{"preference":{"providerID":"openai","modelID":"gpt-5.6-sol"}}
```

`{"preference": null}` clears. The response is the normalized Agent row only
after durable commit and live publication succeed. Before mutation, exact-resolve
the stable Agent id against one binding, reject set for direct/category policy,
and exact-match the model against the current backend provider catalog.

Use existing bounded structured API errors:

- `400`: malformed identity/body or model absent from the exact catalog;
- `404`: unknown Agent id;
- `409`: preference set requested for an explicitly configured Agent;
- `503`: control unavailable or owner/store mutation failure;
- `200`: commit and publication completed.

`/tui/bootstrap.capabilities.agentModelPreferences` is true only when the real
control is installed. `GET /tui/agent-models` refreshes the same normalized
state for long-running/attached clients; no generated SDK change is required.

## TUI module and interaction

### Decoding and synchronized state

Add one hya-owned decoder for `agentModels` rows and capability metadata. It
strictly validates booleans, source tags, modes, and provider/model identities.
Unknown or malformed rows fail closed; a remembered model is active only when
it is also present in the exact decoded provider catalog. Components consume
only normalized rows.

Sync/LocalProvider owns refresh, targeted current/effective lookup, set, and
clear through the existing SDK client's generic `fetch` and directory header.
Do not add a client, component-local server-state store, or frontend persistence
file. PUT success replaces the synchronized row. Failure preserves prior state,
keeps the dialog usable, and shows the backend's bounded contextual message.

Session hydration and CLI seeds remain current-run state and do not call the
preference route. Normal `/models`, recent, and favorite TUI actions call the
same targeted mutation only for an eligible current Agent; old backends retain
current-run switching but do not claim persistence.

### Dedicated all-Agent flow

Register `agent.model.list` with slash command `/agent-models` and title
`Configure agent models`. Refresh the dedicated list, then show all catalog
rows grouped as Main, Subagent, and System without changing
`local.agent.current()`.

Configured rows remain visible and disabled with `Configured by Agent policy`.
Eligible rows show their effective model; stale retained preferences have a
text status. Selecting an eligible row reuses `DialogModel` with an explicit
target Agent id and title `Select model for <agent>`. It marks the target's
effective row and closes only after successful persistence. The dedicated flow
stores only the base model and therefore does not open the reasoning-variant
picker.

The normal `/agents` selector and Tab cycling remain role-`main` only. Normal
`/models` still targets the current main Agent and retains its existing
current-run variant behavior.

## Compatibility and failure matrix

| Condition | Behavior |
| --- | --- |
| No preference row | Existing model resolution unchanged |
| Valid preference, no Agent model/category | Preference becomes low-precedence Agent default |
| Direct model or category configured | Configuration suppresses memory; row may remain dormant; PUT set returns 409 |
| Request/spawn/CLI model or category supplied | Invocation override wins and is not persisted |
| Explicit Workflow Stage route | Stage route wins; DSL/Event output unchanged |
| Hidden `compaction` preference | Both native and local compaction requests use it; branch choice remains unchanged |
| Stale provider/model row | Row retained and reported stale; execution falls through safely |
| Missing/malformed new wire fields | New TUI fails closed and does not restore them |
| Store/owner mutation fails | HTTP 503; memory retains prior state; TUI keeps current UI state and shows error |
| Backend lacks capability | New command hidden; current-run `/models` remains functional |
| Alternate or in-memory database | Preferences follow that backend database; in-memory state is not restart-durable |
| Existing database/client | Additive migration/bootstrap field; old client ignores it; Event bytes unchanged |
| Catalog Agent removed/re-added | Row is dormant while absent and reactivates only for the same unconfigured stable id |
| In-flight binding/resident | Keeps captured preference/model; later work sees the new committed snapshot |

## Public test seams

1. `SessionStore` owner-fenced CRUD, per-Agent isolation, concurrency, and
   file-backed reopen.
2. Core immutable snapshots plus one pure resolver for stale, configured,
   reasoning-only, and every existing precedence layer.
3. App list/set control, failed-commit publication safety, root Session defaults,
   spawn capture, relevant-only admission fingerprints, and resident pinning.
4. Workflow no-route inheritance versus explicit Stage routes and relevant-only
   request hashes.
5. Provider request capture for `title`, `summary`, and both native/local
   `compaction` paths.
6. Public bootstrap/list/PUT HTTP behavior for all catalog Agent classes,
   structured errors, exact catalog validation, and restart.
7. Hya-owned TypeScript decoding/state plus targeted model dialog behavior at
   80 columns, including stale/configured/old-backend/error cases.
8. Actual backend/TUI proof: configure independent main, subagent, and hidden
   Agent models, restart the same database, observe restored rows, and verify a
   deterministic subagent provider request uses its remembered model.

These seams are the proposed TDD contract requiring final user approval before
implementation.

## Rollback

Code rollback can stop reading/writing the additive table and ignore the extra wire fields. Migration rollback is not required: the unused table is inert, old binaries ignore it, and deleting user preference rows during downgrade would be destructive. Session Events and prepared Agent/Workflow bundles are unchanged.
