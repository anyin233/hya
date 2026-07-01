# OpenCode prompt composer reference

Research date: 2026-07-01.

Primary sources:

- Commands docs: <https://opencode.ai/docs/commands/>
- Skills docs: <https://opencode.ai/docs/skills/>
- References docs: <https://opencode.ai/docs/references/>
- Source snapshot: <https://github.com/sst/opencode/tree/2b611a5b1465e64dd72a806cac36c4bdc10afcea>

## Command behavior

OpenCode treats slash commands as a prompt-head mode:

- `footer.prompt.tsx` computes slash state with a `slashHead(text)` helper that
  only returns a command head when the prompt text starts with `/`.
- `slashQuery(text, cursor)` keeps slash completion active only when the cursor
  is at the end of that head token.
- slash options are a catalog, not a hard-coded single branch. The catalog
  includes local footer actions, project commands, MCP prompts, and skill
  commands.
- selecting a slash item either executes a local action (`/new`, `/exit`,
  external editor, skill menu) or inserts the selected command token into the
  prompt.

Relevant source:

- `packages/opencode/src/cli/cmd/run/footer.prompt.tsx`
- `packages/opencode/src/cli/cmd/run/footer.command.tsx`
- `packages/opencode/src/command/index.ts`

Implication for hya: preserve the existing command registry and extend it with
permission/YOLO. The trigger must use the raw prompt, not `trim()`, so only a
literal first-character `/` can become a command.

## Mention behavior

OpenCode's mention trigger is token-aware:

- `mentionTriggerIndex(value, offset)` returns an `@` index only when the `@` is
  at prompt start or follows whitespace.
- The active mention closes when the query after `@` contains whitespace.
- It intentionally does not trigger inside email addresses or words.
- Mention options are assembled from files, subagents, and MCP resources.
- File options support search, directory expansion, and line-range syntax.
- A selected mention becomes structured prompt metadata rather than only visible
  text.

Relevant source:

- `packages/tui/src/prompt/display.ts`
- `packages/tui/test/prompt/display.test.ts`
- `packages/opencode/src/cli/cmd/run/footer.prompt.tsx`

Implication for hya: keep the same token-boundary semantics but add the
project-specific first-level choice requested by the user: skill or file. File
selection should still produce `@relative/path` visible text so hya's existing
bounded file-expansion path remains the single citation materializer.

## Skill behavior

OpenCode discovers skills from `SKILL.md` files and loads them on demand:

- supported skill roots include project-local OpenCode skills, user OpenCode
  skills, Claude skills, and Agents skills;
- each skill requires frontmatter `name` and `description`;
- skills can be surfaced as slash command sources;
- loaded skill content is handled through the skill/tool mechanism rather than
  preloading every skill into the prompt.

Relevant source:

- `packages/opencode/src/skill/index.ts`
- `packages/opencode/src/command/index.ts`
- `packages/opencode/src/session/prompt.ts`

Implication for hya: the TUI should not inline `SKILL.md` content. Selecting a
skill in the prompt composer should create an explicit instruction to use the
existing `skill` tool, preserving permission checks and on-demand loading.

## Prompt submission and file materialization

OpenCode resolves prompt references in two stages:

- the composer records selected file parts and command metadata;
- session prompt submission resolves file/directory references and command
  templates close to the runtime boundary.

Relevant source:

- `packages/opencode/src/session/prompt.ts`
- `packages/opencode/src/cli/cmd/run/stream.transport.ts`

Implication for hya: a full OpenCode-style structured prompt-parts protocol is
not necessary for this task. hya already has a safe text materialization path in
`crates/hya-backend/src/tui/reference.rs`; this task should improve selection
UX and preserve that runtime boundary.

## Local hya anchors

Current hya behavior to reuse:

- `crates/hya-backend/src/tui/controller.rs` owns prompt key handling,
  dialogs, slash dispatch, and completion.
- `crates/hya-backend/src/tui/commands.rs` owns the native slash registry and
  markdown custom commands.
- `crates/hya-backend/src/tui/prompt.rs` owns paste placeholders and the current
  `mention_trigger_index`.
- `crates/hya-backend/src/tui/reference.rs` owns bounded file/directory mention
  expansion.
- `crates/hya-app/src/skills.rs` parses/discovers `SKILL.md` metadata for system
  prompt context.
- `crates/hya-tool/src/skill.rs` owns runtime skill loading and permission.
- `crates/hya-legacy-tui` owns pure rendering for `DialogView` and should remain
  terminal-I/O-free.
