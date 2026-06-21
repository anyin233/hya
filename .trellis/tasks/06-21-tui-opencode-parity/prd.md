# Refactor Rust TUI toward opencode parity

## Goal

Bring the Rust `yaca` TUI much closer to OpenCode's terminal UI in visible
conversation structure, spacing, sidebar content, tool/action presentation, and
error highlighting while keeping the existing yaca event-sourced architecture.

## Requirements

- Study the upstream OpenCode TUI source and use it as the reference for
  presentation behavior, not just the provided screenshot.
- Preserve yaca's current Rust/ratatui stack. Do not replace it with OpenTUI,
  Ink, curses, a webview, or any other terminal UI stack.
- Preserve the crate boundary where `yaca-tui` is a pure renderer over projected
  state plus interaction state.
- Implement first-class prompt popups for both `/` commands and `@` references:
  - `/` opens a command popup with the existing slash commands;
  - `@` opens a reference popup for available local files, directories, and
    future reference providers;
  - Tab/Enter can complete the selected popup item.
- Change Ctrl-C semantics:
  - the first Ctrl-C clears prompt content or interrupts an active turn;
  - two quick Ctrl-C presses exit the TUI;
  - this behavior must be tested in the controller layer.
- Let Tab toggle yolo mode when no popup completion is active. Yolo mode should
  be visible in the TUI and should auto-allow permission requests while enabled.
- Add paste handling inspired by Claude Code/OpenCode:
  - long pasted text is inserted as a visible placeholder like
    `[Pasted Text #1]`;
  - submission expands placeholders back to the original pasted content;
  - two consecutive paste actions reveal the raw pasted text/path in the prompt
    rather than only adding another opaque placeholder.
- Support basic image paste/attachment handling:
  - pasted image file paths are detected by extension via terminal paste;
  - Ctrl-V/system-clipboard image read is supported when the platform/backend
    makes image bytes available, and otherwise degrades to path/text handling;
  - image attachments render as visible placeholders like `[Image #1]`;
  - the stored prompt state preserves enough attachment metadata for later
    provider/multimodal integration even if current providers submit a textual
    placeholder first.
- Improve transcript spacing so user messages, assistant text, reasoning, tools,
  system messages, and errors have deliberate separation similar to OpenCode.
- Replace the current generic tool line with opencode-inspired inline/block tool
  rows:
  - short read/grep/glob/shell-style actions render as compact inline rows;
  - tools with output, diagnostics, or errors render as emphasized blocks when
    the projected data supports it;
  - failed tools use the error color and show the error text prominently.
- Treat session-level or turn-level failures as first-class error timeline rows
  instead of only plain grey `sys` text.
- Expand the sidebar from a minimal context box into an information panel that
  exposes the session title/id, model/state, goal/loop/team/permission summaries,
  and future-friendly sections for context/files/status where data exists.
- Keep narrow terminals usable by hiding or overlaying sidebar content according
  to existing yaca constraints.
- Keep slash dialogs, permission prompts, and the input box functional.
- Refactor toward reusable UI modules and pure view-model functions; avoid one
  large widget file collecting every presentation concern.
- Add focused tests before implementation for each visible behavior being
  changed.
- Run multiple verification cycles, including render-buffer checks that compare
  visible output at narrow and wide sizes.

## Acceptance Criteria

- [ ] Planning artifacts (`prd.md`, `design.md`, `implement.md`, and research
- [x] Planning artifacts (`prd.md`, `design.md`, `implement.md`, and research
      notes) cite the OpenCode reference paths and the yaca files to change.
- [x] `yaca-tui` exposes a modular rendering structure for status/sidebar,
      transcript/message rows, tool rows, error rows, prompt, and dialogs.
- [x] Render tests prove wide layout shows an enriched sidebar and narrow layout
      keeps prompt/transcript readable without sidebar loss.
- [ ] Render tests prove conversation spacing: user cards, assistant text,
      tool rows, and system/error rows are separated intentionally.
- [ ] Render tests prove tool errors and session/turn errors are highlighted
      with error styling and readable labels/messages.
- [x] Existing controller behavior for input, slash completion, permission
      prompt, model switching, resume, and scrolling still passes.
- [x] `/` and `@` popup behavior is covered by controller/render tests.
- [x] Ctrl-C single-clear / double-exit behavior is covered by controller tests.
- [x] Tab yolo-mode toggle is covered by controller tests and permission
      handling tests.
- [x] Paste placeholder expansion, double-paste reveal, and image placeholder
      handling are covered by pure input-state tests.
- [ ] Final verification runs:
      `cargo fmt --all --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      and `cargo test --workspace`.

## Notes

- Local repository did not contain a `reference/` directory. Reference source is
  the upstream `anomalyco/opencode` repository cloned to
  `/tmp/yaca-opencode-ref` at commit `f12ac6f234ebe31982ee78f3359e8170cb09ffc9`.
