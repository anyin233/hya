# Subagent Bundles

## Introduction

A subagent bundle is an `AgentSetBundle` used as a reusable set of spawnable
agent definitions. It packages workers, their prompts, model policies, resource
views, and explicit `can_spawn` edges without owning a Workflow or
pre-allocating runtime sessions and channels.

`hya/subagents` is a first-party reference package and is available without
installation. It contains one worker identity, `hya-worker`, so installing it
does not duplicate the trusted `hya/core-agents` preset. It remains a public,
clamped bundle: it cannot claim reserved system ids or acquire the preset's full
host tool plane.

Every spawned agent is a resident actor (there is no transient/resident
choice since 0.41.0; `spawn_lifecycle` is a removed manifest key). A worker
runs its directive, `report`s its result — which archives it with a state
handoff — and can be woken again, under the same handle and session, by
follow-up mail from its parent. The earlier `hya-transient-worker` and
`hya-resident-worker` identities were replaced by `hya-worker`.

## Usage

The default catalog already exposes the worker. An authorized caller spawns it
with the `task` tool:

```json
{"description":"Review parser","subagent_type":"hya-worker","prompt":"Review the parser change and report concrete defects."}
```

`task` returns the worker's handle immediately; the result arrives as its
report mail; block on it with `wait` instead of polling. Send follow-up work
with `send` to the handle; that mail wakes an
archived worker. Stop a worker you no longer need with `archive` (it can still
be woken later). The caller's roster and `can_spawn` closure must authorize the selected stable id.

Operators may install a public bundle with identity `hya/subagents` (or the
same namespace) as a local override. `hya bundle list` and `bundle
info hya/subagents` then show the installed definition. Uninstalling that
override restores the first-party package on the next root binding:

```sh
hya bundle install ./subagents-override.hyabundle
hya bundle info hya/subagents
hya bundle remove hya/subagents
```

The caller's captured `TurnBinding` resolves the selected definition and its
resource policy before admission. The worker registers a durable child
identity, processes mailbox work under actor-claim fencing, and recovers from
the event log; the bundle does not create a second replay or mailbox authority.

## Interface definitions

The package uses the standard `AgentSetBundle` source contract. Each `agents[]`
entry exposes `id: string`, `role: main | subagent`, optional `description` and
`prompt`, optional `model_policy`, a narrowing `resource_view`, and sorted
`can_spawn: string[]` references. A `spawn_lifecycle` key is rejected with
`RemovedManifestKey`.

| Agent id | Role | Prompt | Behavior |
| --- | --- | --- | --- |
| `hya-worker` | `subagent` | `prompts/worker.md` | Resident worker: runs the directive, reports, is archived, and is woken by follow-up mail. |

Spawn retains the parent's immutable runtime binding, authorized roster,
resource policy, guidance, and optional sidecar factory. Session ids, member
ids, handles, actor claims, and channel ids are minted by the engine and
persisted as events. They are never declared by this bundle.
