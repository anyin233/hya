# Model-specific default reasoning effort Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `subagent-driven-development` or `executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Do not start until `task.py start` has moved the task to `in_progress`.

**Goal:** Make native TUI reasoning effort default per model with precedence explicit agent/profile config → last-used for exact provider/model → highest supported effort.

**Architecture:** Add a pure resolver in `hya-provider`, carry provider reasoning variants through `hya-app::ModelEntry`, persist native TUI last-used preferences in `HistoryStore`, and update controller/runtime paths to resolve and validate reasoning by the active model. OpenCode explicit/config reasoning remains stable.

**Tech Stack:** Rust 2024 workspace, `cargo`, `serde_json`, existing `anyhow`, ratatui/crossterm TUI controller tests, provider unit tests.

## Global Constraints

- No production Rust code before a failing test has been written and observed failing.
- Library crates deny `unwrap_used` and `expect_used`; keep unwrap/expect in tests only where existing test modules already allow them.
- Preserve event-sourced architecture; do not add reasoning preference defaults to `SessionStore` events.
- Keep `hya-render-tui` as pure rendering/state; terminal I/O and persistence stay in `hya-cli`.
- Do not change provider-specific request encoders unless a test proves a regression in existing behavior.
- Full Rust verification after implementation: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

---

## File map

- Modify `crates/hya-provider/src/lib.rs`: add `resolve_default_reasoning` and resolver unit tests.
- Modify `crates/hya-app/src/config.rs`: add `ModelEntry.reasoning_variants` and config tests for family-specific variants.
- Modify `crates/hya-cli/src/tui/history.rs`: add last-used reasoning preference persistence and optional session meta field.
- Modify `crates/hya-cli/src/tui/controller.rs`: carry provider/model in selection effects and build `/think` choices dynamically.
- Modify `crates/hya-cli/src/tui.rs`: resolve startup/model-switch/resume/custom-command reasoning and persist `/think` selections.
- Modify `crates/hya-cli/src/tui/harness.rs`: teach harness model entries about reasoning variants and update reasoning-effect handling.
- Modify `crates/hya-server/src/opencode/reasoning_options_tests.rs` only if needed to lock no-signal compatibility.
- Optionally modify `crates/hya-server/src/opencode/reasoning_options.rs` only for behavior-preserving reuse of the provider helper.

---

### Task 1: Provider resolver

**Files:**
- Modify: `crates/hya-provider/src/lib.rs`

**Interfaces:**
- Produces: `pub fn resolve_default_reasoning(explicit: Option<ReasoningEffort>, last_used: Option<ReasoningEffort>, supported: &[String]) -> Option<ReasoningEffort>`.
- Consumes: existing `ReasoningEffort::parse` and enum ordering.

- [ ] **Step 1: Write failing resolver tests**

Add tests to `reasoning_effort_tests`:

```rust
#[test]
fn default_reasoning_keeps_explicit_off() {
    let supported = vec!["low".to_string(), "high".to_string()];

    let resolved = super::resolve_default_reasoning(Some(R::Off), Some(R::High), &supported);

    assert_eq!(resolved, Some(R::Off));
}

#[test]
fn default_reasoning_uses_supported_last_used_before_highest() {
    let supported = vec!["minimal".to_string(), "low".to_string(), "xhigh".to_string()];

    let resolved = super::resolve_default_reasoning(None, Some(R::Low), &supported);

    assert_eq!(resolved, Some(R::Low));
}

#[test]
fn default_reasoning_ignores_unsupported_last_used_and_picks_highest() {
    let supported = vec!["low".to_string(), "medium".to_string(), "high".to_string()];

    let resolved = super::resolve_default_reasoning(None, Some(R::XHigh), &supported);

    assert_eq!(resolved, Some(R::High));
}

#[test]
fn default_reasoning_picks_max_for_google_or_anthropic_variants() {
    let supported = vec!["high".to_string(), "max".to_string()];

    let resolved = super::resolve_default_reasoning(None, None, &supported);

    assert_eq!(resolved, Some(R::Max));
}

#[test]
fn default_reasoning_stays_unset_without_reasoning_support() {
    let supported = Vec::new();

    let resolved = super::resolve_default_reasoning(None, None, &supported);

    assert_eq!(resolved, None);
}
```

- [ ] **Step 2: Run red test**

Run:

```bash
cargo test -p hya-provider default_reasoning --lib
```

Expected: compile failure or test failure because `resolve_default_reasoning` does not exist.

- [ ] **Step 3: Implement minimal resolver**

Add the pure helper after the `impl ReasoningEffort` block:

```rust
#[must_use]
pub fn resolve_default_reasoning(
    explicit: Option<ReasoningEffort>,
    last_used: Option<ReasoningEffort>,
    supported: &[String],
) -> Option<ReasoningEffort> {
    if explicit.is_some() {
        return explicit;
    }

    let supported_efforts = supported
        .iter()
        .filter_map(|level| ReasoningEffort::parse(level))
        .collect::<Vec<_>>();

    if let Some(effort) = last_used
        && (effort == ReasoningEffort::Off || supported_efforts.contains(&effort))
    {
        return Some(effort);
    }

    supported_efforts.into_iter().max()
}
```

- [ ] **Step 4: Run green test**

Run:

```bash
cargo test -p hya-provider default_reasoning --lib
```

Expected: tests pass.

---

### Task 2: Model entries carry reasoning variants

**Files:**
- Modify: `crates/hya-app/src/config.rs`
- Modify follow-up compile sites in `crates/hya-cli/src/tui/controller.rs` and test constructors that instantiate `ModelEntry`.

**Interfaces:**
- Produces: `ModelEntry { id, provider, reasoning_variants }`.
- Consumes: `ProviderKind::reasoning_variants()`.

- [ ] **Step 1: Write failing config test**

Add to `crates/hya-app/src/config.rs` tests a unit that exercises a helper or the resolved model construction. If no helper exists yet, first extract a small helper under test:

```rust
#[test]
fn model_entries_include_provider_reasoning_variants() {
    let parsed = parse_providers(FIXTURE).unwrap();

    let entries = model_entries(&parsed);

    let openai = entries.iter().find(|entry| entry.provider == "gw-oai").unwrap();
    assert_eq!(
        openai.reasoning_variants,
        vec!["minimal", "low", "medium", "high", "xhigh"]
    );
    let anthropic = entries.iter().find(|entry| entry.provider == "gw-anth").unwrap();
    assert_eq!(
        anthropic.reasoning_variants,
        vec!["low", "medium", "high", "max"]
    );
}
```

- [ ] **Step 2: Run red test**

Run:

```bash
cargo test -p hya-app model_entries_include_provider_reasoning_variants --lib
```

Expected: compile failure because `ModelEntry.reasoning_variants` or `model_entries` does not exist.

- [ ] **Step 3: Implement model metadata**

Add the field:

```rust
pub struct ModelEntry {
    pub id: String,
    pub provider: String,
    pub reasoning_variants: Vec<String>,
}
```

Add a private helper to avoid duplicate construction:

```rust
fn model_entries(providers: &[ParsedProvider]) -> Vec<ModelEntry> {
    providers
        .iter()
        .flat_map(|provider| {
            let variants = provider.kind.reasoning_variants();
            provider.models.iter().map(move |model| ModelEntry {
                id: model.clone(),
                provider: provider.id.clone(),
                reasoning_variants: variants.clone(),
            })
        })
        .collect()
}
```

Use `model_entries(&parsed)` in `load()` before moving providers into the router, or iterate by reference and clone the fields needed for `HttpProvider::new`.

- [ ] **Step 4: Update compile sites**

Update all test-only `ModelEntry` literals, especially `Controller::with_models`, to include `reasoning_variants: Vec::new()` or a test helper with explicit variants.

- [ ] **Step 5: Run green test**

Run:

```bash
cargo test -p hya-app model_entries_include_provider_reasoning_variants --lib
```

Expected: pass.

---

### Task 3: HistoryStore last-used persistence

**Files:**
- Modify: `crates/hya-cli/src/tui/history.rs`

**Interfaces:**
- Produces: `record_model_reasoning(provider, model, effort)` and `last_model_reasoning(provider, model)`.
- Produces optional `SessionMeta.reasoning_effort` if needed for resume snapshot compatibility.
- Consumes: `ReasoningEffort` from `hya-provider`.

- [ ] **Step 1: Write failing persistence tests**

Add tests under `history.rs`:

```rust
#[test]
fn model_reasoning_is_keyed_by_provider_and_model() {
    let root = temp_root();
    let store = HistoryStore::new(root.clone());

    store
        .record_model_reasoning("openai", "gpt-5.5", hya_provider::ReasoningEffort::XHigh)
        .expect("record openai reasoning");
    store
        .record_model_reasoning("anthropic", "gpt-5.5", hya_provider::ReasoningEffort::Max)
        .expect("record anthropic reasoning");

    let reopened = HistoryStore::new(root);
    assert_eq!(
        reopened.last_model_reasoning("openai", "gpt-5.5").expect("read openai"),
        Some(hya_provider::ReasoningEffort::XHigh)
    );
    assert_eq!(
        reopened.last_model_reasoning("anthropic", "gpt-5.5").expect("read anthropic"),
        Some(hya_provider::ReasoningEffort::Max)
    );
}

#[test]
fn model_reasoning_preserves_explicit_off() {
    let root = temp_root();
    let store = HistoryStore::new(root);

    store
        .record_model_reasoning("openai", "gpt-5.5", hya_provider::ReasoningEffort::Off)
        .expect("record off");

    assert_eq!(
        store.last_model_reasoning("openai", "gpt-5.5").expect("read off"),
        Some(hya_provider::ReasoningEffort::Off)
    );
}
```

- [ ] **Step 2: Run red test**

Run:

```bash
cargo test -p hya-cli model_reasoning_ --lib
```

Expected: compile failure because the methods do not exist.

- [ ] **Step 3: Implement persistence**

Add `use std::collections::BTreeMap;` and `use hya_provider::ReasoningEffort;`.

Implement a private key helper and read/write methods. Keep helpers private unless tests need public access:

```rust
fn model_reasoning_key(provider: &str, model: &str) -> String {
    format!("{provider}\u{0}{model}")
}

fn model_reasoning_path(&self) -> PathBuf {
    self.root.join("model_reasoning.json")
}
```

Read missing/corrupt files as an empty map for writes and as `Ok(None)` for lookups. Write pretty JSON to a temp path and rename it into place.

- [ ] **Step 4: Run green test**

Run:

```bash
cargo test -p hya-cli model_reasoning_ --lib
```

Expected: pass.

---

### Task 4: Dynamic `/think` choices and validation

**Files:**
- Modify: `crates/hya-cli/src/tui/controller.rs`

**Interfaces:**
- Produces: a selected-model lookup that returns provider/id/reasoning variants.
- Changes: `TuiEffect::SelectModel` should include enough provider/model metadata to key preferences exactly.
- Changes: `DialogMode::Think` should map selected dialog item labels instead of indexing a hardcoded array.

- [ ] **Step 1: Write failing controller tests**

Add tests near existing slash/model tests:

```rust
fn model_entry(id: &str, provider: &str, variants: &[&str]) -> ModelEntry {
    ModelEntry {
        id: id.to_string(),
        provider: provider.to_string(),
        reasoning_variants: variants.iter().map(|level| (*level).to_string()).collect(),
    }
}

#[test]
fn think_dialog_uses_active_model_reasoning_variants() {
    let mut controller = Controller::with_models_and_sessions(
        AppState {
            model: "gpt-5.5".to_string(),
            ..AppState::default()
        },
        vec![model_entry("gpt-5.5", "openai", &["minimal", "low", "medium", "high", "xhigh"])],
        Vec::new(),
    );

    type_text(&mut controller, "/think");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

    let labels = controller
        .app
        .dialog
        .as_ref()
        .expect("think dialog")
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["off", "minimal", "low", "medium", "high", "xhigh"]);
}

#[test]
fn think_dialog_selection_returns_selected_dynamic_level() {
    let mut controller = Controller::with_models_and_sessions(
        AppState {
            model: "claude".to_string(),
            ..AppState::default()
        },
        vec![model_entry("claude", "anthropic", &["low", "medium", "high", "max"])],
        Vec::new(),
    );

    type_text(&mut controller, "/think");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);
    for _ in 0..4 {
        assert_eq!(controller.handle_key(key(KeyCode::Down)), TuiEffect::None);
    }

    assert_eq!(
        controller.handle_key(key(KeyCode::Enter)),
        TuiEffect::SelectReasoning("max".to_string())
    );
}
```

- [ ] **Step 2: Run red test**

Run:

```bash
cargo test -p hya-cli think_dialog_ --lib
```

Expected: fail because dialog remains hardcoded and/or `ModelEntry` has no variants until prior tasks are complete.

- [ ] **Step 3: Implement dynamic choices**

Add a helper on `Controller`:

```rust
fn active_model_entry(&self) -> Option<&ModelEntry> {
    self.available_models.iter().find(|entry| entry.id == self.app.model)
}

fn active_reasoning_levels(&self) -> Vec<String> {
    let mut levels = vec!["off".to_string()];
    if let Some(entry) = self.active_model_entry() {
        levels.extend(entry.reasoning_variants.iter().cloned());
    }
    levels
}
```

Use `active_reasoning_levels()` in `open_think_dialog` and in the `DialogMode::Think` Enter branch. Do not keep a second hardcoded level array.

- [ ] **Step 4: Update model selection effect**

Change `TuiEffect::SelectModel(String)` to carry a typed payload such as:

```rust
SelectModel(ModelEntry)
```

or add a small controller-local struct if carrying the app config type through the effect is too broad. Update tests and `tui.rs` match arms accordingly.

- [ ] **Step 5: Run green test**

Run:

```bash
cargo test -p hya-cli think_dialog_ --lib
```

Expected: pass.

---

### Task 5: Runtime resolution and persistence wiring

**Files:**
- Modify: `crates/hya-cli/src/tui.rs`
- Modify: `crates/hya-cli/src/tui/harness.rs`

**Interfaces:**
- Consumes: `resolve_default_reasoning`.
- Consumes: `HistoryStore::last_model_reasoning` and `record_model_reasoning`.
- Produces: runtime helper(s) that resolve by active `ModelEntry`.

- [ ] **Step 1: Write failing runtime/unit tests**

If full async TUI run tests are too heavy, add pure helper tests in `tui.rs` for the runtime resolver. Example helper target:

```rust
fn resolve_runtime_reasoning(
    explicit: Option<ReasoningEffort>,
    last_used: Option<ReasoningEffort>,
    model: Option<&ModelEntry>,
) -> Option<ReasoningEffort>
```

Tests:

```rust
#[test]
fn runtime_reasoning_defaults_to_highest_model_variant() {
    let entry = ModelEntry {
        id: "claude".to_string(),
        provider: "anthropic".to_string(),
        reasoning_variants: vec!["low".to_string(), "medium".to_string(), "high".to_string(), "max".to_string()],
    };

    let resolved = resolve_runtime_reasoning(None, None, Some(&entry));

    assert_eq!(resolved, Some(ReasoningEffort::Max));
}

#[test]
fn runtime_reasoning_preserves_last_used_off() {
    let entry = ModelEntry {
        id: "gpt-5.5".to_string(),
        provider: "openai".to_string(),
        reasoning_variants: vec!["minimal".to_string(), "low".to_string(), "medium".to_string(), "high".to_string(), "xhigh".to_string()],
    };

    let resolved = resolve_runtime_reasoning(None, Some(ReasoningEffort::Off), Some(&entry));

    assert_eq!(resolved, Some(ReasoningEffort::Off));
}
```

- [ ] **Step 2: Run red test**

Run:

```bash
cargo test -p hya-cli runtime_reasoning_ --lib
```

Expected: compile failure because helper does not exist.

- [ ] **Step 3: Implement runtime helper**

Implement helper in `tui.rs` that delegates to `hya_provider::resolve_default_reasoning` with the model entry's variants. Keep it small and testable.

- [ ] **Step 4: Wire startup**

Before `AppState` construction in `run()`, find the current `ModelEntry`, read last-used from history, resolve, assign `agent.reasoning`, and set `AppState.reasoning_effort` from the resolved value.

- [ ] **Step 5: Wire model switch**

In `TuiEffect::SelectModel`, use provider/model metadata from the effect payload, read last-used for that exact pair, resolve, update `agent.reasoning` and `controller.app.reasoning_effort`, then switch the engine model.

- [ ] **Step 6: Wire `/think` selection**

Change `apply_reasoning` to accept active model metadata and the history store:

```rust
fn apply_reasoning(
    agent: &mut AgentSpec,
    app: &mut AppState,
    history: &HistoryStore,
    model: &ModelEntry,
    level: &str,
) -> String
```

Validate `level` against `off + model.reasoning_variants`. On valid selection, set `agent.reasoning`, update `app.reasoning_effort`, and persist with `record_model_reasoning`.

- [ ] **Step 7: Wire resume and custom command override**

On resume, restore optional session meta reasoning if implemented; otherwise resolve for the resumed model. Before `SubmitConfigured` turns with `model: Some`, resolve reasoning for the override model so the outgoing request does not reuse an effort unsupported by the previous model.

- [ ] **Step 8: Update harness**

Update `DummyHarness::new` and `apply_effect` to use `ModelEntry` variants and the same reasoning semantics, or explicitly keep it minimal with `Vec::new()` variants where reasoning is not under test.

- [ ] **Step 9: Run green tests**

Run targeted tests:

```bash
cargo test -p hya-cli runtime_reasoning_ --lib
cargo test -p hya-cli think_dialog_ --lib
cargo test -p hya-cli model_reasoning_ --lib
```

Expected: pass.

---

### Task 6: Compatibility tests for OpenCode no-signal behavior

**Files:**
- Modify only if needed: `crates/hya-server/src/opencode/reasoning_options_tests.rs`
- Modify only if needed: `crates/hya-server/src/opencode/reasoning_options.rs`

**Interfaces:**
- Preserves: `resolve_reasoning(None, empty_options, model, {}) == None`.
- Preserves: explicit `none` maps to `ReasoningEffort::Off`.

- [ ] **Step 1: Run existing compatibility tests before editing server code**

Run:

```bash
cargo test -p hya-server reasoning_options --lib
```

Expected: existing tests pass unless the environment's known unrelated skill-order assertions are included by a broader filter. If they fail due to this task's changes, fix before proceeding.

- [ ] **Step 2: Add tests only for any changed behavior**

If server code is refactored to call the shared helper, add a test that keeps no-signal behavior explicit:

```rust
#[test]
fn empty_inputs_do_not_default_to_model_highest_reasoning() {
    let result = resolve_reasoning(
        None,
        &empty_options(),
        &ModelRef::new("12th-anth/claude-opus-4-8"),
        &json!({}),
    );

    assert_eq!(result, None);
}
```

- [ ] **Step 3: Avoid adding server preference I/O**

Do not read `HistoryStore` or `model_reasoning.json` from `hya-server` in this task.

---

### Task 7: End-to-end verification and manual QA

**Files:**
- No planned source changes unless verification exposes a defect.

- [ ] **Step 1: Format check**

Run:

```bash
cargo fmt --all --check
```

Expected: exit 0.

- [ ] **Step 2: Clippy**

Run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: exit 0.

- [ ] **Step 3: Tests**

Run:

```bash
cargo test --workspace
```

Expected: exit 0, except the pre-existing unrelated `hya-server` skill-order assertion failures if still present in this environment. If those failures persist, report them explicitly and include the targeted test evidence for this task.

- [ ] **Step 4: Manual TUI QA through tmux**

Run the built binary or `cargo run -p hya-cli --bin hya` in `interactive_bash` with a temporary `HYA_HISTORY_DIR`.

Drive these paths:

1. Open `/think`; verify choices are active-model-specific.
2. Select `off`; verify sidebar/status does not fall back to highest effort.
3. Select a non-off level; switch models away and back; verify exact provider/model last-used restoration.
4. Try an unsupported direct `/think <level>`; verify a clear system message.
5. Start `/new` or relaunch with the same temp history; verify persisted last-used behavior.

If local provider config is unavailable, use the harness or fake/dev provider path that exercises the controller/runtime behavior and document the limitation.

---

## Rollback points

- If resolver tests fail unexpectedly, revert only `crates/hya-provider/src/lib.rs` changes and re-check `ReasoningEffort` ordering assumptions.
- If TUI controller changes spread too broadly, keep `TuiEffect::SelectModel(String)` and add a provider lookup helper in `tui.rs`; do not refactor unrelated controller behavior.
- If session meta restoration gets noisy, defer optional `SessionMeta.reasoning_effort` and keep resume behavior to model-default resolution only.
- If OpenCode tests start failing, back out server refactors; this task does not require OpenCode behavioral changes.

## Pre-start checklist

- [ ] `prd.md`, `design.md`, and `implement.md` have passed plan review.
- [ ] User has reviewed/approved artifacts or explicitly asked to proceed.
- [ ] Run `python3 ./.trellis/scripts/task.py start .trellis/tasks/06-24-model-default-reasoning-effort` only after review approval.
- [ ] Start implementation with Task 1 RED test; do not write production code first.
