# Fix batch F1 - configuration.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/configuration.md`

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


### `docs/configuration.md`

**CONTRADICTION 1**

- The doc claims: Disk-command frontmatter table: "subtask | When `true`, run the command in a **child session**." and inline-config table: "subtask | no | `true` → child session." (docs/cli.md:164 repeats it for /review.)
- Reality: `subtask` is parsed into CommandFrontmatter/InlineCommand and serialized onto the /api/command wire, and nothing else. No Rust code outside command_sources.rs/command_catalog.rs reads it, and the TypeScript TUI never references the field. No child session is created; the flag is inert advertised metadata.
- Source: `crates/hya-server/src/compat/command_sources.rs:13,28,64-71; crates/hya-server/src/compat/command_catalog.rs:23,183-193; no consumer under crates/ or packages/hya-tui-ts/src/`

**CONTRADICTION 2**

- The doc claims: "Files are collected **recursively** and sorted by path. The file stem becomes the slash-command name."
- Reality: command_name() strips the discovery root prefix and joins the remaining path components with '/', then strips '.md'. A nested file `.opencode/command/git/commit.md` registers as the command name `git/commit`, not `commit`. "File stem" is only correct for files sitting directly in the root, which contradicts the recursive collection described in the same sentence.
- Source: `crates/hya-server/src/compat/command_sources.rs:138-162`

**CONTRADICTION 3**

- The doc claims: "`SHELL` | Shell program for PTY sessions on Compat PTY routes ... Defaults to `/bin/sh` when unset **or empty** for PTY spawn."
- Reality: default_shell() is `std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())`. std::env::var returns Ok("") for a variable that is set but empty, so the Err arm never fires and the PTY command becomes the empty string, not "/bin/sh". The fallback applies only when SHELL is unset. (The shell-candidate list in pty_shell.rs uses var_os with no emptiness filter either, so an empty SHELL inserts an empty candidate path.)
- Source: `crates/hya-server/src/compat/pty_payload.rs:11,87-89; crates/hya-server/src/compat/pty_shell.rs:18-22`

**CONTRADICTION 4**

- The doc claims: 'Files are collected **recursively** and sorted by path. The file stem becomes the slash-command name.'
- Reality: `command_name()` strips the ROOT prefix and joins every remaining path component with `/`, then removes `.md`. So `.opencode/command/git/review.md` registers as `/git/review`, not `/review`. The 'file stem' rule is only correct for files sitting directly in the root — and the same sentence advertises recursive collection, so it is wrong exactly in the nested case it invites. Introduced by this pass (blame 3e2c1828a, 2026-08-06).
- Source: `crates/hya-server/src/compat/command_sources.rs:154-161`

**CONTRADICTION 5**

- The doc claims: '`SHELL` … Defaults to `/bin/sh` when unset **or empty** for PTY spawn.'
- Reality: `fn default_shell() { std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()) }` — `std::env::var` returns `Ok("")` for an empty variable, so the fallback fires only when SHELL is UNSET. With `SHELL=""` the PTY create payload gets an empty command string and the spawn fails; there is no empty-string filter on this path (contrast `pty_shell.rs:20`, which also does not filter empties). Introduced by this pass (blame 3e2c1828a).
- Source: `crates/hya-server/src/compat/pty_payload.rs:11, 87-89`

**CONTRADICTION 6**

- The doc claims: Skill discovery and SKILL.md frontmatter are 'documented in the dedicated Skills reference batch (see crates/hya-tool/src/skill_catalog.rs for the current source of truth **until that document lands**)'.
- Reality: docs/skills.md exists in the repo and is already wired into docs/README.md (lines 20 and 73) and cross-linked from docs/agent-bundle-authoring.md:146 and docs/compat-parity.md:87. skills.md itself lists 'Configuration (configuration.md) — pointer to this guide' as a Related link, so the intended two-way link was written on one side only. The document has landed; the sentence is false. Introduced by this pass (blame 3e2c1828a).
- Source: `docs/skills.md; docs/README.md:20,73`

**CONTRADICTION 7**

- The doc claims: 'There is no general MCP config-delete route in `0.34.6`' and 'Version `0.34.6` does not add plugin watching, hot add/remove, or a plugin reload command'. NOTE: pre-existing (blame 4e50eda9d, 2026-07-31) — NOT introduced by this pass; included because it is a live factual error in a document the pass heavily rewrote and left untouched.
- Reality: The workspace version is 0.34.14. Both statements are pinned to a release eight patch versions old, so a reader cannot tell whether they still hold. The underlying behavioural claims are still true of the code, but the version anchor is stale.
- Source: `Cargo.toml:9 (version = "0.34.14")`

**CONTRADICTION 8**

- The doc claims: "**Catalog filter:** before writing models into `config.yaml`, any model id containing `imagine`, `image`, or `video` is dropped (media-generation models the agent catalog cannot use)." — written as an unqualified step of the OAuth login flow, directly under a list covering both `openai-codex` and `grok-build` catalog fetches.
- Reality: The filter is only in `parse_openai_list_catalog` (the `grok-build` / OpenAI-compatible `/models` path). `parse_codex_models_catalog` builds its list from `models[].slug` with no id filtering at all, so Codex model ids containing `image`/`video`/`imagine` ARE written to `config.yaml`. The claim is true for grok-build and false for openai-codex.
- Source: `crates/hya-app/src/oauth/models_catalog.rs:124-149 (codex, unfiltered) vs :176-210 (openai-list, filtered at :199)`

**CONTRADICTION 9**

- The doc claims: "Consequences: config always beats a same-id manifest; **config order determines hook-chain fold order**; ..."
- Reality: `Config.plugins` is a `BTreeMap<String, PluginEntry>`, and `hya_plugin::config::merge` iterates it with `for (id, entry) in config`. Config entries are therefore emitted in lexicographic plugin-id order, not in the order they were written in the YAML file. Renaming a plugin key changes fold order; reordering the YAML block does not. docs/plugin-protocol.md's weaker phrasing ('config entries first, then directory manifests') is fine; this sentence is not.
- Source: `crates/hya-app/src/config.rs:43 ; crates/hya-plugin/src/config.rs:57-72`

**CONTRADICTION 10**

- The doc claims: The table lists all eleven hook names -- including `goal.evaluate`, `loop.verifier`, and `loop.planner` -- each with a default posture (Open), under the heading "A plugin may declare these hook names in its initialize handshake", with no caveat.
- Reality: crates/hya-plugin/src/dispatcher.rs has no dispatch arm for GoalEvaluate, LoopVerifier, or LoopPlanner; they parse and can be stored on a connection but are never called. (The only other repo hits for those identifiers are unrelated hya-core `LoopVerifier`/`LoopPlanner` Rust traits.) docs/plugin-protocol.md correctly marks them 'Dead hooks (registered but never dispatched)'. A plugin author reading configuration.md alone would build on a hook that never fires, and the two docs now disagree.
- Source: `crates/hya-plugin/src/dispatcher.rs ; crates/hya-plugin/src/messages.rs:64-70`

**CONTRADICTION 11**

- The doc claims: `keybinds` | factory defaults | Per-command overrides. Unknown keys throw `Unrecognized keybind(s): …`. … Full command table: [TUI Keybindings](tui-keybindings.md).
- Reality: The accepted keys are the `Definitions` keys in keybind.ts (`app_exit`, `session_new`, `command_list`, `editor_open`, `status_view`, …), plus a handful that genuinely are dotted (`dialog.select.*`, `prompt.autocomplete.*`, `permission.prompt.fullscreen`). The linked 'full command table' lists CommandMap *values* (`app.exit`, `session.new`, `command.palette.show`), which `parse()` rejects. Following the pointer reliably produces the very error the row documents.
- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:42-224 (Definitions), 415-425 (parse / unknownKeys), 224-405 (CommandMap)`

**CONTRADICTION 12**

- The doc claims: Invalid `model.json` entries toast a warning rather than failing startup.
- Reality: No toast is raised on load. `readJson(<state>/model.json)` is followed by `.catch(() => {})` and each field is guarded by `Array.isArray(...)` / `typeof === 'object'` — bad content is silently discarded. The 'is not valid' warning toast is emitted only by `model.set` / `model.toggleFavorite` and the agent-model `createEffect`, i.e. when a model is selected that no configured provider serves.
- Source: `packages/hya-tui-ts/src/upstream/context/local.tsx:180-192 vs 285-345, 520-532`

**STILL OPEN 1 - User-defined slash commands from disk — discovery directories and the `subtask` frontmatter key** (`contradicted`)

- Source: `?`
- Why it is still open: The discovery-directory half was genuinely fixed: docs/configuration.md:1170-1176 now lists exactly `<workdir>/.opencode/command/**/*.md` and `<workdir>/.opencode/commands/**/*.md` and explicitly denies a home tier and `.hya/prompts`, matching command_sources.rs:48-51. Two problems remain. (1) `subtask` semantics are asserted but not implemented: line 1185 says "When `true`, run the command in a **child session**" (repeated at line 1215, and in docs/cli.md:164 for /review). `subtask` is only deserialized into CommandFrontmatter/InlineCommand and re-serialized onto the wire — grep across crates/ and packages/ finds no consumer outside command_sources.rs, command_catalog.rs and a metadata API test; the TypeScript TUI never reads the field at all. Nothing spawns a child session. (2) Line 1175 says "The file stem becomes the slash-command name", which contradicts the recursive collection described one sentence earlier: command_name() (command_sources.rs:154-162) joins the path *relative to the root* with `/`, so `.opencode/command/git/commit.md` registers as `/git/commit`, not `/commit`.

**STILL OPEN 2 - env EDITOR / VISUAL and the terminal-integration probes (TERM_PROGRAM, TMUX, STY, ZED_TERM, DISPLAY, WAYLAND_DISPLAY, OPENCODE_EDITOR_SSE_PORT, OPENCODE_ZED_DB, CLAUDE_CODE_SSE_PORT)** (`thin`)

- Source: `?`
- Why it is still open: EDITOR/VISUAL is properly closed (docs/configuration.md:743 gives precedence and the `/editor` binding, matching editor.ts:27). The nine probes are handled by one bare sentence at docs/configuration.md:786-789 that names them and says "None of those are hya-specific settings" — no accepted values, no effect, so a reader cannot use or predict any of them. Four of the nine are properly documented elsewhere (docs/architecture/tui.md:156-161 gives CLAUDE_CODE_SSE_PORT precedence, OPENCODE_EDITOR_SSE_PORT fallback, OPENCODE_ZED_DB, and ZED_TERM=true / TERM_PROGRAM=zed), so the real remainder is TMUX, STY, DISPLAY, WAYLAND_DISPLAY — and for those the sentence's category is wrong: they are not editor integration. TMUX/STY select OSC-52 tmux passthrough wrapping (clipboard.ts:27) and pick the `multiplexer` field (app.tsx:239); WAYLAND_DISPLAY/DISPLAY select the `displayServer` value (app.tsx:240-243) and WAYLAND_DISPLAY also picks the native clipboard copy command (clipboard.ts:101).

**STILL OPEN 3 - skill discovery order (SKILL.md search paths)** (`thin`)

- Source: `crates/hya-tool/src/skill_catalog.rs:45-77`
- Why it is still open: The new `## Skills` section in docs/configuration.md (lines 1233-1239) is a bare mention: it names the concept and the frontmatter keys in a parenthetical, gives NO discovery order at all, and points the reader at `crates/hya-tool/src/skill_catalog.rs` 'for the current source of truth until that document lands'. The dedicated document DID land in the same pass (docs/skills.md, wired into docs/README.md items 6 and 73, and correctly listing all 10 roots in order), so the substance exists but the named target doc still sends the reader to source code. Fix is a one-line cross-reference to docs/skills.md, not new prose.

**STILL OPEN 4 - SKILL.md frontmatter keys** (`thin`)

- Source: `crates/hya-tool/src/skill_catalog.rs:28-43`
- Why it is still open: Same section. configuration.md only lists the key NAMES in a parenthetical (`name`, `description`, `allowed-tools`, `model`, `disable`, `license`, body) with no types, no required/optional split, no defaults, and no example — a bare mention. The usable version (types, `allowed-tools` empty-means-unrestricted, `disable` default false, `license` parsed-but-unused, silent-skip failure modes) is in docs/skills.md lines 52-78, which configuration.md does not link.

**STILL OPEN 5 - [config-key] skill allowed-tools frontmatter** (`thin`)

- Source: `crates/hya-tool/src/skill_catalog.rs:28-43`
- Why it is still open: docs/configuration.md:1233-1239 (`## Skills`) is a two-sentence stub. It NAMES the frontmatter fields (`name`, `description`, `allowed-tools`, `model`, `disable`, `license`) in a parenthetical list and then tells the reader to read `crates/hya-tool/src/skill_catalog.rs` as 'the current source of truth until that document lands'. No field semantics are given there at all: not that an empty/absent `allowed-tools` means unrestricted, not that `disable: true` drops the skill from discovery, not that `license` is parsed but unused. This is exactly the bare-mention-of-names case. Mitigating fact the parent should know: the real content DOES exist and is correct at docs/skills.md:52-64 -- but configuration.md never links to it and actively asserts the document has not landed yet, so a reader following the target doc is sent to source code instead.

**STILL OPEN 6 - [behavior] skill discovery search path** (`thin`)

- Source: `crates/hya-tool/src/skill_catalog.rs:46-62`
- Why it is still open: The gap asked for the ordered ten-directory, first-wins search path in configuration.md, mirroring the existing `## Custom Commands` search-path list. docs/configuration.md:1233-1239 contains ZERO directories -- it only says 'Skill discovery directories, first-wins name resolution ... are documented in the dedicated Skills reference batch'. Nothing in configuration.md lets a reader place a SKILL.md. Again mitigated: docs/skills.md:82-105 has the full correct ordered list (verified against `skill_dirs_for_workdir`, including the HOME-conditional entries), but configuration.md does not cross-reference it.

**STILL OPEN 7 - OAuth catalog media-model filter (ids containing `imagine`/`image`/`video` are dropped)** (`contradicted`)

- Source: `crates/hya-app/src/oauth/models_catalog.rs:196`
- Why it is still open: The filter IS now documented (docs/configuration.md:336-339, `**Catalog filter:**`), but it is written as an unconditional step of the OAuth login flow, placed immediately after a numbered list whose step 2 covers BOTH `openai-codex` and `grok-build` catalog fetches. In the source the filter exists only in `parse_openai_list_catalog` (models_catalog.rs:199-201), which serves the `grok-build` / OpenAI-compatible `GET {base}/models` path. `parse_codex_models_catalog` (models_catalog.rs:124-149) parses the Codex `models[].slug` array with NO media filter, so a Codex slug containing `image`/`video` is written straight into `config.yaml`. A reader following this doc would wrongly conclude Codex catalogs are filtered too.

**STILL OPEN 8 - TUI configuration file — `keybinds` key** (`contradicted`)

- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:42-120, 415-430 (Definitions / KeybindNames / parse)`
- Why it is still open: Line 1264 describes `keybinds` as 'Per-command overrides' and points the reader at 'Full command table: [TUI Keybindings]' as the key vocabulary. That table's identifiers are command names, which are NOT valid keys — `parse()` throws `Unrecognized keybind(s): session.new`. The row even documents the error message it will cause the reader to hit. Everything else in the TUI Configuration table (theme, leader_timeout, attention.*, prompt.max_height/max_width, scroll_speed, scroll_acceleration, diff_style, mouse) I verified line-by-line against config/index.tsx and it is accurate; only the keybinds vocabulary pointer is wrong. (`$schema` is also an accepted optional key and is not listed — minor.)

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
