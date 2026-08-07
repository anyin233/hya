# Fix batch F4 - cli.md, development.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/cli.md`
- `docs/development.md`

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


### `docs/cli.md`

**CONTRADICTION 1**

- The doc claims: Backend-provided commands table: "/init | Guided AGENTS.md setup | Expandable template; supports $ARGUMENTS" and "/review | ... | Expandable; runs as a subtask (subtask: true) in a child session", under the lead-in "Expandable templates are expanded server-side."
- Reality: All seven built-in catalog entries are constructed by command_info(), which sets expandable: false. expand_prompt() only matches entries with expandable == true, so /init and /review are never expanded server-side; command_prompt_text() falls back to the literal "/init <args>". Only CommandInfo::command (user-defined) and CommandInfo::skill set expandable: true.
- Source: `crates/hya-server/src/compat/command_catalog.rs:29-41,91-98,162-179,211,225; crates/hya-server/src/compat/session_legacy.rs:266-278`

**CONTRADICTION 2**

- The doc claims: "The frontend ships a large named keybind registry ... (`Definitions`: on the order of 300 named entries including leader and chord defaults)."
- Reality: The `Definitions` object contains 173 named entries in total (155 bare identifiers plus 18 quoted dotted keys such as "dialog.select.prev" and "prompt.autocomplete.next"), including `leader`. The stated figure is ~1.7x the real count. docs/tui-keybindings.md, which the same paragraph points to as the full table, lists 191 table rows, so no other registry makes up the difference.
- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:45-229`

**CONTRADICTION 3**

- The doc claims: 'operational notices (warnings, readiness lines) go to stderr' (line 28), while the Readiness contract section (line 350-358) treats `hya server listening on <url>` as the readiness line.
- Reality: The readiness line is emitted with `println!("hya server listening on {url}")` — stdout, not stderr. Only the HYA_STARTUP_TRACE backend_listen mark uses eprintln!. hya-sdk works around this by merging both pipes (spawn_line_reader on stdout and stderr), which is why the contradiction is not caught by the SDK.
- Source: `crates/hya-backend/src/serve.rs:58 and :122-126, crates/hya-sdk/src/server.rs:116-117`

**STILL OPEN 1 - Server-provided built-in slash commands (/init, /review, /help, /model, /clear, /sessions, /think)** (`contradicted`)

- Source: `?`
- Why it is still open: The new "Backend-provided commands" table (docs/cli.md:161-169) says the catalog serves "Expandable templates ... expanded server-side" and marks /init as "Expandable template; supports $ARGUMENTS" and /review as "Expandable; runs as a subtask (subtask: true) in a child session". In crates/hya-server/src/compat/command_catalog.rs:175 every built-in goes through command_info(), which hardcodes expandable: false. expand_prompt() (line 98) filters on `item.expandable`, so it returns None for init/review and command_prompt_text() (session_legacy.rs:271) falls back to the literal string `/init <args>`. Only user-defined commands (CommandInfo::command, line 211) and skills (line 225) are expandable:true. The `subtask` half is also unbacked — see the separate finding. A reader following this table would expect the AGENTS.md template to be substituted server-side; it is not. The table is otherwise correct (names, descriptions, literal templates, ${path} substitution, upsert-overrides-builtin).


### `docs/development.md`

**CONTRADICTION 1**

- The doc claims: "| `sync-compat` | Import providers/models/MCP/skills from an OpenCode/Compat config. |"
- Reality: sync_compat::run only performs discover::collect_supported_mcp + discover::collect_skills and applies MCP entries and skill symlinks. There is no provider or model handling anywhere under crates/xtask/src/sync_compat/. Provider/model import is a different code path (`hya --import compat` → hya_app::config::import_compat_models_into_config), and docs/configuration.md:929-930 says so explicitly.
- Source: `crates/xtask/src/sync_compat.rs:6-38; crates/xtask/src/sync_compat/discover.rs; crates/xtask/src/sync_compat/apply.rs; crates/hya-ts/src/main.rs:140-168`

**STILL OPEN 1 - cargo xtask dev commands (sync-compat, migrate, startup-bench, matrix-check)** (`contradicted`)

- Source: `?`
- Why it is still open: Dispatcher behaviour, the usage string, the alias, HYA_BACKEND_BIN on startup-bench and the matrix.toml path are all correct (verified against crates/xtask/src/main.rs:24-32, startup_bench.rs:288, matrix_check.rs:21). But docs/development.md:106 describes sync-compat as "Import providers/models/MCP/skills from an OpenCode/Compat config." sync_compat/run() only calls discover::collect_supported_mcp and discover::collect_skills; grep for `provider`/`model` across crates/xtask/src/sync_compat/discover.rs and apply.rs returns nothing. docs/configuration.md:929-930 states the opposite and correct rule — "Compat provider/model sections are handled by explicit `hya --import compat`, not this xtask. The xtask focuses on MCP and skills." So development.md contradicts both the code and the sibling doc.

**CRITIC 1 - What `cargo xtask sync-compat` actually migrates**

- Source: `crates/xtask/src/sync_compat.rs:30-39 — the task collects only `discover::collect_skills(...)` and `discover::collect_supported_mcp(&config)` and passes exactly those to `apply::apply` / `apply::print_dry_run`. There is no provider or model collection anywhere in crates/xtask/src/sync_compat/.`
- Why it matters: docs/development.md:106 says sync-compat will "Import providers/models/MCP/skills from an OpenCode/Compat config." docs/configuration.md:929-930 says the opposite: "Compat provider/model sections are handled by explicit `hya --import compat`, not this xtask. The xtask focuses on MCP and skills." The source settles it in configuration.md's favour. A user following development.md will run the xtask expecting their providers/models to migrate, get nothing, and have no signal that they needed `hya --import compat`.

**CRITIC 2 - How to invoke the xtask dev tooling**

- Source: `The repository has no `.cargo/` directory and no `[alias]` table anywhere (verified: `ls -a` at repo root shows no `.cargo`; `Cargo.toml` has no alias section). Without a cargo alias, `cargo xtask …` fails with "no such command". crates/xtask is a normal workspace member, so the working invocation is `cargo run -p xtask -- <task>`.`
- Why it matters: docs/development.md:112-114 tells the reader to run `cargo xtask sync-compat --help`, `cargo xtask matrix-check`, `cargo xtask startup-bench`. docs/configuration.md:914 and :935-959 use `cargo run -p xtask -- sync-compat …` for the same tool. Every command in development.md's block fails as an unknown cargo subcommand; configuration.md's form is the one that works.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
