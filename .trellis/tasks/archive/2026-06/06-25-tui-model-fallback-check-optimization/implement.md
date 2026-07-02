# TUI Model Fallback and `tui-check` Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Safely reject unknown native TUI `/model <id>` commands in yaca and track the durable upstream `tui-check` frame-grouping fix.

**Architecture:** The yaca change stays in the native TUI controller/runtime boundary: resolve a requested model against the configured catalog before mutating state, and emit only `SystemMessage` on invalid or ambiguous refs. The checker change belongs upstream in `oh-my-openagent`, where `tui-grid.ts` should group independent frame spans before deciding `borderMisaligned`.

**Tech Stack:** Rust 2024 (`yaca-cli`, `yaca-app`, `yaca-tui` contracts), ratatui/TUI controller tests, Bun/TypeScript for upstream `oh-my-openagent` checker tests.

## Global Constraints

- Do not implement until plan review passes and the user approves the split scope.
- Use TDD: write and observe failing tests before behavior changes.
- Preserve event-sourced architecture and keep terminal I/O/event-loop behavior in `crates/yaca-cli`.
- Keep `yaca-tui` pure rendering/state; this task should not require `yaca-tui` rendering changes.
- Do not use `unwrap`, `expect`, `as any`, `@ts-ignore`, or `@ts-expect-error` to bypass correctness.
- Do not patch generated installed package caches as the durable `tui-check` solution.
- Rust verification gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

---

## File structure

- Modify: `crates/yaca-cli/src/tui/controller.rs`
  - Add failing controller tests for unknown/ambiguous direct `/model` commands.
  - Resolve direct model commands before mutating `AppState.model` or `active_model`.
- Possibly modify: `crates/yaca-cli/src/tui.rs` or `crates/yaca-cli/src/tui/harness.rs`
  - Only if the no-runtime-switch behavior cannot be proven through controller effects alone.
- Audit and possibly modify: `crates/yaca-cli/src/tui/controller.rs::set_active_model_by_identity`
  - Ensure lookup miss semantics match the real caller: session resume/hydration should clear `active_model` on miss so stale provider metadata cannot survive after `AppState.model` changes.
- Modify: `.trellis/spec/frontend/quality-guidelines.md`
  - Add the unknown/ambiguous `/model` no-mutation contract and temporary checker ownership note.
- Add or modify task artifacts under `.trellis/tasks/06-25-tui-model-fallback-check-optimization/`
  - Store review notes, fixtures, and upstream tracking links.
- Upstream modify: `packages/shared-skills/skills/visual-qa/scripts/tui-grid.ts`
  - Durable checker algorithm change, in `oh-my-openagent`.
- Upstream modify: `packages/shared-skills/skills/visual-qa/scripts/tui-grid.test.ts`
  - Regression tests for valid independent frames and malformed single boxes.

---

### Task 1: Lock direct `/model` no-mutation behavior with failing controller tests

**Files:**
- Modify: `crates/yaca-cli/src/tui/controller.rs`

**Interfaces:**
- Consumes: `Controller::with_models_and_sessions`, `Controller::handle_key`, `Controller::active_model`, `TuiEffect`, `model_entry` test helper.
- Produces: red tests that describe the required direct command behavior.

- [ ] **Step 1: Add failing tests beside existing `/model` tests**

Add tests with these exact behavioral assertions:

```rust
#[test]
fn slash_model_unknown_bare_id_preserves_current_model() {
    let alpha = model_entry("alpha", "openai", &["low", "high"]);
    let mut controller = Controller::with_models_and_sessions(
        AppState {
            model: alpha.model_ref(),
            ..AppState::default()
        },
        vec![alpha.clone()],
        Vec::new(),
    );

    type_text(&mut controller, "/model nope");

    let effect = controller.handle_key(key(KeyCode::Enter));
    assert!(matches!(effect, TuiEffect::SystemMessage(message) if message.contains("nope")));
    assert_eq!(controller.app.model, "openai/alpha");
    assert_eq!(controller.active_model(), Some(alpha));
}

#[test]
fn slash_model_unknown_provider_prefixed_preserves_current_model() {
    let alpha = model_entry("alpha", "openai", &["low", "high"]);
    let mut controller = Controller::with_models_and_sessions(
        AppState {
            model: alpha.model_ref(),
            ..AppState::default()
        },
        vec![alpha.clone()],
        Vec::new(),
    );

    type_text(&mut controller, "/model openai/nope");

    let effect = controller.handle_key(key(KeyCode::Enter));
    assert!(matches!(effect, TuiEffect::SystemMessage(message) if message.contains("openai/nope")));
    assert_eq!(controller.app.model, "openai/alpha");
    assert_eq!(controller.active_model(), Some(alpha));
}

#[test]
fn slash_model_ambiguous_bare_id_preserves_current_model() {
    let anthropic = model_entry("shared", "anthropic", &["low", "medium", "high", "max"]);
    let openai = model_entry("shared", "openai", &["minimal", "low", "medium", "high", "xhigh"]);
    let mut controller = Controller::with_models_and_sessions(
        AppState {
            model: openai.model_ref(),
            ..AppState::default()
        },
        vec![anthropic.clone(), openai.clone()],
        Vec::new(),
    );

    type_text(&mut controller, "/model shared");

    let effect = controller.handle_key(key(KeyCode::Enter));
    assert!(matches!(effect, TuiEffect::SystemMessage(message) if message.contains("ambiguous") && message.contains("anthropic/shared") && message.contains("openai/shared")));
    assert_eq!(controller.app.model, "openai/shared");
    assert_eq!(controller.active_model(), Some(openai));
}
```

- [ ] **Step 2: Run the focused tests and verify they fail on old behavior**

Run:

```sh
cargo test -p yaca-cli tui::controller::tests::slash_model_unknown_bare_id_preserves_current_model tui::controller::tests::slash_model_unknown_provider_prefixed_preserves_current_model tui::controller::tests::slash_model_ambiguous_bare_id_preserves_current_model
```

Expected before implementation: at least the unknown tests fail because old behavior emits `SelectModel` and mutates `app.model`; the ambiguous test fails because old behavior selects the first matching bare id.

---

### Task 2: Resolve direct `/model` commands before mutation

**Files:**
- Modify: `crates/yaca-cli/src/tui/controller.rs`

**Interfaces:**
- Consumes: `ModelEntry::model_ref()` and the controller's `available_models` catalog.
- Produces: successful `TuiEffect::SelectModel(ModelEntry)` only for catalog-resolved entries; `SystemMessage` for unknown/ambiguous requests.

- [ ] **Step 1: Add a private resolver inside `impl Controller`**

Use exact-ref precedence, then unique bare-id resolution:

```rust
fn resolve_model_command(&self, requested: &str) -> Result<ModelEntry, ModelCommandError> {
    let exact = self
        .available_models
        .iter()
        .filter(|entry| entry.model_ref() == requested)
        .cloned()
        .collect::<Vec<_>>();
    if let [entry] = exact.as_slice() {
        return Ok(entry.clone());
    }
    if exact.len() > 1 {
        return Err(ModelCommandError::Ambiguous {
            requested: requested.to_string(),
            candidates: exact.into_iter().map(|entry| entry.model_ref()).collect(),
        });
    }

    let bare = self
        .available_models
        .iter()
        .filter(|entry| entry.id == requested)
        .cloned()
        .collect::<Vec<_>>();
    match bare.as_slice() {
        [entry] => Ok(entry.clone()),
        [] => Err(ModelCommandError::Unknown(requested.to_string())),
        _ => Err(ModelCommandError::Ambiguous {
            requested: requested.to_string(),
            candidates: bare.into_iter().map(|entry| entry.model_ref()).collect(),
        }),
    }
}
```

Add a small private error enum near the helper:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelCommandError {
    Unknown(String),
    Ambiguous {
        requested: String,
        candidates: Vec<String>,
    },
}
```

- [ ] **Step 2: Add message formatting without mutating state**

```rust
fn model_command_error_message(error: ModelCommandError) -> String {
    match error {
        ModelCommandError::Unknown(requested) => {
            format!("unknown model '{requested}'; type /model to pick from the list")
        }
        ModelCommandError::Ambiguous { requested, candidates } => {
            format!(
                "model '{requested}' is ambiguous; use one of: {}",
                candidates.join(", ")
            )
        }
    }
}
```

- [ ] **Step 3: Replace the direct `/model <arguments>` branch**

The branch must validate first and mutate only on success:

```rust
Some(CommandKind::Model) if !arguments.is_empty() => match self.resolve_model_command(arguments) {
    Ok(entry) => {
        self.app.model = entry.model_ref();
        self.active_model = Some(entry.clone());
        TuiEffect::SelectModel(entry)
    }
    Err(error) => TuiEffect::SystemMessage(model_command_error_message(error)),
},
```

- [ ] **Step 4: Run focused tests and existing known-model tests**

Run:

```sh
cargo test -p yaca-cli tui::controller::tests::slash_model_unknown_bare_id_preserves_current_model tui::controller::tests::slash_model_unknown_provider_prefixed_preserves_current_model tui::controller::tests::slash_model_ambiguous_bare_id_preserves_current_model tui::controller::tests::slash_model_provider_prefixed_selects_matching_provider_entry tui::controller::tests::model_dialog_enter_returns_selected_entry
```

Expected after implementation: all listed tests pass.

- [ ] **Step 5: Add and run current-uncataloged preservation coverage**

Add this controller test if the initial red set does not already cover it:

```rust
#[test]
fn slash_model_unknown_preserves_uncataloged_current_model_string() {
    let mut controller = Controller::with_models_and_sessions(
        AppState {
            model: "custom-outside-catalog".to_string(),
            ..AppState::default()
        },
        vec![model_entry("alpha", "openai", &["low", "high"])],
        Vec::new(),
    );

    type_text(&mut controller, "/model nope");

    let effect = controller.handle_key(key(KeyCode::Enter));
    assert!(matches!(effect, TuiEffect::SystemMessage(message) if message.contains("nope")));
    assert_eq!(controller.app.model, "custom-outside-catalog");
    assert_eq!(controller.active_model(), None);
}
```

Run:

```sh
cargo test -p yaca-cli tui::controller::tests::slash_model_unknown_preserves_uncataloged_current_model_string
```

Expected before implementation: fails if the old path mutates `app.model` to `nope`; expected after implementation: passes.

---

### Task 3: Audit active-model identity lookup miss semantics

**Files:**
- Modify if needed: `crates/yaca-cli/src/tui/controller.rs`

**Interfaces:**
- Consumes: `Controller::set_active_model_by_identity(provider, model)`.
- Produces: explicit behavior for lookup misses so session resume cannot retain stale `active_model` metadata when the resumed model is outside the catalog. Failed direct `/model` commands preserve state through the resolver path and do not call this helper on error.

- [ ] **Step 1: Search callers before changing helper semantics**

Use structural search or codegraph callers for `set_active_model_by_identity`.

Expected: every caller is classified as either hydration/resolution that should clear on miss, or a direct command path that should avoid the helper and preserve state before mutation.

- [ ] **Step 2: Add a failing helper test for resume miss clearing**

```rust
#[test]
fn set_active_model_by_identity_clears_previous_entry_on_miss() {
    let openai = model_entry("shared", "openai", &["minimal", "low", "medium", "high", "xhigh"]);
    let mut controller = Controller::with_models_and_sessions(
        AppState::default(),
        vec![openai.clone()],
        Vec::new(),
    );

    assert_eq!(controller.set_active_model_by_identity(Some("openai"), "openai/shared"), Some(openai.clone()));
    assert_eq!(controller.set_active_model_by_identity(Some("openai"), "openai/missing"), None);
    assert_eq!(controller.active_model(), None);
}
```

- [ ] **Step 3: Implement miss behavior deliberately**

Because the production caller is resume/hydration, keep the helper assignment explicit and clearing on miss:

```rust
self.active_model = selected.clone();
selected
```

Direct `/model <id>` failures must preserve state by resolving before mutation and returning `TuiEffect::SystemMessage` without calling this helper.

- [ ] **Step 4: Run helper tests**

Run:

```sh
cargo test -p yaca-cli tui::controller::tests::set_active_model_by_identity_matches_provider_prefixed_model tui::controller::tests::set_active_model_by_identity_clears_previous_entry_on_miss
```

Expected: existing provider-prefixed match remains green; new miss-clearing test passes after the helper behavior is confirmed.

---

### Task 4: Prove runtime model switching cannot observe failed direct commands

**Files:**
- Prefer modifying: `crates/yaca-cli/src/tui/controller.rs`
- Modify only if needed: `crates/yaca-cli/src/tui.rs` or `crates/yaca-cli/src/tui/harness.rs`

**Interfaces:**
- Consumes: `TuiEffect::SelectModel` is the only runtime trigger for `engine.switch_model`.
- Produces: evidence that failed direct `/model` emits no `SelectModel`.

- [ ] **Step 1: Check whether controller tests are sufficient**

If the controller tests assert the full effect is `TuiEffect::SystemMessage(_)`, the runtime cannot enter the `SelectModel` arm for failed direct commands. If reviewers require a runtime/harness test, add it in the existing TUI harness test area rather than changing production runtime code.

- [ ] **Step 2: Add harness coverage only if the review requires it**

The test should drive `/model nope`, then assert the last selected model/snapshot in the harness remains the previous valid model. Do not add production hooks solely for the test.

- [ ] **Step 3: Run runtime-adjacent tests**

Run the focused yaca-cli TUI test subset that contains the new or existing harness tests.

Expected: failed direct `/model` is represented as a system message and no model switch state changes.

---

### Task 5: Update Trellis spec with the no-mutation contract and checker ownership note

**Files:**
- Modify: `.trellis/spec/frontend/quality-guidelines.md`

**Interfaces:**
- Consumes: confirmed behavior and ownership facts from `.trellis/tasks/06-25-tui-model-fallback-check-optimization/research/evidence.md`.
- Produces: durable guidance for future TUI model/reasoning changes and visual QA review.

- [ ] **Step 1: Add direct `/model` validation rules**

Under `Scenario: Native TUI model identity and reasoning defaults`, add bullets to the validation matrix:

```markdown
- Unknown direct `/model <id>` -> system message with the requested id, no `AppState.model`, `active_model`, runtime agent model, reasoning, or session snapshot mutation.
- Ambiguous bare `/model <id>` when multiple providers expose the same id -> system message listing provider-prefixed refs; require `/model <provider>/<id>`.
```

- [ ] **Step 2: Add temporary `tui-check` ownership guidance**

Under the code review checklist, add:

```markdown
- If `tui-check` reports `borderMisaligned=true` on a capture with multiple independent valid frames, verify manually and track the durable fix upstream in `oh-my-openagent`; do not patch the installed generated package cache.
```

- [ ] **Step 3: Validate task context manifests**

Run:

```sh
export TRELLIS_CONTEXT_ID='compat_ses_1049b54e4ffekMcVcoTL7zT6lA'; python3 ./.trellis/scripts/task.py validate 06-25-tui-model-fallback-check-optimization
```

Expected: all context manifests validate.

---

### Task 6: Upstream `tui-check` durable fix

**Files:**
- Upstream modify: `packages/shared-skills/skills/visual-qa/scripts/tui-grid.test.ts`
- Upstream modify: `packages/shared-skills/skills/visual-qa/scripts/tui-grid.ts`

**Interfaces:**
- Consumes: `checkTui(text, expectedColumns)` behavior from upstream `oh-my-openagent`.
- Produces: `borderMisaligned` based on independent frame groups instead of one global width set.

- [ ] **Step 1: Add upstream failing tests**

Add tests for these fixtures:

```text
split-pane valid fixture:
┌──────────┐  ┌──────┐
│ left     │  │ side │
└──────────┘  └──────┘

overlay/prompt valid fixture:
┌──────────────────────────────┐
│ main content                  │
└──────────────────────────────┘
        ┌──────────┐
        │ dialog   │
        └──────────┘

malformed single-frame fixture:
┌──┐
│가가│
└──┘
```

Expected: the first two return `borderMisaligned: false`; the malformed fixture remains `true`.

- [ ] **Step 2: Implement grouped frame analysis**

In `tui-grid.ts`, replace the global frame-width set with frame groups derived from row adjacency and overlapping/touching box-glyph column spans. Compute width mismatch per group and set `borderMisaligned` only when a group is internally inconsistent.

- [ ] **Step 3: Run upstream verification**

Run in the upstream package:

```sh
bun test packages/shared-skills/skills/visual-qa/scripts/tui-grid.test.ts
```

Expected: existing overflow, ANSI, wide-character, and malformed-box tests pass; new independent-frame tests pass.

- [ ] **Step 4: Track upstream result in the yaca task**

Record the upstream PR or issue URL in `.trellis/tasks/06-25-tui-model-fallback-check-optimization/research/evidence.md` and replace any temporary spec note after an upstream release is installed.

---

### Task 7: Full verification and manual QA after implementation

**Files:**
- No production file changes expected in this task unless verification exposes defects.

**Interfaces:**
- Consumes: completed D1 yaca implementation and, when available, D2 upstream checker patch.
- Produces: evidence that the user-facing behavior works through the native TUI and checker surfaces.

- [ ] **Step 1: Run Rust workspace checks**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: fmt and clippy pass. If `cargo test --workspace` still fails in known unrelated `crates/yaca-server/tests/compat_instance_api.rs` cases expecting `demo`/`scoped` but observing `brainstorming`, record that as pre-existing and keep focused TUI tests green.

- [ ] **Step 2: Manual native TUI QA**

Launch:

```sh
./target/debug/yaca --mini
```

Drive these paths in a terminal session:

1. Select a known provider-prefixed model.
2. Type `/model nope` and press Enter.
3. Observe a system message containing `nope`.
4. Confirm the status line still shows the previous valid model.
5. Type `/model openai/nope` and confirm the same no-mutation behavior.
6. If duplicate provider ids are configured, type `/model shared` and confirm ambiguity guidance lists provider-prefixed refs.
7. Send a small prompt and confirm the runtime still routes through the previous valid model.

- [ ] **Step 3: Manual `tui-check` QA after upstream patch is available**

Run the checker on a prior false-positive capture or equivalent split-pane/dialog fixture and a malformed single-box fixture.

Expected: valid independent frames report `borderMisaligned: false`; malformed single box reports `borderMisaligned: true`; no overflow or ANSI leakage appears in the valid capture.

---

## Self-review

- Spec coverage: every PRD acceptance criterion maps to a task above; the runtime criterion is covered by the no-`SelectModel` controller effect and optional harness test if demanded by review.
- Placeholder scan: no `TBD`, no unspecified edge handling, no future-only test directive without expected behavior.
- Type consistency: new Rust names are private to `controller.rs`; existing public interfaces remain unchanged.
- Multi-deliverable structure: treat yaca D1 and upstream D2 as independent child deliverables after approval; each child artifact must restate its dependencies rather than relying on parent/child tree position.
