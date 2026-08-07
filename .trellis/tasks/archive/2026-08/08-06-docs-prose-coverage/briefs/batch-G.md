# Batch G - tools-and-permissions.md, agent-tool-surface.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 2 file(s). Do not create or edit any other file.

- `docs/architecture/tools-and-permissions.md`
- `docs/architecture/agent-tool-surface.md`

You have **26 gap entries** and **8 stale claims** to resolve.

These two files CURRENTLY CONTRADICT EACH OTHER on the write/edit tool schemas, the builtin tool inventory, and the `skill` row. You own both. Read the source, decide which is right, and make them agree -- do not simply pick one. Leave the `skill` row pointing at docs/skills.md, which Batch L is writing in parallel.

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
   list does not count. 15 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/architecture/tools-and-permissions.md`

**1. [behavior] MAX_TOOL_OUTPUT_CHARS / cap_tool_output** — `contradicted` · severity high

- Source: `crates/hya-tool/src/output_cap.rs:11-29`
- Evidence: docs/architecture/tools-and-permissions.md:40 states "Large string outputs are truncated at 16 KiB and include a truncation marker" and docs/project-structure.md:132 states "Tool output is capped at 16 KiB for large text fields." The real global cap is `MAX_TOOL_OUTPUT_CHARS = 5000` characters, applied to every successful tool result at crates/hya-core/src/engine/turn.rs:854. 16 KiB is only the shell tool's own internal stdout/stderr cap (crates/hya-tool/src/shell.rs).
- Write: Rewrite the `## Output Limits` section to describe TWO distinct, stacked caps. (1) Per-tool caps: `shell`/`bash` cap combined stdout+stderr at 16 KiB and spill the full text to `.hya/tool-output/`; `glob` and `grep` cap rows at 100. (2) A GLOBAL cap applied afterwards to every successful builtin, MCP and plugin result before it becomes an `Event::ToolResult`: `cap_tool_output` keeps only the LAST 5000 characters and replaces the value with a string prefixed `[tool output truncated: original N chars; showing last 5000 chars]`. Note that the global cap replaces a structured JSON result with a plain string, so downstream consumers must tolerate that shape change. Apply the same correction to docs/project-structure.md:132.

**2. [api] Action enum (permission verbs)** — `thin` · severity high

- Source: `crates/hya-tool/src/permission.rs:14-29`
- Evidence: docs/architecture/tools-and-permissions.md:53 says only "`Action` | Resource operation category such as `Read`, `Edit`, `Grep`, or `Bash`." The full 14-value list is nowhere in docs/, and the enum has no rustdoc on the defining item (permission.rs:12-29).
- Write: Replace the `Action` table row with an explicit list of all fourteen actions, serialized lowercase in saved-permission rows and rules: `tool`, `read`, `edit`, `glob`, `grep`, `bash`, `task`, `mcp`, `webfetch`, `websearch`, `todowrite`, `skill`, `lsp`, `external_directory`. Say which tools raise which action (read/ls -> read; write/edit/apply_patch -> edit; glob/find -> glob; grep -> grep; shell/bash -> bash; task -> task; MCP bridge -> mcp; webfetch -> webfetch; websearch -> websearch; skill -> skill; lsp -> lsp) and that `external_directory` is raised by any tool touching a path outside the session workdir.

**3. [api] Resource enum (permission objects)** — `thin` · severity medium

- Source: `crates/hya-tool/src/permission.rs:32-59`
- Evidence: docs/architecture/tools-and-permissions.md:54 says "`Resource` | Path, glob, command, subagent, or any resource" — it omits Tool, Url, WebSearch and Skill, and never explains the `pattern()` flattening. No rustdoc on the enum.
- Write: List all nine resource shapes and what each carries: `Tool(name)`, `Path(resolved path)`, `Glob(pattern)`, `Command(shell command text, also used for the namespaced MCP tool name)`, `Subagent(agent id)`, `Url(fetched url)`, `WebSearch(query)`, `Skill(skill name)`, and `Any`. State that every resource flattens to a single match-pattern string via `Resource::pattern()`, and that `Any` flattens to `"*"` — which is why a resource-level "allow always" grant (which stores `Rule(action, "*", Allow)`) allows the entire action.

**4. [api] PermissionInterceptor trait** — `undocumented` · severity medium

- Source: `crates/hya-tool/src/permission.rs:404-417`
- Evidence: Grep for "interceptor" across in-scope docs matches only docs/hya-pi-compat-comparison.md in unrelated prose. The trait has no rustdoc at permission.rs:404-417, and `PermissionPlane::with_interceptor` (permission.rs:502-505) is undocumented.
- Write: Add the interceptor to the `## Ask Flow` section as an explicit ordered stage. Document that `PermissionInterceptor` is an optional async hook installed via `PermissionPlane::with_interceptor`; it is consulted AFTER remembered grants and BEFORE the user ask channel, at both the invocation gate and the resource gate. Returning `Some(Decision)` short-circuits the prompt; returning `None` defers to the normal ask channel. The interceptor also contributes its own identity to `semantic_identity_v1`, so swapping interceptors changes the policy fingerprint. The only shipped implementation is the plugin `PermissionBridge`.

**5. [behavior] external directory gate** — `thin` · severity high

- Source: `crates/hya-tool/src/tool.rs:599-621`
- Evidence: docs/architecture/agent-tool-surface.md documents the gate only for read (lines 174-186), edit (288-290) and glob/grep (357-362), and explicitly notes FIND skips it (388). It never mentions that write (write.rs:98-112), lsp (lsp.rs:127-144) and shell's `workdir` argument (shell.rs:250-261) are also gated. No doc collects the enforcement points in one place.
- Write: Add an `## External directory boundary` section listing every enforcement point: read, write, edit, apply_patch (via its per-path Edit checks), lsp, glob, grep, and shell's optional `workdir` argument. State the shared shape: for any resolved path outside the session workdir, an `Action::ExternalDirectory` assert runs on the CONTAINING directory's `<dir>/*` pattern BEFORE the normal Read/Edit check, and it is the one action a call-level grant never satisfies (so it prompts separately even inside an already-approved tool call). Note the boundary is lexical — `..` is popped textually and symlinks are not canonicalized — and that `find` deliberately does not perform this check.

**6. [behavior] per-turn external directory allowlist** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:885-902`
- Evidence: No in-scope doc mentions `run_turn_with_external_dirs`, per-turn reference directories, or a per-turn permission rule overlay. Grep for "reference directories" and "external dirs" across docs/ returns nothing.
- Write: In the new external-directory section, document the escape hatch: `run_turn_with_external_dirs` layers `Rule(ExternalDirectory, "<dir>/*", Allow)` onto the session's snapshot rules for the duration of that turn, so directories the caller explicitly attached never prompt. The server derives that list from the session's reference directories (crates/hya-server/src/compat/session_prompt.rs:114-124). Make clear the overlay is per-turn and is not persisted as a saved permission.

**7. Permission bridge (items 44,45,46): first non-defer plugin decides, SHA-256 semantic identity v1 invalidation, and the WireResource union tool|path|glob|command|subagent|url|web_search|skill|any** — `undocumented` · severity high

- Source: `crates/hya-plugin/src/permission_bridge.rs:17,81; crates/hya-plugin/src/messages.rs:229`
- Evidence: docs/architecture/tools-and-permissions.md is the natural home and never mentions plugins as a permission source beyond one clause ('Compat plugin permission hook mapping' in docs/compat-parity.md:85). Its Resource row (line 54) lists only 'Path, glob, command, subagent, or any resource'. grep 'permission.ask', 'semantic identity', 'PermissionBridge' in scope: zero hits.
- Write: Three things. (1) Resolution: permission.ask is polled across plugins in load order and the FIRST plugin returning allow_once, allow_always, or reject wins; if every plugin defers (or every plugin errors) the host falls through to its normal interactive user prompt. (2) Cache invalidation: remembered plugin-mediated decisions are keyed by a domain-separated SHA-256 digest over the literal domain string b"hya.plugin.permission-bridge.semantic-identity/v1" plus, per participating plugin, its id, its canonical initialize declaration, and its effective permission.ask posture — so adding, removing, or changing any permission plugin automatically invalidates previously cached decisions. (3) Fix the incomplete Resource row: the permission.ask wire resource is a tagged union with NINE variants — tool, path, glob, command, subagent, url, web_search, skill, and any.

**8. `skill` tool contract and the 10-file sampling cap (items 133,134)** — `contradicted` · severity medium

- Source: `crates/hya-tool/src/skill.rs:14,98`
- Evidence: docs/architecture/tools-and-permissions.md:33 describes the tool as '| `skill` | skill path/name input | Skill content. |'. The code takes ONLY `{name}` (required) — a path is not an accepted input — and the output is not bare 'skill content' but a <skill_content> block plus a file:// base directory and a sampled file list. FILE_SAMPLE_LIMIT=10 appears in no doc.
- Write: Correct the row to: input `{ "name": string }` (name only — a path is rejected); output a <skill_content> block containing the skill body, the canonicalized base directory as a file:// URL, and a sampled file list. Add that the tool asserts Action::Skill / Resource::Skill(name) permission before returning anything, and that the file list is CAPPED at FILE_SAMPLE_LIMIT = 10 files, listed recursively in sorted order, excluding SKILL.md itself — the output says so explicitly, and a skill with more than 10 supporting files will show an incomplete list.

**9. External directories become per-turn permission rules** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:885`
- Evidence: Zero hits. docs/architecture/tools-and-permissions.md describes Rule/Action/Mode but never mentions external directories; docs/architecture/runtime.md mentions run_turn_with_external_dirs nowhere.
- Write: Add a section 'External directories': each directory passed to run_turn_with_external_dirs is converted into a Rule{action: Action::ExternalDirectory, resource: "<dir>/*", mode: Mode::Allow} and layered onto the session's permission snapshot for the duration of that turn only. It is not persisted as a SessionPermissionSet, so it does not survive the turn.

**10. Structured ToolError value / tool_error_type taxonomy** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/tool_error.rs:4`
- Evidence: crates/hya-core/src/engine/tool_error.rs has zero doc comments. No in-scope doc lists the error `type` values. docs/architecture/tools-and-permissions.md covers permission modes but not the error payload a client must switch on.
- Write: Document the ToolError `value` payload as {"error":{"type":..., "message":...}} and enumerate every `type`: input, permission, io, json, cancelled, overloaded, operation_id_conflict, operation_already_handled, unknown_agent_id, agent_spawn_not_allowed, unsupported_inline_agent_field, unknown. Say which are retryable (overloaded, io) versus terminal, since clients switch on this string.

**STALE 1.** The document claims: "Large string outputs are truncated at 16 KiB and include a truncation marker." — presented as the general tool-output limit.

- Reality: The general limit is `MAX_TOOL_OUTPUT_CHARS = 5000` CHARACTERS, applied by `cap_tool_output` to every successful builtin/MCP/plugin result at crates/hya-core/src/engine/turn.rs:854, keeping only the LAST 5000 chars behind a `[tool output truncated: original N chars; showing last 5000 chars]` banner. 16 KiB is only the shell tool's internal stdout/stderr cap (crates/hya-tool/src/shell.rs:41-181), which additionally spills the full text to `.hya/tool-output/`.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: Builtin table documents `write` as `{ "path": string, "content": string }` and `edit` as `{ "path": string, "old": string, "new": string }`.

- Reality: The advertised model-facing schemas require `filePath` + `content` (crates/hya-tool/src/write.rs:33-41) and `filePath` + `oldString` + `newString` (crates/hya-tool/src/edit.rs:16-26). The short spellings are runtime-only serde aliases and will fail provider-side schema validation. docs/architecture/agent-tool-surface.md:158-166,274-278 already states this correctly for read/edit, so the two docs contradict each other.
- Action: correct or delete. Do not merely supplement.

**STALE 3.** The document claims: "The builtin registry includes:" followed by a table intended as the builtin inventory.

- Reality: The table omits six registered canonical builtins: `list_agents`, `send`, `roster`, `channels`, `join`, `leave` (crates/hya-tool/src/tool.rs:237-271 registers 26 canonical names). docs/architecture/agent-tool-surface.md:21-34 has the complete list; this table has not been updated alongside it.
- Action: correct or delete. Do not merely supplement.

**STALE 4.** The document claims: `Resource` = 'Path, glob, command, subagent, or any resource.'

- Reality: crates/hya-plugin/src/messages.rs:229 WireResource is a nine-variant tagged union: tool, path, glob, command, subagent, url, web_search, skill, and any. The doc omits tool, url, web_search, and skill.
- Action: correct or delete. Do not merely supplement.

**STALE 5.** The document claims: The `skill` tool takes 'skill path/name input' and returns 'Skill content.'

- Reality: crates/hya-tool/src/skill.rs:98 accepts ONLY `{name}` (a path is not an accepted input) and returns a <skill_content> block plus the canonicalized base directory as a file:// URL and a file list capped at FILE_SAMPLE_LIMIT = 10 entries.
- Action: correct or delete. Do not merely supplement.

### `docs/architecture/agent-tool-surface.md`

**1. [tool] write** — `contradicted` · severity high

- Source: `crates/hya-tool/src/write.rs:24-96`
- Evidence: docs/architecture/tools-and-permissions.md:22 documents write as `{ "path": string, "content": string }`; the advertised JSON schema at write.rs:33-41 declares `required: ["filePath", "content"]` and `path` is only a serde alias. docs/architecture/agent-tool-surface.md has dedicated READ and EDIT sections but no WRITE section, so BOM propagation, parent-dir creation, and the formatter/LSP post-step are never spelled out for write.
- Write: Add a `## WRITE` section parallel to the existing READ/EDIT sections. State: advertised schema requires `filePath` and `content` (`path` is a runtime-only alias, same as READ); parent directories are created; an existing file's UTF-8 BOM is preserved and a BOM present in the incoming content is propagated; after the write the configured formatter runs, the BOM is re-synced, the LSP plane is touched and diagnostics are appended to the human-readable output and returned under `metadata.diagnostics`. Also note WRITE performs an `Action::ExternalDirectory` check on `<parent>/*` for paths outside the workdir (write.rs:98-112) before the `Action::Edit` check. Fix the `write` row in docs/architecture/tools-and-permissions.md:22 to say `filePath` at the same time.

**2. [tool] apply_patch (alias patch)** — `thin` · severity high

- Source: `crates/hya-tool/src/apply_patch/mod.rs:26-151`
- Evidence: Only one-line table rows exist: docs/architecture/tools-and-permissions.md:24 ("patch text | Aggregate diff and per-file metadata") and docs/project-structure.md:117. docs/architecture/agent-tool-surface.md lists it in the builtin inventory (line 29) and in the model-filter rules but never documents its input. No doc names the `patchText` parameter or the envelope format.
- Write: Add an `## APPLY_PATCH` section. Document: the parameter is `patchText` (alias `patch`) carrying a Codex/Compat-style patch envelope; supported hunk kinds are add, update, delete and move; every path in the envelope must be relative and must not escape the session workdir (an escaping or absolute path is an input error); every touched path is permission-checked as `Action::Edit` before any file is written, so a denial leaves the whole patch unapplied; the result is a Compat-style title plus an aggregate diff and per-file metadata; the same post-edit formatter + BOM re-sync + LSP-diagnostics step as write/edit runs afterward. Also record that `apply_patch` is the ONLY file-mutation tool advertised to gpt-* models (edit/write are hidden there), which is already stated at docs/architecture/agent-tool-surface.md:88-101.

**3. [tool] lsp** — `thin` · severity medium

- Source: `crates/hya-tool/src/lsp.rs:29-125`
- Evidence: docs/architecture/tools-and-permissions.md:32 says `lsp | operation input | LSP provider response`; docs/project-structure.md:124 says "Dispatch workspace-symbol/diagnostic-style LSP operations". Neither the nine operation values (crates/hya-tool/src/lsp_plane.rs:12-40) nor the line/character parameters appear anywhere in docs/.
- Write: Add an `## LSP` section listing the exact `operation` enum values: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, prepareCallHierarchy, incomingCalls, outgoingCalls. Document that the call takes a file path plus a 1-based `line` and `character` (converted to LSP 0-based internally), plus an optional `query` used only by workspaceSymbol. Note the tool is `ToolPermission::ReadOnly`, that it performs an `Action::ExternalDirectory` check for files outside the workdir (lsp.rs:127-144), and that with no language server registered for the file type it returns an explanatory message rather than an error.

**4. [tool] task** — `thin` · severity high

- Source: `crates/hya-tool/src/task.rs:105-352`
- Evidence: docs/architecture/agent-tool-surface.md:47-52 describes the capabilities in prose ("one subagent, a multi-member hya extension, model/category overrides, session resumption, background execution, resident agents, and inline ephemeral agent definitions") but names no parameter. No doc gives the field names, the `subagent_type` default, or the resume sentinels.
- Write: Expand the `task` paragraph into a parameter table: `description` (short label), `prompt` (the work), `subagent_type` (agent id, defaults to "general"), `category`/`model` overrides, `task_id` for resuming an existing subagent session where the sentinels `new`, `null`, `none`, `undefined` all mean "start fresh", `command`, `background` (bool), `resident` (long-lived actor rather than a one-shot turn), `inline_agent` (a request-scoped agent overlay; unsupported fields fail with UNSUPPORTED_INLINE_AGENT_FIELD), and the hya-specific `members[]` array that fans one call out to several subagents. State that every member is checked against `Action::Task` and against the caller's `can_spawn` roster.

**5. [tool] skill** — `thin` · severity medium

- Source: `crates/hya-tool/src/skill.rs:93-148`
- Evidence: docs/architecture/tools-and-permissions.md:33 says `skill | skill path/name input | Skill content` — ambiguous about whether it takes a path or a name. The schema takes exactly one required property `name`. No doc describes the returned envelope.
- Write: Add a short `### SKILL` subsection: the only parameter is `name`, and it must be a skill name from the `available_skills` list injected into the system prompt (not a path). The tool asserts `Action::Skill` on `Resource::Skill(name)`, then returns a `<skill_content>` envelope containing the SKILL.md body, the skill's absolute base directory (relative paths inside the skill resolve against it), and a sampled `<skill_files>` listing capped at 10 entries — so the file list is explicitly incomplete. Correct the `skill path/name input` wording in docs/architecture/tools-and-permissions.md:33.

**6. [tool] todowrite (alias todo)** — `thin` · severity low

- Source: `crates/hya-tool/src/todo.rs:70-123`
- Evidence: docs/architecture/tools-and-permissions.md:35 says `todowrite (todo) | todo items | Latest todo snapshot for the session`; docs/project-structure.md:127 similar. The item shape `{content, status, priority}` and the replace-not-append semantics are undocumented.
- Write: Add a short `### TODOWRITE` subsection: the input is a `todos` array of `{content, status, priority}` objects; the call REPLACES the session's whole todo list rather than appending; the list lives only in the in-memory TodoPlane (it is not persisted independently of the event log); the result echoes the list back with a title carrying the count of still-open items.

**7. [tool] question** — `thin` · severity medium

- Source: `crates/hya-tool/src/question.rs:37-131`
- Evidence: docs/architecture/agent-tool-surface.md:36-40 only says `question` "accepts a batch of structured questions". docs/architecture/tools-and-permissions.md:31 says "prompt/options input". No field names anywhere.
- Write: Document the `question` input: an array of question objects, each with `question` (the text), optional `header`, `options` as `[{label, description}]`, `multiple` (allow several selections) and `custom` (allow a free-text answer outside the option list). The tool routes through the InteractionPlane and returns the chosen option labels; a question the user did not answer renders as `Unanswered` rather than failing the call.

**8. [tool] ask_user** — `thin` · severity medium

- Source: `crates/hya-tool/src/tool.rs:983-1037`
- Evidence: docs/architecture/agent-tool-surface.md:36-40 says only "`ask_user` is a single free-text/select interaction". The `kind` discriminator and the non-error cancellation contract are undocumented.
- Write: Document `ask_user`: a `kind` of `"text"` (with an optional `default`) or `"select"` (with `options` and `allow_custom`), plus the question text. Crucially: a cancelled or failed ask does NOT produce a tool error — it returns `{"answer": "", "cancelled": true}`, so callers must inspect `cancelled` rather than relying on an error. Contrast this with `question`, which renders unanswered entries as "Unanswered".

**9. [tool] webfetch (alias fetch)** — `thin` · severity medium

- Source: `crates/hya-tool/src/webfetch/mod.rs:38-164`
- Evidence: docs/architecture/tools-and-permissions.md:29 says `webfetch (fetch) | URL input | Fetched web content`; docs/project-structure.md:122 similar. Greps for "5 MB", "5MB", "format" and "120" in docs/ return nothing for webfetch. No limit, format or attachment behavior is documented.
- Write: Add a `### WEBFETCH` subsection: parameters are `url` (http/https only), `format` = `text` | `markdown` (default) | `html`, and `timeout` in seconds — default 30 s, clamped to a maximum of 120 s. Responses larger than 5 MB are rejected outright. Responses whose content type is jpeg/png/gif/webp are returned as base64 data-URI attachments instead of text. The tool asserts `Action::WebFetch` on `Resource::Url(url)` and carries `ToolPermission::Tool`, so under `permission.model: default` it asks before every fetch.

**10. [tool] websearch (alias search)** — `thin` · severity medium

- Source: `crates/hya-tool/src/websearch.rs:119-158`
- Evidence: docs/configuration.md:340-361 documents the `tools.websearch` CONFIG block (provider/endpoint/key/enabled) and docs/architecture/agent-tool-surface.md:103-140 explains why the provider filter was removed, but no doc lists the tool's own call parameters. Grep for `numResults`, `livecrawl`, `contextMaxCharacters` in docs/ returns nothing.
- Write: Add the websearch call parameters next to the existing provider discussion: `query` (required), `numResults` (default 8), `livecrawl` = `fallback` (default) | `preferred`, `type` = `auto` (default) | `fast` | `deep`, and `contextMaxCharacters` (default 10000). Note that the tool is itself an MCP client: Exa is called at https://mcp.exa.ai/mcp with the key appended as an `?exaApiKey` query parameter, Parallel at https://search.parallel.ai/mcp with a bearer token, and that it asserts `Action::WebSearch` on `Resource::WebSearch(query)`.

**11. [tool] send** — `thin` · severity medium

- Source: `crates/hya-tool/src/mailbox.rs:228-279`
- Evidence: docs/architecture/agent-tool-surface.md:33 lists `send` in the "Agents and teams" row with the gloss "use team mail/channels"; docs/testing/agent-matrix.md:63 lists it as a covered tool name. No doc gives its parameters. docs/adr/0001-event-sourced-mailbox-and-channels.md covers the event model, not the tool schema.
- Write: Add a `### Mailbox tools` subsection and document `send` first: required `to` (a teammate handle such as `reviewer-3`, or a channel with a leading `#` such as `#build`) and `body`; optional `kind` = `message` (default) | `announcement`. An empty/whitespace body is an input error. Channel mail reaches every current subscriber. The result metadata returns the resolved sender handle (`from`), the normalized recipient address (`to`) and the `recipients` count. Note all mailbox tools report "available only inside a running team" when the mailbox plane is disconnected.

**12. [tool] roster** — `thin` · severity low

- Source: `crates/hya-tool/src/mailbox.rs:284-302`
- Evidence: Named only in the tool inventory (docs/architecture/agent-tool-surface.md:33), in the default-allow read-only list (docs/configuration.md:399) and in docs/testing/agent-matrix.md:63. The returned fields are documented nowhere.
- Write: In the new `### Mailbox tools` subsection, document `roster`: no parameters; returns one row per live teammate with `handle`, agent type, session id, scheduling mode, `status` (one of idle | busy | done | failed, folded in from `AgentActivityChanged` by the resident supervisor) and `current_task`. It is registered `ToolPermission::ReadOnly`, so it allows without prompting under `default`.

**13. [tool] channels** — `thin` · severity low

- Source: `crates/hya-tool/src/mailbox.rs:363-404`
- Evidence: Named only in docs/architecture/agent-tool-surface.md:33, docs/configuration.md:399 and docs/testing/agent-matrix.md:52,63. No parameter or output description in docs/.
- Write: In `### Mailbox tools`, document `channels`: no parameters; lists every mail channel of the acting agent's team with its member list and message count. Registered `ToolPermission::ReadOnly`.

**14. [tool] join** — `undocumented` · severity medium

- Source: `crates/hya-tool/src/mailbox.rs:414-440`
- Evidence: Grep for `join` across in-scope docs returns only unrelated matches (bundle 'joins in the owner', 'bun-disjoint'). The only listing of the tool name is docs/testing/agent-matrix.md:63, a test-coverage table, not a description.
- Write: In `### Mailbox tools`, document `join`: takes a channel name, with the leading `#` optional (`#build` and `build` are the same channel). It subscribes the acting agent, and CREATES the channel if it does not exist yet — there is no separate create-channel tool. It is a mutating mailbox operation and is registered `ToolPermission::Tool`, so it asks under `default`.

**15. [tool] leave** — `undocumented` · severity medium

- Source: `crates/hya-tool/src/mailbox.rs:445-471`
- Evidence: Grep for `leave` across in-scope docs matches only unrelated prose ('leaves the previous snapshot active', 'to leave offline mode'). The only listing is docs/testing/agent-matrix.md:63 and a test note at docs/testing/process-e2e.md:134.
- Write: In `### Mailbox tools`, document `leave`: takes a channel name (leading `#` optional) and unsubscribes the acting agent from it. After leaving, channel posts no longer reach the agent but direct handle mail still does. Registered `ToolPermission::Tool`.

**16. [api] ToolError taxonomy** — `stale` · severity medium

- Source: `crates/hya-tool/src/tool.rs:38-64`
- Evidence: docs/architecture/agent-tool-surface.md:453-457 states "Tool errors are categorized as `input`, `permission`, `io`, `json`, `cancelled`, or `unknown`". crates/hya-core/src/engine/tool_error.rs:17-32 maps twelve variants, adding `overloaded`, `operation_id_conflict`, `operation_already_handled`, `unknown_agent_id`, `agent_spawn_not_allowed` and `unsupported_inline_agent_field`.
- Write: Replace the six-value list with the full mapping from `ToolError` variant to the wire `type` string emitted inside `{"error":{"type":...,"message":...}}`: Input->input, Permission->permission, Io->io, Json->json, Cancelled->cancelled, Overloaded->overloaded, OperationIdConflict->operation_id_conflict, OperationAlreadyHandled->operation_already_handled, UnknownAgentId->unknown_agent_id, AgentSpawnNotAllowed->agent_spawn_not_allowed, UnsupportedInlineAgentField->unsupported_inline_agent_field, Other->unknown. Note that only `permission` is protected from rewriting by `tool.execute.after` hooks.

**STALE 1.** The document claims: "Tool errors are categorized as `input`, `permission`, `io`, `json`, `cancelled`, or `unknown`."

- Reality: crates/hya-core/src/engine/tool_error.rs:17-32 emits twelve type strings; the doc is missing `overloaded`, `operation_id_conflict`, `operation_already_handled`, `unknown_agent_id`, `agent_spawn_not_allowed` and `unsupported_inline_agent_field`.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: The doc pins its claims to explicit line ranges, e.g. `ToolRegistry::builtins()` at crates/hya-tool/src/tool.rs:145-183, permission classes at tool.rs:127-141 and tool.rs:265-272, `SEARCH_LIMIT` at tool.rs:76, hidden aliases at tool.rs:178-182, the shared walker at tool.rs:289-300, GLOB at tool.rs:349-441, GREP at tool.rs:445-579, FIND at tool.rs:640-690.

- Reality: Every one of those anchors has drifted. Actual locations: `builtins()` at tool.rs:237, `ToolPermission`/`ResolvedTool::invocation` at tool.rs:184-215, `builtin_permission()` at tool.rs:539-547, `SEARCH_LIMIT` at tool.rs:130, alias registration at tool.rs:315-372, GLOB at tool.rs:629-717, GREP at tool.rs:726-855, LS at tool.rs:862-912, FIND at tool.rs:920-965, `ask_user` at tool.rs:983-1037. The prose is still accurate; only the anchors are wrong, but they make the doc untrustworthy to verify against.
- Action: correct or delete. Do not merely supplement.

**STALE 3.** The document claims: "`ToolRegistry::builtins()` installs 26 canonical schema names before model filtering."

- Reality: Still correct (26 canonical names), but the cited range crates/hya-tool/src/tool.rs:145-183 no longer contains `builtins()` — it is at tool.rs:237-271. Included here so the anchor is corrected together with the count check.
- Action: correct or delete. Do not merely supplement.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 26 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
