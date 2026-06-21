# Implementation Plan

## Gate

Do not start implementation until this task is activated with:

```sh
python3 ./.trellis/scripts/task.py start 06-21-tui-opencode-parity
```

## Iteration 1: Baseline And Input-State Failing Tests

- Run the focused current tests:
  - `cargo test -p yaca-tui`
  - `cargo test -p yaca-cli tui`
- Add pure prompt-state tests before production code for:
  - `/` popup opens, filters, and completes existing commands;
  - `@` popup opens, filters, and completes file/reference options;
  - Tab toggles yolo when no popup is active;
  - first Ctrl-C clears content or arms exit, quick second Ctrl-C exits;
  - long/multiline paste inserts `[Pasted Text #N]` and submit expands it;
  - consecutive paste reveals the previous raw pasted text/path;
  - image path paste inserts `[Image #N]` and stores attachment metadata.
- Add an integration-level event test for `crossterm::event::Event::Paste`
  handling once the prompt-state API exists.

## Iteration 2: Prompt State And Controller Integration

- Add a prompt-state module with no terminal dependencies.
- Replace direct `AppState.input` mutation in the controller with prompt-state
  operations while keeping the public `AppState.input` field synchronized for
  rendering.
- Add a yolo flag to `AppState` and render it in status/prompt/sidebar.
- Wire yolo mode into permission handling so active yolo auto-allows prompts.
- Re-run:
  - `cargo test -p yaca-cli tui`
  - `cargo test -p yaca-tui`

## Iteration 3: Popup Rendering And Paste/Image UI

- Add render-test helpers that can inspect both text output and cell styles.
- Render `/` and `@` popups near the prompt with selected-row highlighting.
- Render paste and image placeholders in the prompt text and sidebar/status where
  useful.
- Keep prompt cursor behavior stable.

## Iteration 4: View Model Refactor

- Add failing tests for:
  - user message card spacing and assistant text separation;
  - enriched sidebar sections;
  - tool error foreground/message;
  - system/turn error row foreground/message.
- Introduce typed display rows in `view_model.rs`.
- Add pure tests for row classification:
  - user text;
  - assistant text;
  - known inline tools;
  - tool errors;
  - system errors versus normal system messages.
- Keep old renderer temporarily compiling through adapters if needed.

## Iteration 5: Widget Modules

- Split `widgets.rs` into focused modules:
  - `widgets/status.rs`
  - `widgets/transcript.rs`
  - `widgets/tools.rs`
  - `widgets/sidebar.rs`
  - `widgets/prompt.rs`
  - `widgets/overlays.rs`
  - `widgets/mod.rs`
- Move code without changing behavior first, then run:
  - `cargo fmt --all --check`
  - `cargo test -p yaca-tui`

## Iteration 6: Transcript And Tool Rendering

- Implement opencode-inspired row rendering:
  - user bordered/padded rows;
  - assistant text indentation;
  - compact inline tool rows;
  - block/error rows where needed.
- Re-run the focused failing tests until green.
- Add one regression test for wrapped/multiline error readability.

## Iteration 7: Sidebar Polish

- Implement enriched sidebar sections from available `AppState` and projection
  data.
- Keep narrow layout behavior and prompt cursor behavior intact.
- Re-run:
  - `cargo test -p yaca-tui`
  - `cargo test -p yaca-cli tui`

## Iteration 8: Full Verification

- Run the full project gate:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- Review diff for accidental changes outside `crates/yaca-tui`,
  `crates/yaca-cli` tests if needed, and Trellis task docs.

## Implemented In This Pass

- Baseline verified before changes:
  - `cargo test -p yaca-tui`
  - `cargo test -p yaca-cli tui`
- Added `/` command completion popup behavior through the existing command
  registry.
- Added `@` reference completion popup behavior using local files/directories
  scanned from the active workdir.
- Added Tab yolo-mode toggle when no completion popup is active; yolo is visible
  in status/prompt/sidebar/footer and auto-allows permission prompts.
- Changed Ctrl-C handling to first clear/interrupt/arm exit and only exit on a
  quick second Ctrl-C. Added a regression that typing after an armed exit
  disarms the double-press window.
- Added paste placeholders:
  - long/multiline text becomes `[Pasted Text #N]`;
  - submit expands placeholders back to original text;
  - consecutive paste reveals the previous original text/path.
- Added image paste placeholders for:
  - raw local image paths;
  - Markdown image links;
  - `<image ... path="...">` tags.
- Added prompt attachment metadata in `AppState`.
- Split prompt behavior into `crates/yaca-cli/src/tui/prompt.rs`.
- Added render-buffer tests for yolo/exit hints, sidebar transcript summary, and
  system/turn error highlighting with error color.
- Enriched the sidebar with transcript message/tool/error/attachment counts.
- Split `crates/yaca-tui/src/widgets.rs` into focused submodules:
  - `widgets/status.rs`
  - `widgets/transcript.rs`
  - `widgets/sidebar.rs`
  - `widgets/prompt.rs`
  - `widgets/overlays.rs`
  - `widgets/error.rs`

## Deferred Follow-Ups

- True clipboard image bytes are not available through `crossterm::Event::Paste`;
  this pass supports the terminal-compatible path/Markdown/tag forms and keeps
  attachment metadata ready for a later clipboard/provider bridge.
- Tool block rendering remains compact inline for this pass, with error styling
  preserved through the existing tool status path.
- A deeper display-row view-model refactor can follow now that widget modules
  and render-buffer tests are in place.

## Rollback Points

- If display-row refactor becomes too broad, keep existing projection conversion
  and only add error/tool/sidebar tests plus localized renderer changes.
- If style assertions are too brittle, keep text and row-structure assertions
  for this pass and manually inspect rendered buffers from deterministic tests.
- If image submission cannot become true multimodal without widening core
  protocol, keep attachment metadata in prompt state and submit a clear textual
  placeholder in this task. If system-clipboard image bytes require a new crate,
  keep the adapter isolated and make path-based image paste work first.
