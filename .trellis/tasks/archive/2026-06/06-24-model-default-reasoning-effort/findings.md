# Findings: model-specific default reasoning effort

## Code-backed facts

- `ReasoningEffort` lives in `crates/hya-provider/src/lib.rs` and orders efforts as `Off < Minimal < Low < Medium < High < XHigh < Max`. It already parses `off|none|minimal|low|medium|med|high|xhigh|max` and encodes provider labels/budgets.
- Provider support is exposed by `ProviderKind::reasoning_variants` in `crates/hya-provider/src/http.rs`:
  - Anthropic: `low`, `medium`, `high`, `max`
  - OpenAI-compatible: `minimal`, `low`, `medium`, `high`, `xhigh`
  - Google: `high`, `max`
- `HttpProvider::catalog` copies those variants into `ProviderModel.reasoning_variants`, then `ProviderRouter::catalog` exposes them through `SessionEngine::provider_catalog`.
- Native TUI model selection receives `Vec<ModelEntry>` from `hya_app::config`; `ModelEntry` currently stores only `{ id, provider }`, so the TUI cannot render model-specific `/think` variants without extending this type or deriving a second catalog input.
- Native TUI `/think` is hardcoded to `off`, `low`, `medium`, `high` in `Controller::open_think_dialog`, and `TuiEffect::SelectReasoning` only returns a string.
- Native TUI `apply_reasoning` currently treats `off|none` as `agent.reasoning = None`, which loses the distinction between “explicit off” and “no preference/default available.”
- `AgentSpec.reasoning` is copied into `CompletionRequest.reasoning` in `crates/hya-core/src/engine/turn/messages.rs`; provider encoders already omit reasoning request fields for `None` and for provider-specific off/no-budget cases.
- `HistoryStore` under `crates/hya-cli/src/tui/history.rs` is the existing local TUI persistence mechanism. It writes JSON session history under `HYA_HISTORY_DIR` or `~/.hya/history`. SQLite `SessionStore` is for event logs/projections and saved permissions, not user preferences.
- The OpenCode-compatible path already parses agent-file/inline-agent reasoning through `AgentEntry.variant` and `AgentEntry.options`, using `resolve_reasoning` and `apply_agent_entry` before request construction.

## Planning implications

- Put the default selection algorithm in a shared Rust module rather than baking it into the TUI event loop. The resolver should take explicit agent effort, optional last-used effort, and supported variants.
- Native TUI needs a way to preserve explicit `Off` in preference state while still passing `None` to provider encoders.
- The most local persistence fit is a small TUI/user state file adjacent to `HistoryStore` rather than adding a new event kind or SQLite table. Session events should remain about replayable conversation state.
- `/think` UI should derive its choices from the active model's supported variants plus `off`; direct `/think <level>` should reject unsupported levels for the active model instead of accepting arbitrary known effort names.
- Model switching, command-configured model override, new session, and resume should all re-resolve the active reasoning default for the effective model.

## Known unrelated issue

- `cargo test --workspace` currently fails unrelated `hya-server` skill-order assertions in this environment because global `brainstorming` appears before test-local skills. Treat this as pre-existing unless the current work touches those tests.
