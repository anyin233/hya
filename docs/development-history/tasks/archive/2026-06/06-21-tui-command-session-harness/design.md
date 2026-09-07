# Design: Compat-style TUI commands, sessions, and test harness

## Current State

`crates/yaca-cli/src/tui.rs` owns terminal setup, event polling, key handling, permission prompts, prompt submission, turn spawning, cancellation, and view state updates in one file. `crates/yaca-tui` is already a pure render crate with `AppState`, widgets, layout, and render tests. Providers already include `FakeProvider`, which can emit canonical events without model calls.

The immediate bugs come from this shape:

- `Ctrl-C` and `Ctrl-D` are explicitly mapped to `Quit` in both the main event loop and `handle_key`.
- Slash commands are not parsed or dispatched; `/model` is treated as a normal prompt.
- TUI history is not persistent because the default TUI uses an in-memory `SessionStore`.

## Chosen Approach

Introduce a small TUI controller layer inside `yaca-cli` while keeping rendering in `yaca-tui`.

The controller will translate terminal events into semantic commands, update dialog/input/session state, and request runtime side effects through a narrow runtime trait. This gives tests a cheap fake runtime and lets the live TUI keep using `SessionEngine`, `EventBus`, and crossterm.

The first implementation should stay incremental. It should not rebuild all of compat. It should establish the architecture and ship the missing user-facing behavior for model switching, safe `Ctrl-C`, dummy-model testing, and JSON/JSONL session resume.

## Modules

- `crates/yaca-cli/src/tui/mod.rs`
  - Terminal setup/teardown and live event loop.
  - Wires the controller to `SessionEngine`, `EventBus`, crossterm events, and permission requests.

- `crates/yaca-cli/src/tui/controller.rs`
  - Pure-ish state machine for input, commands, dialogs, selected model, active session id, running/cancelled state, and input history.
  - Exposes `handle_key`, `handle_mouse`, `submit_input`, `open_dialog`, and `apply_runtime_event`.

- `crates/yaca-cli/src/tui/keymap.rs`
  - Maps key strokes into semantic `TuiCommand`s.
  - Provides compat-inspired defaults: model list, resume list, new session, help, scrolling, input editing, dialog movement, interrupt, exit.

- `crates/yaca-cli/src/tui/commands.rs`
  - Registry for slash commands and palette commands.
  - `/model`, shortcut model list, and palette model list all dispatch `TuiCommand::OpenModelDialog`.
  - `/resume` dispatches `TuiCommand::OpenResumeDialog`.

- `crates/yaca-cli/src/tui/dialog.rs`
  - Shared list-dialog state: items, filter text, selected index, paging, submit/cancel.
  - Used for model selection, resume, help, and permission choices.

- `crates/yaca-cli/src/tui/history.rs`
  - Per-session JSON/JSONL history manager.
  - Owns session bundle creation, event append, meta updates, listing, and loading.

- `crates/yaca-cli/src/tui/harness.rs`
  - Test-only fake runtime and helper for driving controller events.
  - Uses `FakeProvider` or a simple scripted provider with fixed responses.

## Model Switching

Model metadata will come from resolved config. `ResolvedConfig` should expose a sorted list of model ids in addition to `router` and `default_model`. The offline fallback should expose at least `offline`.

The controller stores `active_model: String`. Selecting a model updates:

- `active_model`;
- `AppState.model`;
- the next `AgentSpec.model` passed to `SessionEngine::run_turn`;
- the active session's `meta.json`, so resumed sessions show the last model used.

Current in-flight turns are not migrated. If the user opens the model dialog while a turn is running, selection can update the next turn, but the running turn continues with the model it started with.

## Ctrl-C and Interrupt Semantics

The live event loop should no longer preemptively break on `Ctrl-C`. It should send the key to the controller.

Controller behavior:

- Permission/model/resume/help dialog open: close dialog.
- Input non-empty: clear input.
- Running turn: cancel the current cancellation token, mark UI as interrupting, and keep process alive.
- Idle, empty input, no dialog: return `TuiEffect::Exit`.

`Ctrl-D` can remain an exit shortcut only when idle and empty; in input mode it should behave as delete-character where supported.

## Slash Commands and Palette Foundation

Slash command parsing happens only when the submitted input begins with `/` and matches a registered command name or alias.

Initial commands:

- `/model`: open model dialog.
- `/resume`: open session resume dialog.
- `/new`: create a new session.
- `/help`: open help dialog.

Unknown slash commands should not be sent to the model. They should produce a system/status message explaining the unknown command and listing `/help`.

The command registry should include metadata: name, aliases, description, default key binding label, and handler enum. This allows the help dialog and later command palette to reuse the same source of truth.

## Session History Storage

Do not make one central history database the canonical store.

Use a directory of independent session bundles:

```text
<history-root>/
  index.json              # small, rebuildable cache for listing only
  sessions/
    <session-uuid>/
      meta.json           # canonical session metadata
      events.jsonl        # canonical event envelopes, one JSON object per line
```

Default history root:

- `YACA_HISTORY_DIR` if set;
- otherwise platform data directory via `dirs` if available;
- otherwise `~/.yaca/history`.

`meta.json` fields:

- `id`
- `title`
- `summary`
- `model`
- `agent`
- `workdir`
- `created_at`
- `updated_at`
- `message_count`
- `last_user_message`

`events.jsonl` stores serialized `Envelope` values. The file is append-only for normal operation. The active TUI still uses `SessionEngine` and its in-memory `SessionStore` for the current runtime, while the history manager mirrors session envelopes from the bus into the active session's JSONL file.

Resume flow:

1. List session metas from `index.json` when valid, falling back to scanning `sessions/*/meta.json`.
2. User selects a session from `/resume`.
3. Load `events.jsonl`, skipping malformed lines only for that session and reporting a warning in the UI.
4. Hydrate the in-memory `SessionStore` by appending those events in file order.
5. Set active session id, active model from metadata or latest assistant message, and projection from the hydrated store.
6. New prompts append through the engine and mirror new bus envelopes back to the same `events.jsonl`.

If one session bundle is large or corrupt, listing should still show other sessions. The index is rebuildable and not authoritative.

## Testing Strategy

Tests should be modular and cheap:

- Controller unit tests:
  - `Ctrl-C` clear/interrupt/exit states.
  - Slash command parsing and unknown command behavior.
  - Dialog selection and navigation.
  - Model selection updates active model and next-agent spec.

- History tests:
  - create session bundle;
  - append/read envelopes;
  - rebuild index from metas;
  - ignore or isolate malformed session files;
  - resume hydrates projection.

- Harness tests:
  - create temp history root;
  - use dummy provider fixed response like `dummy response`;
  - simulate `/model`, choose model, submit prompt, assert provider saw selected model;
  - simulate `/resume`, choose prior session, assert transcript restores;
  - simulate permission prompt keys.

- Render tests:
  - model/resume/help dialogs render selected state and hints;
  - status/sidebar show active model and restored session label.

The fake runtime must be the default for TUI tests. No test should require network credentials or paid model calls.

## Non-goals

- Full compat parity in one pass.
- Replacing ratatui with compat's OpenTUI stack.
- Migrating server APIs from SQLite to JSON history in this task.
- Long-term compaction/summarization beyond metadata summaries.
