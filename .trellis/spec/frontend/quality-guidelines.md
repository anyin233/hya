# Quality Guidelines

> Code quality standards for frontend development.

---

## Overview

TUI changes must be covered by render tests using `ratatui::backend::TestBackend`.
Tests should assert stable semantics and important layout behavior across terminal
sizes rather than brittle full-frame snapshots.

The normal project gate applies:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## Forbidden Patterns

- Non-Rust TUI renderers in this crate.
- Terminal I/O, async streaming, or crossterm event loops inside `hya-render-tui`.
- Raw color literals inside widgets when a semantic theme field can express the role.
- Layout code that indexes optional sidebar columns eagerly; use explicit `if`
  branches when a rectangle may not exist.

---

## Required Patterns

- Write failing render tests before changing TUI behavior.
- Use saturating geometry math for terminal dimensions.
- Keep `AppState` application idempotent and projection-driven.
- Preserve prompt visibility on narrow terminals.

---

## Testing Requirements

Every TUI layout change should include at least one focused render test. Responsive
changes should cover narrow and wide widths, currently represented by 80-column
and 120-column tests.

---

## Code Review Checklist

- Does `crates/hya-cli/src/tui.rs` still own terminal/event-loop behavior?
- Does the TUI remain readable at 80 columns?
- Are status labels understandable without color?
- Do new tests fail on the old behavior and pass on the new behavior?
- If `tui-check` reports `borderMisaligned=true` on a capture with multiple independent valid frames, verify manually and track the durable fix upstream in `oh-my-openagent`; do not patch the installed generated package cache.

---

## Scenario: Native TUI model identity and reasoning defaults

### 1. Scope / Trigger

- Trigger: any native `--mini` TUI change that selects models, displays the active model, resolves reasoning effort, or persists per-model reasoning preferences.
- Applies to `crates/hya-app/src/config.rs`, `crates/hya-cli/src/tui.rs`, `crates/hya-cli/src/tui/controller.rs`, and `crates/hya-cli/src/tui/history.rs`.

### 2. Signatures

- Direct model command: `/model <model>` or `/model <provider>/<model>`.
- `ModelEntry { id: String, provider: String, reasoning_variants: Vec<String> }` is the TUI/catalog identity type.
- `ModelEntry::model_ref() -> String` returns `<provider>/<id>` when `provider` is known, otherwise `<id>`.
- `ModelEntry::matches_model_ref(model: &str) -> bool` accepts both bare ids and provider-prefixed refs.
- `HistoryStore::record_model_reasoning(provider, model, effort)` persists last-used effort keyed by exact provider/model.

### 3. Contracts

- Provider/model identity must not be collapsed to a bare model id when duplicate ids can exist across providers.
- Dialog selection effects carry the selected `ModelEntry`, not only a `String` model id.
- Runtime model switches send provider-prefixed `ModelRef`s to the engine/provider when provider identity is known.
- The status line may display provider-prefixed refs such as `qa-oai/shared` to make duplicate-model routing explicit.
- `/think` options are derived from the active `ModelEntry.reasoning_variants`; do not hardcode `low|medium|high` in controller paths.

### 4. Validation & Error Matrix

- Unknown `/think <level>` -> system message with available levels, no state mutation.
- Unsupported effort for the active model -> system message with active model and available levels, no state mutation.
- Display state `think:none` -> `/think` dialog marks the `off` row as current; `none` is the stored/display string for explicit `ReasoningEffort::Off`, while `off` is the command/menu label.
- Missing reasoning variants -> reasoning remains unset unless the user explicitly selects `off`.
- Corrupt `model_reasoning.json` -> ignore as empty preferences and overwrite on the next successful write.
- Provider-prefixed `/model <provider>/<model>` that matches the catalog -> select that provider's `ModelEntry`; do not create an unknown fallback entry.
- Unknown direct `/model <id>` -> system message with the requested id, no `AppState.model`, `active_model`, runtime agent model, reasoning, or session snapshot mutation.
- Ambiguous bare `/model <id>` when multiple providers expose the same id -> system message listing provider-prefixed refs; require `/model <provider>/<id>`.

### 5. Good/Base/Bad Cases

- Good: `/model qa-oai/shared` selects the OpenAI-compatible duplicate, status shows `qa-oai/shared`, and `/think` lists `off|minimal|low|medium|high|xhigh`.
- Good: `/model qa-anth/shared` selects the Anthropic duplicate, status shows `qa-anth/shared`, and `/think` lists `off|low|medium|high|max`.
- Base: `/model gpt-5.5` still works when the id is unique.
- Bad: direct `/model qa-oai/shared` creates `ModelEntry { provider: "", id: "qa-oai/shared" }`, which loses provider-specific reasoning variants and preference keys.

### 6. Tests Required

- Unit: provider resolver tests for explicit > last-used > highest, preserving explicit `Off`.
- Unit: `ModelEntry`/controller tests for provider-prefixed `/model` selection and duplicate model ids.
- Unit: `/think` dialog tests for provider-specific variants including `minimal`, `xhigh`, and `max`.
- Unit: `/think` dialog test that `reasoning_effort: "none"` selects and marks the `off` row current.
- Runtime/helper: selected `ModelEntry` converts to provider-prefixed `ModelRef` before engine switch.
- Manual QA: run native `./target/debug/hya --mini` at 80 columns and drive provider-prefixed duplicate model paths plus `/think`.

### 7. Wrong vs Correct

#### Wrong

```rust
let model = ModelRef::new(entry.id.clone());
```

This silently routes duplicate bare ids to whichever provider resolves first.

#### Correct

```rust
let model = ModelRef::new(entry.model_ref());
```

This preserves exact provider/model identity through the TUI -> engine -> provider boundary.
