# Evidence: TUI model fallback and `tui-check` optimization

## Request

- Unknown `/model <id>` should raise an error and fall back to the last used/current valid model.
- `tui-check optimize start`: reduce false positives from terminal visual QA border alignment checks.

## Confirmed `/model` behavior

- `crates/yaca-cli/src/tui/controller.rs` handles direct `/model <arguments>` in `dispatch_slash`.
- Current direct model command flow:
  1. sets `self.app.model = arguments.to_string()` before validation;
  2. searches `available_models` with `ModelEntry::matches_model_ref(arguments)`;
  3. when no match exists, creates a synthetic `ModelEntry` with empty provider and no reasoning variants;
  4. sets `active_model` to that synthetic entry;
  5. emits `TuiEffect::SelectModel(entry)`.
- `crates/yaca-cli/src/tui.rs` handles `TuiEffect::SelectModel(entry)` by resolving reasoning, assigning `agent.model = model_ref_for_entry(&entry)`, calling `engine.switch_model`, and updating the session model snapshot.
- Therefore unknown direct model refs must be rejected in the controller before emitting `SelectModel`, otherwise runtime state and session metadata can be mutated.

## Existing known-model contracts to preserve

- `/model` with no argument opens the model picker.
- Model picker selection returns the selected catalog `ModelEntry`.
- `/model <provider>/<model>` that matches the catalog selects that provider-specific entry.
- Duplicate model IDs across providers are valid; provider-prefixed refs preserve routing and reasoning variants.
- `/think` levels are derived from the active `ModelEntry.reasoning_variants`.

## Confirmed `tui-check` behavior

- The installed `visual-qa` skill script exposes `tui-check` through `scripts/cli.ts` and implements terminal capture analysis in `scripts/tui-grid.ts`.
- `checkTui(text, expectedColumns)` strips ANSI before measuring width, records line widths, overflow lines, wide-character columns, and whether ANSI is present.
- Current border heuristic:
  - any line containing a box-drawing character is considered a frame line;
  - all frame-line display widths are inserted into a single `frameWidths` set;
  - `borderMisaligned` is true whenever `frameWidths.size > 1`.
- This correctly flags the existing malformed fixture `┌──┐ / │가가│ / └──┘`, where the content line is wider than the box borders.
- It also falsely flags captures with multiple independent valid frames, such as a centered dialog plus a full-width prompt, because those valid frames naturally have different widths.

## Prior QA evidence

- Archived task `06-24-model-default-reasoning-effort` recorded capture `.trellis/workspace/reasoning-qa/captures/prefix-anth-think.txt` at 80x24.
- That capture had `maxWidth: 80`, no overflow lines, no ANSI leakage, and no wide-character drift.
- Both visual QA oracle passes judged `borderMisaligned: true` a checker false positive caused by separate centered dialog and full-width prompt borders.

## Confirmed `tui-check` ownership

- The installed `visual-qa` package cache is generated output, not the durable implementation target for a checker fix.
- Package metadata points to the upstream repository `https://github.com/code-yeongyu/oh-my-openagent`.
- The canonical source path for the checker is `packages/shared-skills/skills/visual-qa/scripts/tui-grid.ts` in that upstream repository.
- The matching upstream test path is `packages/shared-skills/skills/visual-qa/scripts/tui-grid.test.ts`.
- Therefore a durable `tui-check` fix should target the upstream source repository or be tracked as an out-of-repo/upstream follow-up, not committed only as a local patch to the installed package cache.

## Upstream tracking status

- Split-scope implementation is active for this yaca task: D1 is the in-repository `/model` no-mutation fix; D2 remains the durable upstream `oh-my-openagent` `tui-check` frame-grouping follow-up.
- No generated installed package-cache checker files are part of the yaca change. The yaca-side documentation now records that `borderMisaligned=true` on captures with multiple independent valid frames must be manually verified and tracked upstream instead of patched locally.
- The D2 follow-up should add upstream tests for valid independent frames and malformed single boxes, then update `packages/shared-skills/skills/visual-qa/scripts/tui-grid.ts` so border width consistency is computed per independent frame group.
- Until the upstream patch is released and installed, yaca verification can only treat the current checker result as objective evidence for overflow, ANSI leakage, and obvious malformed boxes; known independent-frame false positives remain upstream-owned.

## Planning implications

- `/model` fix should be small and local: validate the argument against `available_models` before mutating controller state or emitting `SelectModel`.
- Tests should first assert no mutation and a visible error effect for unknown bare and provider-prefixed model refs.
- A harness/runtime test should ensure an unknown model does not reach provider routing or `engine.switch_model` state.
- `tui-check` fix should group or validate frame lines by actual frame instance rather than globally comparing every frame width in a capture.
- `tui-check` ownership is outside this repository. The owned source has been identified upstream, so this yaca task should either split the `tui-check` fix into an upstream patch/follow-up or explicitly limit yaca implementation to the `/model` behavior and documentation.

## Open issue for merged design

- Decide whether this Trellis task should implement only the yaca-side `/model` fix and document/upstream the `tui-check` change separately, or expand scope to patch the upstream `oh-my-openagent` source as a second repository change.
- Decision: implement only the yaca-side `/model` fix in this repository and document/track the checker fix as upstream `oh-my-openagent` work.
