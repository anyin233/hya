# Design: opencode-inspired Rust TUI refactor

## Scope

This task refactors the existing Rust/ratatui TUI toward the OpenCode TUI's
visible organization and interaction quality. It explicitly keeps ratatui as the
terminal UI stack. It does not replace ratatui with OpenTUI, does not add plugin
slots, and does not change provider or tool execution behavior beyond making
already-projected errors, prompt input, and tool states more readable and useful.

## Reference Model

OpenCode's current TUI is a componentized terminal app:

- a session route composes scrollback, prompt, permission/question panels,
  dialogs, and sidebar;
- message parts render independently rather than as one concatenated transcript;
- user prompts are padded bordered panels;
- assistant text, reasoning, tools, and metadata have separate visual rows;
- tool rendering chooses inline rows for short actions and block rows when
  output/diff/diagnostics/error detail exists;
- error rows use the error color and are visually distinct from normal system
  text;
- sidebar is a fixed-width information panel with title/session/workspace,
  context metrics, status sections, and footer.
- prompt input owns autocomplete for `/` and `@`, paste placeholders,
  attachments, and mode-aware key handling.

hya will adopt these concepts with ratatui primitives and projected data.

## Rust Module Shape

Keep `hya-render-tui` as a pure renderer. Refactor internal modules to:

- `layout.rs`
  - Keep responsive row/body/sidebar calculation.
  - Add constants for sidebar width, prompt height, and wide breakpoint.
- `theme.rs`
  - Extend the existing dark theme with named styles or helper methods for
    user rail, assistant rail, error rows, block backgrounds, and selected rows.
- `view_model.rs`
  - Convert `Projection` plus `AppState` into typed display rows:
    `TranscriptRow::{User, AssistantText, Reasoning, ToolInline, ToolBlock,
    System, Error, Spacer}`.
  - Preserve raw tool input/error enough for rendering; truncate at render time
    where width is known.
  - Add classifiers for known error-bearing system messages as the first pass.
- `widgets/status.rs`
  - Render top status line: product, model, session, running/idle, goal/loop
    compact badges.
- `widgets/transcript.rs`
  - Render row sequence from the display model, including spacing rules.
  - Own user card, assistant text, system row, and error row rendering.
- `widgets/tools.rs`
  - Own tool display classification and inline/block formatting.
  - Map known hya tool names (`read`, `write`, `edit`, `glob`, `grep`,
    `shell`/`bash`, `task`, unknown) to opencode-style labels/icons.
- `widgets/sidebar.rs`
  - Render title/session/model/state plus sections for goal, loop, team,
    permission, and context summary from available data.
- `widgets/prompt.rs`
  - Keep prompt rendering, yolo indicator, popup anchor, attachment badges, and
    cursor math together.
- `widgets/overlays.rs`
  - Keep permission and list dialog rendering.
- `input.rs` or `prompt_state.rs` in `hya-render-tui` or `hya-cli::tui`
  - Own pure prompt state transitions that can be tested without a terminal:
    text editing, paste placeholder insertion/expansion, attachment tracking,
    popup mode, popup movement/completion, yolo flag, and double-Ctrl-C timing.
  - The controller should delegate to this model instead of growing another
    large `match KeyCode` block.

The public API can remain `draw(frame, state)`. Internal `mod widgets;` can
either re-export render functions from submodules or be replaced by a
`widgets/mod.rs`.

## Transcript Presentation

Spacing rules:

- First row starts at the top of the scroll region with no leading blank line.
- User messages are bordered/padded blocks with a primary rail and blank space
  after them.
- Assistant text starts with left indentation and no repeated `hya` label on
  every wrapped line.
- Reasoning is a subdued `Thinking`/`Thought` row.
- Inline tools use fixed-width icons and concise labels.
- Tool blocks and errors add a leading blank line and a left border.
- System messages remain muted unless classified as errors.

Error rules:

- Tool `Error` states render with `theme.error`, a failure symbol, and the
  message text.
- System messages that start with `input error:`, `turn error:`, `error:`, or
  contain obvious HTTP/provider failure prefixes render as error rows with an
  `error` label.
- Future protocol work should project `Event::Error`; this pass should not
  require changing the server event loop unless tests prove it is necessary.

## Prompt Interaction

The prompt becomes a small state machine rather than a raw string.

State fields:

- visible input text;
- paste entries `{id, placeholder, original}`;
- attachments `{id, placeholder, source_path?, mime}`;
- popup mode: none, slash commands, or at references;
- popup selection and options;
- yolo mode flag;
- last Ctrl-C timestamp for double-press exit.

Command/reference popups:

- `/` at the beginning of the prompt opens a slash-command popup. Existing
  command registry entries should feed this popup so `/model`, `/resume`,
  `/new`, and `/help` remain single-source.
- `@` after whitespace or at prompt start opens a reference popup. The first
  implementation can list files/directories under the working directory with
  prefix filtering; later reference providers can plug into the same option
  type.
- Tab and Enter complete the selected popup item. When no popup is open, Tab
  toggles yolo mode.

Ctrl-C:

- If input or paste/attachment state is non-empty, first Ctrl-C clears prompt
  state.
- If a turn is running, first Ctrl-C interrupts.
- A second Ctrl-C within a short window exits.
- If idle and empty, first Ctrl-C arms exit and shows a footer/status hint; a
  quick second Ctrl-C exits.

Paste:

- Long text or multi-line paste inserts `[Pasted Text #N]` and stores the
  original content.
- Submitting expands placeholders into original text before calling
  `TuiEffect::Submit`.
- If another paste occurs immediately after a paste placeholder was inserted,
  reveal the original raw text/path for the prior paste in visible input before
  adding or handling the next paste. This matches the user's requirement that
  consecutive paste actions expose the raw material.
- Pasted paths with common image extensions (`png`, `jpg`, `jpeg`, `gif`,
  `webp`, `avif`, `svg`) insert `[Image #N]` and store metadata. In the first
  provider-compatible pass, submission can include a textual attachment marker;
  the metadata remains available for later multimodal provider requests.
- `crossterm::event::Event::Paste(String)` is the primary terminal boundary for
  pasted text and paths. System-clipboard image bytes can be added through a
  small clipboard adapter if a dependency is introduced; unsupported platforms
  must degrade without blocking typing/submission.

Yolo mode:

- Yolo mode is a TUI permission mode, toggled with Tab when no popup is active.
- The status/prompt line must show that yolo is active.
- While active, incoming permission requests are automatically answered with an
  allow decision and should not block the prompt.

## Sidebar Presentation

Wide sidebar remains optional, but its contents become richer:

- Header: session title when present, else short session label.
- Core context: model, session, state.
- Runtime sections: goal, loop, team, permission.
- Context summary from available state: message count, tool count, error count.
- Footer: `hya` product label plus status hint.

This gives the user the visible context density they expect from OpenCode while
using only data hya already has.

## Testing Strategy

Use TDD for implementation:

- Add pure prompt-state tests before changing controller code:
  - slash popup opens and completes;
  - at-reference popup opens and completes;
  - Tab toggles yolo when no popup is active;
  - first Ctrl-C clears/arms, quick second Ctrl-C exits;
  - paste placeholder expands on submit;
  - consecutive paste reveals raw text/path;
  - image path paste records an attachment placeholder.
- Add render tests for user/assistant spacing before changing renderer code.
- Add render tests for tool error highlight before changing tool rendering.
- Add render tests for system error classification before changing view model.
- Add render tests for richer sidebar before changing sidebar renderer.
- Keep existing controller tests passing.

`ratatui::backend::TestBackend` buffer tests are the primary contract. Where
colors matter, inspect buffer cell styles in helper tests instead of relying
only on text substrings.

## Risks And Constraints

- `ratatui` paragraph wrapping can make exact snapshot tests brittle. Prefer
  targeted assertions on content, blank rows, and selected cell foregrounds.
- The current projection ignores `Event::Error`. A protocol-level fix is better
  long-term but wider than the first UI parity pass; classify injected system
  errors first and document the protocol follow-up if needed.
- OpenCode has richer SDK data than hya currently projects. Sidebar context
  should summarize available hya data without inventing fake token/cost values.
- hya currently sends one plain user prompt string to the engine. Attachment
  metadata should be preserved in the TUI state and represented textually on
  submit until the provider/core protocol grows true multimodal prompt parts.
- Auto-allow yolo mode is intentionally TUI-scoped for this pass. A later core
  permission-mode API can make it cleaner, but the first implementation should
  stay narrow.
- Keep `unwrap`/`expect` out of library code. Tests may continue to use local
  allowances consistent with existing tests.
