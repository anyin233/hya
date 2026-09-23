# Subagent Bundles

## Introduction

A subagent bundle is an `AgentSetBundle` used as a reusable set of spawnable
agent definitions. It packages transient and resident workers, their prompts,
model policies, resource views, and explicit `can_spawn` edges without owning a
Workflow or pre-allocating runtime sessions and channels.

`hya/subagents` is a first-party reference package and is available
without installation. It contains unique
`hya-transient-worker` and `hya-resident-worker` identities, so installing it
does not duplicate the trusted `hya/core-agents` preset. It remains a public,
clamped bundle: it cannot claim reserved system ids or acquire the preset's full
host tool plane.

## Usage

The default catalog already exposes both workers. An authorized caller can use
the `task` tool with `subagent_type: "hya-transient-worker"` for one bounded task:

```json
{"description":"Review parser","subagent_type":"hya-transient-worker","prompt":"Review the parser change and report concrete defects."}
```

Use `subagent_type: "hya-resident-worker"` when the worker must accept later mailbox
directives. The caller's roster and `can_spawn` closure must authorize the
selected stable id.

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
resource policy before admission. Transient workers run one child turn and
report a terminal result. Resident workers register a durable child identity,
process mailbox work under actor-claim fencing, and recover from the event log;
the bundle does not create a second replay or mailbox authority.

## Interface definitions

The package uses the standard `AgentSetBundle` source contract. Each `agents[]`
entry exposes `id: string`, `role: main | subagent`, optional `description` and
`prompt`, optional `model_policy`, `spawn_lifecycle: transient | resident`, a
narrowing `resource_view`, and sorted `can_spawn: string[]` references.

`spawn_lifecycle` is consulted only after catalog resolution and admission:
`transient` selects the one-shot team runner; `resident` selects the durable
resident supervisor. Both paths retain the parent's immutable runtime binding,
authorized roster, resource policy, guidance, and optional sidecar factory.
Session ids, member ids, handles, actor claims, and channel ids are minted by
the engine and persisted as events. They are never declared by this bundle.
