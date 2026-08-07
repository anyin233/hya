# Fix batch G1 - configuration.md, development.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/configuration.md`
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


### `docs/configuration.md`

**CONTRADICTION 1**

- The doc claims: Line 1286, `keybinds` row: "A value may be `false`, `"none"`, a key string, a keystroke object (`event` / `preventDefault` / `fallthrough`), or an array of those."
- Reality: The object form is `BindingObject`, whose `key` field is REQUIRED (`Schema.StructWithRest(Schema.Struct({ key: Schema.Union([Schema.String, KeyStroke]), event?, preventDefault?, fallthrough? }), …)`). The doc lists only the three optional fields and omits the required `key`, so a reader who follows it and writes `app_exit: { event: "press" }` gets a schema decode failure. The doc also conflates two distinct shapes: `KeyStroke` is `{ name, ctrl?, shift?, meta?, super?, hyper? }` and is a separate union member, never mentioned. Finally, "or an array of those" is wrong at the edges — the array member type is `BindingItem` (string | KeyStroke | BindingObject); `false` and `"none"` are top-level-only literals and are rejected inside an array.
- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:8-33 (KeyStroke, BindingObject, BindingItem, BindingValueSchema)`

**CONTRADICTION 2**

- The doc claims: "When the key is absent, the default is `false` (`FormatterConfig::Disabled`). `true` enables the built-in formatter set; a mapping supplies **fully custom entries**." (line 1124-1126, introduced by commit 3e2c1828 "docs: complete hya configuration reference" — the line did not exist before the rewrite)
- Reality: A mapping does NOT supply fully custom entries. `FormatterConfig::Custom(BTreeMap<String, FormatterEntry>)` is doc-commented in source as "Merge custom entries over the builtin set", and `custom_definitions(entries)` begins `let mut definitions = builtin_definitions();` then, per key, either removes (`disabled: true`), merges into the existing builtin definition, or appends a new one. All 26 builtins stay active unless individually disabled. Additionally, `disabled: true` on either `ruff` or `uv` removes both.
- Source: `crates/hya-tool/src/formatter_definition.rs:10-18 (enum doc comment), :93-115 (custom_definitions); crates/hya-tool/src/formatter_catalog.rs:24-155 (builtin list)`

**CONTRADICTION 3**

- The doc claims: "2. Otherwise the last-used effort, kept when it is `none`/`off` or present in the advertised variants." -- presented as step 2 of the live default-reasoning precedence chain a user experiences.
- Reality: `resolve_default_reasoning(explicit, last_used, supported)` has a single production caller, crates/hya-app/src/config.rs:1288, which hardcodes `None` as `last_used`. No other crate (hya-core, hya-server, hya-backend) and no TS TUI code passes a last-used effort. The branch is exercised only by unit tests in crates/hya-provider/src/lib.rs:338/351. Effective shipped precedence is: explicit config default, else highest advertised variant.
- Source: `crates/hya-provider/src/lib.rs:266-286, crates/hya-app/src/config.rs:1286-1292`

**CONTRADICTION 4**

- The doc claims: TUI environment variables table: "`HYA_DISABLE_COPY_ON_SELECT` | Disables copy-on-mouse-selection **and the selection key intercept**. Always true on win32."
- Reality: Same inversion as above for the second half: the selection key intercept is registered under `if (!HyaFlag.disableCopyOnSelect) return`, so the flag enables it. Only copy-on-mouse-selection is disabled. (docs/architecture/tui.md's row for the same variable says just "Disable copy-on-select" and is correct.)
- Source: `packages/hya-tui-ts/src/upstream/app.tsx:399-406`

**STILL OPEN 1 - formatter — map form semantics (and the builtin set it merges over)** (`contradicted`)

- Source: `crates/hya-tool/src/formatter_definition.rs:16-17, 78-115; crates/hya-tool/src/formatter_catalog.rs:24-155`
- Why it is still open: The two facts the gap asked for ARE now correct (absent => false/Disabled at formatter_config.rs:24-27; a parse error prints `hya: formatter config error (...); formatter status disabled` and returns Disabled without aborting startup at formatter_config.rs:73-86). But the same paragraph adds a NEW wrong claim: 'a mapping supplies fully custom entries' (configuration.md:1126). `FormatterConfig::Custom` is documented in source as 'Merge custom entries over the builtin set', and `custom_definitions()` literally starts from `builtin_definitions()` and then merges/removes per key — it does not replace. A reader who writes `formatter: {treefmt: {...}}` expecting only treefmt to run will silently get all 26 builtins plus treefmt. The doc's own example (`gofmt: {disabled: true}`) only makes sense under merge semantics, so the prose contradicts both the source and the example under it. Compounding this, the doc never names a single builtin, so a reader cannot know which keys are valid override targets (gofmt, prettier, oxfmt, biome, ruff, uv, rustfmt, shfmt, nixfmt, clang-format, terraform, … 26 total), nor the special-case that disabling `ruff` OR `uv` drops BOTH python formatters (formatter_definition.rs:95-101).

**STILL OPEN 2 - ReasoningEffort::parse aliases and resolve_default_reasoning last-used precedence** (`contradicted`)

- Source: `crates/hya-provider/src/lib.rs:137, crates/hya-provider/src/lib.rs:209`
- Why it is still open: The alias half is now correct and complete (none/off, med/medium, case-insensitive trim, unknown = config error). The precedence half is wrong as user-facing behavior: docs/configuration.md:280 tells the reader that when `reasoning.default` is omitted the effective default is "the last-used effort, kept when it is none/off or present in the advertised variants", ahead of the highest-supported rule. `resolve_default_reasoning` has exactly one non-test caller in the whole workspace -- crates/hya-app/src/config.rs:1288 -- and it passes `None` for `last_used`. Nothing in hya-app, hya-core, hya-server, or the TS TUI ever supplies a last-used effort (grep for `last_used`/`lastUsed` returns only lib.rs and its unit tests). So step 2 never fires in shipped behavior; a user who reads this section will expect hya to remember the effort they last picked and it never does. The doc needs to either drop step 2 or mark it as an unreached branch of the helper.

**CRITIC 1 - Project `references` / `reference` config block (`@alias` reference aliases, git-repo references, and the per-turn external-directory grant they create)**

- Source: `crates/hya-server/src/compat/reference_entries.rs:5-33 (schema: string shorthand, `{path,description,hidden}`, `{repository,branch,description,hidden}`), reference_entries.rs:57-63 (alias validation), reference_repository.rs:50-57 (branch validation), reference_repository.rs:143-150 (git cache root `$XDG_DATA_HOME/compat/repos`, fallback `~/.local/share/compat/repos`), reference_cache.rs:9-33 (background clone), reference.rs:91-106 (read from the `/config` + `/global/config` bag), reference.rs:108-125 + reference.rs:155-198 (`<available_references>` system-prompt guidance), crates/hya-server/src/compat/session_prompt.rs:115 → crates/hya-core/src/engine/turn.rs:174-211 (turn-scoped `Action::ExternalDirectory` Allow rules)`
- Why it matters: This is a complete, live user/integrator feature with no entry point in docs. Docs mention that "reference aliases" appear in `@` autocomplete (docs/cli.md:214, docs/tui-reference.md:428) and that the prompt path "derives the list from the session's reference directories" (docs/architecture/tools-and-permissions.md:258), but nowhere says where references come from, what keys they take, or that they exist at all as something you configure. Concretely undiscoverable today: (1) the only way to declare them is `PATCH /config` or `PATCH /global/config` with a `references` (or `reference`) object — there is no `config.yaml` key and no on-disk file, so an integrator building a frontend cannot know to send it; (2) a string value starting with `.`/`/`/`~` is a local path while any other string is parsed as a git repository that hya background-clones into `$XDG_DATA_HOME/compat/repos`; (3) aliases containing `/`, whitespace, backtick or comma are silently dropped; (4) security-relevant — every reference path is layered onto the turn's permission snapshot as `Rule { action: ExternalDirectory, resource: "<dir>/*", mode: Allow }`, so tools read/write/shell outside the session workdir with **no permission prompt** for any directory listed there; (5) references carrying a `description` are injected into the system prompt as `<available_references>`, which changes model behavior. docs/architecture/server-client.md:169-181 actively describes `/config` and `/global/config` as an inert "bag" that hya does not consume, which is wrong for this key and steers readers away.

**CRITIC 2 - The built-in formatter set enabled by `formatter: true` — which formatters ship, which file extensions each claims, and the exact argv hya runs on your files**

- Source: `crates/hya-tool/src/formatter_catalog.rs:21-165 (27 `BuiltinSpec` entries: gofmt, mix, prettier, oxfmt, biome, zig, clang-format, ktlint, ruff, rubocop, standardrb, dart, rustfmt, terraform, latexindent, gleam, shfmt, ormolu, cljfmt, dfmt, htmlbeautifier, nixfmt, air, uv, ocamlformat, pint, …, each with an `extensions` list and a `CheckKind` availability probe), crates/hya-tool/src/formatter_command.rs:113-131 (per-formatter argv, e.g. `gofmt -w $FILE`, `rubocop --autocorrect $FILE`, `ktlint -F $FILE`, `terraform fmt $FILE`), crates/hya-tool/src/formatter_definition.rs`
- Why it matters: docs/configuration.md:1118-1155 documents that `formatter: true` "enables the built-in formatter set" and that formatting runs after every successful `write`, `edit`, and `apply_patch` — but never says what that set is. A user who flips the flag has no way to learn from the docs that hya will invoke third-party binaries (`prettier`, `ruff`, `rustfmt`, `rubocop --autocorrect`, `terraform fmt`, `clang-format`, `shfmt`, `nixfmt`, `ktlint -F`, …) that mutate their source files in place after every agent edit. They cannot answer basic questions: which binaries must be on PATH for the flag to do anything; why `.ts` files are being reformatted (prettier, oxfmt and biome all claim `.ts`, so precedence matters); which formatters gate on a local config file or `node_modules` rather than a bare PATH lookup; or which formatter to `disabled: true` to stop an unwanted rewrite. The only formatter names in docs are `gofmt` and `treefmt`, both appearing incidentally inside custom-entry examples.


### `docs/development.md`

**CRITIC 1 - Standard Rust quality-gate command**

- Source: `/chivier-disk/yanweiye/Projects/yaca/.github/workflows/ci.yml:90 (`cargo test --workspace --jobs 1 --exclude hya-e2e`) and :96 (`cargo test -p hya-e2e -- --test-threads=1`)`
- Why it matters: `docs/development.md:23-27` gives the "standard gate" as `cargo fmt --all --check` / `cargo clippy …` / `cargo test --workspace` (no exclusion). `docs/testing/README.md:26-32` gives the "Default quality gate" as `cargo test --workspace --exclude hya-e2e` and explicitly says "`--exclude hya-e2e` matches CI: Track P spawns real backend processes and is run separately below with `--test-threads=1`." Following development.md runs the process-E2E suite multi-threaded and produces spurious port/process failures. `AGENTS.md:120-123` repeats the un-excluded form too.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
