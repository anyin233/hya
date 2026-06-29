# TUI model fallback and check optimization

## Goal

Make native `hya --mini` model switching fail safely when a user types an
unknown `/model <id>`, and tighten the terminal visual QA checker so valid TUI
layouts with multiple independent frames do not report false border alignment
failures.

User value:

- A mistyped model command should not silently mutate the active model or route
  the next turn to an invalid/providerless model.
- `tui-check` should remain useful as objective QA evidence: it should flag real
  overflow, wide-character drift, and broken boxes, while avoiding false
  positives for legitimate split-pane or floating overlay layouts.

## Requirements

- Direct `/model <model>` or `/model <provider>/<model>` must select only a
  model entry present in the configured model catalog.
- Unknown `/model <id>` must produce a visible system message error and must not
  mutate `AppState.model`, the controller active model, runtime `AgentSpec.model`,
  reasoning defaults, or the session model snapshot.
- The fallback state after an unknown `/model <id>` is the last valid/current
  model already in use, not a synthetic `ModelEntry { provider: "", id: <id> }`.
- Provider-prefixed known models such as `/model openai/shared` must continue to
  select the matching provider entry and preserve provider-specific reasoning
  variants.
- The model picker dialog must continue to select catalog entries normally.
- `tui-check` must keep detecting true over-width lines and truly malformed boxes
  such as CJK content wider than a surrounding border.
- `tui-check` must not mark `borderMisaligned: true` merely because a capture
  contains multiple valid independent frame widths, such as a full-width prompt
  plus a centered modal/dialog or split-pane layout.
- Any checker change must live in the owned source for the checker. The durable
  source has been identified as upstream `oh-my-openagent`, not the installed
  generated `visual-qa` package cache.

## Acceptance Criteria

- [ ] A failing controller test is added first for unknown bare `/model nope`:
      the effect is a system-message error, `app.model` remains the previous
      valid model, and `active_model()` remains the previous valid `ModelEntry`.
- [ ] A failing controller test is added first for unknown provider-prefixed
      `/model missing/shared` or `/model openai/missing`: same no-mutation
      behavior and a clear error message.
- [ ] Existing known-model tests still pass, including provider-prefixed
      duplicate ID selection and provider-specific `/think` options.
- [ ] A runtime or harness-level test confirms an unknown `/model` command does
      not cause `engine.switch_model` or future prompt routing to use the
      unknown model.
- [ ] A failing `tui-check` test is added first for a capture with two valid
      independent frame widths, expecting `borderMisaligned: false`.
- [ ] Existing `tui-check` tests for real malformed CJK box alignment, overflow,
      ANSI stripping, and wide-character columns continue to pass.
- [ ] Manual QA drives `./target/debug/hya --mini`: select a known model, type
      an unknown `/model`, observe the system error, then send a prompt or inspect
      status to confirm the last valid model remains active.
- [ ] Terminal QA runs `tui-check` on the prior false-positive style capture (or
      an equivalent generated fixture) and observes no border false positive with
      `maxWidth <= cols`, no overflow lines, and no ANSI leakage.

## Notes

- Confirmed `/model` root cause: `crates/hya-cli/src/tui/controller.rs` currently
  handles direct `/model <arguments>` by mutating `self.app.model` first, finding
  a matching `ModelEntry`, and falling back to a synthetic providerless entry
  when no catalog entry matches.
- Confirmed runtime effect path: `crates/hya-cli/src/tui.rs` handles
  `TuiEffect::SelectModel(entry)` by resolving reasoning, assigning
  `agent.model = model_ref_for_entry(&entry)`, calling `engine.switch_model`, and
  updating the session model snapshot. Therefore the controller must reject
  unknown direct model refs before emitting `SelectModel`.
- Confirmed prior QA evidence: archived task
  `06-24-model-default-reasoning-effort` recorded a capture with `maxWidth: 80`,
  no overflow, no ANSI leakage, and no wide-character drift where
  `borderMisaligned: true` was ruled a checker false positive caused by separate
  centered dialog and full-width prompt borders.
- Confirmed checker root cause: installed `visual-qa` skill script
  `scripts/tui-grid.ts` currently puts every line containing any box-drawing
  character into one `frameWidths` set and returns `borderMisaligned` whenever
  the set has more than one width. That detects the existing malformed CJK box
  fixture, but also flags multiple independent valid frames.
- Confirmed checker ownership: the installed package cache is generated output.
  The durable source is `https://github.com/code-yeongyu/oh-my-openagent`, path
  `packages/shared-skills/skills/visual-qa/scripts/tui-grid.ts`, with matching
  tests at `packages/shared-skills/skills/visual-qa/scripts/tui-grid.test.ts`.
- Out of scope unless explicitly approved: changing provider/router model
  resolution semantics outside the native TUI command path; redesigning hya TUI
  rendering; committing generated package-cache changes as the durable
  `tui-check` fix instead of patching or filing the upstream source change.
