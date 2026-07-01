# TUI Prompt Composer Skill/File Slash Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the native TUI prompt composer described in `prd.md`: `@` source choice for skill/file insertion and strict line-head slash commands including `/permission` and `/yolo`.

**Architecture:** Keep prompt control in `crates/hya-backend/src/tui`, keep file citation materialization in `reference.rs`, keep skill loading in `hya-tool::SkillTool`, and keep `hya-legacy-tui` as pure rendering. Add small catalog/helper modules rather than changing `hya-proto` or provider request types.

**Tech Stack:** Rust 2024, ratatui/crossterm native TUI, existing hya workspace crates, existing Trellis task workflow.

---

## Gate

Do not start implementation until this task is reviewed and activated:

```sh
python3 ./.trellis/scripts/task.py start 07-01-tui-prompt-composer-skill-file-slash
```

## File Structure

- Modify `crates/hya-backend/src/tui/controller.rs`
  - Add dialog modes for `AtSourceChoice`, `FileCompletion`, `SkillCompletion`,
    and `Permission`.
  - Add skill catalog storage and current `@` suppression state.
  - Enforce raw-first-character slash dispatch.
  - Add `/permission` and `/yolo` handling.
- Modify `crates/hya-backend/src/tui/commands.rs`
  - Preserve the existing native slash command inventory.
  - Add or reconcile `/init` and `/review` from the already implemented
    OpenCode-compatible command catalog.
  - Add `CommandKind::Permission` and `CommandKind::Yolo`.
  - Add command specs and aliases.
- Modify `crates/hya-backend/src/tui/prompt.rs`
  - Tighten/extend mention trigger tests.
  - Keep paste expansion behavior unchanged.
- Create `crates/hya-backend/src/tui/skill_mentions.rs`
  - Parse visible `@skill:<name>` tokens.
  - Append the explicit skill-tool instruction block.
  - Ensure file mention expansion ignores the skill prefix.
- Modify `crates/hya-backend/src/tui/reference.rs`
  - Ignore `@skill:<name>` tokens in file reference parsing.
- Modify `crates/hya-backend/src/tui.rs`
  - Refresh skill items when the active workdir changes.
  - Apply skill mention expansion before spawning turns.
- Modify `crates/hya-tool/src/skill.rs`
  - Expose shared default skill directories.
  - Keep `SkillPlane::default` and the picker/loadable contract aligned.
- Modify `crates/hya-app/src/runtime.rs`
  - Delegate `skill_dirs()` to the shared runtime-loadable directory list.
- Modify docs if needed:
  - `docs/getting-started.md`
  - `docs/cli.md`
  - `docs/opencode-parity.md`

## Task 1: Lock the slash command inventory before changing behavior

**Files:**
- Modify: `crates/hya-backend/src/tui/commands.rs`
- Modify: `crates/hya-backend/src/tui/controller.rs`
- Read: `crates/hya-server/src/opencode/command_catalog.rs`
- Read: `docs/cli.md`

- [ ] **Step 1: Add a native built-in inventory test**

Add a test near the existing controller slash tests:

```rust
#[test]
fn slash_help_lists_every_native_builtin_command() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, "/help");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

    let labels = controller
        .app
        .dialog
        .as_ref()
        .expect("help dialog")
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();

    for expected in [
        "/model",
        "/resume",
        "/new",
        "/compact",
        "/agent",
        "/tools",
        "/mcp",
        "/think",
        "/export",
        "/quit",
        "/help",
    ] {
        assert!(
            labels.contains(&expected),
            "missing native slash command {expected}"
        );
    }
}
```

- [ ] **Step 2: Add an OpenCode catalog reconciliation test**

Add a test that names the already implemented server catalog commands that must
be accounted for in the native TUI plan:

```rust
#[test]
fn opencode_catalog_commands_are_accounted_for_in_native_tui() {
    let native = commands::COMMANDS
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    for expected in ["init", "review", "help", "model", "clear", "sessions", "think"] {
        assert!(
            native.contains(&expected)
                || matches!(expected, "clear" | "sessions"),
            "OpenCode catalog command /{expected} needs native TUI coverage or an explicit alias"
        );
    }
}
```

When implementing `/init` and `/review`, remove the alias exception style and
assert their native presence directly. `/clear` and `/sessions` are already
covered as aliases of `/new` and `/resume`.

- [ ] **Step 3: Add custom command source regression coverage**

Add or extend tests in `crates/hya-backend/src/tui/commands.rs` so native TUI
markdown commands continue loading from all existing directories:

```rust
#[test]
fn markdown_command_dirs_include_hya_and_opencode_sources() {
    let root = PathBuf::from("/tmp/project");
    let dirs = super::markdown_command_dirs(&root);
    let rendered = dirs
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert!(rendered.iter().any(|path| path.ends_with(".config/opencode/commands")));
    assert!(rendered.iter().any(|path| path.ends_with(".config/opencode/command")));
    assert!(rendered.iter().any(|path| path.ends_with(".config/hya/prompts")));
    assert!(rendered.iter().any(|path| path.ends_with(".opencode/commands")));
    assert!(rendered.iter().any(|path| path.ends_with(".opencode/command")));
    assert!(rendered.iter().any(|path| path.ends_with(".hya/prompts")));
}
```

- [ ] **Step 4: Run inventory tests**

Run:

```sh
cargo test -p hya-backend tui::controller::slash_help_lists_every_native_builtin_command tui::controller::opencode_catalog_commands_are_accounted_for_in_native_tui tui::commands
```

Expected before reconciliation: the OpenCode catalog reconciliation test fails
until `/init` and `/review` are either implemented natively or explicitly
documented as server-only. Preferred path for this task is native coverage.

## Task 2: Add failing prompt-composer controller tests

**Files:**
- Modify: `crates/hya-backend/src/tui/controller.rs`
- Modify: `crates/hya-backend/src/tui/prompt.rs`

- [ ] **Step 1: Add source-choice tests**

Add tests that describe the desired `@` behavior:

```rust
#[test]
fn at_popup_opens_source_choice() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, "@");

    let dialog = controller.app.dialog.as_ref().expect("source dialog");
    assert_eq!(dialog.title, "insert @ reference");
    assert_eq!(dialog.items[0].label, "file");
    assert_eq!(dialog.items[1].label, "skill");
}

#[test]
fn source_choice_escape_suppresses_current_at_token() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, "@");
    let _ = controller.handle_key(key(KeyCode::Esc));
    type_text(&mut controller, "literal");

    assert!(controller.app.dialog.is_none());
    assert_eq!(controller.app.input, "@literal");
}
```

- [ ] **Step 2: Add file-branch completion test**

```rust
#[test]
fn source_choice_file_branch_completes_reference() {
    let mut controller = Controller::new(AppState::default());
    controller.set_references(vec![DialogItem {
        label: "@crates/hya-backend/src/tui/controller.rs".to_string(),
        detail: "file".to_string(),
    }]);

    type_text(&mut controller, "@");
    let _ = controller.handle_key(key(KeyCode::Enter));
    type_text(&mut controller, "crates");
    let _ = controller.handle_key(key(KeyCode::Enter));

    assert_eq!(
        controller.app.input,
        "@crates/hya-backend/src/tui/controller.rs "
    );
}
```

- [ ] **Step 3: Add skill-branch completion test**

```rust
#[test]
fn source_choice_skill_branch_completes_skill() {
    let mut controller = Controller::new(AppState::default());
    controller.set_skills(vec![DialogItem {
        label: "release".to_string(),
        detail: "prepare release notes".to_string(),
    }]);

    type_text(&mut controller, "@");
    let _ = controller.handle_key(key(KeyCode::Down));
    let _ = controller.handle_key(key(KeyCode::Enter));
    type_text(&mut controller, "rel");
    let _ = controller.handle_key(key(KeyCode::Enter));

    assert_eq!(controller.app.input, "@skill:release ");
}
```

- [ ] **Step 4: Add strict slash tests**

```rust
#[test]
fn slash_dispatch_requires_raw_first_character() {
    let mut controller = Controller::with_models(
        AppState::default(),
        vec!["openai/gpt-5.5".to_string()],
    );

    type_text(&mut controller, " /model");

    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::Submit(" /model".to_string()));
}

#[test]
fn slash_completion_requires_raw_first_character() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, " /");

    assert!(controller.app.dialog.is_none());
}
```

- [ ] **Step 5: Run the focused failing tests**

Run:

```sh
cargo test -p hya-backend tui::controller -- --nocapture
```

Expected before implementation: the new source-choice, skill-completion, and raw
slash tests fail because the controller still opens reference completion directly
and dispatches slash after `trim()`.

## Task 3: Implement `@` source choice and branch completion

**Files:**
- Modify: `crates/hya-backend/src/tui/controller.rs`

- [ ] **Step 1: Add controller state**

Add `AtSourceChoice`, `FileCompletion`, and `SkillCompletion` to `DialogMode`;
add `skills` and `suppressed_mention_trigger` fields to `Controller`; initialize
them in `with_models_and_sessions`.

```rust
skills: Vec<DialogItem>,
suppressed_mention_trigger: Option<usize>,
```

- [ ] **Step 2: Add public setter**

```rust
pub fn set_skills(&mut self, skills: Vec<DialogItem>) {
    self.skills = skills;
}
```

- [ ] **Step 3: Add source-choice dialog helpers**

```rust
fn open_at_source_choice_dialog(&mut self) {
    self.app.dialog = Some(DialogView {
        title: "insert @ reference".to_string(),
        subtitle: "choose what @ should insert".to_string(),
        items: vec![
            DialogItem {
                label: "file".to_string(),
                detail: "cite a workspace file or directory".to_string(),
            },
            DialogItem {
                label: "skill".to_string(),
                detail: "ask the assistant to load a skill".to_string(),
            },
        ],
        selected: 0,
    });
    self.dialog_mode = Some(DialogMode::AtSourceChoice);
}
```

- [ ] **Step 4: Split completion dialogs**

Replace `ReferenceCompletion` use for file references with `FileCompletion`, and
add a parallel skill completion dialog that filters `self.skills` by the active
query after `@`.

- [ ] **Step 5: Update popup key handling**

Behavior:

- Esc in `AtSourceChoice` stores the current mention index in
  `suppressed_mention_trigger`.
- Enter/Tab in `AtSourceChoice` opens file or skill completion.
- `f` and `s` in `AtSourceChoice` select file or skill.
- Enter/Tab in `FileCompletion` inserts the selected `@path`.
- Enter/Tab in `SkillCompletion` inserts `@skill:<name> `.

- [ ] **Step 6: Update `refresh_inline_popup`**

Order:

1. raw slash completion;
2. active mention detection;
3. if mention is suppressed, close only source/completion dialogs for that token;
4. if no branch selected, open `AtSourceChoice`;
5. if branch selected, refresh filtered file/skill items.

- [ ] **Step 7: Run focused tests**

Run:

```sh
cargo test -p hya-backend tui::controller::at_popup_opens_source_choice tui::controller::source_choice_file_branch_completes_reference tui::controller::source_choice_skill_branch_completes_skill tui::controller::source_choice_escape_suppresses_current_at_token
```

Expected after implementation: all named tests pass.

## Task 4: Add skill mention expansion without bypassing the skill tool

**Files:**
- Create: `crates/hya-backend/src/tui/skill_mentions.rs`
- Modify: `crates/hya-backend/src/tui.rs`
- Modify: `crates/hya-backend/src/tui/reference.rs`
- Modify: `crates/hya-backend/src/tui/mod.rs` if the module list is explicit

- [ ] **Step 1: Write failing skill mention tests**

```rust
#[test]
fn expands_skill_mentions_into_tool_instruction_context() {
    let expanded = expand_skill_mentions("Use @skill:release please");

    assert!(expanded.starts_with("Use @skill:release please"));
    assert!(expanded.contains("<context source=\"@skills\">"));
    assert!(expanded.contains("call the `skill` tool with {\"name\":\"release\"}"));
}

#[test]
fn skill_mentions_are_deduplicated() {
    let expanded = expand_skill_mentions("@skill:release and @skill:release");

    assert_eq!(expanded.matches("{\"name\":\"release\"}").count(), 1);
}

#[test]
fn malformed_skill_mentions_are_ignored() {
    assert_eq!(expand_skill_mentions("email@example.com"), "email@example.com");
    assert_eq!(expand_skill_mentions("@skill: "), "@skill: ");
}
```

- [ ] **Step 2: Implement parser and expansion**

Create a helper that scans whitespace-delimited mention tokens, accepts
`@skill:<name>` at mention boundaries, trims trailing punctuation, and appends
the instruction context only when names are present.

- [ ] **Step 3: Prevent file mention expansion from treating skill tokens as paths**

In `reference::parse_mention`, add:

```rust
if path.starts_with("skill:") {
    return None;
}
```

- [ ] **Step 4: Wire expansion before turn spawn**

In the native TUI submission path, call `skill_mentions::expand_skill_mentions`
after leading agent routing and before `spawn_turn` calls
`reference::expand_mentions`.

- [ ] **Step 5: Run focused tests**

Run:

```sh
cargo test -p hya-backend tui::skill_mentions tui::reference
```

Expected after implementation: skill mention tests pass, and existing reference
tests still pass.

## Task 5: Align skill discovery with runtime loadability

**Files:**
- Modify: `crates/hya-tool/src/skill.rs`
- Modify: `crates/hya-app/src/runtime.rs`
- Modify: `crates/hya-app/src/skills.rs` if test helpers need public reuse
- Modify: `crates/hya-backend/src/tui.rs`

- [ ] **Step 1: Add shared default directory tests**

In `crates/hya-tool/src/skill.rs` tests, assert that default dirs include hya
roots and the OpenCode-compatible project roots chosen by the design:

```rust
#[test]
fn default_dirs_include_hya_and_opencode_project_roots() {
    let dirs = SkillPlane::default_dirs();

    assert!(dirs.iter().any(|path| path == &PathBuf::from(".hya/skills")));
    assert!(dirs.iter().any(|path| path == &PathBuf::from(".opencode/skill")));
    assert!(dirs.iter().any(|path| path == &PathBuf::from(".opencode/skills")));
}
```

- [ ] **Step 2: Expose `SkillPlane::default_dirs`**

```rust
impl SkillPlane {
    #[must_use]
    pub fn default_dirs() -> Vec<PathBuf> {
        let mut dirs = vec![
            PathBuf::from(".opencode/skill"),
            PathBuf::from(".opencode/skills"),
            PathBuf::from(".hya/skills"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".config/hya/skills"));
        }
        dirs
    }
}
```

Then make `SkillPlane::default()` call `Self::default_dirs()`.

- [ ] **Step 3: Delegate `hya_app::skill_dirs`**

Change `crates/hya-app/src/runtime.rs::skill_dirs` to return
`hya_tool::SkillPlane::default_dirs()`.

- [ ] **Step 4: Add TUI skill item collection**

In `crates/hya-backend/src/tui.rs`, add a helper:

```rust
fn skill_items() -> Vec<hya_legacy_tui::DialogItem> {
    hya_app::skills::discover_skills(&hya_app::skill_dirs())
        .into_iter()
        .map(|skill| hya_legacy_tui::DialogItem {
            label: skill.name,
            detail: skill.description,
        })
        .collect()
}
```

Call `controller.set_skills(skill_items())` at startup and after workdir/session
refreshes, next to `set_references` and `set_custom_commands`.

- [ ] **Step 5: Run focused tests**

Run:

```sh
cargo test -p hya-tool skill
cargo test -p hya-app skills
cargo test -p hya-backend tui::controller::source_choice_skill_branch_completes_skill
```

Expected after implementation: skill dirs are shared and picker tests pass.

## Task 6: Add missing slash commands: `/init`, `/review`, `/permission`, `/yolo`

**Files:**
- Modify: `crates/hya-backend/src/tui/commands.rs`
- Modify: `crates/hya-backend/src/tui/controller.rs`
- Read or move/reuse: `crates/hya-server/src/opencode/command_templates/initialize.txt`
- Read or move/reuse: `crates/hya-server/src/opencode/command_templates/review.txt`

- [ ] **Step 1: Add failing command tests for the reconciled command set**

```rust
#[test]
fn slash_init_is_native_command_or_prompt_template() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, "/init");

    assert!(!matches!(
        controller.handle_key(key(KeyCode::Enter)),
        TuiEffect::SystemMessage(message) if message.contains("unknown command")
    ));
}

#[test]
fn slash_review_submits_review_prompt_template() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, "/review HEAD~1");

    assert!(matches!(
        controller.handle_key(key(KeyCode::Enter)),
        TuiEffect::SubmitCommand { command, arguments, .. }
            if command == "review" && arguments == "HEAD~1"
    ));
}

#[test]
fn permission_command_opens_permission_dialog() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, "/permission");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::None);

    let dialog = controller.app.dialog.as_ref().expect("permission dialog");
    assert_eq!(dialog.title, "permissions");
}

#[test]
fn yolo_command_sets_modes_from_arguments() {
    let mut controller = Controller::new(AppState::default());

    type_text(&mut controller, "/yolo on");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::SystemMessage("YOLO mode enabled".to_string()));
    assert!(controller.app.yolo);

    type_text(&mut controller, "/yolo off");
    assert_eq!(controller.handle_key(key(KeyCode::Enter)), TuiEffect::SystemMessage("YOLO mode disabled".to_string()));
    assert!(!controller.app.yolo);
}
```

- [ ] **Step 2: Extend `CommandKind` and command specs**

Add `Init`, `Review`, `Permission`, and `Yolo` variants plus specs:

```rust
CommandSpec {
    name: "init",
    aliases: &[],
    description: "Create or update project instructions",
    key_hint: "init",
    kind: CommandKind::Init,
},
CommandSpec {
    name: "review",
    aliases: &[],
    description: "Review code changes",
    key_hint: "review",
    kind: CommandKind::Review,
},
CommandSpec {
    name: "permission",
    aliases: &["permissions"],
    description: "Show or change permission mode",
    key_hint: "perm",
    kind: CommandKind::Permission,
},
CommandSpec {
    name: "yolo",
    aliases: &[],
    description: "Toggle automatic permission approval",
    key_hint: "tab",
    kind: CommandKind::Yolo,
},
```

- [ ] **Step 3: Implement `/init` and `/review` without dropping existing custom commands**

Preferred implementation:

- `/init` dispatches through the same behavior the project already documents for
  native TUI. If an AGENTS.md direct creation helper exists, reuse it; otherwise
  submit the existing initialize prompt template as `SubmitCommand`.
- `/review <args>` submits the review prompt template as `SubmitCommand` with
  arguments preserved.
- Do not let built-in `/init` or `/review` prevent project/user custom commands
  from overriding command templates if that is the established command catalog
  precedence. If precedence is unclear, keep built-ins authoritative and document
  it in `docs/cli.md`.

- [ ] **Step 4: Add permission dialog**

Add `DialogMode::Permission`, `open_permission_dialog`, and Enter handling for
the three rows described in `design.md`.

- [ ] **Step 5: Add `/yolo` dispatch**

Add a helper:

```rust
fn set_yolo_from_command(&mut self, arguments: &str) -> TuiEffect {
    let next = match arguments {
        "" | "toggle" => Some(!self.app.yolo),
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    };
    let Some(enabled) = next else {
        return TuiEffect::SystemMessage("usage: /yolo [on|off|toggle]".to_string());
    };
    self.app.yolo = enabled;
    if enabled {
        TuiEffect::SystemMessage("YOLO mode enabled".to_string())
    } else {
        TuiEffect::SystemMessage("YOLO mode disabled".to_string())
    }
}
```

- [ ] **Step 6: Run focused command tests**

Run:

```sh
cargo test -p hya-backend tui::controller::slash_init_is_native_command_or_prompt_template tui::controller::slash_review_submits_review_prompt_template tui::controller::permission_command_opens_permission_dialog tui::controller::yolo_command_sets_modes_from_arguments
```

Expected after implementation: all named tests pass, and `/help` lists the full
native inventory.

## Task 7: Enforce raw slash semantics and preserve old behavior

**Files:**
- Modify: `crates/hya-backend/src/tui/controller.rs`
- Modify: `crates/hya-backend/src/tui/commands.rs`

- [ ] **Step 1: Change submit slash detection**

In `submit_input`, keep blank-prompt handling while inspecting the raw prompt for
slash:

```rust
let input = self.prompt.expanded_input(&self.app.input).trim_end().to_string();
if input.trim().is_empty() {
    self.clear_prompt();
    return TuiEffect::None;
}
if let Some(command) = input.strip_prefix('/') {
    self.clear_prompt();
    return self.dispatch_slash(command);
}
self.clear_prompt();
```

Keep history and scroll behavior equivalent to the existing non-command path.

- [ ] **Step 2: Confirm completion already uses raw start**

Keep completion gated by `self.app.input.starts_with('/')` and no whitespace.
Add the new regression test from Task 1 so this behavior cannot drift.

- [ ] **Step 3: Run existing controller tests**

Run:

```sh
cargo test -p hya-backend tui::controller
```

Expected after implementation: existing slash/custom command/model/agent tests
continue to pass, with the previous `/yolo is not builtin` expectation replaced
by the new `/yolo` behavior.

## Task 8: Update docs and parity notes

**Files:**
- Modify: `docs/getting-started.md`
- Modify: `docs/cli.md`
- Modify: `docs/opencode-parity.md`

- [ ] **Step 1: Document prompt composer behavior**

Add a concise section:

```markdown
### Prompt composer

- Type `@` at the start of a token to choose either a skill or a workspace file.
- File selections insert `@path` and are expanded into bounded context on submit.
- Skill selections insert `@skill:name` and ask the assistant to load that skill through the `skill` tool.
- Type `/` as the first character to open slash commands. Slash in the middle of a prompt is ordinary text.
```

- [ ] **Step 2: Document `/permission` and `/yolo`**

Add commands:

```markdown
- `/init` - start guided project instruction setup.
- `/review [target]` - review changes, defaulting to the current workspace state.
- `/permission`, `/permissions` - show permission mode and quick actions.
- `/yolo [on|off|toggle]` - change automatic permission approval mode.
```

- [ ] **Step 3: Update parity note**

Mark native prompt composer support as implemented for:

- first-level `@` skill/file choice;
- line-head slash command trigger;
- no-missing slash inventory coverage for native TUI built-ins and already
  implemented OpenCode-compatible `/init`/`/review`;
- permission/YOLO command coverage.

Do not claim structured prompt-parts parity with OpenCode.

## Task 9: Verification

**Files:**
- No code edits unless verification exposes a defect.

- [ ] **Step 1: Run formatter**

```sh
cargo fmt --all --check
```

Expected: exits 0.

- [ ] **Step 2: Run clippy**

```sh
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: exits 0.

- [ ] **Step 3: Run tests**

```sh
cargo test --workspace
```

Expected: exits 0.

- [ ] **Step 4: Build local executable**

```sh
cargo build -p hya-backend
```

Expected: exits 0 and produces `target/debug/hya-backend`.

- [ ] **Step 5: Review diff scope**

```sh
git status --short
git diff -- .trellis/tasks/07-01-tui-prompt-composer-skill-file-slash crates/hya-backend crates/hya-tool crates/hya-app docs
```

Expected: changes are limited to this task's docs and the implementation files
listed above.

## Review Checklist

- `@` source choice appears before file/skill completion.
- File selections still materialize through `reference::expand_mentions`.
- Skill selections never inline `SKILL.md` in the TUI.
- Slash commands dispatch only when the raw prompt starts with `/`.
- `/permission` and `/yolo` are in help, completion, and dispatch.
- `/init` and `/review` are reconciled with the already supported OpenCode
  command catalog and docs.
- Existing yolo auto-allow behavior still uses `AppState.yolo`.
- `hya-legacy-tui` remains render-only.
- Full Rust verification passes before reporting implementation complete.
