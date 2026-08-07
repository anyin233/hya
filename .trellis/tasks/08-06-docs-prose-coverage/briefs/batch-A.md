# Batch A - configuration.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/configuration.md`

You have **66 gap entries** and **8 stale claims** to resolve.

This is the largest single file in the wave (62 gaps). Write only a POINTER STUB for Skills -- Batch L owns the authoring content -- and only a POINTER to docs/tui-keybindings.md for slash commands, which already contains the full table. You DO own the TUI environment-variable reference table.

## Non-negotiable rules

1. **Confirm every claim against the source before you write it.** Every entry
   below carries a `source` reference. Open it. If the source contradicts the
   entry, the SOURCE WINS -- write what the code does and report the discrepancy.
2. **If you cannot confirm a claim from source, do not write it.** Say you could
   not confirm it. Plausible prose that is wrong is worse than an admitted gap,
   because a reader trusts the document.
3. **Stale and contradicted entries are corrected or deleted, never merely
   supplemented.** A document that contradicts the code is a defect.
4. **Do not edit any file outside your batch.** Other writers are working in
   parallel. In particular never touch `docs/README.md`, `README.md`, `AGENTS.md`,
   `DESIGN.md`, or `docs/project-structure.md` -- a later reconciliation pass owns
   all cross-links and the docs map. Some entries below suggest edits to other
   files; ignore that part and write only your own.
5. **Match the existing documentation style.** Read the file you are editing
   before writing. Use the project's vocabulary as defined in `CONTEXT.md`.
6. **A feature counts as documented only if a reader can use it** from what you
   write: what it does, its parameters or keys, and its semantics. A name in a
   list does not count. 31 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/configuration.md`

**1. [api] plugin hook name vocabulary** — `thin` · severity medium

- Source: `crates/hya-plugin/src/messages.rs:23-46`
- Evidence: docs/architecture/runtime.md:132 states "Prepared canonical hook IDs are limited to `event`, `tool.execute.before`, and `tool.execute.after`" — but that is the BUNDLE restriction. The full plugin wire vocabulary (11 names) is nowhere; grep for `goal.evaluate`, `loop.verifier`, `chat.params`, `message.user.before` across in-scope docs returns nothing.
- Write: In the `## Plugins` section, add a table of the hook names a plugin may declare in its initialize handshake, and their posture (Safe or Open): `event`, `command.execute.before`, `experimental.text.complete`, `message.user.before`, `chat.params`, `tool.execute.before`, `tool.execute.after`, `permission.ask`, `goal.evaluate`, `loop.verifier`, `loop.planner`. State explicitly that AgentBundle sidecars may select only the three bundle-legal IDs (`event`, `tool.execute.before`, `tool.execute.after`) — the wider list applies to config-declared plugins only — so the runtime.md sentence is not contradicted.

**2. [behavior] MCP connection handshake** — `undocumented` · severity medium

- Source: `crates/hya-mcp/src/manager.rs:178-210`
- Evidence: docs/configuration.md:447-457 and docs/architecture/runtime.md describe MCP status composition and reconciliation, but the handshake itself is undocumented: grep for `notifications/initialized`, `2025-06-18`, `tools/list` and `resources/list` across in-scope docs returns nothing.
- Write: In the `## MCP Servers` section, document the connection sequence so authors can debug a failing server: hya spawns `command` over stdio, sends `initialize` with `protocolVersion: 2025-06-18` and `clientInfo` naming hya under a 5-second timeout, then sends the required `notifications/initialized`, then `tools/list`, then a best-effort `resources/list`. Only the first four steps are mandatory — a failing `resources/list` still leaves the server Connected with zero resources. Note `timeout_ms` defaults to 30 s (crates/hya-mcp/src/client.rs:15) and applies to subsequent requests, not to the 5 s initialize bound.

**3. [behavior] remote (url) MCP servers unsupported** — `thin` · severity medium

- Source: `crates/hya-app/src/config.rs:928-930`
- Evidence: docs/configuration.md:48 and docs/getting-started.md:143 say the import brings in "supported local MCP servers", which only hints at the limitation. docs/configuration.md's MCP section documents `command` as "stdio command for the server process" but never states that url/remote entries are silently dropped, and docs/hya-pi-compat-comparison.md:132 lists remote MCP as a Compat feature without saying hya lacks it.
- Write: State the limitation explicitly in `## MCP Servers`: hya supports stdio/local MCP servers only. `mcp.<name>.command` is an argv array; there is no `url` / remote transport key. When importing a Compat/OpenCode config, only entries with `type: local` are kept — `type: remote` / url entries are dropped silently, not converted and not warned about. Point users who need a remote server at a local stdio proxy.

**4. [config-key] skill allowed-tools frontmatter** — `undocumented` · severity high

- Source: `crates/hya-tool/src/skill_catalog.rs:28-43`
- Evidence: Grep for `allowed-tools` across docs/, README.md, CONTEXT.md, DESIGN.md, AGENTS.md, CHANGELOG.md returns ZERO matches. docs/configuration.md documents custom-command frontmatter (line ~685) but has no Skills section at all; docs/opencode-feature-inventory.md:21 only lists "frontmatter validation" as an upstream feature to reach parity with.
- Write: Add a `## Skills` section documenting SKILL.md YAML frontmatter. Fields: `name` and `description` (used for the available-skills summary injected into the system prompt), `allowed-tools` (a per-skill tool allowlist; an empty or absent list means no restriction), `model` (per-skill model override), `disable` (`true` drops the skill from discovery entirely so it never appears in the catalog or the prompt), and `license`. State that every field beyond name/description is optional so minimal existing skills keep parsing.

**5. [behavior] skill discovery search path** — `thin` · severity high

- Source: `crates/hya-tool/src/skill_catalog.rs:46-62`
- Evidence: docs/hya-pi-compat-comparison.md:80 mentions "hya skill locations such as `.hya/skills` and user config skill directories" and docs/compat-parity.md:87 names only `.hya/skills` and `~/.config/hya/skills`. The full ten-directory, first-wins search order is in no in-scope doc, and docs/configuration.md has no Skills section.
- Write: In the new `## Skills` section, give the discovery search path as an ordered list, and state that resolution is FIRST-WINS BY SKILL NAME so an earlier directory shadows a later one: 1. `<workdir>/.hya/skills`, 2. `~/.config/hya/skills`, 3. `~/.claude/skills`, 4. `~/.config/opencode/skills`, 5. `~/.config/opencode/skill`, 6. `<workdir>/.opencode/skills`, 7. `<workdir>/.opencode/skill`, 8. `<workdir>/.agents/skills`, 9. `~/.codex/skills`, 10. `~/.agents/skills`. Mirror the formatting of the existing `## Custom Commands` search-path list.

**6. User-defined slash commands from config maps (`command:` / `commands:` in opencode.json / opencode.jsonc)** — `undocumented` · severity high

- Source: `crates/hya-server/src/compat/command_sources.rs:30-43,60,100-107`
- Evidence: docs/configuration.md '## Custom Commands' (lines 673-700) documents ONLY markdown files. Neither `command:` nor `commands:` as a config map, nor the four scanned files (<workdir>/opencode.json, <workdir>/opencode.jsonc, <workdir>/.opencode/opencode.json, <workdir>/.opencode/opencode.jsonc), appear anywhere in the in-scope docs.
- Write: In '## Custom Commands', add a subsection documenting inline config commands. hya reads BOTH a `command` map and a `commands` map from, in order: <workdir>/opencode.json, <workdir>/opencode.jsonc, <workdir>/.opencode/opencode.json, <workdir>/.opencode/opencode.jsonc (JSONC is accepted, i.e. comments are allowed). Each entry is keyed by the command name and has the fields: `template` (required, the prompt body), `description`, `agent`, `model`, `subtask` (boolean). These are upserted over the backend built-ins, so an entry named `review` replaces the built-in /review. Give a short JSON example.

**7. User-defined slash commands from disk — actual discovery directories and the `subtask` frontmatter key** — `contradicted` · severity high

- Source: `crates/hya-server/src/compat/command_sources.rs:45-71`
- Evidence: docs/configuration.md:675-682 lists six directories. The code (`disk_commands`) scans only two: <workdir>/.opencode/command and <workdir>/.opencode/commands. `grep -rn 'hya/prompts|config/opencode/command|\.hya/prompts' crates packages` returns NOTHING, so $HOME/.config/opencode/commands, $HOME/.config/opencode/command, $HOME/.config/hya/prompts and <workdir>/.hya/prompts are not read by any code path. The frontmatter example (lines 687-696) also omits `subtask`, which the parser accepts (CommandFrontmatter.subtask, line 12).
- Write: Rewrite the numbered source list in '## Custom Commands' to the two directories the code actually scans: 1. <workdir>/.opencode/command/**/*.md, 2. <workdir>/.opencode/commands/**/*.md. Files are collected recursively and sorted by path; the file stem becomes the slash-command name. Delete the four home-directory / .hya/prompts entries and the 'Project commands override user commands with the same file stem' sentence (there is no user tier). Add `subtask: true` to the frontmatter key list and the example, and say it makes the command run in a child session. Keep the existing $ARGUMENTS / $1 / $2 templating text.

**8. env HYA_SUBAGENT_MAX_DEPTH, HYA_SUBAGENT_MAX_CONCURRENCY, HYA_SUBAGENT_BUDGET, HYA_SUBAGENT_TURN_BUDGET, HYA_SUBAGENT_MESSAGE_BUDGET (5 features)** — `undocumented` · severity high

- Source: `crates/hya-app/src/config.rs:1332,1337,1342,1347,1352`
- Evidence: None of the five names appears in any in-scope doc. `grep -in 'subagent|max_depth|max_concurrency|budget' docs/configuration.md` returns nothing at all, so there is no config-key equivalent documented either. docs/configuration.md:412-414 nonetheless claims its table is the list of HYA_* variables 'verified against the source'.
- Write: Add five rows to the '## Environment Variables' HYA_* table, each with Effect / Default / Source columns, source `crates/hya-app/src/config.rs`. HYA_SUBAGENT_MAX_DEPTH — overrides the subagent spawn max-depth limit otherwise resolved from config. HYA_SUBAGENT_MAX_CONCURRENCY — overrides the cap on concurrently running subagents. HYA_SUBAGENT_BUDGET — overrides the per-run subagent budget. HYA_SUBAGENT_TURN_BUDGET — overrides the per-team turn budget. HYA_SUBAGENT_MESSAGE_BUDGET — overrides the per-team message budget. For each, state that it takes precedence over the config-resolved value and that unparseable values fall back to the config value.

**9. env HYA_STARTUP_TRACE** — `undocumented` · severity medium

- Source: `crates/hya-ts/src/main.rs:269; crates/hya-backend/src/serve.rs:106; packages/hya-tui-ts/src/hya/startup-trace.ts:8`
- Evidence: Zero hits for HYA_STARTUP_TRACE across docs/**, README.md, CONTEXT.md, DESIGN.md, AGENTS.md, CHANGELOG.md.
- Write: Add a HYA_STARTUP_TRACE row to the HYA_* table. When set to `1` or `true` (case-insensitive; any other value is off), all three layers emit newline-delimited JSON to STDERR of the form {"hya_startup":true,"mark":"<mark>","wall_ms":<unix_ms>,"detail":"<escaped>"} (the `detail` field is omitted when there is none). Marks emitted by hya-ts include hya_ts_start, backend_spawn, and backend_listen; the backend and the TypeScript TUI emit their own marks. Default: off. Sources: crates/hya-ts/src/main.rs, crates/hya-backend/src/serve.rs, packages/hya-tui-ts/src/hya/startup-trace.ts. Also mention it in docs/troubleshooting.md as the first tool for slow-startup reports.

**10. env HYA_EVENT_BUS_CAPACITY** — `undocumented` · severity medium

- Source: `crates/hya-app/src/config.rs:1365`
- Evidence: Zero hits across all in-scope docs.
- Write: Add a HYA_EVENT_BUS_CAPACITY row to the HYA_* table: overrides the live EventBus broadcast channel capacity. Only a valid usize strictly greater than 0 is honored; anything else (unparseable, or 0) falls back to hya_core::bus::DEFAULT_BUS_CAPACITY. Source crates/hya-app/src/config.rs. Note that raising it trades memory for tolerance of slow SSE consumers.

**11. env HYA_DEFER_SIDEPLANES** — `thin` · severity medium

- Source: `crates/hya-app/src/runtime.rs:3868`
- Evidence: Mentioned only in test-harness contexts: docs/troubleshooting.md:125 ('the harness sets HYA_DEFER_SIDEPLANES=0 for MCP fixtures') and docs/testing/process-e2e.md:40. It is absent from the docs/configuration.md HYA_* table, so a user reading the 'complete' env reference cannot discover it, and neither doc states the default or the accepted off-values.
- Write: Add a HYA_DEFER_SIDEPLANES row to the HYA_* table. Default: ON. When on, hya binds the HTTP listener first and connects MCP servers afterwards, so startup does not block on slow MCP servers — meaning MCP tools may not be registered for the very first prompt. Set it to `0`, `false`, `off`, or `no` (case-insensitive) to restore the classic await-MCP-before-listen startup path. Cross-reference docs/testing/process-e2e.md, which relies on this. Source crates/hya-app/src/runtime.rs.

**12. env HYA_TUI_TS_DIR and env HYA_BACKEND_BIN (2 features) — resolution order** — `thin` · severity medium

- Source: `crates/hya-ts/src/main.rs:43,57; crates/hya-ts/src/lib.rs:355,380`
- Evidence: docs/architecture/tui.md:31 names both only as 'explicit development or diagnostic overrides' with no precedence, no fallback chain, and no value semantics. Neither appears in the docs/configuration.md HYA_* table that claims completeness, and neither appears in docs/cli.md.
- Write: Add both to the HYA_* table with their full resolution chains. HYA_TUI_TS_DIR — highest-priority override for the hya-tui-ts runtime directory; resolution order is (1) HYA_TUI_TS_DIR, (2) <exe_dir>/../lib/hya/hya-tui-ts (installed/release layout), (3) <workspace>/packages/hya-tui-ts (dev layout); source crates/hya-ts/src/lib.rs. HYA_BACKEND_BIN — path to the hya-backend binary; consulted AFTER the --backend-bin flag and BEFORE the sibling-executable and target/{release,debug} fallbacks; also read by the startup bench (crates/xtask/src/startup_bench.rs); source crates/hya-ts/src/lib.rs.

**13. env EDITOR / VISUAL (and the terminal-integration probes TERM_PROGRAM, TMUX, STY, ZED_TERM, DISPLAY, WAYLAND_DISPLAY, OPENCODE_EDITOR_SSE_PORT, OPENCODE_ZED_DB, CLAUDE_CODE_SSE_PORT)** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/editor.ts`
- Evidence: Zero hits for EDITOR or VISUAL in any in-scope doc. The /editor slash command that consumes them is also undocumented.
- Write: Add a row to the 'Related, non-HYA_ variables' table: EDITOR / VISUAL — selects the external editor opened by the TUI /editor slash command (<leader>e). Add a sentence noting that the frontend additionally probes TERM_PROGRAM, TMUX, STY, ZED_TERM, DISPLAY, WAYLAND_DISPLAY, OPENCODE_EDITOR_SSE_PORT, OPENCODE_ZED_DB, and CLAUDE_CODE_SSE_PORT to detect editor/terminal integration, and that none of those are hya-specific settings. Source packages/hya-tui-ts/src/upstream/editor.ts.

**14. env SHELL** — `undocumented` · severity low

- Source: `crates/hya-server/src/compat/pty_payload.rs:88; crates/hya-server/src/compat/pty_shell.rs:20`
- Evidence: No in-scope doc mentions SHELL as an input. docs/compat-parity.md discusses PTY routes but never the shell selection.
- Write: Add a row to the 'Related, non-HYA_ variables' table: SHELL — the shell program used for PTY sessions created through the Compat PTY routes; defaults to /bin/sh when unset or empty. Sources crates/hya-server/src/compat/pty_payload.rs and crates/hya-server/src/compat/pty_shell.rs.

**15. env COMPAT_REPO_CLONE_GITHUB_BASE_URL** — `undocumented` · severity low

- Source: `crates/hya-server/src/compat/reference_repository.rs:160`
- Evidence: Zero hits in any in-scope doc, even though its siblings COMPAT_CONFIG and COMPAT_WEBSEARCH_PROVIDER are both in the docs/configuration.md tables.
- Write: Add a row to the 'Related, non-HYA_ variables' table: COMPAT_REPO_CLONE_GITHUB_BASE_URL — overrides the GitHub base URL used when cloning reference repositories (useful for GitHub Enterprise or an internal mirror). Source crates/hya-server/src/compat/reference_repository.rs. While editing, also note that XDG_DATA_HOME (falling back to $HOME) is the base for the reference-repository store as well as the installed-bundle data dir.

**16. env HYA_VERSION and HYA_CHANNEL (2 features)** — `undocumented` · severity low

- Source: `packages/hya-tui-ts/src/hya/platform.ts:34`
- Evidence: Zero hits for HYA_VERSION or HYA_CHANNEL across all in-scope docs.
- Write: Add two rows to the HYA_* table: HYA_VERSION — the version string surfaced by the TUI (status line / help), default the literal `local` when unset; HYA_CHANNEL — the release channel surfaced by the TUI, default the literal `local` when unset. Both are normally injected by the release build, so seeing `local` in the TUI means an unreleased/dev build. Source packages/hya-tui-ts/src/hya/platform.ts.

**17. config file format: YAML only (serde_norway); unknown top-level keys silently ignored; empty/whitespace file == no config** — `thin` · severity medium

- Source: `crates/hya-app/src/config.rs:641-643, 1435-1437`
- Evidence: docs/configuration.md documents the file paths and says an empty file falls back to offline, but never states the parser is strict YAML (serde_norway), never states that unknown/misspelled top-level keys are silently ignored (no deny_unknown_fields), so a typo like `provider:` instead of `providers:` produces no error at all.
- Write: In the intro of docs/configuration.md, add one paragraph: the file is parsed strictly as YAML (serde_norway::from_str, config.rs:641-643); JSON/TOML are not accepted. Unknown top-level keys are ignored without warning — a misspelled section (`provider:` vs `providers:`) is silently dropped, so verify with `hya-backend models` after editing. A file that is empty or whitespace-only is treated exactly like a missing file (config.rs:1435-1437) and hya runs offline.

**18. providers.<id>.models[].reasoning.variants — per-provider-kind default variant sets** — `thin` · severity medium

- Source: `crates/hya-provider/src/http.rs:43-54 (referenced from crates/hya-app/src/config.rs:200, 1204-1221)`
- Evidence: docs/configuration.md:279-295 documents the `variants` key and the full value vocabulary, and documents grok-build's fallback set (line 196-198), but gives no default variant set for anthropic, openai/openai-compatible, openai-response/openai-codex, or google. A user cannot tell what they get when they omit `variants`.
- Write: In the reasoning subsection of the Providers section, add a table of the per-`kind` default variant lists taken from crates/hya-provider/src/http.rs:43-54: anthropic = low, medium, high, max; openai / openai-compatible / openai-completion = minimal, low, medium, high, xhigh; openai-response and openai-codex = none, minimal, low, medium, high, xhigh, max; grok-build = low, medium, high; google = high, max. State that `variants` replaces (does not extend) this kind default, and that when `reasoning.default` is omitted the effective default is the highest effort in the resulting list (ordering Off < Minimal < Low < Medium < High < XHigh < Max).

**19. mcp.<name>.timeout_ms and plugins.<id>.timeout_ms — default values when omitted** — `thin` · severity low

- Source: `crates/hya-mcp/src/client.rs:15 and crates/hya-mcp/src/manager.rs:182-185; crates/hya-plugin/src/client.rs:26-28`
- Evidence: docs/configuration.md:133 and :627 list `timeout_ms` as "Optional request timeout" with no unit-independent default. Neither doc states what happens when the key is omitted.
- Write: In both the MCP Servers table/example and the Plugins field table, state that `timeout_ms` is milliseconds and that when omitted the per-call timeout is 30 s (DEFAULT_CALL_TIMEOUT, crates/hya-mcp/src/client.rs:15 and crates/hya-plugin/src/client.rs:26). For plugins also note the two fixed non-configurable timeouts: initialize 5 s and shutdown 1 s (crates/hya-plugin/src/client.rs:27-28).

**20. plugins.<id>.env is NOT run through {env:}/{file:} secret templating (unlike mcp.<name>.env)** — `thin` · severity medium

- Source: `crates/hya-plugin/src/config.rs:25 (contrast crates/hya-app/src/config.rs:1274-1284)`
- Evidence: docs/configuration.md:131-132 explicitly says MCP `env` values accept `{env:}`/`{file:}`, and docs/configuration.md:628 says plugin `env` is "Environment variables passed to the plugin process as configured" — the asymmetry is never stated, so a reader will reasonably copy the MCP template syntax into a plugin block and get the literal string `{env:TOKEN}` passed to the child.
- Write: In the Plugins field table row for `env`, add an explicit warning: plugin `env` values are passed VERBATIM to the child process; the `{env:VAR}` / `{file:/path}` secret templating supported by `providers.<id>.api_key` and `mcp.<name>.env` is NOT applied here (crates/hya-plugin/src/config.rs:25). Export the variable in the parent shell instead. Show the contrast with the MCP example already in the doc.

**21. subagents block: subagents.max_depth / max_concurrency / per_run_budget / per_team_turn_budget / per_team_message_budget** — `undocumented` · severity high

- Source: `crates/hya-app/src/config.rs:156-166; crates/hya-core/src/orchestrator.rs:18, 48-54`
- Evidence: grep for `subagents`, `max_depth`, `max_concurrency`, `per_run_budget`, `per_team_turn_budget`, `per_team_message_budget` across docs/**/*.md and the root docs returns no hit for the config block (only unrelated prose in ADR-0003 and a test-harness `tree_max_depth` field in docs/testing/process-e2e.md). Only rustdoc on config.rs:151-166 mentions it. docs/configuration.md has no `subagents:` section at all.
- Write: Add a new `## Subagent Limits` section to docs/configuration.md with a YAML example and a table of all five keys, their types, and their defaults: `subagents.max_depth` (u32, default 5 — maximum nesting depth of subagent spawns, lead session = depth 0); `subagents.max_concurrency` (usize, default 100 = DEFAULT_GENERAL_STREAM_PERMITS — ceiling on concurrently streaming general members, normalized into 1..=100, excess members park rather than fail); `subagents.per_run_budget` (u64, default 1024 — maximum total members spawned under one top-level run); `subagents.per_team_turn_budget` (u64, default 1024 — total resident turns one team may run; tripping it KILLS the team, the runaway re-wake backstop from ADR-0002); `subagents.per_team_message_budget` (u64, default 1024 — total MailSent events one team may emit; tripping it KILLS the team, the A<->B message-loop backstop). Note that every field is optional and omitted fields keep their default. Sources: crates/hya-app/src/config.rs:156-166, crates/hya-core/src/orchestrator.rs:18, 48-54. Link to docs/adr/0002-resident-actor-model-and-autonomous-main-agent.md.

**22. categories — logical model categories block** — `thin` · severity high

- Source: `crates/hya-app/src/config.rs:100, 1377-1385`
- Evidence: docs/adr/0004-model-category-resolution-and-precedence.md:20-21 says categories are "config-driven concrete refs under a `categories:` block in ~/.config/hya/config.yaml" but shows no YAML, no key shape, and no failover semantics for the file format. docs/configuration.md — the doc a user actually reads to write config.yaml — never mentions `categories` at all.
- Write: Add a `## Model Categories` section to docs/configuration.md with a YAML example, e.g. `categories:\n  deep:\n    - anthropic/claude-sonnet-4-6\n    - gateway/gpt-5.6-sol`. Document the shape as a map of category name -> ordered list of concrete `provider/model` refs; the first entry is preferred and the rest form a spawn-time failover chain (first candidate whose provider is configured wins). State that a category with an empty list is dropped at load (config.rs:1377-1385), that there are NO built-in categories (the old `tier-cheap/strong/max/writer` placeholders were removed), and that an unknown category simply fails to resolve and falls back through the precedence chain to the global default model. Cross-link docs/adr/0004-model-category-resolution-and-precedence.md for the full spawn/inline/bundle precedence order.

**23. formatter — default value and the fact that a formatter parse error never fails startup** — `thin` · severity low

- Source: `crates/hya-app/src/formatter_config.rs:12, 15-26, 40-53, 79-82, 94-104`
- Evidence: docs/configuration.md:649-671 shows `formatter: true` and the map form and lists the per-entry fields, but never states the default when the key is absent (false = Disabled) nor that the formatter block is read by a SEPARATE parser from the same config.yaml, so a malformed formatter block only prints `hya: formatter config error (...); formatter status disabled` and disables formatting instead of failing startup or forcing offline.
- Write: In the Formatter section add: `formatter` is untagged — it is either a bool or a map. When the key is absent the default is `false` (FormatterConfig::Disabled, formatter_config.rs:12,40-53); `true` enables the built-in formatter set; a mapping supplies fully custom entries. Also state that the formatter block is parsed independently of the rest of config.yaml (formatter_config.rs:94-104): a parse error there disables only formatting and prints `hya: formatter config error (...); formatter status disabled` on stderr (formatter_config.rs:79-82) — it does not abort startup and does not push hya offline. Note `$FILE` in `command` is the placeholder for the file being formatted.

**24. auth file schema ~/.config/hya/auth/<provider>.yaml (plain API-key form)** — `thin` · severity medium

- Source: `crates/hya-app/src/auth.rs:114-171, 199-205`
- Evidence: docs/configuration.md:317-319 and :224-226 document the path and the OAuth bundle fields (access_token, refresh_token, expires_at, account_id) but never document the full key set, in particular the plain API-key form `type: api` + `token`, nor `oauth_type`, nor `id_token`, nor that the auth directory uses $XDG_CONFIG_HOME unconditionally when it is set (unlike config.yaml discovery).
- Write: In the Auth Tokens section add a small schema block for `~/.config/hya/auth/<provider>.yaml`: `type` (default `api`, or `oauth`), `token` (the plain API key for `type: api`), and for `type: oauth` also `oauth_type` (`openai-codex` | `grok-build`), `access_token`, `refresh_token`, `expires_at` (RFC3339 UTC), optional `account_id`, optional `id_token` (auth.rs:114-171). State that the directory resolves to `$XDG_CONFIG_HOME/hya/auth` when that variable is set, otherwise `$HOME/.config/hya/auth` (auth.rs:199-205), and that any saved credential always beats an inline `providers.<id>.api_key`.

**25. Compat config format: strict JSON first, JSONC (comments + trailing commas) fallback** — `thin` · severity low

- Source: `crates/hya-app/src/config.rs:845-855, 1087-1185`
- Evidence: docs/configuration.md:50-51 lists the candidate filenames including `opencode.jsonc` but nowhere states that a `.json` file with comments also parses, or how the JSONC fallback works.
- Write: In the Compat migration subsection, add one sentence: the discovered Compat config is parsed as strict JSON first; if that fails, `//` and `/* */` comments and trailing commas are stripped and it is re-parsed as JSONC (config.rs:845-855, 1087-1185). This applies to any candidate filename, so a commented `opencode.json` also imports.

**26. Compat import provider-kind inference** — `undocumented` · severity medium

- Source: `crates/hya-app/src/config.rs:942-957`
- Evidence: docs/configuration.md:48-52 and :539-545 describe what the import copies (base URLs, model ids, keys, local MCP) but never say how the resulting hya `kind` is chosen. No doc in scope mentions the inference at all.
- Write: In the Compat migration subsection, document the kind inference: hya lowercases the Compat provider id plus its npm package and display name and guesses `kind` from that string — containing "anthropic" -> `anthropic`; containing "google" or "gemini" -> `google`; anything else -> `openai-compatible` (config.rs:942-957). Tell the user to review and fix `kind` by hand after `hya --import compat` for providers that need `openai-response`, `openai-codex`, or `grok-build`.

**27. project config file: {workdir}/opencode.json(c) and .opencode/opencode.json(c) `default_agent`** — `undocumented` · severity high

- Source: `crates/hya-server/src/compat/bound_agent_metadata.rs:33-35, 107-133, 152-160`
- Evidence: grep for `opencode.json` across the in-scope docs matches only docs/configuration.md:50 (the Compat IMPORT discovery list) and docs/configuration.md:136. No doc says that hya reads a per-project opencode.json at runtime, or that `default_agent` is honoured from it.
- Write: Add a `## Project Config (opencode.json)` section to docs/configuration.md. State that at runtime hya reads, in this order, {workdir}/opencode.json, {workdir}/opencode.jsonc, {workdir}/.opencode/opencode.json, {workdir}/.opencode/opencode.jsonc, and that a LATER file which sets a key overrides an earlier one (bound_agent_metadata.rs:33-35, 107-133). Only `default_agent` is honoured — inline `agent`, `permission`, `model`, and `options` fields present in an OpenCode project config are deliberately NOT read (bound_agent_metadata.rs:152-160). Unreadable or invalid files are skipped silently with no error. Show a two-line JSON example.

**28. project config inline slash commands: opencode.json `command` / `commands`** — `undocumented` · severity medium

- Source: `crates/hya-server/src/compat/command_sources.rs:17-28, 31-44, 104-111`
- Evidence: docs/configuration.md's "Custom Commands" section (lines 673-701) covers only markdown files and never mentions that slash commands can also be declared inline in the project opencode.json/.jsonc. No in-scope doc mentions the `command`/`commands` config maps.
- Write: In the Custom Commands section (or the new Project Config section), document inline commands: the same four project config paths ({workdir}/opencode.json, opencode.jsonc, .opencode/opencode.json, .opencode/opencode.jsonc) may carry BOTH a singular `command` map and a plural `commands` map, and the two are read and concatenated (command_sources.rs:31-44). Each entry is keyed by command name with fields: `template` (required, the prompt body), `description`, `agent`, `model`, `subtask` (bool). `$1`, `$2`, … and `$ARGUMENTS` inside `template` become numbered hint slots (command_sources.rs:104-111). Give a short JSON example.

**29. disk slash commands: {workdir}/.opencode/command and .opencode/commands markdown files** — `stale` · severity high

- Source: `crates/hya-server/src/compat/command_sources.rs:8-14, 46-72`
- Evidence: docs/configuration.md:673-696 lists SIX discovery paths ($HOME/.config/opencode/commands, $HOME/.config/opencode/command, $HOME/.config/hya/prompts, {workdir}/.opencode/commands, {workdir}/.opencode/command, {workdir}/.hya/prompts). Only two of those exist in the code (command_sources.rs:47-50 reads exactly {workdir}/.opencode/command and {workdir}/.opencode/commands); grep for `hya/prompts` and `.hya/prompts` across crates/ and packages/ returns zero hits, and there is no $HOME-scoped command directory at all. The doc also claims "Project commands override user commands with the same file stem" — there are no user commands, and code just sorts by path.
- Write: Rewrite the Custom Commands discovery list to exactly the two real roots: {workdir}/.opencode/command and {workdir}/.opencode/commands, markdown files, collected recursively and sorted by path (command_sources.rs:46-72). Delete the three nonexistent $HOME paths and the two `.hya/prompts` / `$HOME/.config/hya/prompts` paths and the "project overrides user" sentence. Document the real frontmatter key set: `description`, `agent`, `model`, `subtask` (bool) — `subtask` is currently missing from the doc (command_sources.rs:8-14). Keep the `$1`/`$2`/`$ARGUMENTS` hint-slot explanation, which is correct (command_sources.rs:98-111).

**30. plugin manifest .hya/plugins/*/plugin.toml schema** — `thin` · severity medium

- Source: `crates/hya-plugin/src/manifest.rs:1-30; crates/hya-app/src/plugins.rs:8-11, 124-140`
- Evidence: docs/configuration.md:136 and :607 mention the path `<workdir>/.hya/plugins/**/plugin.toml` and docs/hya-pi-compat-comparison.md:336 repeats it, but no doc gives a single TOML key, an example manifest, the hook syntax, the merge rule with config-declared plugins, or the malformed-manifest behavior.
- Write: In the Plugins section add a `### Directory manifests` subsection with a full example TOML and a key table for {cwd}/.hya/plugins/<dir>/plugin.toml (manifest.rs:1-30): `id` (required String), `kind` (default `rust`; also `compat` — alias `opencode` — or `other`), `command` (required array of strings, the stdio argv), `enabled` (bool, default true), `timeout_ms` (optional u64), and repeated `[[hooks]]` tables with `name` plus optional `posture` (`safe` | `open`). State that the scan is one level deep under .hya/plugins, that unknown hook names are warned about and dropped, and that a malformed manifest is skipped with a stderr notice rather than failing startup (plugins.rs:124-140). State the merge rule already implied elsewhere: on an id collision the config.yaml `plugins.<id>` entry wins over the directory manifest (config.rs:89).

**31. skill discovery order (SKILL.md search paths)** — `thin` · severity high

- Source: `crates/hya-tool/src/skill_catalog.rs:45-63`
- Evidence: docs/compat-parity.md:87 names only `.hya/skills` and `~/.config/hya/skills`; docs/hya-pi-compat-comparison.md:129 loosely names `.opencode`, `.claude/skills`, `.agents/skills`. No in-scope doc gives the ordered, complete list, and docs/configuration.md — where a user would look — has no skills section at all.
- Write: Add a `## Skills` section to docs/configuration.md listing the discovery directories in exact precedence order from skill_catalog.rs:45-63: {workdir}/.hya/skills, $HOME/.config/hya/skills, $HOME/.claude/skills, $HOME/.config/opencode/skills, $HOME/.config/opencode/skill, {workdir}/.opencode/skills, {workdir}/.opencode/skill, {workdir}/.agents/skills, $HOME/.codex/skills, $HOME/.agents/skills. Note both the singular `skill` and plural `skills` spellings are scanned for the opencode roots, that $HOME-based entries are skipped when HOME is unset, and that a skill is a directory containing SKILL.md.

**32. SKILL.md frontmatter keys** — `undocumented` · severity high

- Source: `crates/hya-tool/src/skill_catalog.rs:28-43`
- Evidence: grep for `allowed-tools` across all in-scope docs returns zero hits; grep for SKILL.md finds only prose mentions (docs/project-structure.md:125, docs/hya-pi-compat-comparison.md:102, docs/testing/process-e2e.md:24) and no key list. Only the rustdoc on SkillFrontmatter (skill_catalog.rs:28-30) describes the shape.
- Write: In the new Skills section, document the SKILL.md YAML frontmatter keys (skill_catalog.rs:28-43) with a worked example: `name` (skill id shown to the model), `description` (the summary advertised before the body is loaded), `allowed-tools` (list of strings; empty or absent means no restriction), `model` (per-skill model override), `disable` (bool, default false — set true to hide the skill), and `license` (parsed but currently unused). Note every key beyond name/description is optional so minimal existing skills keep working, and the markdown body after the frontmatter is the skill content loaded on demand by the `skill` tool.

**33. AGENTS.md project-context discovery (ancestor walk, $HOME stop boundary, parent-first ordering)** — `thin` · severity medium

- Source: `crates/hya-core/src/prompt.rs:29-52`
- Evidence: docs/compat-parity.md:86 says only "CLI discovers AGENTS.md"; docs/FOLLOWUPS.md:20 lists it as a wave item; docs/testing/agent-matrix.md:41 references a test. No doc states the walk direction, the $HOME stop boundary, or the ordering of multiple files in the prompt.
- Write: Add a `## Project Context (AGENTS.md)` section to docs/configuration.md: hya canonicalizes the workdir and walks upward toward the filesystem root collecting every AGENTS.md it finds, STOPPING once it has processed $HOME (so files above your home directory are never read), then reverses the list so the outermost/parent AGENTS.md appears first in the system prompt and the workdir-local one last (prompt.rs:29-52). Unreadable or missing files are skipped silently. Note this is the sole discovery implementation — callers re-export it rather than reimplement walk order.

**34. HYA_SUBAGENT_MAX_DEPTH / HYA_SUBAGENT_MAX_CONCURRENCY / HYA_SUBAGENT_BUDGET / HYA_SUBAGENT_TURN_BUDGET / HYA_SUBAGENT_MESSAGE_BUDGET** — `undocumented` · severity high

- Source: `crates/hya-app/src/config.rs:1332-1356`
- Evidence: grep for `HYA_SUBAGENT` across docs/**/*.md and the root docs returns zero hits. The docs/configuration.md "Environment Variables" table (lines 416-422) lists only HYA_MODEL, HYA_COMPACTION_THRESHOLD, HYA_COMPACTION_KEEP_RECENT, HYA_COMPAT_ADAPTER_DIR, HYA_FRONTEND_BIN.
- Write: Add five rows to the HYA_* environment-variable table in docs/configuration.md, and cross-link them from the new Subagent Limits section: `HYA_SUBAGENT_MAX_DEPTH` (u32 -> subagents.max_depth, default 5), `HYA_SUBAGENT_MAX_CONCURRENCY` (usize -> subagents.max_concurrency, default 100), `HYA_SUBAGENT_BUDGET` (u64 -> subagents.per_run_budget, default 1024 — call out that the env name drops the `PER_RUN` part), `HYA_SUBAGENT_TURN_BUDGET` (u64 -> subagents.per_team_turn_budget, default 1024), `HYA_SUBAGENT_MESSAGE_BUDGET` (u64 -> subagents.per_team_message_budget, default 1024). State the precedence rule explicitly: for these five, the ENV VALUE WINS over the config.yaml value (the opposite direction from HYA_MODEL), and an unparseable env value is ignored so the file/default value stands (config.rs:1332-1356).

**35. HYA_EVENT_BUS_CAPACITY** — `undocumented` · severity medium

- Source: `crates/hya-app/src/config.rs:1364-1372; crates/hya-core/src/bus.rs:8`
- Evidence: grep for `HYA_EVENT_BUS_CAPACITY` and "bus capacity" across all in-scope docs returns zero hits.
- Write: Add a row to the HYA_* environment-variable table: `HYA_EVENT_BUS_CAPACITY` — usize ring capacity of the live EventBus; the value must parse as a usize and be greater than 0 or it is ignored; default DEFAULT_BUS_CAPACITY = 8192 (config.rs:1364-1372, hya-core/src/bus.rs:8). Note explicitly that there is NO config.yaml key for this — it is env-only — and that raising it trades memory for tolerance of slow SSE subscribers.

**36. HYA_COMPACTION_THRESHOLD / HYA_COMPACTION_KEEP_RECENT default values** — `thin` · severity low

- Source: `crates/hya-app/src/runtime.rs:117-124; crates/hya-core/src/compaction.rs:28-35`
- Evidence: docs/configuration.md:419-420 lists both variables but gives their defaults as the symbolic strings `CompactionConfig::default().token_threshold` and `CompactionConfig::default().keep_recent`, which a user cannot read without opening the source.
- Write: Replace the symbolic defaults in the env table with the real numbers from crates/hya-core/src/compaction.rs:28-35: HYA_COMPACTION_THRESHOLD default 100000 (estimated tokens; a session compacts once its estimate exceeds this) and HYA_COMPACTION_KEEP_RECENT default 6 (most-recent messages kept verbatim during compaction). Also state that neither has a config.yaml key — both are env-only.

**37. HYA_DEFER_SIDEPLANES** — `thin` · severity medium

- Source: `crates/hya-app/src/runtime.rs:3866-3878`
- Evidence: Mentioned only as a test-harness detail in docs/troubleshooting.md:125 and docs/testing/process-e2e.md:40 ("the harness sets HYA_DEFER_SIDEPLANES=0"). It is absent from the docs/configuration.md environment-variable table, and no doc states its default or its accepted values.
- Write: Add a row to the HYA_* environment-variable table: `HYA_DEFER_SIDEPLANES` — default TRUE. When deferred, MCP connection is postponed until after the engine is built so the HTTP listener comes up immediately. Set it to `0`, `false`, `off`, or `no` (case-insensitive, surrounding whitespace trimmed) to restore the classic await-MCP-before-listen path; ANY other value, including an empty value and leaving it unset, means deferred (runtime.rs:3866-3878). Note the practical consequence: with the default, `GET /mcp` may briefly report servers as not yet connected right after startup.

**38. COMPAT_REPO_CLONE_GITHUB_BASE_URL** — `undocumented` · severity low

- Source: `crates/hya-server/src/compat/reference_repository.rs:159-165`
- Evidence: grep for `COMPAT_REPO_CLONE` across all in-scope docs returns zero hits.
- Write: Add a row to the non-HYA_ environment-variable table in docs/configuration.md: `COMPAT_REPO_CLONE_GITHUB_BASE_URL` — overrides the GitHub clone base used when hya clones a reference repository; the default is `https://github.com/<path>.git`. Trailing slashes on the supplied value are trimmed before the path is appended (reference_repository.rs:159-165). Useful for pointing reference-repo cloning at an internal GitHub mirror.

**39. HYA_BACKEND_BIN and HYA_TUI_TS_DIR** — `thin` · severity low

- Source: `crates/hya-ts/src/main.rs:43, 57, 171`
- Evidence: docs/architecture/tui.md:31 names both variables in one sentence ("HYA_TUI_TS_DIR, HYA_BACKEND_BIN, --backend-bin, and --bun provide ... overrides") with no semantics, no precedence order, and no default. Neither appears in the docs/configuration.md environment-variable table, unlike its sibling HYA_FRONTEND_BIN.
- Write: Add two rows to the HYA_* environment-variable table: `HYA_BACKEND_BIN` — path to the hya-backend binary that the `hya` / `hya-ts` launcher spawns; resolution order is the CLI `--backend-bin` flag first, then HYA_BACKEND_BIN, then a sibling of the current executable, then the workspace target directory (hya-ts/src/main.rs:57,171). `HYA_TUI_TS_DIR` — overrides the resolved TypeScript TUI runtime directory used by the launcher (hya-ts/src/main.rs:43). Note that HYA_FRONTEND_BIN (already in the table) is the mirror-image variable used by hya-backend to find the frontend.

**40. SHELL (PTY plane)** — `undocumented` · severity low

- Source: `crates/hya-server/src/compat/pty_payload.rs:88; crates/hya-server/src/compat/pty_shell.rs:20`
- Evidence: grep for `SHELL` in the in-scope docs matches only docs/architecture/agent-tool-surface.md:425, which is about the SHELL tool's permission Action, not the environment variable.
- Write: Add a row to the non-HYA_ environment-variable table: `SHELL` — the shell binary used to start a PTY plane session; when unset hya falls back to `/bin/sh` (pty_payload.rs:88, pty_shell.rs:20).

**41. COMPAT_TERMINAL (set, not read, in PTY children)** — `undocumented` · severity low

- Source: `crates/hya-server/src/compat/pty_state.rs:106`
- Evidence: grep for `COMPAT_TERMINAL` across all in-scope docs returns zero hits.
- Write: Add a row to the non-HYA_ environment-variable table marked as an OUTPUT variable: `COMPAT_TERMINAL` is SET to `1` by hya in the environment of every PTY child process so that child programs can detect they are running inside the hya terminal (pty_state.rs:106). hya never reads it; setting it yourself has no effect on hya.

**42. has_meaningful_permission gate — a literal `permission: {model: default, rules: []}` counts as ABSENT** — `thin` · severity medium

- Source: `crates/hya-app/src/config.rs:666-670, 1429-1451`
- Evidence: docs/configuration.md:408 says "Omitting `permission` is equivalent to `model: default` with no rules; a permission-only config remains active while hya uses the offline provider." That is misleading in the exact starter-file case: the starter config written on first run IS `permission: {model: default, rules: []}`, and has_meaningful_permission (config.rs:666-670) returns false for it, so it does NOT keep load() from returning None. Nothing in the doc explains this.
- Write: In the First-Run / Offline Behavior section, sharpen the offline condition list to match config.rs:1429-1451: hya returns "no usable config" (offline) when there is no config file, when the file is empty/whitespace-only, OR when the resolved set has no providers AND no MCP servers AND no plugins AND no MEANINGFUL permission block AND no `tools:` block. Define meaningful precisely: a `permission:` block counts only if its `model` differs from `default` or it has at least one rule (config.rs:666-670) — so the literal starter block `permission: {model: default, rules: []}` is treated as absent. Correct the sentence at line 408 accordingly: a permission-only config keeps hya's config active only when the permission block is meaningful by that rule.

**43. load_categories() / load_subagent_limits() re-read config.yaml independently of load()** — `undocumented` · severity medium

- Source: `crates/hya-app/src/config.rs:1391-1410`
- Evidence: No in-scope doc mentions that categories and subagent limits are read on a separate path. docs/configuration.md's offline section implies a single all-or-nothing config load, which would wrongly suggest that categories and subagent caps are inert while offline.
- Write: In the new Model Categories and Subagent Limits sections (and the offline section), state that `categories:` and `subagents:` are re-read by separate loaders that reopen and reparse config.yaml independently of the main `load()` (config.rs:1391-1410). Consequence: both still take effect even when hya is running on the offline provider because load() returned None. Also note that a parse failure on this path degrades silently to the defaults — no error is printed — so a malformed `categories:`/`subagents:` block looks like the keys being ignored.

**44. config key `categories` (logical model tiers → ordered provider/model candidate lists), CategoryRegistry::resolution_candidates** — `undocumented` · severity high

- Source: `crates/hya-app/src/config.rs:99, crates/hya-core/src/category.rs:96`
- Evidence: Grepped docs/configuration.md for `categories` / `category` — zero hits. Only docs/adr/0004-model-category-resolution-and-precedence.md:21 says categories are "config-driven concrete refs under a `categories:` block in ~/.config/hya/config.yaml" and CONTEXT.md:253 defines the term. Neither shows the YAML shape, and the user-facing config reference never mentions the key at all.
- Write: Add a `## Categories` section (after `## Providers`, before `## Auth Tokens`) documenting the top-level `categories:` key. Shape is `BTreeMap<String, Vec<String>>` (config.rs:99): each key is a logical category name (`deep`, `quick`), each value an ordered list of concrete `provider/model` refs where the FIRST is preferred and the rest are the failover chain. Give a runnable YAML example. State the semantics from crates/hya-core/src/category.rs: empty/whitespace-only candidates are trimmed and dropped; a category whose list is entirely empty is dropped from the registry (from_candidates → None); resolution picks the first candidate the router can actually serve and otherwise falls back to the first candidate so the failure surfaces as a real provider error instead of a silent misroute (resolve_servable, category.rs:134). Note that an unknown category simply fails to resolve and falls through the agent model-precedence chain (link docs/adr/0004).

**45. Per-family default reasoning variant menus (ProviderKind::reasoning_variants)** — `thin` · severity medium

- Source: `crates/hya-provider/src/http.rs:43`
- Evidence: docs/configuration.md:293 says "When reasoning metadata is omitted, hya uses the provider's variants and selects their highest supported effort as the startup default" but never lists what each provider's variants are. Only grok-build's menu is documented (configuration.md:197-198: low/medium/high, defaulting to high). Nothing states the Anthropic, OpenAI chat, Responses/Codex or Google menus.
- Write: In the reasoning subsection near line 291, add a table of per-kind default reasoning variant menus from http.rs:43: `anthropic` → low, medium, high, max; `openai`/`openai-compatible`/`openai-completion` → minimal, low, medium, high, xhigh; `openai-response` and `openai-codex` → none, minimal, low, medium, high, xhigh, max; `grok-build` → low, medium, high; `google` → high, max. State that these are the fallback menu used when a model has no `reasoning.variants` override, and that the startup default is the highest entry in the menu. Also note (http.rs:468) that a route emits an empty variant list when `reasoning_request` is false.

**46. ReasoningEffort → provider budget mapping (anthropic_budget, google_budget, openai_label)** — `thin` · severity medium

- Source: `crates/hya-provider/src/lib.rs:176, crates/hya-provider/src/lib.rs:188`
- Evidence: docs/architecture/providers.md:96 says only "reasoning effort maps to Gemini thinking-budget settings". docs/configuration.md:294-295 covers only the OpenAI label collapse. Grep for `thinkingBudget`, `thinking budget`, `1024`, `16000`, `31999` across in-scope docs: no hits. A user cannot tell what `high` actually costs on any provider.
- Write: Add a mapping table under the reasoning subsection. Anthropic thinking budget (lib.rs:176): off → none, minimal → none (thinking disabled), low → 1024, medium → 4096, high → 16000, xhigh → 24000, max → 31999. Google `thinkingBudget` (lib.rs:188): off/minimal/low/medium produce NO budget at all (this is the surprising part — worth calling out), high → 16000, xhigh → 20000, max → 32768 for model ids containing both `2.5` and `pro`, else 24576. OpenAI `reasoning_effort` label (lib.rs:164): off emits nothing, and BOTH xhigh and max collapse to the literal `xhigh`.

**47. Model ref resolution: bare id vs `providerID/modelID`, `#variant` suffix stripping, and Compat model-ref object form** — `thin` · severity medium

- Source: `crates/hya-provider/src/http.rs:350, crates/hya-server/src/compat/model_ref.rs:65`
- Evidence: CONTEXT.md:256 mentions "a reasoning variant (the `#variant` suffix on a concrete model ref)" as a glossary term only. docs/configuration.md `## Model Selection` and docs/cli.md `--model <PROVIDER/MODEL>` never explain which forms resolve, nor that the variant suffix is stripped before matching and never sent upstream. No doc mentions the `providerID: "hya"` drop or the `{providerID, modelID|id, variant}` object form.
- Write: In `## Model Selection`, add a "Model ref forms" subsection. A route serves a model ref addressed as (a) the bare `modelID`, (b) `providerID/modelID` where providerID equals that route's configured id, or (c) either form with a non-empty `#variant` suffix appended (http.rs:350). The `#variant` suffix is split off before matching and is never sent upstream — it only selects the reasoning variant; an empty variant (trailing `#`) is not treated as a suffix (http.rs:351). Give examples: `gpt-5.6-sol`, `gateway/gpt-5.6-sol`, `gateway/gpt-5.6-sol#high`. Then in docs/architecture/server-client.md's Provider/auth row, note that Compat HTTP surfaces additionally accept a model-ref object `{providerID, modelID|id, variant?}` or a plain string, that `providerID: "hya"` is dropped so the bare id still resolves, and that `variant` becomes the `#variant` suffix (compat/model_ref.rs:65).

**48. Plugin `chat_params` hook wire form and reasoning parse fallback** — `thin` · severity medium

- Source: `crates/hya-plugin/src/dispatcher.rs:98, :221`
- Evidence: docs/architecture/runtime.md:207 lists "chat params/messages" among hookable surfaces and docs/configuration.md:636 says the host supports "command/message/text/chat hooks" — neither names the fields a plugin may rewrite. Grep for `chat_params`, `ChatParams` in-scope: no hits.
- Write: In the `## Plugins` section, add a `### chat_params hook` subsection. Each plugin declaring the ChatParams hook can rewrite the outgoing completion request before dispatch; the wire form exposes `model`, `system`, `messages`, `tools`, `temperature`, `max_output_tokens`, `reasoning` and `headers` (dispatcher.rs:98). Note that `headers` here become per-request extra HTTP headers merged over the route's auth headers. Document the fallback rule: a plugin-supplied `reasoning` string that fails to parse as a ReasoningEffort leaves the original effort in place rather than clearing it (dispatcher.rs:221).

**49. Compat config import: provider `kind` heuristic and provider filtering** — `thin` · severity medium

- Source: `crates/hya-app/src/config.rs:942, :857`
- Evidence: docs/configuration.md:42-54 and the `### Compat migration into hya` section describe what `hya --import compat` imports (base URLs, model IDs, API key values/templates) but never say how the imported provider's `kind` is chosen, nor which providers are skipped.
- Write: In the `### Compat migration into hya` subsection, add the import rules. Kind heuristic (config.rs:942): the provider id / npm package / display name text is matched case-insensitively — text containing `anthropic` maps to `kind: anthropic`, text containing `google` or `gemini` maps to `kind: google`, and everything else maps to `kind: openai-compatible`. Warn that this means a Responses-only or Codex upstream imports as an OpenAI Chat Completions route and must be corrected by hand. Filtering (config.rs:857): disabled providers are skipped, as is any provider without a `base_url` or without at least one model; when the Compat default model belongs to a provider, its id is folded into that provider's model list.

**50. OAuth catalog media-model filter (ids containing `imagine`/`image`/`video` are dropped)** — `undocumented` · severity medium

- Source: `crates/hya-app/src/oauth/models_catalog.rs:196`
- Evidence: docs/configuration.md:226-232 documents the post-login catalog fetch and the upsert of the "full `models` list" but never mentions that some models are filtered out. Grep for `imagine`, `filter` in-scope: no hits.
- Write: In the `### OAuth login` numbered list (step 3), add that the imported catalog is filtered before it is written into `config.yaml`: any model id containing `imagine`, `image` or `video` is dropped, because those are media-generation models the agent catalog cannot use. Tell the user that if they need such a model they must add it to `providers.<id>.models` by hand.

**51. OAuth error taxonomy: NeedsLogin vs Entitlement → ProviderError::AuthExpired, and the Grok 403/400/401 mapping** — `thin` · severity medium

- Source: `crates/hya-app/src/config.rs:1479, crates/hya-app/src/oauth/grok_build.rs:124`
- Evidence: docs/configuration.md:234-240 documents only the revoked-refresh-token (`invalid_grant`) case and the printed re-login command. The Entitlement case (HTTP 403 on refresh → an AuthExpired hint explaining the API-key / subscription-upgrade path) is documented nowhere. Grep for `Entitlement`, `403`, `entitlement` in-scope: no hits.
- Write: Extend the refresh-failure paragraph after line 234. There are two distinct failure modes, both surfaced as `ProviderError::AuthExpired{provider, hint}` by the OAuth bearer resolver (config.rs:1479): (1) NeedsLogin — the refresh token is revoked or invalid (`invalid_grant`, or HTTP 400/401 on the Grok token endpoint) — the hint is the `hya-backend oauth login --provider <name> --type <type>` command line; (2) Entitlement — HTTP 403 from the Grok refresh endpoint means the account lacks the subscription entitlement — the hint explains the API-key / upgrade path and re-login will NOT fix it. Also note that Grok refresh requires a rotated refresh token when the response supplies one (grok_build.rs:124).

**52. Auth credential file format (`type: api` vs `type: oauth`), atomic 0600 write, legacy hand-written token parsing** — `thin` · severity medium

- Source: `crates/hya-app/src/auth.rs:87, :219, :287`
- Evidence: docs/configuration.md:317-319 says tokens are stored under `~/.config/hya/auth/<provider>.yaml` as "plain API keys or OAuth bundles" and lists the OAuth fields at line 224, but never shows the YAML discriminator or the file permissions. Grep for `type: api`, `0600`, `oauth_type`, `id_token` in-scope: no hits.
- Write: In `## Auth Tokens`, add the on-disk document shape from auth.rs:87. An auth file is one of two documents: a static token (`type: api` with `token:`) or a full OAuth bundle (`type: oauth` with `oauth_type`, `access`, `refresh`, `expires_at`, `account_id`, `id_token`). Show both as short YAML blocks. Add that the file is written atomically — a temp `.<provider>.yaml.tmp` is created, chmodded to 0600 on unix, then renamed into place (auth.rs:219). Add the compatibility note that if YAML deserialization yields no credential, hya scrapes a bare `token: "..."` line and unquotes it as an API credential (auth.rs:287), so a hand-written one-line file still works.

**53. ReasoningEffort::parse aliases and resolve_default_reasoning last-used precedence** — `thin` · severity low

- Source: `crates/hya-provider/src/lib.rs:137, :209`
- Evidence: docs/configuration.md:291 lists the seven accepted values and documents `none`, but the `med` → Medium alias and case-insensitivity are undocumented, and the "last-used" middle step of the default-resolution precedence is missing (the doc only covers explicit config and highest-supported).
- Write: In the reasoning subsection, add: effort strings are parsed case-insensitively after trimming, and two compatibility aliases are accepted — `off` is a synonym for `none`, and `med` is a synonym for `medium`; any other string is a config error (lib.rs:137). Then correct the default-resolution precedence to the full three-step chain from lib.rs:209: (1) explicit `reasoning.default` from config; (2) otherwise the last-used effort, kept when it is `none` or is present in the advertised variants; (3) otherwise the highest supported level. If the model advertises no reasoning at all the result is `None` and no default is shown.

**54. OAuth refresh skew (300s) and the process-wide single-flight refresh lock** — `thin` · severity low

- Source: `crates/hya-app/src/oauth/ensure.rs:17, :72`
- Evidence: docs/configuration.md:234 says only "Access tokens are refreshed automatically when near expiry" — the 5-minute skew value and the single-flight lock are undocumented. Grep for `skew`, `300`, `single-flight` in-scope: no hits.
- Write: In the OAuth login section where automatic refresh is described, add the concrete semantics from crates/hya-app/src/oauth/ensure.rs: `ensure_access_token` loads the saved credential on every stream and refreshes first when the token is within a 5-minute (300s) skew of its stated expiry; a plain `type: api` credential is returned untouched with no refresh. Add that refresh is guarded by a process-wide mutex plus a re-read of the credential after acquiring it, so concurrent sessions cannot both burn a rotated refresh token.

**55. OAuth login side-effects on `default_model` and provider id validation** — `undocumented` · severity low

- Source: `crates/hya-app/src/config.rs:472, crates/hya-app/src/oauth/mod.rs:237`
- Evidence: docs/configuration.md:229-232 documents that a provider block is upserted but not that `default_model` may also be rewritten, nor which provider ids are rejected. Grep for `provider id validation`, `default_model only` in-scope: no hits.
- Write: In the OAuth login numbered list (step 3), add two rules. (a) `default_model` is overwritten only when it is missing, empty, or literally `offline` — an existing real default is preserved (config.rs:472). (b) The `--provider` id is validated before anything is written: ids containing `/`, `\`, `..` or whitespace are rejected, because the id becomes the `auth/<id>.yaml` filename (oauth/mod.rs:237).

**56. Codex OAuth endpoints/constants and the pinned models-endpoint client_version** — `undocumented` · severity low

- Source: `crates/hya-app/src/oauth/openai_codex.rs:22, crates/hya-app/src/oauth/models_catalog.rs:38, crates/hya-app/src/oauth/openai_codex.rs:369`
- Evidence: docs/configuration.md:227 gives the models URL without query or headers. The client_id, issuer/authorize/token hosts, device API base and scope are documented nowhere, and neither is the id_token account-id extraction. Grep for `client_id`, `auth.openai.com`, `client_version` in-scope: no hits.
- Write: Add a `#### Codex OAuth endpoints` note under the OpenAI Codex subsection, useful for proxy/firewall allowlisting. From openai_codex.rs:22: client_id `app_EMoamEEZ73f0CkXaXp7hrann`; issuer, authorize and token endpoints on `auth.openai.com`; device API base `/api/accounts`; scope `openid profile email offline_access`. From models_catalog.rs:38: the post-login catalog fetch is `GET https://chatgpt.com/backend-api/codex/models?client_version=0.144.0` with `OpenAI-Beta: responses=experimental` and `User-Agent: codex_cli_rs` — flag that `client_version` is pinned and may need bumping if the endpoint starts rejecting it. Add that the ChatGPT account id sent as `chatgpt-account-id` is read from the id_token (falling back to the access token) by decoding the JWT payload WITHOUT signature verification and reading `https://api.openai.com/auth`.chatgpt_account_id (openai_codex.rs:369).

**57. Grok OAuth constants, scopes, slow_down backoff, and JWT-derived expiry** — `undocumented` · severity low

- Source: `crates/hya-app/src/oauth/grok_build.rs:11, :97, :194`
- Evidence: docs/configuration.md's Grok Build subsection documents only the request headers and config shape. Grep for `auth.x.ai`, `slow_down`, `grok-cli:access` in-scope: no hits.
- Write: Add a `#### Grok OAuth endpoints` note under the Grok Build subsection: client_id `b1a00492-...` (read the full value from grok_build.rs:11), device and token endpoints on `auth.x.ai`, and scope `openid profile email offline_access grok-cli:access api:access conversations:read conversations:write`. Document the device-code polling behavior (grok_build.rs:97): `authorization_pending` keeps polling, each `slow_down` adds 5s to the interval capped at 30s, and `access_denied` / `expired_token` fail fast. Note that when the token response omits `expires_in`, `expires_at` is derived from the access token's JWT `exp` claim (grok_build.rs:194).

**58. Codex device-code `interval` string parsing quirk** — `undocumented` · severity low

- Source: `crates/hya-app/src/oauth/openai_codex.rs:294`
- Evidence: No in-scope doc discusses device-code polling intervals at all.
- Write: One sentence in the Codex OAuth endpoints note: Codex returns the device-flow `interval` as a JSON string rather than a number, so hya parses either form, floors it at 1 second, and defaults to 5 seconds when absent (openai_codex.rs:294). Include this so a future maintainer does not "fix" the lenient parse.

**59. plugin.toml manifest format (item 49): id, kind, command, enabled, timeout_ms, hooks:[{name, posture?}]** — `thin` · severity high

- Source: `crates/hya-plugin/src/manifest.rs:11`
- Evidence: docs/configuration.md:136 and :607 and docs/hya-pi-compat-comparison.md:336 all name the file path `<workdir>/.hya/plugins/**/plugin.toml`, but NO doc anywhere shows the TOML content, lists its fields, or gives an example. The 'Config entries support' table at docs/configuration.md:622 documents the YAML PluginEntry shape only.
- Write: Add a 'plugin.toml manifest' subsection with a complete annotated example and a field table: `id` (required, must match the plugin's handshake id), `kind` (rust|compat|other, default rust), `command` (required array of argv strings), `enabled` (default true), `timeout_ms` (optional per-call override), and `hooks = [{name = "...", posture = "safe"|"open"}]`. Warn that an UNKNOWN hook name is silently dropped with only a warning — the manifest still loads and the plugin runs without that hook. Note that manifest posture entries act as posture overrides and survive the config/manifest merge, while YAML config entries carry no posture overrides at all.

**60. Plugin directory scan depth (item 50): plugins_dir() = cwd/.hya/plugins, one level of immediate subdirectories only** — `contradicted` · severity medium

- Source: `crates/hya-app/src/plugins.rs:8`
- Evidence: docs/configuration.md:136, docs/configuration.md:607 and docs/hya-pi-compat-comparison.md:336 all write the path as `<workdir>/.hya/plugins/**/plugin.toml`, whose `**` glob implies recursive discovery at any depth. crates/hya-app/src/plugins.rs scan_manifests reads plugin.toml from each IMMEDIATE subdirectory of cwd/.hya/plugins only; a nested plugin.toml is never found.
- Write: Replace the `**` glob with the literal layout `<workdir>/.hya/plugins/<name>/plugin.toml` and say the scan is exactly one directory deep — nested directories are not searched. Also state that a subdirectory whose plugin.toml is unreadable or unparseable is SKIPPED with a notice on stderr rather than failing startup, so a typo produces a silently missing plugin.

**61. Config-over-manifest merge precedence and disabled filtering (item 53)** — `undocumented` · severity medium

- Source: `crates/hya-plugin/src/config.rs:39`
- Evidence: docs/configuration.md:606 says plugins 'may be declared directly in config or discovered from <workdir>/.hya/plugins/**/plugin.toml' but never says which wins when both declare the same id, nor in what order they load. crates/hya-plugin/src/config.rs has 1 doc-comment line.
- Write: State the merge rule from merge(): config entries are emitted FIRST (skipping any with enabled:false), then manifests are appended only if their id was not already claimed by a config entry AND the manifest itself is enabled. Consequences worth spelling out: config always beats a same-id manifest; config order determines hook-chain fold order; manifest posture overrides survive the merge while config entries never carry postures; and setting enabled:false in config does NOT re-open the id for a manifest to claim — the plugin is simply absent.

**62. Compat adapter environment variables (items 63,64,65): COMPAT_CONFIG, COMPAT_CONFIG_DIR, COMPAT_CONFIG_CONTENT, COMPAT_DISABLE_PROJECT_CONFIG, COMPAT_PURE, HYA_COMPAT_OPTIONS_JSON, HYA_DIRECTORY, HYA_WORKTREE, HYA_SERVER_URL, HYA_PROJECT_ID** — `contradicted` · severity high

- Source: `crates/hya-plugin-compat/adapter/src/initialize.ts:139,155; crates/hya-plugin-compat/adapter/src/loader/discovery.ts:56`
- Evidence: docs/configuration.md:410-430 presents itself as the authoritative list — 'hya reads the following HYA_* variables (verified against the source listed in each row)' — and lists only HYA_MODEL, HYA_COMPACTION_THRESHOLD, HYA_COMPACTION_KEEP_RECENT, HYA_COMPAT_ADAPTER_DIR, HYA_FRONTEND_BIN, plus BUN, COMPAT_WEBSEARCH_PROVIDER, PARALLEL_API_KEY, EXA_API_KEY. It omits five HYA_* variables the adapter reads. grep counts across all in-scope docs: COMPAT_PURE 0, HYA_COMPAT_OPTIONS_JSON 0, HYA_DIRECTORY 0, HYA_WORKTREE 0, HYA_SERVER_URL 0, HYA_PROJECT_ID 0; the single COMPAT_CONFIG hit is in the unrelated --import compat paragraph.
- Write: Add the missing rows and stop the table implying completeness. HYA_COMPAT_OPTIONS_JSON — a JSON blob with a `plugin: [spec | [spec, options]]` array APPENDED after the discovered specs; malformed JSON becomes an INVALID_PARAMS initialize error (the plugin fails to load). HYA_DIRECTORY — the adapter's directory. HYA_WORKTREE — the stop boundary for the ancestor config walk. HYA_SERVER_URL — the serverUrl handed to plugins, default http://127.0.0.1:0. HYA_PROJECT_ID — the compat project id, defaulting to the worktree path. And the COMPAT_* family: COMPAT_CONFIG (explicit config file), COMPAT_CONFIG_DIR (extra config dir), COMPAT_CONFIG_CONTENT (inline JSON config), COMPAT_DISABLE_PROJECT_CONFIG (skip the project-config ancestor walk), COMPAT_PURE (set to `true` or `1` to load ZERO plugins — the escape hatch when a plugin breaks startup).

**63. TUI configuration file (theme, keybinds, leader_timeout, attention, prompt.max_height/max_width, scroll_speed, scroll_acceleration, diff_style, mouse)** — `undocumented` · severity high

- Source: `packages/hya-tui-ts/src/upstream/config/index.tsx:18`
- Evidence: docs/configuration.md has no TUI section — grep for 'theme', 'mouse', 'diff_style', 'attention', 'sound', 'scroll' in docs/configuration.md returns nothing relevant (only 'TUI' at lines 404/532/636/675/699 about permissions, import, plugins and custom commands). docs/opencode-feature-inventory.md:17 still calls a dedicated TUI config a 'should-have' that 'need[s] scope decision', while the schema is fully implemented.
- Write: Add a top-level '## TUI Configuration' section documenting the validated TUI config schema with defaults and value ranges: `theme` (theme name, default `hya`); `keybinds` (per-command overrides validated against the full command table — an unknown key throws `Unrecognized keybind(s): …`; a value may be `false`, the string `"none"`, a key string, a keystroke object with `event`/`preventDefault`/`fallthrough`, or an array of those); `leader_timeout` (positive integer ms, default 2000); `attention` with `enabled` (default false), `notifications` (default true), `sound` (default true), `volume` 0–1 (default 0.4), `sound_pack` (default `hya.default`) and per-name `sounds` file overrides for the six slots `default`, `question`, `permission`, `error`, `done`, `subagent_done`; `prompt.max_height` (caps the prompt textarea, default one third of terminal height, minimum 6) and `prompt.max_width` (positive integer or `"auto"` = 70% of width, minimum 75, used by the home prompt); `scroll_speed` (multiplier, >= 0.001) and `scroll_acceleration` ({ enabled }) applied to every scrollbox including the sidebar and observation panes; `diff_style` (`auto` = split above 120 columns, `stacked` = always unified); `mouse` (default true, ANDed with the `HYA_DISABLE_MOUSE` env var). State where the file lives and give one complete example. Also update docs/opencode-feature-inventory.md:17 so it no longer says this needs a scope decision.

**64. TUI environment variables: HYA_DISABLE_MOUSE, HYA_DISABLE_TERMINAL_TITLE, HYA_DISABLE_COPY_ON_SELECT, HYA_SHOW_TTFD, HYA_WAIT_THEME, HYA_SYNC_PLUGIN_START, HYA_VERSION, HYA_CHANNEL, HYA_STARTUP_TRACE, HYA_ROUTE, HYA_FAST_BOOT** — `contradicted` · severity high

- Source: `packages/hya-tui-ts/src/hya/platform.ts:24`
- Evidence: docs/configuration.md:412 says 'hya reads the following HYA_* variables (verified against the source listed in each row)' and then lists exactly five: HYA_MODEL, HYA_COMPACTION_THRESHOLD, HYA_COMPACTION_KEEP_RECENT, HYA_COMPAT_ADAPTER_DIR, HYA_FRONTEND_BIN. None of the eleven TUI variables appear anywhere in the in-scope docs; HYA_DB, HYA_TUI_TS_DIR and HYA_BACKEND_BIN are also missing from that table even though they are documented elsewhere (docs/cli.md:72, docs/architecture/tui.md:31). The 'verified' completeness claim is therefore false.
- Write: Add a 'TUI environment variables' subsection to the existing '## Environment Variables' section (or extend the table), and soften the completeness claim at line 412 so it is scoped to the backend table. Document, with Effect / Default / Source columns pointing at packages/hya-tui-ts/src/hya/platform.ts: HYA_DISABLE_MOUSE (truthy `1`/`true` disables OpenTUI mouse capture regardless of the `mouse` config key); HYA_DISABLE_TERMINAL_TITLE (suppresses all terminal-title writes even when `terminal.title.toggle` is on); HYA_DISABLE_COPY_ON_SELECT (disables copy-on-mouse-selection and the selection key intercept; implicitly always true on win32); HYA_SHOW_TTFD (renders OpenTUI's `<TimeToFirstDraw />` first-paint overlay); HYA_WAIT_THEME (classic mode — blocks first paint up to 1s waiting for OS light/dark detection; default is instant dark with async correction); HYA_SYNC_PLUGIN_START (classic mode — gates the shell routes on sequential builtin plugin-host start; default paints shell chrome immediately and records `shell_paint=immediate`); HYA_VERSION and HYA_CHANNEL (strings shown in the sidebar footer and home footer, both defaulting to `local`; a channel other than `latest` also reveals the raw session id in the sidebar title); HYA_STARTUP_TRACE (emits one JSON line per startup mark on stderr); HYA_ROUTE (JSON, picks the initial route: home / session+sessionID / plugin+id); HYA_FAST_BOOT (skips the initial loading overlay). Also add HYA_DB, HYA_TUI_TS_DIR and HYA_BACKEND_BIN rows so the table is actually complete.

**65. TUI XDG data/cache/config/state directories and the files it writes (model.json recents+favorites, session pins, worktree root)** — `thin` · severity medium

- Source: `packages/hya-tui-ts/src/hya/platform.ts:6`
- Evidence: docs/cli.md:72 documents only `$XDG_STATE_HOME/hya/sessions.db` (backend), docs/cli.md:102 the bundle registry under `$XDG_DATA_HOME/hya/bundles`. No doc mentions the TUI's own `<xdg>/hya` data/cache/config/state derivation, `model.json`, or the `<data>/worktree` root.
- Write: In the TUI Configuration section, add a 'Where the TUI stores state' table: XDG_DATA_HOME / XDG_CACHE_HOME / XDG_CONFIG_HOME / XDG_STATE_HOME each derive a `<xdg>/hya` directory (with the standard `~/.local/share`, `~/.cache`, `~/.config`, `~/.local/state` fallbacks). State holds `model.json` (recent + favorite model lists) and the session pin list backing the nine quick-switch slots; data plus `/worktree` is the TUI's worktree root. Note that invalid `model.json` entries toast a warning rather than failing startup, and that stale pins whose session no longer exists are filtered out on read.

**66. Editor context integration (Zed / Claude-Code SSE)** — `undocumented` · severity medium

- Source: `packages/hya-tui-ts/src/upstream/context/editor.ts:117`
- Evidence: grep for 'OPENCODE_EDITOR_SSE_PORT', 'CLAUDE_CODE_SSE_PORT', 'OPENCODE_ZED_DB', 'ZED_TERM' across all in-scope docs returns nothing.
- Write: Add rows to the non-`HYA_` environment-variable table and a short paragraph in the TUI section: the TUI discovers a live editor connection via `OPENCODE_EDITOR_SSE_PORT` or `CLAUDE_CODE_SSE_PORT`, and reads Zed's selection database at `OPENCODE_ZED_DB` (Zed detected via `ZED_TERM` / `TERM_PROGRAM`) to attach the current file and selection to the prompt. The attached label appears in the prompt footer and `prompt.editor_context.clear` dismisses it.

**STALE 1.** The document claims: 'hya reads the following `HYA_*` variables (verified against the source listed in each row).'

- Reality: The 'verified' claim is false as a completeness statement; the table omits ~12 HYA_* variables the code reads, including all five HYA_SUBAGENT_* limits and HYA_DB, which is documented only in docs/cli.md.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: 'The TUI loads markdown prompt commands from: 1. $HOME/.config/opencode/commands/*.md 2. $HOME/.config/opencode/command/*.md 3. $HOME/.config/hya/prompts/*.md 4. <workdir>/.opencode/commands/*.md 5. <workdir>/.opencode/command/*.md 6. <workdir>/.hya/prompts/*.md' plus 'Project commands override user commands with the same file stem.'

- Reality: crates/hya-server/src/compat/command_sources.rs:45-51 (disk_commands) scans exactly two roots: <workdir>/.opencode/command and <workdir>/.opencode/commands. Grepping crates/ and packages/ for 'hya/prompts', 'config/opencode/command', and '.hya/prompts' returns zero matches — entries 1, 2, 3 and 6 do not exist, and with no user tier the override sentence is meaningless.
- Action: correct or delete. Do not merely supplement.

**STALE 3.** The document claims: 'Optional frontmatter fields are parsed:' followed by an example showing only description, agent, and model.

- Reality: CommandFrontmatter (crates/hya-server/src/compat/command_sources.rs:8-14) also parses `subtask: bool`, which routes the command into a child session. The doc also never mentions the `command:` / `commands:` config maps read from opencode.json/opencode.jsonc, which are a second, undocumented source of slash commands.
- Action: correct or delete. Do not merely supplement.

**STALE 4.** The document claims: Markdown prompt commands are loaded from six directories: $HOME/.config/opencode/commands/*.md, $HOME/.config/opencode/command/*.md, $HOME/.config/hya/prompts/*.md, <workdir>/.opencode/commands/*.md, <workdir>/.opencode/command/*.md, <workdir>/.hya/prompts/*.md — and "Project commands override user commands with the same file stem."

- Reality: crates/hya-server/src/compat/command_sources.rs:46-50 reads exactly two roots, both project-local: {workdir}/.opencode/command and {workdir}/.opencode/commands. grep across crates/ and packages/ finds no reference to any `hya/prompts` or `.hya/prompts` directory and no $HOME-scoped command directory, so four of the six documented paths do not exist and there are no "user commands" to be overridden; files are simply collected and sorted by path (command_sources.rs:52). The doc also omits the real `subtask` frontmatter key (command_sources.rs:8-14) and omits inline `command`/`commands` maps in project opencode.json (command_sources.rs:17-28).
- Action: correct or delete. Do not merely supplement.

**STALE 5.** The document claims: `COMPAT_WEBSEARCH_PROVIDER` selects the web-search backend, and `PARALLEL_API_KEY` / `EXA_API_KEY` are API keys for the corresponding websearch providers, all sourced to crates/hya-tool/src/websearch.rs.

- Reality: grep for COMPAT_WEBSEARCH_PROVIDER, PARALLEL_API_KEY, and EXA_API_KEY across all of crates/ and packages/ returns zero hits. crates/hya-tool/src/websearch.rs:23-40 reads no environment variables at all — the provider, endpoint, key, and enabled flag come only from the `tools.websearch` config block. These three rows should be deleted.
- Action: correct or delete. Do not merely supplement.

**STALE 6.** The document claims: Plugins are 'discovered from `<workdir>/.hya/plugins/**/plugin.toml`' — the `**` glob implies recursive discovery at any depth.

- Reality: crates/hya-app/src/plugins.rs:8 plugins_dir() resolves cwd/.hya/plugins and scan_manifests reads plugin.toml from each IMMEDIATE subdirectory only. A plugin.toml nested deeper is never found.
- Action: correct or delete. Do not merely supplement.

**STALE 7.** The document claims: 'hya reads the following `HYA_*` variables (verified against the source listed in each row)' — presented as the authoritative, source-verified list, containing five HYA_* rows and four non-HYA_ rows.

- Reality: The bundled compat adapter also reads HYA_COMPAT_OPTIONS_JSON (loader/discovery.ts:56) and HYA_DIRECTORY / HYA_WORKTREE / HYA_SERVER_URL / HYA_PROJECT_ID (initialize.ts:155), plus the COMPAT_CONFIG / COMPAT_CONFIG_DIR / COMPAT_CONFIG_CONTENT / COMPAT_DISABLE_PROJECT_CONFIG / COMPAT_PURE family (initialize.ts:139). None appear in either table.
- Action: correct or delete. Do not merely supplement.

**STALE 8.** The document claims: "hya reads the following `HYA_*` variables (verified against the source listed in each row)." followed by a five-row table (HYA_MODEL, HYA_COMPACTION_THRESHOLD, HYA_COMPACTION_KEEP_RECENT, HYA_COMPAT_ADAPTER_DIR, HYA_FRONTEND_BIN).

- Reality: The claim of verified completeness is false. Missing at minimum: the eleven TUI variables in packages/hya-tui-ts/src/hya/platform.ts (HYA_DISABLE_MOUSE, HYA_DISABLE_TERMINAL_TITLE, HYA_DISABLE_COPY_ON_SELECT, HYA_SHOW_TTFD, HYA_WAIT_THEME, HYA_SYNC_PLUGIN_START, HYA_VERSION, HYA_CHANNEL, HYA_STARTUP_TRACE, HYA_ROUTE, HYA_FAST_BOOT), plus HYA_TUI_TS_DIR and HYA_BACKEND_BIN (documented only in docs/architecture/tui.md:31) and HYA_DB (documented only in docs/cli.md:72).
- Action: correct or delete. Do not merely supplement.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 66 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
