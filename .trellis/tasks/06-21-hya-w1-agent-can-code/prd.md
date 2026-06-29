# Wave 1: agent can code (permission fix + ls/find tools)

Child of `06-20-hya-pi-parity`. See parent `design.md` / `implement.md` (Wave 1) for
full design. This is the P0 foundation: today hya **cannot** edit/write/run shell in
any mode.

## Goal
Make mutating tools (edit/write/shell) actually work, gated by the existing permission
plane: interactive approval in the TUI, workdir-scoped/`--yolo` auto-responder headless.
Add `ls` + `find` tools. Edit ambiguity guard.

## Confirmed facts (inspection)
- BUG: `hya-cli/src/main.rs::build_session_engine` drops the Ask receiver
  (`let (permission, _asks) = …`); Edit/Bash → `Mode::Ask` → dead channel → fail.
- **The TUI overlay ALREADY EXISTS** and is rendered: `hya-render-tui/src/lib.rs`
  `AppState.permission: Option<PermissionPrompt>` (L20), `PermissionPrompt`
  {title, detail, selected, reply} + `options()` → [Allow once / Allow all / Deny]
  (L41-57), `draw_permission` (L268-327) called from `draw` (L258). So Wave-1 TUI work
  is **wiring**, not building: the cli event loop must `select!` on the Ask receiver,
  populate `AppState.permission`, hold the `AskRequest` (with its `reply` oneshot)
  separately, and on confirm map `selected`→`Decision` and `reply.send(...)`.
- `hya-tool` already exports `AskRequest`, `Decision`, `Action`, `Mode`, etc.
- Tool consts/helpers exist: `truncate`, `MAX_OUTPUT_BYTES`, `MAX_LIST_ITEMS`; test
  module pattern present at end of `tool.rs`.
- write/edit assert `Action::Edit`; shell asserts `Action::Bash`; read/glob/grep already Allow.

## Requirements
- R1. edit/write/shell succeed: TUI (approve) + headless (WorkdirScoped default / Yolo).
- R2. Decoupling: keep `hya-render-tui` pure — do NOT put tokio/oneshot in `PermissionPrompt`;
  the cli holds the pending `AskRequest`.
- R3. Add `ls` (dir entries: name/type/size) and `find` (name/glob recursive + metadata).
- R4. Edit ambiguity guard: >1 match errors unless `replace_all: true`.
- R5. A dropped/unanswered reply must resolve to `Reject` (no hung tool).
- R6. Quality gate green; existing tests pass (incl. tui_render, tool tests).

## Acceptance criteria
- [ ] AC1. `hya exec "write 'hi' to ./qa.txt"` (real model) creates the file; an
      out-of-workdir path is rejected without `--yolo`, allowed with it. (evidence: transcript)
- [ ] AC2. tmux TUI: a write-triggering prompt shows the overlay; pressing the Allow
      key writes the file. (evidence: `tmux capture-pane`)
- [ ] AC3. `ls` + `find` registered, schema-valid, correct, permission-checked. (unit tests)
- [ ] AC4. `path_in_workdir` + responder decisions + edit guard unit-tested (RED→GREEN).
- [ ] AC5. `cargo fmt --check` + `clippy -D warnings` + `cargo test --workspace` green.

## Out of scope
- Project context, slash commands, compaction, providers, session tree, modes (later waves).
