# Design: native TUI prompt composer for skill/file `@` and line-head `/`

Requirements and acceptance criteria live in `prd.md`. OpenCode research and
source anchors live in `research/opencode-prompt-composer.md`.

## Summary

Keep the native TUI text-first architecture and improve the composer around it:

- `@` becomes a small source-selection state machine: source choice, then file
  completion or skill completion.
- File completion inserts the existing `@relative/path` syntax and continues to
  use `reference::expand_mentions`.
- Skill completion inserts a visible `@skill:<name>` token. Submission augments
  the prompt with a short instruction to call the existing `skill` tool for that
  name. The TUI does not inline skill contents.
- `/` becomes a raw-line-head command trigger. Only a prompt whose first byte is
  `/` can open command completion or dispatch as a command.
- `/permission` and `/yolo` join the existing native command registry without
  shrinking the command set already supported by hya.

No `hya-proto` event shape, provider request shape, or server API shape changes
are required for this task.

## Current Architecture To Preserve

Native TUI composition currently crosses these files:

- `crates/hya-backend/src/tui/controller.rs`
  owns input mutation, dialogs, completion mode, slash dispatch, and
  `TuiEffect`.
- `crates/hya-backend/src/tui/commands.rs`
  owns built-in slash commands and markdown custom commands.
- `crates/hya-backend/src/tui/prompt.rs`
  owns paste/image placeholders and the current mention trigger helper.
- `crates/hya-backend/src/tui/reference.rs`
  owns bounded `@path` context expansion and workspace containment.
- `crates/hya-backend/src/tui.rs`
  refreshes references/custom commands from the active workdir and turns
  `TuiEffect` values into engine actions.
- `crates/hya-app/src/skills.rs` discovers skill metadata for prompt context.
- `crates/hya-tool/src/skill.rs` loads skills through the permission plane.
- `crates/hya-legacy-tui` renders `DialogView` and must stay pure rendering.

The implementation should add small prompt-composer helpers instead of moving
engine logic into the render crate.

## User Interaction Spec

### `@` Source Choice

Trigger:

- A typed `@` opens the source picker when `mention_trigger_index` returns the
  index of the active mention token.
- Valid trigger positions are prompt start or after whitespace.
- The active token is invalid as soon as the substring after `@` contains
  whitespace.
- Esc closes the popup and suppresses the current `@` token until the token is
  changed by Backspace or a whitespace boundary. This lets users type a literal
  `@` without fighting the popup.

First-level picker:

- Title: `insert @ reference`
- Items:
  - `file` with detail `cite a workspace file or directory`
  - `skill` with detail `ask the assistant to load a skill`
- Default selection: `file`, preserving the common current `@path` flow.
- Enter/Tab selects the highlighted source.
- `f` selects file and `s` selects skill as shortcuts while this source picker
  is open.

After selecting file:

- Mode changes to file completion.
- The prompt still contains the original `@`; printable characters append to the
  prompt and become the file prefix filter.
- Filtering uses the substring after the active `@`.
- Completion candidates are workspace-contained file/directory references.
- Selecting a candidate replaces the active token with the candidate label and a
  trailing space, for example `@crates/hya-backend/src/tui/controller.rs `.
- The submitted prompt is then handled by the existing
  `reference::expand_mentions` path.

After selecting skill:

- Mode changes to skill completion.
- The prompt still contains the original `@`; printable characters append to the
  prompt and become the skill-name prefix filter.
- Completion candidates are discovered `SKILL.md` entries.
- Selecting a candidate replaces the active token with
  `@skill:<skill-name> `.
- The submitted prompt is augmented with a bounded instruction block, for
  example:

```text
<context source="@skills">
The user explicitly selected these skills for this prompt:
- release: call the `skill` tool with {"name":"release"} before acting, then follow the loaded instructions.
</context>
```

The visible user text remains unchanged except for the inserted token. The TUI
does not read or inline `SKILL.md`; `hya-tool::SkillTool` still performs loading
and permission checks.

### Skill Candidate Contract

The skill picker must only list skills that the runtime can load. This requires a
shared directory contract:

- Add `SkillPlane::default_dirs() -> Vec<PathBuf>` in `crates/hya-tool/src/skill.rs`.
- Use the same default directories for `SkillPlane::default`,
  `hya_app::skill_dirs`, and the native TUI skill picker.
- Include the existing hya roots:
  - `.hya/skills`
  - `~/.config/hya/skills`
- Add OpenCode-compatible project roots only if `SkillPlane` also reads them:
  - `.opencode/skill`
  - `.opencode/skills`

The first pass does not need to support remote skill URLs, `.claude/skills`, or
`.agents/skills`, because listing a skill that the local runtime cannot load
would be worse than not listing it.

### Slash Command Trigger

Trigger:

- Command completion opens only when `app.input.starts_with('/')` and the input
  contains no whitespace.
- Command dispatch happens only when the submitted, paste-expanded raw prompt
  starts with `/`.
- Use `trim_end()` to remove terminal newline/space noise, then inspect the raw
  first character. Do not call `trim()` before slash detection.

Examples:

| Input | Behavior |
| --- | --- |
| `/model` | slash command |
| `/model gpt-5.5` | slash command with arguments |
| ` /model` | ordinary prompt |
| `please use /model` | ordinary prompt |
| `https://example.com/a/b` | ordinary prompt |

### Slash Command Inventory

Do not implement slash completion from an ad hoc subset. The implementation must
begin from an inventory of commands hya already supports and keep that inventory
covered by tests.

Current native TUI built-ins from `crates/hya-backend/src/tui/commands.rs`:

| Primary | Aliases | Current native behavior |
| --- | --- | --- |
| `/model` | `/models` | Open model selector or select a model by argument. |
| `/resume` | `/sessions` | Open persisted session selector. |
| `/new` | `/clear` | Start a fresh session. |
| `/compact` | none | Compact older context for future provider requests. |
| `/agent` | `/agents` | Open built-in agent profile selector. |
| `/tools` | none | Show builtin tools and MCP status. |
| `/mcp` | none | Show MCP/builtin tool status. |
| `/think` | none | Open/select reasoning effort. |
| `/export` | none | Export transcript as Markdown. |
| `/quit` | `/exit`, `/q` | Exit the TUI. |
| `/help` | `/?` | Show command help. |
| custom markdown commands | file stem | Submit expanded prompt templates from existing hya/opencode command directories. |

OpenCode-compatible command catalog entries already implemented in
`crates/hya-server/src/opencode/command_catalog.rs`:

| Command source | Existing support | Native TUI target |
| --- | --- | --- |
| `/init` | Server catalog template and docs mention. | Add native TUI dispatch or shared prompt-template loading; do not leave docs claiming unsupported behavior. |
| `/review` | Server catalog template with subtask metadata. | Add native prompt-template dispatch if feasible; otherwise document as server-catalog-only with a test proving it is intentionally excluded. Preferred path is native dispatch. |
| `/help`, `/model`, `/clear`, `/sessions`, `/think` | Server catalog mirrors native concepts. | Keep native behavior authoritative. |
| config commands from `opencode.json` / `opencode.jsonc` | Server catalog supports `command` / `commands`. | Account for this source in the native boundary. If not loaded in the first implementation, add an explicit tested gap and do not silently omit it from the inventory. |
| recursive disk commands under `.opencode/command(s)` | Server catalog supports nested markdown files. | Prefer extending native markdown command loading to recursive paths so slash completion matches server metadata. |
| skill commands | Server catalog exposes local skills as `source: "skill"`. | Primary UX for this task is `@ -> skill`; slash skill commands must be explicitly documented as either bridged or intentionally deferred. |

Target native TUI additions for this task:

| Primary | Aliases | Native behavior |
| --- | --- | --- |
| `/permission` | `/permissions` | Open permission status/action dialog. |
| `/yolo` | none | Toggle or set automatic permission approval. |
| `/init` | none | Reconcile current docs/server support with native TUI. Preferred behavior: run the existing guided AGENTS.md setup prompt/template. |
| `/review` | none | Reconcile server support with native TUI. Preferred behavior: submit the existing review prompt template with arguments. |

### Slash Command Catalog

Extend `CommandKind` with:

```rust
pub enum CommandKind {
    Init,
    Review,
    Model,
    Resume,
    NewSession,
    Compact,
    Agent,
    Tools,
    Think,
    Permission,
    Yolo,
    Export,
    Quit,
    Help,
}
```

Add command specs:

- `/init`
  - description: `Create or update project instructions`
  - dispatches through a prompt template or direct AGENTS.md setup path that
    matches the already implemented OpenCode-compatible command.
- `/review`
  - description: `Review changes`
  - dispatches through the already implemented review command template, with
    arguments preserved.
- `/permission`, alias `/permissions`
  - description: `Show or change permission mode`
  - opens the permission dialog.
- `/yolo`
  - description: `Toggle automatic permission approval`
  - dispatches immediately with optional arguments.

Keep existing built-ins, aliases, OpenCode-compatible prompt commands that are
already implemented in hya, and custom markdown commands in the same completion
catalog. If a command source cannot be bridged in this task, the code must carry
a regression test and docs note naming the exact gap.

### Permission Dialog

Add `DialogMode::Permission`.

Dialog:

- Title: `permissions`
- Subtitle: `current mode and quick actions`
- Items:
  - `YOLO: on` or `YOLO: off`, detail `Enter toggles automatic permission approval`
  - `tools`, detail `Show builtin tools and MCP status`
  - `help`, detail `Show slash commands and shortcuts`

Enter behavior:

- First row toggles `AppState.yolo`.
- `tools` opens the existing tools dialog.
- `help` opens the existing help dialog.

This keeps `/permission` useful without inventing a full persisted permission
policy editor.

### `/yolo` Arguments

Supported forms:

- `/yolo`: toggle current value.
- `/yolo toggle`: toggle current value.
- `/yolo on`: set `AppState.yolo = true`.
- `/yolo off`: set `AppState.yolo = false`.

Unknown arguments return a system message:

```text
usage: /yolo [on|off|toggle]
```

After a successful change, return a system message:

```text
YOLO mode enabled
```

or:

```text
YOLO mode disabled
```

The prompt/status/sidebar rendering already observes `AppState.yolo`.

## Data Structures

Add prompt-composer-specific modes in the controller:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogMode {
    Model,
    Agent,
    Resume,
    Help,
    Tools,
    Think,
    Permission,
    CommandCompletion,
    AtSourceChoice,
    FileCompletion,
    SkillCompletion,
}
```

Track source-choice suppression in `Controller`:

```rust
struct Controller {
    // existing fields...
    suppressed_mention_trigger: Option<usize>,
    skills: Vec<DialogItem>,
}
```

Rules:

- `suppressed_mention_trigger` stores the byte index of the current active `@`
  token after Esc from `AtSourceChoice`.
- It is cleared when the active mention index changes, when whitespace closes the
  token, or when the prompt is cleared/submitted.
- `skills` is refreshed alongside references whenever the active workdir changes.

Add a small skill mention helper module:

```rust
pub fn expand_skill_mentions(input: &str) -> String;
```

Behavior:

- Scans visible prompt text for `@skill:<name>` tokens at mention boundaries.
- Deduplicates names while preserving sorted deterministic output.
- Ignores malformed tokens and leaves visible text unchanged.
- Appends the `<context source="@skills">...</context>` block only when at least
  one skill mention exists.

Keep file mention expansion in `reference.rs`.

## Runtime Flow

Startup / workdir refresh:

1. `tui.rs` computes file reference items from the active workdir.
2. `tui.rs` computes skill items from the shared skill dirs.
3. `controller.set_references(...)`, `controller.set_skills(...)`, and
   `controller.set_custom_commands(...)` refresh popup catalogs.

Typing:

1. Character insertion updates `app.input`.
2. `refresh_inline_popup()` checks slash first.
3. If slash is not active, `mention_trigger_index` checks `@`.
4. A fresh valid `@` opens `AtSourceChoice`.
5. A selected source opens `FileCompletion` or `SkillCompletion`.
6. Completion candidates are recomputed from the current query.

Submission:

1. `PromptState::expanded_input(&app.input)` expands paste placeholders.
2. `trim_end()` is applied.
3. If the raw resulting string starts with `/`, dispatch slash.
4. Otherwise clear prompt state.
5. Apply `expand_skill_mentions`.
6. In `spawn_turn`, the existing `reference::expand_mentions` still materializes
   file/directory citations.
7. Existing leading `@agent` routing remains before file expansion, as it does
   today.

The ordering keeps manually typed `@plan ...` agent routing intact while the new
source picker no longer lists agent profiles as file references.

## Compatibility Notes

- Existing custom command templates keep the current `SubmitCommand` /
  `SubmitConfigured` behavior.
- Existing `@path#Lstart-end` support remains in `reference.rs`.
- Existing file reference size caps and workspace containment stay unchanged.
- Existing Tab YOLO toggle remains available when no popup is active.
- `hya-legacy-tui::DialogView` can support this task as-is. If the selected row
  text becomes cramped, change only labels/details, not renderer ownership.

## Testing Strategy

Add failing tests first in `crates/hya-backend/src/tui/controller.rs` and
`crates/hya-backend/src/tui/prompt.rs`:

- `at_popup_opens_source_choice`
- `source_choice_file_branch_completes_reference`
- `source_choice_skill_branch_completes_skill`
- `source_choice_escape_suppresses_current_at_token`
- `email_like_at_does_not_open_popup`
- `slash_dispatch_requires_raw_first_character`
- `slash_completion_requires_raw_first_character`
- `permission_command_opens_permission_dialog`
- `permission_dialog_toggles_yolo`
- `yolo_command_sets_modes_from_arguments`
- `unknown_yolo_argument_returns_usage`

Add tests in `crates/hya-backend/src/tui/skill_mentions.rs`:

- single selected skill appends one instruction block;
- duplicate skill tokens produce one instruction;
- malformed `@skill:` and email-like tokens are ignored.

Add tests in `crates/hya-tool/src/skill.rs` and `crates/hya-app/src/skills.rs`:

- `SkillPlane::default_dirs()` includes every directory surfaced by
  `hya_app::skill_dirs`.
- a skill under an OpenCode-compatible directory is listed only when
  `SkillPlane` can load it.

Keep existing render-buffer tests focused on observable text if `/permission`
needs a dialog assertion.

## Risks

- Listing a skill that cannot be loaded would create a bad UX. The shared skill
  directory contract is mandatory.
- Directly inlining skill content would bypass the permission plane. The design
  explicitly forbids it.
- `@skill:<name>` overlaps with file syntax if a real file named `skill:<name>`
  exists. This is acceptable because `reference::parse_mention` rejects absolute
  paths but not colons; therefore `expand_skill_mentions` should run before file
  expansion and `reference::parse_mention` should ignore tokens with the
  `skill:` prefix.
- Slash command behavior currently uses `trim()` in `submit_input`; this must be
  changed carefully so blank prompts are still ignored.

## Rollback Shape

If the skill branch proves too broad during implementation, keep the source
choice and file branch, but hide the skill branch behind an empty-skill fallback
message only when no loadable skills are found. Do not ship a picker that lists
unloadable skills.
