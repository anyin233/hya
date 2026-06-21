# Opencode-style TUI commands sessions and test harness

## Goal

Make the yaca TUI behave more like opencode for everyday interactive work: slash commands and keyboard shortcuts should drive the same command system, model switching must work, `Ctrl-C` must not accidentally kill the process, TUI behavior must be testable without real model calls, and previous conversations must be resumable.

## Requirements

- `/model` must switch the active model for future assistant turns.
- The model switcher must be available through both slash command input and an opencode-style shortcut.
- `Ctrl-C` must be contextual:
  - when text is present, clear the input instead of quitting;
  - when an assistant turn is running, interrupt/cancel that turn instead of quitting the process;
  - when a dialog is open, close the dialog first;
  - only quit when the TUI is idle and there is no input/dialog to handle.
- The TUI must move away from one monolithic key handler toward modular input, command, dialog, and session-management units.
- First-pass opencode-style functionality must include:
  - slash commands: `/model`, `/resume`, `/new`, `/help`;
  - command palette foundation for the same command registry;
  - dialog navigation with Up/Down, Tab/Shift-Tab, Enter, Esc, Home/End, PageUp/PageDown;
  - input history navigation;
  - transcript scrolling with PageUp/PageDown, Home/End, and mouse wheel where crossterm events expose it;
  - permission prompt keyboard behavior that follows the same dialog navigation rules.
- Tests must avoid real model calls by using a fixed dummy provider response.
- Tests must cover reducer-level input/key behavior, command dispatch, dialogs, session resume, permission prompt behavior, and a minimal end-to-end TUI turn with a dummy response.
- Conversation history must not be stored in one ever-growing database file.
- Conversation history must be split by session using JSON-compatible files:
  - one session directory per conversation;
  - `meta.json` for title/model/workdir/timestamps/summary;
  - `events.jsonl` for canonical event envelopes;
  - a small rebuildable index file is allowed for fast listing, but it must not be the only source of truth.
- A damaged or huge session file must not prevent other sessions from listing or resuming.
- Existing SQLite-backed `SessionStore` behavior used by server/tests must not be broken.

## Acceptance Criteria

- [ ] Typing `/model`, selecting a different model, and sending the next prompt causes the next provider request to use that selected model.
- [ ] The selected model is reflected in the status/sidebar.
- [ ] `Ctrl-C` clears non-empty input without exiting.
- [ ] `Ctrl-C` during a running turn cancels the turn and leaves the TUI process alive.
- [ ] `Ctrl-C` exits only when idle with empty input and no dialog.
- [ ] `/resume` opens a resumable session list backed by per-session JSON/JSONL files.
- [ ] Resuming a session restores prior transcript projection and appends later messages to that session's `events.jsonl`.
- [ ] `/new` starts a new session and creates an independent session history bundle.
- [ ] `/help` exposes current commands and core shortcuts.
- [ ] The TUI test harness can run a full prompt/response flow with a dummy model response and no external network/model calls.
- [ ] Unit tests cover the command registry, key handling, model dialog, resume dialog, input history, and history file recovery/listing behavior.
- [ ] Existing `cargo test` suites continue to pass.

## Notes

- This task intentionally mimics opencode interaction patterns while keeping yaca's Rust/ratatui architecture.
- The history storage correction from the user is a hard constraint: avoid a single central history DB as the canonical store.
