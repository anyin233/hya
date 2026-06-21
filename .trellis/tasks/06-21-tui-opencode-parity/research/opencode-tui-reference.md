# OpenCode TUI Reference Notes

Reference source:

- Repository: `https://github.com/anomalyco/opencode`
- Local clone: `/tmp/yaca-opencode-ref`
- Commit: `f12ac6f234ebe31982ee78f3359e8170cb09ffc9`

## Relevant OpenCode Paths

- `specs/tui-package.md`
  - Defines the TUI package boundary: OpenTUI renderer, Solid composition,
    components, routes, dialogs, themes, keymaps, SDK synchronization, tool
    presentation, plugin slots, local persistence, and presentation utilities.
  - Important rule for yaca: keep UI presentation independent from backend
    implementation modules. yaca's equivalent is keeping `yaca-tui` pure over
    `Projection`/`AppState`.
- `packages/tui/src/routes/session/index.tsx`
  - Session root composes scrollback, message rendering, prompt, permission and
    question prompts, sidebar, toast, key commands, and visibility toggles.
  - User messages are left-border panels with padding, margin, hover background,
    file badges, timestamps, and queued status.
  - Assistant messages render each part independently: reasoning header,
    markdown text, tool rows, assistant metadata footer, and message-level error
    block.
  - Tool rendering dispatches by tool name into inline rows or block tools.
    Failed inline tools switch to error color and can expand the detailed error.
- `packages/tui/src/routes/session/sidebar.tsx`
  - Sidebar is a fixed-width panel (`42`) with panel background, title/session
    metadata, workspace/share information, plugin content slots, and footer.
- `packages/tui/src/feature-plugins/sidebar/context.tsx`
  - Sidebar context section shows token count, context percentage, and cost.
- `packages/tui/src/feature-plugins/sidebar/files.tsx`
  - Modified files section shows file paths plus additions/deletions and can
    collapse when long.
- `packages/tui/src/feature-plugins/sidebar/mcp.tsx`
  - Status section uses colored dots and human-readable state text.
- `packages/tui/src/routes/session/footer.tsx`
  - Footer balances directory on the left with status counters/actions on the
    right: permissions, LSP, MCP, `/status`.
- `packages/tui/src/routes/session/permission.tsx`
  - Permission prompts are full-width bottom panels with warning border, typed
    permission-specific body, option strip, and keyboard hints.
- `packages/tui/src/component/prompt/index.tsx`
  - Prompt owns command bindings, paste handling, attachment insertion,
    placeholder/extmark tracking, submission expansion, and interrupt behavior.
  - `prompt.paste` reads clipboard content; image mime types become file parts,
    text becomes either direct insertion or a virtual paste placeholder.
  - `onPaste` normalizes CRLF/CR line endings at the boundary and delegates
    empty paste events to clipboard read for image-only paste cases.
- `packages/tui/src/component/prompt/autocomplete.tsx`
  - Autocomplete reopens `/` when slash command text is at the start of the
    prompt before whitespace.
  - Autocomplete reopens `@` when the nearest trigger before the cursor has no
    intervening whitespace.
- `packages/tui/src/prompt/part.ts`
  - `expandPastedTextPlaceholders` replaces visible paste placeholders with the
    original pasted text before submission/copy.
- `packages/tui/src/component/prompt/local-attachment.ts`
  - Recognizes common image extensions plus PDF. SVG is treated as text; other
    image/PDF files are read as bytes for attachment metadata.
- `packages/tui/src/config/keybind.ts`
  - `ctrl+c` is both app exit and input clear through mode-aware command
    dispatch; `tab` is agent cycle unless popup completion owns it.
- `packages/tui/src/component/error-component.tsx` and
  `packages/tui/src/util/error.ts`
  - Fatal errors have a dedicated screen; arbitrary errors are normalized into
    human-readable messages, formatted data, and issue-report context.

## Additional Behavioral Reference

- The user explicitly wants Claude Code/OpenCode-like paste UX: pasted text
  should not flood the prompt by default, but the original content must still be
  what gets submitted. yaca should implement this with Rust-side prompt state
  rather than terminal-only text substitution.
- Claude Code is used here as a behavior reference only; no Claude Code source
  is assumed available in this repository.

## yaca Current State

- `crates/yaca-tui/src/lib.rs`
  - Pure renderer entry point; `AppState` stores projection, goal/loop/team,
    permission/dialog/input/running/scroll/model/session label.
- `crates/yaca-tui/src/layout.rs`
  - Four vertical rows: status, body, 3-line prompt, footer.
  - Sidebar appears only when body width is at least `110`; width is `38`.
- `crates/yaca-tui/src/view_model.rs`
  - Converts projection messages into simple `TimelineItem` / `TimelinePart`.
  - Tool inputs/errors are ellipsized immediately.
- `crates/yaca-tui/src/widgets.rs`
  - Status, timeline, sidebar, prompt, permission, dialog, footer, and tool
    status formatting are all in one file.
  - System messages are plain muted `sys` rows.
  - Tool calls are one compact row with `tool {name} {status}`.
- `crates/yaca-proto/src/projection.rs`
  - `Event::Error` is currently ignored by the projection, so turn/session
    failures are not first-class visible rows.
- `crates/yaca-cli/src/tui.rs`
  - `spawn_turn` converts prompt and turn failures into injected system
    messages (`input error: ...`, `turn error: ...`), which the TUI renders as
    muted system text.
- `crates/yaca-cli/src/tui/controller.rs`
  - Input state is currently a plain string.
  - Tab currently completes slash commands only when the prompt starts with `/`;
    otherwise it does nothing.
  - Ctrl-C currently clears non-empty input, interrupts a running turn, and exits
    immediately when idle/empty. There is no double-press exit guard.
- `crates/yaca-tool/src/permission.rs`
  - `PermissionPlane` already has `AllowAlways`; yolo mode can be expressed in
    the TUI by auto-allowing while the mode is enabled, then later by a more
    explicit permission-mode API if needed.

## Design Implications

- Treat yaca errors as first-class display rows. The least invasive first pass is
  a view-model classifier that detects `input error:` / `turn error:` / `error:`
  system messages and renders them as error rows. A later protocol pass can add
  projected `Event::Error` rows.
- Split `widgets.rs` into modules so each OpenCode concept has a Rust peer:
  status, transcript rows, sidebar, prompt, overlays, and formatting utilities.
- Keep tests focused on the `ratatui::backend::TestBackend` buffer and pure
  view-model functions. Avoid depending on terminal screenshots for the first
  implementation pass.
- Model the first implementation as parity-inspired, not a full port:
  no plugin slots, no OpenTUI-specific hover behavior, no markdown renderer
  replacement, and no diff widget unless projected tool metadata already carries
  enough data.
- Add a real prompt-state model before adding more key behavior. It should own
  text, paste placeholders, attachments, popup mode, popup selection, yolo mode,
  and double-Ctrl-C timing so terminal event handling stays small.
