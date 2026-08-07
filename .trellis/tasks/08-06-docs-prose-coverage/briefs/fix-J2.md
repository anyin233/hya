# Fix batch J2 - tui-reference.md, tui-keybindings.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/tui-reference.md`
- `docs/tui-keybindings.md`

Do not create or edit any other file.

## What the three kinds of finding mean

- **CONTRADICTION** - the document says something the source does not support. The
  new writing introduced it. This is the worst kind: a reader trusts it today.
  Fix by correcting or DELETING the claim. Never leave the wrong text alongside a
  correction.
- **STILL OPEN** - an original gap the previous writer did not really close.
  Usually "thin": the feature is named but a reader still could not use it.
- **CRITIC** - something no gap entry covered, found by a fresh reader.

## Non-negotiable rules

1. **Open the cited source before you change anything.** Every finding names a
   `file:line`. The auditor may itself be wrong - if the source supports the
   current documentation, KEEP it and say so in your report. Do not "fix" correct
   text because a report told you to.
2. Deleting an unsupported claim is a valid and often correct fix. Do not invent
   replacement behaviour to fill the space.
3. Do not weaken precise contract wording into vague prose. Some sentences in
   these documents are asserted verbatim by tests in `crates/hya-bundle/tests/`;
   if you rewrite a sentence that reads like a contract, keep its exact terms.
4. Edit only your files. Other writers are working in parallel.
5. Do not run `git commit`.

## Findings


### `docs/tui-reference.md`

**CONTRADICTION 1**

- The doc claims: Observation pane header is `handle - agent_type - Working|Finished|Failed|Cancelled|Idle - task - placement - focused|open - read-only`, and then separately: 'When focused, a hint line prints: ctrl+x ←/→ panes · 1-9 · esc main · ctrl+x w close'.
- Reality: There is no separate hint line. `observationPresentation` builds ONE array whose last element is the focused hint and joins the whole thing with ' - ', and `ObservationTranscript` renders that single string in one word-wrapped `<text>`. The doc's quoted header format string is therefore incomplete (it omits the hint element) and the 'hint line' framing describes a second line that does not exist.
- Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:1325-1345 and 1356-1365`

**STILL OPEN 1 - Tool renderers (bash/Shell, glob, read, grep, webfetch, websearch, write, edit, task, apply_patch, todowrite, question, skill, GenericTool fallback)** (`thin`)

- Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:2179 (dispatch), 2536-3045 (renderers), 3089 (toolDisplays set)`
- Why it is still open: The 'Tool rendering' section names all 13 dedicated renderers correctly and then describes only `bash` (Shell block) and the GenericTool fallback in usable detail. `glob`, `read`, `grep`, `webfetch`, `websearch`, `skill` get nothing but their name — the source gives each a distinct icon and pending label (`✱`/'Finding files...', `→`/'Reading file...' plus `↳ Loaded <path>` sublines, `✱`/'Searching content...', `%`/'Fetching from the web...', `◈`/'Searching web...', `→`/'Loading skill...') that a reader cannot recover from the doc. Worse, `write` and `question` are NOT inline rows at all — they use `BlockTool` with titles `# Wrote <path>` and `# Questions` (index.tsx:2546, 3016) — while the doc's single 'Inline row states' paragraph implies everything that is not shell/generic renders as an inline row. So a reader cannot map a transcript row back to the tool that produced it.

**STILL OPEN 2 - External editor integration (prompt.editor) — and the session.export flow that also opens $EDITOR** (`thin`)

- Source: `packages/hya-tui-ts/src/upstream/editor.ts:27, packages/hya-tui-ts/src/upstream/routes/session/index.tsx:1124-1152`
- Why it is still open: The `prompt.editor` half is fully and accurately documented. The half the gap explicitly asked for — 'alongside the `session.export` flow which also opens `$EDITOR`' — was not written anywhere. In the source BOTH export branches call `openEditor`: with `openWithoutSaving` it opens the transcript in `$VISUAL`/`$EDITOR` and never writes a file; without it, it writes `session-<id8>.md`, opens the editor anyway, and then writes the editor's returned text back over the file (`if (result !== undefined) await writeExport(filepath, result)`). The tui-reference 'Export options' dialog row mentions only the filename and the four switches; the 'External editor' section covers only `prompt.editor`. A user running `/export` will be dropped into vim with no warning and can silently overwrite the export.

**CRITIC 1 - Whether a `plan_enter` tool exists**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-tool/src/tool.rs:345 registers `plan_exit` (alias `plan`) and nothing else plan-related; `plan_enter` appears nowhere in `crates/`, only as a dead branch at packages/hya-tui-ts/src/upstream/routes/session/index.tsx:407.`
- Why it matters: docs/tui-reference.md:457 states "A completed `plan_enter` tool call switches the local agent to `plan`; `plan_exit` switches to `build`", presenting `plan_enter` as a real tool. docs/architecture/tools-and-permissions.md:16-51 and docs/architecture/agent-tool-surface.md:24-34 both give a complete 26-name builtin inventory in which `plan_exit` is the only plan tool — no `plan_enter` is ever registered or advertised to any model, so that TUI branch is unreachable. tui-reference.md should mark it as a dead upstream branch (or drop it), the way docs/FOLLOWUPS.md flags other unreachable code.


### `docs/tui-keybindings.md`

**CRITIC 1 - What the `/undo` slash command does**

- Source: `/chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/upstream/routes/session/index.tsx:747-775 — the handler aborts an in-flight turn when `session_status` is not `idle`, reverts at the last user message *before* the current revert point, and then calls `prompt?.set(...)`, overwriting the prompt buffer with that message's text parts and re-attaching its file parts.`
- Why it matters: docs/cli.md:140 documents all three behaviours, including "**Overwrites the prompt buffer** with that message's text parts and re-attaches its file parts (any draft text already typed is lost)". docs/tui-keybindings.md:552 documents the same command as only "Undo the previous user message." The two mirror tables of the same 24 slash commands disagree, and tui-keybindings.md is the doc that calls itself "the canonical reference for keyboard shortcuts, slash commands" (line 3), so the destructive prompt-buffer side effect is missing from the canonical surface.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
