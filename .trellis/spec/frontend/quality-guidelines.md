# Quality Guidelines

> Code quality standards for frontend development.

---

## Overview

TUI changes must be covered by semantic render/layout tests. Use
`ratatui::backend::TestBackend` when output rendering matters, and assert stable
geometry, text, or validation behavior instead of brittle full-frame snapshots.
Reusable `hya-tui-lib` layout/component/layer changes need direct crate tests in
addition to any `hya-tui` compatibility checks.

The normal project gate applies after Rust frontend changes:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p hya
```

For new `hya-tui-lib` public API, also run:

```sh
cargo rustdoc -p hya-tui-lib -- -D warnings
```

---

## Forbidden Patterns

- Non-Rust TUI renderers in this crate.
- Terminal I/O, async streaming, or crossterm event loops inside `hya-tui` or
  `hya-tui-lib`.
- Raw color literals inside widgets when a semantic theme field can express the role.
- Layout code that indexes optional sidebar columns eagerly; use explicit `if`
  branches when a rectangle may not exist.
- Silent same-layer component overlap; library layouts must return typed
  `LayerError`/`ComponentError` results.

---

## Required Patterns

- Write failing render/layout tests before changing TUI behavior.
- Use saturating geometry math for terminal dimensions.
- Keep `AppState` application idempotent and projection-driven.
- Preserve prompt visibility on narrow terminals.
- Keep `hya-tui-lib` app-neutral: no Hya runtime crates, app state, prompt/keymap
  behavior, themes, screens, SDK, terminal I/O, or async runtime.

---

## Testing Requirements

Every TUI layout change should include at least one focused render or layout
test. Responsive changes should cover narrow and wide widths, currently
represented by 80-column and 120-column tests. `hya-tui-lib` component/layer
changes should assert both accepted cases and typed rejection cases.

---

## Code Review Checklist

- Does terminal/event-loop behavior still live outside presentation crates?
- Does the TUI remain readable at 80 columns?
- Are status labels understandable without color?
- Do new tests fail on the old behavior and pass on the new behavior?
- Do reusable primitives belong in `hya-tui-lib`, and are app-specific prompt,
  keymap, theme, screen, SDK, and runtime dependencies kept out of that crate?
- If `tui-check` reports `borderMisaligned=true` on a capture with multiple independent valid frames, verify manually and track the durable fix upstream in `oh-my-openagent`; do not patch the installed generated package cache.

---

## Scenario: Native TUI model identity and reasoning defaults

### 1. Scope / Trigger

- Trigger: any current `hya` TUI change that selects models, displays the active model, resolves reasoning effort, or persists per-model reasoning preferences.
- Applies to `crates/hya-app/src/config.rs`, `crates/hya/src/main.rs`, `crates/hya/src/transport.rs`, `crates/hya-tui/src/app/runtime.rs`, and the relevant `crates/hya-tui/src/state`, `screens`, or `widgets` module.

### 2. Signatures

- Direct model command: `/model <model>` or `/model <provider>/<model>`.
- `ModelEntry { id: String, provider: String, reasoning_variants: Vec<String> }` is the TUI/catalog identity type.
- `ModelEntry::model_ref() -> String` returns `<provider>/<id>` when `provider` is known, otherwise `<id>`.
- `ModelEntry::matches_model_ref(model: &str) -> bool` accepts both bare ids and provider-prefixed refs.
- Persisted reasoning preferences, if added, must be keyed by exact provider/model; do not key duplicate model ids by bare id.

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
- Manual QA: run native `./target/debug/hya` at 80 columns and drive provider-prefixed duplicate model paths plus `/think`.

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

---

## Scenario: Prepared Bun runtime excludes SDK server entrypoints

### 1. Scope / Trigger

- Trigger: installing or release-packaging `packages/hya-tui-ts`, or changing
  the pinned `@opencode-ai/sdk` version/layout.
- The TypeScript TUI is a client of hya's backend. It must not ship an unused
  SDK path capable of spawning an OpenCode server or TUI.

### 2. Signatures

- Prepare dependencies with `bun install --frozen-lockfile --production` in the
  staged runtime.
- Then run
  `bun packages/hya-tui-ts/scripts/prune-sdk-server.ts <runtime-directory>`.
- The staged runtime contains `package.json`, `bun.lock`, `LICENSE`,
  `UPSTREAM.md`, `src/`, and production `node_modules/`.

### 3. Contracts

- Retain `@opencode-ai/sdk` client exports, including `.` and `./v2`, and their
  `dist/index.js` / `dist/v2/index.js` files.
- Remove exports `./server` and `./v2/server`, plus `dist/server.*`,
  `dist/v2/server.*`, and the pinned server-only `dist/process.*` helpers.
- Installer and release packaging call the same pruning script after the locked
  production install. Do not maintain two pruning lists.
- The script fails when the pinned SDK no longer has the expected server
  exports; an SDK layout change requires review rather than silent fallback.

### 4. Validation & Error Matrix

- Missing runtime argument -> preparation fails.
- Missing SDK manifest or expected server exports -> preparation fails before
  placement/archive creation.
- Any server export/file remaining -> installer/release smoke fails.
- Missing client entrypoint or failed `@opencode-ai/sdk/v2` import -> runtime
  verification fails.
- Preparation failure during install -> rollback leaves the prior binaries and
  runtime intact.

### 5. Good/Base/Bad Cases

- Good: the prepared runtime imports `createOpencodeClient` from SDK v2 while
  no SDK server export or process launcher remains.
- Base: development `node_modules` may contain the complete pinned package;
  only the staged install/release runtime is pruned.
- Bad: copying production `node_modules` verbatim and claiming server code is
  excluded merely because the frontend never imports it.

### 6. Tests Required

- Installer fixture creates client and server SDK files, then asserts client
  files remain, server/process files and exports are absent, and rollback works.
- Release smoke repeats the client-present/server-absent assertions against the
  extracted archive.
- Before release, prepare one real locked runtime and import
  `@opencode-ai/sdk/v2` from that runtime.

### 7. Wrong vs Correct

#### Wrong

```sh
bun install --frozen-lockfile --production
mv "$runtime" "$install_dir"
```

#### Correct

```sh
bun install --frozen-lockfile --production
bun packages/hya-tui-ts/scripts/prune-sdk-server.ts "$runtime"
mv "$runtime" "$install_dir"
```
