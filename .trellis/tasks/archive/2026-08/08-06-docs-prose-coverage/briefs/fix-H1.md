# Fix batch H1 - configuration.md, cli.md, getting-started.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/configuration.md`
- `docs/cli.md`
- `docs/getting-started.md`

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

- The doc claims: Disk markdown command frontmatter: '`agent` | Switch to this agent profile before the turn.' and '`model` | Switch the submitted turn to this model.', reinforced by 'If `agent` names a built-in TUI profile, hya applies that profile before the turn starts.'
- Reality: No consumer exists. `CommandRequest` carries only `command`, `arguments`, `text`; both `/command` handlers (session_legacy.rs:111-133, session_prompt.rs:156-178) destructure exactly those and then call `session_agent_with_guidance`, i.e. the session's existing agent. The frontmatter `agent`/`model` reach only `CommandInfo::bootstrap_summary` for the `/api/command` listing. Grepping `subtask|agent|model` off the command path shows the only non-test references are the catalog and a skill template. These rows should read like the corrected `subtask` row.
- Source: `crates/hya-proto/src/api.rs:41-49; crates/hya-server/src/compat/session_legacy.rs:94-133; crates/hya-server/src/compat/session_prompt.rs:144-181; crates/hya-server/src/compat/command_catalog.rs:180-196`

**CONTRADICTION 2**

- The doc claims: Inline config commands table: '`agent` | no | Agent override.' and '`model` | no | Model override.'
- Reality: Same defect as the disk table. `InlineCommand` parses `agent`/`model` and `CommandInfo::command` stores them, but they are only ever serialized into the `/api/command` listing; no server or TUI code reads them to change the agent or model for the turn. The TUI's submit path hard-codes the currently selected agent and model into `session.command`, and the server ignores both fields.
- Source: `crates/hya-server/src/compat/command_sources.rs:23-28,110-125; packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:1021-1028`

**CONTRADICTION 3**

- The doc claims: `HYA_STARTUP_TRACE` emits marks shaped `{"hya_startup":true,"mark":"<mark>","wall_ms":…,"detail":…}` (`detail` omitted when none).
- Reality: Accurate for the Rust emitters (hya-ts/src/main.rs:292-295, hya-backend/src/serve.rs:123-126), but the TUI emitter always adds a `mono_ms` field the documented shape does not mention. Harmless for JSON parsers, wrong for anyone matching the line exactly.
- Source: `packages/hya-tui-ts/src/hya/startup-trace.ts:41-49`

**CONTRADICTION 4**

- The doc claims: Environment Variables table, line 695: "`HYA_MODEL` | Active model id when `--model` is not passed and no `default_model` resolves. | Default: `default_model`, else a `sonnet` model, else the first model, else `offline`." This states config.yaml `default_model` wins over the env var.
- Reality: HYA_MODEL takes precedence OVER config.yaml `default_model`. `resolve_runtime` computes `default_model` from `cfg.default_model` first, then does `model_override.or_else(|| std::env::var("HYA_MODEL").ok()).unwrap_or(default_model)` — so an exported HYA_MODEL overrides a configured `default_model`. The row also contradicts the same file's own Model Selection list (lines 541-548: 1. --model, 2. HYA_MODEL, 3. default_model) and the sample-config comment at line 109 ("Model used when neither `--model` nor `HYA_MODEL` is set"). Practical effect: a user who sets `default_model: claude-sonnet-4-6` and also has a stale `HYA_MODEL` exported gets the env model, not the configured one, contrary to this row.
- Source: `crates/hya-app/src/runtime.rs:1280-1282 (resolve_runtime); crates/hya-app/src/config.rs:1457-1470 (choose_default)`

**CONTRADICTION 5**

- The doc claims: Directory manifests, lines 1017-1019: "A subdirectory whose `plugin.toml` is unreadable or unparseable is **skipped** with a notice on stderr rather than failing startup."
- Reality: Only an UNPARSEABLE manifest prints a notice. An unreadable or missing `plugin.toml` is skipped completely silently: `let Ok(contents) = std::fs::read_to_string(&path) else { continue; };` runs before `Manifest::parse`, and only the parse error arm calls `eprintln!("hya: skipping plugin manifest ...")`. A user whose `.hya/plugins/foo/plugin.toml` has wrong permissions or a typo'd filename will see no stderr message at all, contrary to this sentence.
- Source: `crates/hya-app/src/plugins.rs:131-147 (scan_manifests)`

**STILL OPEN 1 - User-defined slash commands from config maps (`command:` / `commands:` in opencode.json / opencode.jsonc)** (`contradicted`)

- Source: `crates/hya-server/src/compat/command_sources.rs:30-43,60,100-107; crates/hya-proto/src/api.rs:41-49; crates/hya-server/src/compat/session_legacy.rs:94-133`
- Why it is still open: The gap's own items ARE now covered (both `command` and `commands` maps are read, all four config paths listed, `template` marked required, upsert-over-builtins explained, and `subtask` correctly demoted to 'no runtime consumer'). But the same field table at docs/configuration.md:1291-1297 still claims `agent` = 'Agent override.' and `model` = 'Model override.' The server's `CommandRequest` (hya-proto/src/api.rs:41-49) only carries `command`, `arguments`, `text`; the `/command` handlers destructure exactly those three and then run the turn with `session_agent_with_guidance` (the session's own agent). Nothing in hya-server or the TUI ever reads a command entry's `agent`/`model` — they are serialized into the `/api/command` listing and dropped, exactly like `subtask`. A reader following this table will configure a per-command agent/model that silently never takes effect.

**STILL OPEN 2 - User-defined slash commands from disk — discovery directories and the `subtask` frontmatter key** (`contradicted`)

- Source: `crates/hya-server/src/compat/command_sources.rs:45-71; crates/hya-proto/src/api.rs:41-49; packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:1010-1028`
- Why it is still open: The originally-contradicted claims were properly DELETED, not supplemented: the two roots are now exactly `.opencode/command` / `.opencode/commands`, the nested-path naming rule (`git/commit.md` -> `/git/commit`) replaced the old 'file stem' claim, and `subtask` was rewritten from 'runs in a child session' to 'no runtime consumer'. However the same frontmatter table (docs/configuration.md:1262-1267) keeps `agent` = 'Switch to this agent profile before the turn' and `model` = 'Switch the submitted turn to this model', and the prose at line 1282-1283 asserts 'If `agent` names a built-in TUI profile, hya applies that profile before the turn starts.' No such code path exists: the TUI sends the *currently selected* agent (`agent: agent.name`, prompt/index.tsx:1024) and the server drops any agent/model on `CommandRequest`. This is the identical defect the writers correctly fixed for `subtask` but left standing for its two neighbouring rows.


### `docs/cli.md`

**CONTRADICTION 1**

- The doc claims: §Keybindings: 'Override any binding by name in TUI config `keybinds`, or unbind it by setting the value to `false` or the string `"none"`.' — stated with no caveat.
- Reality: The shipped launcher supplies `resolve({}, { terminalSuspend: … })` and loads no on-disk TUI config, so a user has no way to supply a `keybinds` map; backend `config.yaml` has no `tui`/`keybinds` key. docs/tui-keybindings.md:7-14 and docs/configuration.md:1407-1409 both state this plainly, so cli.md is the one page that tells a reader to do something that does not work. The schema facts themselves (`false` / `"none"` disable literals, Definition-name keys) are correct.
- Source: `packages/hya-tui-ts/src/main.tsx:53; packages/hya-tui-ts/src/upstream/config/keybind.ts:28-33`


### `docs/getting-started.md`

**CONTRADICTION 1**

- The doc claims: Key controls table, Ctrl-D row: "Exit when the prompt is empty **and** unfocused; deletes forward inside the prompt; deletes the highlighted entry in the Sessions and Stash dialogs."
- Reality: The `app.exit` binding layer is enabled when the prompt is NOT focused **or** its input is empty: `enabled: () => { const current = promptRef.current; if (!current?.focused) return true; return current.current.input === "" }` (app.tsx:847-852). So exit is armed for a focused-but-empty prompt too. The same repo's docs/tui-keybindings.md:43-44 and the Ctrl-C row two lines above in this very table both correctly say "unfocused **or** empty"; only this Ctrl-D row states the stricter AND condition.
- Source: `packages/hya-tui-ts/src/upstream/app.tsx:845-853`

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
