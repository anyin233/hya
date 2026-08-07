# Fix batch J1 - configuration.md, cli.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/configuration.md`
- `docs/cli.md`

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

- The doc claims: HYA_* table row: "`HYA_DB` | Session SQLite path for the backend. Empty string forces in-memory."
- Reality: HYA_DB is read only by `hya_sdk::server::default_session_db_path` (crates/hya-sdk/src/server.rs:157-165), i.e. only when `hya` / `hya-ts` spawns an owned backend and decides whether to append `--db <path>` to the `hya-backend serve` argv. `hya-backend` itself never reads HYA_DB — grep across crates shows HYA_DB_ENV used nowhere outside hya-sdk. Describing it as "the session SQLite path for the backend" implies it configures a directly-invoked `hya-backend`, where it has no effect at all (that path is governed by `--db` plus `resolve_interactive_db`). docs/cli.md:104-108 scopes this correctly ("Owned backends started by `hya`"); the configuration.md row does not.
- Source: `crates/hya-sdk/src/server.rs:38,74-86,155-165`

**CONTRADICTION 2**

- The doc claims: "**Python pair:** `disabled: true` on **either** `ruff` **or** `uv` removes **both** from the active set (they share `.py` / `.pyi`)." (Formatter section, after the custom-entry field list.)
- Reality: `custom_definitions` computes `disable_python` as `entries.get("ruff").or_else(|| entries.get("uv")).is_some_and(|e| e.disabled)`. `or_else` is only consulted when the `ruff` key is ABSENT. If a `ruff` entry exists and is not disabled, a `uv: { disabled: true }` entry removes only `uv`; `ruff` survives. Failure scenario: `formatter: { ruff: { extensions: [.py] }, uv: { disabled: true } }` -> the doc says both are gone, but `ruff` is still active and still formats `.py` files on write/edit.
- Source: `crates/hya-tool/src/formatter_definition.rs:93-101 (custom_definitions / disable_python)`

**CONTRADICTION 3**

- The doc claims: Plugin directory manifests are discovered from `<workdir>/.hya/plugins/<name>/plugin.toml` — repeated in the sample config comment, the Plugins intro, and the "Directory manifests" heading ("Layout: `<workdir>/.hya/plugins/<name>/plugin.toml`").
- Reality: `plugins_dir()` returns `std::env::current_dir()?.join(".hya/plugins")` — the backend PROCESS working directory — and it is evaluated exactly once during runtime composition (`plugins::resolve(cfg.plugins, plugins::plugins_dir().as_deref())`). It is not the per-session/per-request workdir, which the server resolves independently (`Engine::bind_root_runtime(workdir)`). Failure scenario: run `hya-backend serve` from `~` and drive a session whose workdir is `/proj` containing `/proj/.hya/plugins/memory/plugin.toml`; the doc says the manifest is discovered, but nothing under `/proj` is ever scanned — only `~/.hya/plugins`. The two coincide only on the launcher path, where `hya` spawns the backend with `.current_dir(project)`.
- Source: `crates/hya-app/src/plugins.rs:11-14 (plugins_dir), crates/hya-app/src/runtime.rs:1298, crates/hya-core/src/engine.rs:492-495, crates/hya-sdk/src/server.rs:97`

**CONTRADICTION 4**

- The doc claims: Non-`HYA_` environment table: '`EDITOR` / `VISUAL` | External editor for the TUI `/editor` slash command (`<leader>e`). `$VISUAL` is preferred when set.'
- Reality: `/editor` is not the only consumer. `session.export` (`/export`, `<leader>x`) also calls `openEditor` on both of its paths, so `$VISUAL`/`$EDITOR` governs export too — and in the saving path the editor's output is written back over the exported `.md`. Scoping the variable to `/editor` alone understates its blast radius.
- Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:1124-1152, packages/hya-tui-ts/src/upstream/editor.ts:27`

**STILL OPEN 1 - TUI XDG data/cache/config/state directories and the files it writes (model.json recents+favorites, session pins, worktree root)** (`thin`)

- Source: `packages/hya-tui-ts/src/hya/platform.ts:6, packages/hya-tui-ts/src/upstream/context/local.tsx:418, packages/hya-tui-ts/src/upstream/context/kv.tsx:12`
- Why it is still open: The new 'Where the TUI stores state' table names exactly one file (`model.json`) and then hand-waves: 'session pin list for nine quick-switch slots, other KV'. The pin list is `<state>/session.json` (`path.join(paths.state, "session.json")` in context/local.tsx) and every KV flag the rest of the doc set tells the reader about — `thinking_mode`, `tips_hidden`, `diff_wrap_mode`, `terminal_title_enabled`, `paste_summary_enabled`, `sidebar`, `timestamps`, `scrollbar_visible`, `tool_details_visibility`, `generic_tool_output_visibility`, `which_key_layout`, `which_key_pending_preview`, `diff_viewer_show_file_tree`, `diff_viewer_single_patch`, `diff_viewer_view`, `attention_sound_pack` — is persisted to `<state>/kv.json` (context/kv.tsx). Neither filename appears anywhere in docs/. A reader who is told 'persisted in KV as X' has no way to find, inspect, or reset it. The XDG base rows themselves and the model.json semantics are correct.

**CRITIC 1 - The list of accepted provider `kind` values in config.yaml**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-provider/src/http.rs:32-45 — `enum ProviderKind` has six variants: `OpenAiCompatible`, `OpenAiResponse`, `OpenAiCodex`, `GrokBuild`, `Anthropic`, `Google`.`
- Why it matters: docs/architecture/providers.md:170 states "Six `ProviderKind` values (not three)" and tables all six, and docs/configuration.md:227-232 also tables all six (including `openai-codex`). But the annotated sample config at docs/configuration.md:159 says `kind: anthropic  # openai-completion | openai-response | grok-build | anthropic | google` — five values, silently omitting `openai-codex`, which the very same document documents at :229 and :417-428 as a supported kind with its own OAuth flow. A reader copying the sample config would conclude `openai-codex` is not a legal value.


### `docs/cli.md`

**CONTRADICTION 1**

- The doc claims: "User-defined commands from config and on-disk command sources are merged with this list via upsert: a user-defined command with the same name overrides the built-in of that name (and may set `expandable: true` so the template is expanded server-side)."
- Reality: `expandable` is not a user-settable field anywhere. `CommandInfo::command` (command_catalog.rs:197-214) hardcodes `expandable: true` for every command built from `command_sources`, and neither `CommandFrontmatter` (command_sources.rs:8-14: description, agent, model, subtask) nor `InlineCommand` (lines 22-29: template, description, agent, model, subtask) has an `expandable` key. The parenthetical reads as an authoring option; a user who writes `expandable: true` in frontmatter or opencode.json gets it silently dropped (no deny_unknown_fields). The correct statement is that every user-defined command is unconditionally expandable.
- Source: `crates/hya-server/src/compat/command_catalog.rs:197-214; crates/hya-server/src/compat/command_sources.rs:8-29`

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
