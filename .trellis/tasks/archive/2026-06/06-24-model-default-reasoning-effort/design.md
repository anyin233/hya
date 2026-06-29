# Design: model-specific default reasoning effort

## Summary

Add one canonical reasoning-default resolver and use it to make native TUI model selection choose a model-specific default effort by precedence:

1. explicit agent/profile/config reasoning, including explicit `off` / `none`,
2. last-used effort for the exact provider/model,
3. highest supported effort advertised by that model.

The change should keep provider request encoding unchanged. The resolver produces the `ReasoningEffort` already consumed by `CompletionRequest.reasoning`; existing OpenAI, Anthropic, and Google encoders remain responsible for provider-specific labels and budgets.

## Scope and boundary decisions

### Native TUI is the first-class last-used surface

The user-facing requirement is about interactive model selection and `/think` defaults. Native TUI already owns user-local history through `HistoryStore`, so it is the correct v1 place for per-model last-used persistence.

Persist last-used reasoning as user preference state, not event-sourced session data:

- The value changes across sessions and should influence future sessions.
- Replaying an old session must not change because a later session changed the preference.
- `SessionStore` should remain the event log/projection store, not a mutable preference database.

### OpenCode compatibility stays explicit/config-driven in v1

OpenCode agent files and inline agent config already resolve explicit reasoning through `crates/hya-server/src/opencode/reasoning_options.rs::resolve_reasoning`. Do not introduce a dependency from `hya-server` to `hya-cli::HistoryStore` and do not duplicate preference-file I/O in `hya-server`.

OpenCode can reuse the pure resolver for explicit/config parsing where it helps, but last-used defaults are native TUI-only unless a later task extracts shared preference storage into a non-CLI crate.

This resolves the planner conflict as follows:

- Conservative planner: put the resolver in a shared crate and keep OpenCode no-signal behavior stable.
- Edge-case planner: key last-used by provider/model and preserve explicit `Off`.
- Chosen v1: shared pure resolver, native TUI last-used keyed by provider/model, OpenCode behavior unchanged unless explicit config supplies reasoning.

## Resolver contract

Add a pure helper next to `ReasoningEffort` in `crates/hya-provider/src/lib.rs` because both native CLI/TUI and OpenCode server already depend on `hya-provider`.

Proposed signature:

```rust
#[must_use]
pub fn resolve_default_reasoning(
    explicit: Option<ReasoningEffort>,
    last_used: Option<ReasoningEffort>,
    supported: &[String],
) -> Option<ReasoningEffort>
```

Rules:

- `Some(explicit)` returns immediately, including `Some(ReasoningEffort::Off)`.
- `last_used` is used only when it is `Off` or appears in parsed `supported` variants.
- `Off` is always valid as a user preference even though provider catalogs do not advertise an `off` variant.
- Unknown `supported` strings are ignored.
- Highest fallback is the maximum parsed supported variant by `ReasoningEffort` ordering.
- Empty/no parseable `supported` returns `None`, not `Off`, so models without reasoning support do not display a misleading default.

Rationale:

- Keeping this helper pure makes precedence easy to unit-test.
- Returning `None` for unsupported models preserves the existing provider-encoder contract: absence means no reasoning request field.
- Preserving `Off` before provider encoding avoids losing the difference between “user disabled reasoning” and “no default is available.”

## Model catalog flow

Current facts:

- `ProviderKind::reasoning_variants` advertises provider-family variants.
- `HttpProvider::catalog` writes those variants into `ProviderModel.reasoning_variants`.
- `ProviderRouter::catalog` exposes `ProviderModel` values.
- `hya_app::config::ModelEntry` currently stores only `{ id, provider }`.

Design:

1. Extend `ModelEntry` with `reasoning_variants: Vec<String>`.
2. While loading config, create each `ModelEntry` from the parsed provider kind, not from an extra network call:
   - `ProviderKindConfig` is converted into `ProviderKind` during parsing.
   - `ProviderKind::reasoning_variants()` is deterministic and already matches `HttpProvider::catalog`.
   - If the route ever has `reasoning_request == false`, the provider catalog remains the source of truth for runtime requests; this task only needs configured HTTP providers.
3. Keep existing sorting/dedup behavior in `Controller::with_models_and_sessions`.

This avoids needing to build the router first and re-query `router.catalog()` only to reconstruct metadata already known during config parsing.

## Last-used persistence

Add a small JSON preference map under the existing `HistoryStore` root:

```text
~/.hya/history/model_reasoning.json
```

Structure:

```json
{
  "openai\u0000gpt-5.5": "xhigh",
  "anthropic\u0000claude-sonnet-4-6": "none"
}
```

Implementation details:

- Use an internal `BTreeMap<String, String>` for stable JSON output.
- Use a delimiter that cannot appear accidentally in provider/model display text, or a small serializable key struct if that reads better in implementation.
- Public methods should use provider and model separately:

```rust
pub fn record_model_reasoning(
    &self,
    provider: &str,
    model: &str,
    effort: ReasoningEffort,
) -> anyhow::Result<()>;

pub fn last_model_reasoning(
    &self,
    provider: &str,
    model: &str,
) -> anyhow::Result<Option<ReasoningEffort>>;
```

- Persist explicit `Off` as `none` using `ReasoningEffort::as_str()`.
- Ignore corrupt JSON, unknown effort strings, and missing files by returning `Ok(None)` for reads and overwriting with a valid map on the next successful write.
- Use the existing `anyhow::Context` style; do not introduce a new dependency.
- Last-writer-wins is acceptable for concurrent TUI processes. Atomic rename can be used, but locking is not required.

Session metadata should remain backward compatible. If resume needs to restore a per-session reasoning value, add an optional field to `SessionMeta`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub reasoning_effort: Option<String>
```

The meta field is session snapshot state; the model map is cross-session preference state.

## Native TUI data flow

### Startup and new session

1. Build `RunOptions.models: Vec<ModelEntry>` with reasoning variants.
2. Before constructing `AppState`, resolve the active agent reasoning:
   - If `agent.reasoning` is `Some`, keep it.
   - Else lookup last-used by the active `ModelEntry.provider` and `ModelEntry.id`.
   - Else choose highest supported variant.
3. Assign the resolved value to `agent.reasoning`.
4. Set `AppState.reasoning_effort` from the resolved value (`None` stays hidden/unset).
5. `TuiEffect::NewSession` reuses the current agent/model and therefore keeps the current resolved reasoning for that new session.

### Model switch

`TuiEffect::SelectModel` currently carries only the model id. To safely key preferences by provider/model and handle duplicate model ids across providers, update the effect to carry the selected `ModelEntry` or `{ provider, id }`.

On model switch:

1. Update `controller.app.model` and `agent.model` to the selected model id as today.
2. Call `engine.switch_model(session, ModelRef::new(id))` as today.
3. Resolve reasoning for the selected provider/model using explicit agent reasoning only if it came from active agent/profile config; user-selected per-model values should come through the history map.
4. Update `agent.reasoning` and `controller.app.reasoning_effort`.
5. Inject a concise system message only if the current code path already reports state changes; do not add noisy messages for every automatic fallback.

### `/think` command and dialog

`Controller::open_think_dialog` and `DialogMode::Think` currently hardcode `off|low|medium|high` twice. Replace both with choices derived from the active model:

- Always include `off` first.
- Append parsed/supported model variants in provider-advertised order.
- Mark the current value as `current`.
- If the model has no supported variants, show only `off` with detail `reasoning unavailable` or return a system message that reasoning is unavailable.

Direct `/think <level>` should use the same active-model choices. Unsupported levels return a message like:

```text
unsupported thinking effort 'max' for gpt-5.5 (available: off|minimal|low|medium|high|xhigh)
```

When the user selects a valid level:

- `off` sets `agent.reasoning = Some(ReasoningEffort::Off)` long enough to preserve explicitness in state and persistence.
- Provider request construction must still omit reasoning for `Off` through existing provider encoders.
- Record the selected effort in `HistoryStore::record_model_reasoning(provider, model, effort)`.
- Update `AppState.reasoning_effort` to `Some("none")` or `None` only according to the existing display decision. Prefer `Some("none")` if it makes the sidebar explicitly show that reasoning is disabled.

### Resume

On `TuiEffect::ResumeSession`:

1. Load `SessionMeta` as today.
2. If `meta.reasoning_effort` parses, restore that for the session.
3. Else resolve from last-used/highest for the resumed model.
4. Update `agent.reasoning` and `AppState.reasoning_effort`.

This keeps old metadata valid and gives new metadata a stable session-level snapshot once implemented.

### Custom command model override

`TuiEffect::SubmitConfigured { model: Some(..), .. }` changes `agent.model` for a prompt. The implementation should resolve reasoning for that temporary model before spawning the turn, otherwise a command can send the previous model's reasoning effort to the overridden model.

## OpenCode resolver boundary

Do not add last-used preference persistence to `crates/hya-server` in this task.

Allowed server-side work:

- Refactor explicit reasoning parsing to call shared helper where it does not change behavior.
- Keep `empty_inputs_keep_reasoning_unset` passing.
- Keep disabled variant behavior unchanged.

Not in v1:

- No server read/write of `~/.hya/history/model_reasoning.json`.
- No new OpenCode config key unless a future product decision asks for OpenCode defaults to mirror TUI defaults.

## Compatibility and migration

- Existing session `meta.json` files remain valid via `#[serde(default)]` optional fields.
- Missing `model_reasoning.json` means there are no last-used defaults yet.
- Corrupt `model_reasoning.json` should not prevent the TUI from starting; ignore it and overwrite on next valid selection.
- Existing provider encoders remain unchanged.
- Existing tests that assert OpenCode no-signal behavior remains `None` should keep passing.

## Manual QA surface

The real surface is the native TUI. After implementation and automated tests:

1. Launch `hya` in tmux with a temporary `HYA_HISTORY_DIR` and configured fake/dev provider models if available.
2. Open `/think` and verify the dialog shows provider-specific options, including `xhigh` or `max` for a model that supports it.
3. Select `/think off`, exit, relaunch with the same history directory, and verify the model does not fall back to highest effort.
4. Select a non-off effort, switch away and back to the same provider/model, and verify the last-used effort returns.
5. Switch to another provider with the same model id and verify it does not inherit the first provider's preference.
6. Try an unsupported direct command such as `/think max` on an OpenAI-compatible model and verify a clear system message.

## Out of scope

- Adding new reasoning effort variants.
- Changing Anthropic/OpenAI/Google request encoding or budget mappings.
- Syncing reasoning preferences across machines.
- Adding OpenCode last-used preference persistence.
- Event-sourcing reasoning changes.
- Redesigning transcript rendering for reasoning content.
