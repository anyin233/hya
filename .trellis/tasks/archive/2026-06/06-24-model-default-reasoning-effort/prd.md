# Model-specific default reasoning effort

## Goal

When a user selects or uses a model that supports reasoning, hya should choose a sensible default reasoning effort without requiring the user to manually run `/think` every time. The default should be model-specific and follow this precedence:

1. Reasoning effort explicitly set by the active agent file / agent configuration.
2. The last reasoning effort the user selected for that model.
3. The highest reasoning effort supported by that model.

This should make reasoning-capable models feel ready by default while preserving explicit agent-level choices.

## Confirmed Facts

- `AgentSpec.reasoning` is the value sent into provider completion requests through `crates/hya-core/src/engine/turn/messages.rs`.
- The TUI `/think` flow currently updates only the active in-memory `AgentSpec.reasoning` and `AppState.reasoning_effort` in `crates/hya-cli/src/tui.rs::apply_reasoning`.
- `Controller::open_think_dialog` currently offers only `off`, `low`, `medium`, and `high`, independent of the selected model's supported reasoning variants.
- OpenCode-compatible reasoning option resolution already exists in `crates/hya-server/src/opencode/reasoning_options.rs::resolve_reasoning`, but it returns `None` when no variant or agent option signals reasoning.
- Provider model catalog data can expose supported reasoning variants through `hya_provider::ProviderModel.reasoning_variants`; provider families currently advertise:
  - Anthropic: `low`, `medium`, `high`, `max`
  - OpenAI-compatible: `minimal`, `low`, `medium`, `high`, `xhigh`
  - Google: `high`, `max`
- `crates/hya-app/src/config.rs::ModelEntry` currently stores only model id and provider id, so the main TUI model list does not carry reasoning variant metadata today.
- No existing code path was found that persists “last selected reasoning effort per model.”
- `ProviderKind::reasoning_variants` is the current source of provider-family supported effort names, and `ProviderRouter::catalog` exposes those names through `SessionEngine::provider_catalog`.
- `HistoryStore` is the existing native TUI local persistence mechanism. It writes JSON session history under `HYA_HISTORY_DIR` or `~/.hya/history`; the SQLite `SessionStore` persists event logs and projections, not user preferences.
- The OpenCode-compatible surface already supports agent-file and inline-agent reasoning through `AgentEntry.variant`, `AgentEntry.options`, and `apply_agent_entry`; native TUI profiles are built-in only and do not currently read agent files.
- Session model changes are event-sourced with `ModelSwitched`; reasoning effort changes are not currently event-sourced and are only in the live `AgentSpec` / `AppState`.
- Provider request encoders already treat `ReasoningEffort::Off` as “emit no provider reasoning field,” so explicit-off state must be distinguishable before request encoding if it participates in default resolution.

## Requirements

- Resolve a default reasoning effort per model using this precedence: agentfile / explicit agent config, then last-used effort for that model, then highest supported effort.
- Preserve explicit `off` / `none` as a real choice that disables reasoning instead of falling through to the highest supported effort.
- Store last-used reasoning effort keyed by model identity so switching back to a model restores that model's previous choice.
- Show the currently resolved reasoning effort in the TUI status/sidebar consistently with existing `think:<effort>` rendering.
- Ensure `/think` choices are compatible with the active model's supported efforts, including `max`, `xhigh`, and `minimal` where supported.
- Keep provider-specific request encoding unchanged: the resolved `ReasoningEffort` should continue flowing through existing provider protocol encoders.
- Avoid introducing a second, divergent reasoning resolver for OpenCode API and native TUI paths unless the design explicitly defines their boundary.

## Acceptance Criteria

- [ ] Given an active agent file/config explicitly sets reasoning, that value is used even when the model has a previous last-used effort or a higher supported effort.
- [ ] Given no active agent reasoning is set and the user previously selected an effort for the current model, that model's last-used effort becomes the default when the model is selected again.
- [ ] Given no agent reasoning and no last-used effort, a reasoning-capable model defaults to its highest supported effort.
- [ ] Given the user selects `off` / `none`, reasoning is disabled for that model and does not immediately fall back to highest effort.
- [ ] `/think` presents and accepts all supported effort levels for the selected model, not just `low|medium|high|off`.
- [ ] A model with no reasoning support leaves reasoning unset and does not show a misleading default effort.
- [ ] Existing provider encoders continue to emit provider-specific request fields only when a resolved effort is present.
- [ ] Tests cover the precedence chain and at least one provider family with a non-`high` highest effort (`max` or `xhigh`).

## Open Question

- Pending design confirmation: store “last used effort per model” as native TUI user preference state rather than session event data, unless the final plan finds a reason OpenCode API clients must share the same preference store.

## Out of Scope

- Changing provider-specific budget mappings or adding new provider reasoning effort names.
- Rendering model reasoning content in the transcript; this task concerns selecting request effort defaults.
- Changing token accounting for reasoning usage.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
