# TUI prompt composer skill file references and slash commands

## Goal

Add an OpenCode-inspired native TUI prompt composer so `@` lets the user choose
between introducing a skill and citing a file, while `/` at the beginning of the
prompt opens the command area for built-in and custom commands.

## Background

The native hya TUI already has partial prompt-composer behavior:

- slash completion is backed by `crates/hya-backend/src/tui/commands.rs`;
- native TUI built-ins already include `/model`, `/resume`, `/new`,
  `/compact`, `/agent`, `/tools`, `/mcp`, `/think`, `/export`, `/quit`, and
  `/help` with aliases;
- OpenCode-compatible server command metadata already exposes `/init`, `/review`,
  config commands, disk commands, and skill commands;
- `@file` and `@directory` mentions are expanded into bounded context by
  `crates/hya-backend/src/tui/reference.rs`;
- skills are discoverable in the runtime prompt and loadable through the
  existing `skill` tool;
- Tab toggles YOLO mode, and active YOLO auto-allows permission prompts.

The current gaps are user-facing:

- `@` immediately opens a file/reference popup instead of first asking whether
  the user wants a skill or a file;
- native TUI skill discovery is not exposed as an `@` picker;
- the file picker is only a flat reference popup, not an explicit file-citation
  branch after source selection;
- `/permission` and `/yolo` are not local slash commands;
- slash submission currently uses the trimmed prompt, so a prompt with leading
  spaces before `/cmd` can be treated as a command even though the requested rule
  is strictly "slash at line start".

OpenCode reference behavior is captured in
`research/opencode-prompt-composer.md`.

## Scope

This task covers the native ratatui TUI path:

- prompt input state and key handling in `crates/hya-backend/src/tui`;
- dialog rendering through the existing `hya-legacy-tui` pure rendering boundary;
- file citation using the existing bounded `@mentions` expansion path;
- skill selection by inserting a visible skill mention and adding a short
  prompt-side instruction that tells the model to use the existing `skill` tool;
- slash command registration, completion, dispatch, and help text for the native
  TUI;
- docs/tests for the new behavior.

This task does not introduce structured prompt parts into `hya-proto`, does not
change provider request schemas, and does not replace the ratatui renderer.

## Requirements

- R1: Typing `@` at a valid mention boundary opens a first-level source picker
  with exactly two user choices: skill and file.
- R2: Choosing file opens a filterable file/reference completion dialog. Selecting
  an item inserts the existing visible `@relative/path` syntax so
  `reference::expand_mentions` remains the single materialization path for file
  citations.
- R3: Choosing skill opens a filterable skill completion dialog. Selecting a
  skill inserts a visible `@skill:<name>` token and ensures the submitted prompt
  explicitly asks the assistant to load that skill via the existing `skill` tool.
- R4: The skill picker must not list skills that the runtime `skill` tool cannot
  load. If OpenCode-compatible skill directories are added to the picker, the
  same directories must be added to the runtime `SkillPlane`.
- R5: `@` must only trigger at a mention boundary: prompt start or after
  whitespace, with no whitespace inside the active token. It must not trigger for
  `email@example.com` or `word@fragment`.
- R6: `/` command completion must trigger only when the raw prompt starts with
  `/` and the cursor is still in the first slash token. Slash characters in the
  middle of a prompt, or after leading spaces, must be normal prompt text.
- R7: The native slash command catalog must not regress or omit already
  supported commands. It must preserve the current native TUI built-ins and
  aliases, preserve custom markdown command loading, and explicitly account for
  OpenCode-compatible commands already implemented elsewhere in hya (`/init`,
  `/review`, config commands, disk commands, and skill commands).
- R8: The native slash command catalog must add `/permission`, `/permissions`,
  and `/yolo` to the same help/completion/dispatch surface instead of handling
  them as one-off hidden branches.
- R9: `/permission` opens a permission status/action dialog that exposes the
  current YOLO state and lets the user switch it without leaving the prompt
  composer.
- R10: `/yolo`, `/yolo on`, `/yolo off`, and `/yolo toggle` update the same TUI
  YOLO state that Tab uses today, with clear status feedback.
- R11: Existing behavior for `/model`, `/new`, `/resume`, `/agent`, `/tools`,
  `/mcp`, `/think`, `/export`, `/help`, custom markdown commands, paste
  placeholders, image placeholders, and leading `@agent` routing remains intact.
- R12: `hya-legacy-tui` remains a render/state crate. Terminal I/O and command
  dispatch stay in `hya-backend`.

## Acceptance Criteria

- [ ] AC1: In the native TUI, typing `@` at prompt start opens a dialog whose
  visible choices are skill and file.
- [ ] AC2: Selecting file, typing a prefix, and pressing Enter inserts
  `@relative/path ` into the prompt; submitting that prompt still expands the
  cited file or directory through the existing bounded context block.
- [ ] AC3: Selecting skill, typing a prefix, and pressing Enter inserts
  `@skill:<name> ` into the prompt; submitting that prompt adds an instruction
  to load `<name>` with the `skill` tool, without inlining the `SKILL.md` content
  in the TUI.
- [ ] AC4: `email@example.com`, `word@fragment`, and `hello /model` do not open
  composer popups.
- [ ] AC5: `/model` at the first character keeps the existing model selector
  behavior; ` /model` is submitted as ordinary prompt text.
- [ ] AC6: `/permission` opens a permission dialog that accurately shows whether
  YOLO is on or off and offers a selectable toggle action.
- [ ] AC7: `/yolo`, `/yolo on`, `/yolo off`, and `/yolo toggle` update
  `AppState.yolo`, and the status/prompt UI reflects the new value.
- [ ] AC8: Slash help and completion list every native built-in command and
  alias-supported primary command: `/model`, `/resume`, `/new`, `/compact`,
  `/agent`, `/tools`, `/mcp`, `/think`, `/export`, `/quit`, `/help`,
  `/permission`, and `/yolo`.
- [ ] AC9: OpenCode-compatible commands already present in hya are accounted for:
  `/init` and `/review` are either implemented in native TUI dispatch or loaded
  through a shared command-template path, while config/disk/skill command
  handling is documented and tested for the native TUI boundary.
- [ ] AC10: Existing controller tests for slash commands, custom commands, file
  mentions, paste placeholders, Tab YOLO toggling, and leading `@agent` routing
  still pass.
- [ ] AC11: The task passes:
  `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`,
  and a local executable build.

## Constraints

- Keep changes narrowly scoped to the native TUI prompt composer, command
  registry, skill discovery/loading parity, and tests.
- Preserve bounded file context limits and workspace containment checks.
- Do not bypass the permission plane for skills. The TUI may indicate the
  selected skill, but the existing `skill` tool must remain responsible for
  loading content and enforcing permission.
- Keep library crates free of `unwrap` / `expect` in production code.
