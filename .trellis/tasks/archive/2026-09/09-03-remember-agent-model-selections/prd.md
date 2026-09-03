# Remember per-agent model selections

## Goal

Persist the last selected model for each agent unless configuration explicitly pins that agent model, and expose per-agent selection in the TUI.

## Background

The TUI can select models at runtime, but an agent without an explicit model in
user configuration does not reliably resume the model that the user selected
for that agent in the previous Hya run.

## Requirements

- Preserve explicit per-agent model routing configuration, including a direct
  model or model category, as the highest-precedence startup source. Remembered
  TUI selections must not replace or rewrite explicit configuration.
- For each agent without an explicit configured model, remember the model most
  recently selected for that agent through the TUI and restore it in a later Hya
  run.
- Let the user select and remember a model for every catalog agent, including
  role-`main` agents and subagents, through a dedicated `Agent models` flow that
  selects the target agent and then reuses the existing model picker. The normal
  `/agents` picker remains limited to root-selectable role-`main` agents.
- Keep remembered selections separate per agent. Selecting a model for one agent
  must not change another agent's effective or remembered model.
- Use a remembered selection as that unconfigured agent's runtime default for
  later root sessions and subagent invocations. A request-scoped explicit model
  or category remains higher precedence and is never persisted as the agent's
  remembered default.
- Apply the same remembered default to unconfigured hidden system-agent model
  calls (`title`, `summary`, and both provider-native and local `compaction`
  paths) and to Workflow members when a Stage has no explicit model route.
  Existing explicit Workflow Stage routes remain authoritative and unchanged.
- Persist a deliberate TUI selection immediately through the backend-owned state
  boundary. A clean exit must not be required, and an attached/remote TUI must
  update the backend instance rather than a client-local file.
- Persist only stable model identity data. Do not persist credentials, request
  data, transient provider responses, or session transcripts as part of this
  feature.
- Preserve existing behavior for agents that have neither an explicit model nor
  a remembered selection by using the current default-resolution path.

## Acceptance Criteria

- [x] A TUI model selection for agent A is restored for agent A after Hya exits
  and starts again when agent A has no explicit configured model.
- [x] Independent TUI model selections for agents A and B survive restart and do
  not overwrite each other.
- [x] An explicit configured model for an agent wins over any remembered TUI
  selection for the same agent.
- [x] Spawning an unconfigured subagent without a request-scoped model or
  category uses that subagent's remembered model; an explicit spawn override
  still wins.
- [x] Unconfigured hidden system-agent calls use their remembered models for
  `title`, `summary`, and both `compaction` paths; configured system-agent policy
  and the existing choice between native and local compaction remain authoritative.
- [x] A Workflow member without an explicit Stage model route can inherit its
  Agent's remembered default, while an explicit Stage route still wins and its
  DSL/Event contracts do not change.
- [x] Removing or omitting a remembered selection leaves the current default
  model-resolution behavior unchanged.
- [x] The TUI identifies which agent a model selection changes and shows that
  agent's effective selection when the model picker opens.
- [x] The dedicated `Agent models` flow lists every catalog agent, identifies the
  target in the model picker, and does not make subagents root-selectable.
- [x] Focused frontend and backend tests cover persistence, per-agent isolation,
  precedence, restart restoration, and stale remembered data.

## Out of Scope

- Provider credentials or authentication persistence.
- Changes to Workflow Stage model-route DSL, route outcomes, or fallback policy.
- New provider discovery or model fallback policy.
- A second model catalog, provider picker, or model-selection implementation.

## Resolved Product Decisions

- “Every agent” includes role-`main`, ordinary subagent, and hidden system-agent
  catalog rows.
- All-agent configuration uses a dedicated `Agent models` target picker followed
  by the existing model picker.
- Remembered model state belongs to the selected backend Session database: the
  normal persistent database survives restart, an explicit alternate `--db`
  has independent preferences, and intentional in-memory mode is non-durable.
- A deliberate TUI model choice persists the base provider/model identity only;
  reasoning variants, CLI overrides, Session hydration, and Workflow Stage
  routes remain invocation state and are not written as Agent preferences.
