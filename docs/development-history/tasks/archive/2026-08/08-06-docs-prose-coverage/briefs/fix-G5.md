# Fix batch G5 - DESIGN.md, CONTEXT.md, AGENTS.md, README.md, project-structure.md, compat-parity.md, hya-pi-compat-comparison.md, agent-bundle-authoring.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `DESIGN.md`
- `CONTEXT.md`
- `AGENTS.md`
- `docs/README.md`
- `docs/project-structure.md`
- `docs/compat-parity.md`
- `docs/hya-pi-compat-comparison.md`
- `docs/agent-bundle-authoring.md`

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


### `DESIGN.md`

**STILL OPEN 1 - Transcript rendering: user messages, assistant footer, revert banner (gap #19)** (`stale`)

- Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:1720-1800`
- Why it is still open: docs/tui-reference.md "Transcript" section is new and accurate. But the entry was filed as `stale` because DESIGN.md section 5 described the removed Rust rendering, and the gap required updating it. DESIGN.md sections 2 (Light column), 5 "Prompt Composer" and 6 (Motion) were all updated, yet section 5 "Transcript" is untouched and still reads "Structure: role label followed by wrapped message lines and compact tool rows." `UserMessage` renders no role label at all — it renders an agent-colored `border=["left"]` box with hover highlight, MIME badges, QUEUED badge and an optional compaction divider. The wrong claim was supplemented in a different file rather than corrected in place.

**CRITIC 1 - Theme token field names for custom theme JSON files**

- Source: `/chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/upstream/theme/index.ts:45-48 (`readonly textMuted`, `readonly backgroundPanel`) and packages/hya-tui-ts/src/upstream/theme/assets/hya.json (keys `backgroundPanel`, `backgroundElement`, `borderActive`, `borderSubtle`, `textMuted`)`
- Why it matters: `DESIGN.md:19-35` names the tokens in snake_case — `background_panel`, `background_element`, `border_active`, `border_subtle`, `text_muted` — and instructs "Use only semantic theme fields from `Theme`". `docs/tui-reference.md:527-536` tells authors to drop a JSON object with "a nested `theme` object key" into `~/.config/hya/themes/*.json`, and every shipped asset plus the `Theme` type uses camelCase. There is no snake_case→camelCase normalization in the loader, so a theme written from DESIGN.md's table silently loses those five tokens. (The hex values in DESIGN.md are correct.)


### `CONTEXT.md`

**CRITIC 1 - Roster sidebar in the TUI**

- Source: `/chivier-disk/yanweiye/Projects/yaca/packages/hya-tui-ts/src/upstream/feature-plugins/builtins.ts:20-31 (BUILTINS = HomeFooter, HomeTips, SidebarContext, SidebarMcp, SidebarLsp, SidebarTodo, SidebarFiles, SidebarFooter, Notifications, WhichKey, DiffViewer — no roster plugin)`
- Why it matters: `CONTEXT.md:215-218` defines **Roster sidebar** as "An always-visible Session-screen sidebar block that summarizes live Roster entries for the current Team." `docs/tui-reference.md:90-91` states "There is **no roster section** in the sidebar; subagents use panes and the roster dialog instead", and `docs/adr/0006-tui-session-reset-and-subagent-visibility.md` records the same ('The sidebar has no roster section. Its feature plugins are Context, MCP, LSP, Todo, Modified Files, and a footer.'). CONTEXT.md is the ubiquitous-language glossary, so a stale term there propagates into every other doc and PRD that reuses the vocabulary.

**CRITIC 2 - Where Agent definitions come from (agent catalog authority)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-backend/src/agent_cmd.rs:50-57 ("adds ordinary agents from the build-embedded catalog only — never inspects `.hya`/`.claude`/`.opencode`/config agent files"); no agent-file discovery exists anywhere in hya-app/hya-core`
- Why it matters: `CONTEXT.md:120-123` defines **Agent** as "sourced from a built-in (\"native\") agent or a user-authored markdown file", and `CONTEXT.md:125-127` defines **Agent catalog** as "The disk- and config-discovered set of Agent definitions". `docs/agent-bundle-authoring.md:492` says "Legacy definitions are not parsed, migrated, or used as a fallback" and there is "no legacy agent-file discovery"; `docs/cli.md:459-463` says `agent list` "**never** inspects on-disk agent files under `.hya/`, `.claude/`, or `.opencode/`, nor config-declared agents"; `docs/architecture/agent-tool-surface.md:682-684` names `BundleCatalog` on `RuntimeSnapshot`/`TurnBinding` as the single authority. CONTEXT.md still describes the removed markdown-agent-file model.


### `AGENTS.md`

**CRITIC 1 - Crate component map (hya-app and hya-bundle)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/Cargo.toml:5 (`members = ["crates/*"]`); crates/hya-app/src/lib.rs and crates/hya-bundle/src/lib.rs exist`
- Why it matters: `AGENTS.md:69-93` presents a "Component Map" of every crate but omits `crates/hya-app` and `crates/hya-bundle`, and folds hya-app's responsibilities into the `crates/hya-backend` row ("plugin loading, MCP setup, permission policy"). `docs/project-structure.md:52-53` documents both as first-class crates — hya-app as "Runtime composition: config load, provider/MCP/plugin wiring, session engine build, installed-bundle refresh" and hya-bundle as the AgentBundle prepare/validate/catalog layer. Since AGENTS.md is the file agents read first for crate routing, work targeting runtime composition or bundles gets routed to hya-backend.


### `docs/README.md`

**CRITIC 1 - Documentation index completeness (orphaned architecture page)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/docs/README.md:64-94 (Docs Map) and :48-62 (codebase reading path)`
- Why it matters: `docs/architecture/agent-tool-surface.md` is a 771-line architecture reference and is linked from **zero** other markdown files — it appears in neither the docs/README.md Docs Map nor the "understand the codebase" reading path, while `docs/architecture/tools-and-permissions.md` (which covers a subset of the same ground) is listed. `docs/compat-parity.md` is likewise absent from the Docs Map even though root `README.md:118` links it and docs/README.md claims to be the index. Two docs now describe the builtin-tool surface and only one is reachable.


### `docs/project-structure.md`

**CRITIC 1 - Permission Action raised by lsp / skill / task / todowrite**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-tool/src/lsp.rs:70 (`Action::Lsp`), skill.rs:122 (`Action::Skill`), todo.rs:118 (`Action::TodoWrite`), task.rs:268 (`Action::Task`)`
- Why it matters: `docs/project-structure.md:127-132` lists the "Permission action" for `lsp`, `skill`, `list_agents`, `task`, `todowrite`, `plan_exit`, and the mailbox tools all as `Tool`. `docs/architecture/tools-and-permissions.md:121-127` maps the same tools to distinct actions: `task`→`Task`, `webfetch`→`WebFetch`, `websearch`→`WebSearch`, `todowrite`→`TodoWrite`, `skill`→`Skill`, `lsp`→`Lsp`. Since the same table already uses real Action names (`Read`, `Edit`, `Glob`, `Grep`, `Bash`) for the other rows, readers writing `permission.rules` will target the wrong action and their rule will silently never match.


### `docs/compat-parity.md`

**CRITIC 1 - Canonical builtin tool names vs hidden aliases**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-tool/src/tool.rs:341-345 (`insert_aliased_builtin("apply_patch", "patch", …)` etc. — first argument is canonical/advertised, second is the hidden alias)`
- Why it matters: `docs/compat-parity.md:84` writes "… plus `apply_patch`, `webfetch`, `websearch`, `todowrite`, and `plan_exit` **aliases** matching Compat names." `docs/architecture/tools-and-permissions.md:53-54`, `docs/architecture/agent-tool-surface.md:197-214`, and `docs/project-structure.md:124` all state that those five are the **canonical advertised** schema names and that the five hidden, non-advertised aliases are `patch`, `fetch`, `search`, `todo`, `plan`. Readers of compat-parity.md will look for the canonical name elsewhere.


### `docs/hya-pi-compat-comparison.md`

**CRITIC 1 - Whether dynamic MCP HTTP routes can add tools to a running engine**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-app/src/runtime.rs:4054-4055 (`RuntimeReconciler` + `RuntimeMcpControl` publish a complete candidate through `RuntimeRegistry`), route mounted at /chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/compat/mcp.rs:19`
- Why it matters: `docs/hya-pi-compat-comparison.md:74-77` says the Compat MCP HTTP routes "do not durably rewrite `config.yaml` **or hot-plug new tools into an already-running engine**". `docs/configuration.md:804-813` and `docs/compat-parity.md:91` say the opposite for the tool half: "A complete successful observation is published atomically for the next turn" / "HTTP add/connect/disconnect changes next-turn tool callability atomically", and configuration.md:820-887 even ships a runnable POST /mcp + connect/disconnect walkthrough. Only the `config.yaml` half of the comparison-doc claim is still true.


### `docs/agent-bundle-authoring.md`

**STILL OPEN 1 - resource_view reference grammar / ExportKind grammar (gap items 98, 116)** (`thin`)

- Source: `crates/hya-core/src/runtime_registry.rs:1814 (resolve_global_reference), crates/hya-bundle/src/catalog.rs:11 (ExportKind)`
- Why it is still open: The section opens with "Each `allow` / `deny` entry resolves to a stable id. Accepted forms:" and then closes with "The five **ExportKind** namespaces are: `tool`, `skill`, `mcp`, `hook`, `extension`. Resource stable ids use `bundle:<bundle_id>/<kind>/<local_id>`." A reader following that grammar will write `bundle:<id>/hook/<local_id>` or `harness:extension/...` in an `allow` list. `resolve_global_reference` (crates/hya-core/src/runtime_registry.rs:1814-1875) hard-whitelists the kind to `tool | skill | mcp` for BOTH the `harness:` and `bundle:` prefixes and returns `BundleError::UnknownResourceReference` for `hook` or `extension`. The doc never states that restriction; hook selection is only possible through the separate `hook_refs` field, and extensions are never referenceable at all. The grammar as written is unusable for two of the five kinds it advertises.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
