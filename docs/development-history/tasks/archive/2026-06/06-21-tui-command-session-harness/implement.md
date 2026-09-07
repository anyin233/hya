# Implementation Plan: Compat-style TUI commands, sessions, and test harness

## Validation Commands

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p yaca-cli`
- `cargo test -p yaca-tui`
- `cargo test --workspace`

## Task 1: Expose model inventory from config

- Modify `crates/yaca-cli/src/config.rs`.
- Add `models: Vec<String>` to `ResolvedConfig`.
- Preserve current default model selection.
- Add tests for sorted/deduplicated model ids and offline fallback model inventory.

Expected checks:

- `cargo test -p yaca-cli config`

## Task 2: Split TUI key handling into semantic commands

- Move `crates/yaca-cli/src/tui.rs` into `crates/yaca-cli/src/tui/mod.rs`.
- Add `keymap.rs` and `controller.rs`.
- Define `TuiCommand` and `TuiEffect`.
- Port existing scroll/input/submit behavior into controller tests first.
- Change live event loop so `Ctrl-C` is not intercepted before controller handling.

Required tests:

- `ctrl_c_clears_input_without_exit`
- `ctrl_c_interrupts_running_turn_without_exit`
- `ctrl_c_exits_only_when_idle_empty_and_no_dialog`
- `page_keys_scroll_transcript`

Expected checks:

- `cargo test -p yaca-cli tui::controller`

## Task 3: Add slash command registry and help dialog

- Add `commands.rs`.
- Implement `/model`, `/resume`, `/new`, `/help` command metadata.
- Route submitted slash commands to controller effects instead of provider prompts.
- Unknown slash commands produce a system/status message and do not call provider.
- Add basic help dialog render state in `yaca-tui`.

Required tests:

- `/model` dispatches `OpenModelDialog`.
- `/resume` dispatches `OpenResumeDialog`.
- `/help` opens help.
- unknown slash command does not submit a model prompt.

Expected checks:

- `cargo test -p yaca-cli slash`
- `cargo test -p yaca-tui help`

## Task 4: Add shared list dialog state and model selection

- Add `dialog.rs`.
- Extend `AppState` with a generic dialog/view overlay for model/resume/help.
- Keep permission prompts compatible but move their navigation toward the same list semantics.
- Model dialog reads model ids from config/runtime.
- Selecting a model updates controller `active_model`, `AppState.model`, and future `AgentSpec`.

Required tests:

- model dialog opens with current model selected.
- Up/Down, Tab/Shift-Tab, Home/End, PageUp/PageDown update selection.
- Enter selects model; Esc cancels.
- next submitted turn uses selected model.

Expected checks:

- `cargo test -p yaca-cli model_dialog`
- `cargo test -p yaca-tui dialog`

## Task 5: Implement per-session JSON/JSONL history

- Add `history.rs`.
- Add a history root resolver using `YACA_HISTORY_DIR`, then platform/user fallback.
- Implement session bundle layout:
  - `sessions/<uuid>/meta.json`
  - `sessions/<uuid>/events.jsonl`
  - rebuildable `index.json`
- Mirror live `Envelope`s from `EventBus` into the active session's `events.jsonl`.
- Provide list/load APIs for resume.
- Hydrate the in-memory `SessionStore` from loaded envelopes for resumed sessions.

Required tests:

- creates one directory per session.
- appends event envelopes as JSONL.
- lists sessions from metas when index is missing.
- malformed session does not prevent other sessions from listing.
- resume hydrates projection from JSONL.

Expected checks:

- `cargo test -p yaca-cli history`

## Task 6: Wire `/resume` and `/new`

- `/new` creates a fresh session bundle and switches active session.
- `/resume` opens the resume dialog from history list.
- Selecting a session loads projection, active model, session label, and appends future events to that same bundle.
- Keep existing `tail-session --db` behavior unchanged.

Required tests:

- `/new` creates separate bundles.
- `/resume` restores prior transcript.
- prompt after resume appends to selected session's `events.jsonl`.

Expected checks:

- `cargo test -p yaca-cli resume`

## Task 7: Build dummy TUI harness

- Add `harness.rs` under `#[cfg(test)]`.
- Use `FakeProvider` with fixed text response, e.g. `dummy response`.
- Provide helpers to send key events, submit slash commands, select dialog entries, and drain runtime effects.
- Assert the provider model seen by requests without making network calls.

Required tests:

- full flow: `/model` -> choose model -> prompt -> fixed dummy response.
- full flow: prior JSONL session -> `/resume` -> prompt -> response appended.
- permission prompt selection uses shared dialog keys.

Expected checks:

- `cargo test -p yaca-cli tui_harness`

## Task 8: Final integration and docs

- Update `README.md` and `docs/architecture/tui.md` for:
  - slash commands;
  - `Ctrl-C` semantics;
  - JSON/JSONL session history layout;
  - dummy TUI testing strategy.
- Run full validation.

Expected checks:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
