# Fix ignored Agent model selections

## Goal

Make deliberate model choices control the next real provider request for the
selected Agent instead of every Agent continuing to use the process default.

## Background

The installed `0.36.10` UI accepts normal and targeted Agent model selections,
but the user observes that every Agent request still uses the configured process
default, currently `claude-fable-5`. The previous verification proved picker and
database state, not the model identity sent to a provider.

## Requirements

- Treat the installed behavior reported by the user as ground truth. Do not
  substitute picker state or a persisted preference row for execution proof.
- A deliberate selection through the normal model picker must affect the active
  Agent's next provider request, including an already-open Session.
- A deliberate selection through the targeted Agent model picker must affect
  that target Agent's next root, subagent, Workflow, or system-Agent request as
  applicable; it must not alter another Agent.
- The selected provider/model identity must remain effective after backend/TUI
  restart when the Agent has no direct or category model policy.
- Explicit Agent policy and request, CLI, spawn, or Workflow Stage overrides
  remain higher precedence and are never written as remembered preferences.
- With no eligible remembered selection, the current default path remains
  unchanged.
- TUI selected state, `/tui/agent-models` effective state, Session admission,
  and provider request identity must agree on the same base model.
- Tests and diagnostic output must use fake providers and must not expose keys,
  authorization headers, or user configuration secrets.

## Acceptance Criteria

- [x] A deterministic test fails on the installed regression because the next
  captured provider request contains the default model instead of the selected
  model.
- [x] After a normal model selection in an open Session, its next provider
  request contains the selected model exactly.
- [x] After a targeted selection for Agent B, Agent B's next provider request
  contains its selected model while Agent A remains unchanged.
- [x] A new Session and a restarted backend use the remembered model for an
  unconfigured Agent.
- [x] Explicit Agent and request-scoped routes still win over remembered state.
- [x] Picker/API state and captured provider-request identity agree after set,
  clear, stale fallback, and restart.
- [x] Focused regression tests, touched-area gates, and an actual installed TUI
  smoke all pass.

## Out of Scope

- New model picker commands or catalog discovery behavior.
- Changes to provider authentication, secrets, reasoning variants, or Workflow
  model-route syntax.
- Making explicitly configured Agent rows settable.
