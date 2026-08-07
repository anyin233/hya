# Fix batch H4 - tui-reference.md, tui-keybindings.md, project-structure.md, agent-bundle-authoring.md, agent-matrix.md, AGENTS.md, CONTEXT.md, opencode-feature-inventory.md, hya-pi-compat-comparison.md, FOLLOWUPS.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/tui-reference.md`
- `docs/tui-keybindings.md`
- `docs/project-structure.md`
- `docs/agent-bundle-authoring.md`
- `docs/testing/agent-matrix.md`
- `AGENTS.md`
- `CONTEXT.md`
- `docs/opencode-feature-inventory.md`
- `docs/hya-pi-compat-comparison.md`
- `docs/FOLLOWUPS.md`

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

- The doc claims: Permission prompt section: "**Reject** (for child sessions, or via the reject path) can open a free-text **Reject permission** editor: Return confirms with the message, Escape cancels back."
- Reality: The free-text reject editor is gated solely on the session being a child: `if (option === "reject") { if (session()?.parentID) { setStore("stage", "reject"); return } ... }` — for a root session Reject immediately calls `permission.reply({ reply: "reject" })` with no message editor and no second stage. The trailing "or via the reject path" implies a second, non-child route to the editor that does not exist.
- Source: `packages/hya-tui-ts/src/upstream/routes/session/permission.tsx:411-426`

**STILL OPEN 1 - Shell mode (`!` at column 0)** (`thin`)

- Source: `packages/hya-tui-ts/src/upstream/component/prompt/index.tsx:800`
- Why it is still open: The 'Shell mode' section documents only how to ENTER and EXIT the mode and its cosmetic effects (agent label becomes `Shell`, shell placeholder set, `esc exit shell mode` hint, Escape/Backspace-at-offset-0 to leave). It never says what the mode actually DOES: submitting in shell mode calls `sdk.client.session.shell({ sessionID, agent, model, command: inputText })` (prompt/index.tsx:999-1008) — the buffer is run as a shell command against the session instead of being sent to the model — and the prompt then auto-resets to `normal` mode (`setStore("mode", "normal")`) after the submit. Neither the shell-execution semantics nor the auto-reset appear anywhere in docs/tui-reference.md, docs/tui-keybindings.md, or docs/cli.md. A reader cannot tell from the current prose whether `!` prefixes a message, runs a command, or just re-themes the prompt.

**CRITIC 1 - Whether the shell/bash tool carries a model-authored `description`**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-tool/src/shell.rs:27`
- Why it matters: docs/tui-reference.md:281-286 documents the transcript Shell block as rendering `# <description>[ in <workdir>]`, and :192 says the bash permission title is "tool `description` or `Shell command`". docs/compat-parity.md:84 states "Shell now follows current Compat command-titled output and no longer exposes or records a model-authored `description`", and both docs/architecture/tools-and-permissions.md:36 and docs/architecture/agent-tool-surface.md:156-161 give the shell schema as `{command, timeout, workdir, env}` with no `description`. `ShellInput` / `ShellTool::schema()` confirm there is no `description` field, so for hya's own shell calls the renderer always falls back to the literal "Shell" — tui-reference.md describes a field the backend can never populate.


### `docs/tui-keybindings.md`

**CONTRADICTION 1**

- The doc claims: Section 'Binding collisions': "Two defaults share the same chord", followed by a table listing only `<leader>q` (session.queued_prompts vs app.exit) and `<leader>h` (session.toggle.conceal vs tips.toggle).
- Reality: Many more default chords are shared across commands in keybind.ts. `ctrl+d` is the default for app_exit, session_delete, stash_delete, input_delete AND dialog.move_session.delete. `ctrl+f` is the default for session_pin_toggle, model_favorite_toggle, permission.prompt.fullscreen AND input_move_right. `ctrl+p` is command_list, dialog.select.prev AND prompt.autocomplete.prev. `escape` is session_interrupt, diff_close AND prompt.autocomplete.hide. `tab` is agent_cycle, diff_switch_focus AND prompt.autocomplete.complete. The two listed pairs are resolved by binding layer exactly the same way as these, so the 'Two' framing is an unsupported completeness claim.
- Source: `packages/hya-tui-ts/src/upstream/config/keybind.ts:48-228`

**CONTRADICTION 2**

- The doc claims: Message scrolling table: `session.half.page.up` / `session.half.page.down` = "Scroll up/down half a page" — the same wording already used for `session.page.up` / `session.page.down` ("Scroll up by half a page (implementation uses half viewport height)").
- Reality: The two commands scroll different amounts. `session.page.up`/`.down` call `target.scrollBy(-target.height / 2)` / `(target.height / 2)` (index.tsx:889, 900), while `session.half.page.up`/`.down` call `target.scrollBy(-target.height / 4)` / `(target.height / 4)` (index.tsx:931, 942). The half-page commands move a QUARTER of the viewport, i.e. half of what the page commands move. As written the doc gives two distinct commands identical semantics.
- Source: `packages/hya-tui-ts/src/upstream/routes/session/index.tsx:884-943`


### `docs/project-structure.md`

**STILL OPEN 1 - hya-native embedding API (items 177,178): HyaNativeTransport / HyaNativeClient in-process transport and spawn_event_bridge SSE bridge** (`thin`)

- Source: `crates/hya-native/src/transport.rs:96, crates/hya-native/src/lib.rs:12`
- Why it is still open: The crate row was added to both docs and the spawn_event_bridge half is now fully documented (undecodable frames skipped, 50 ms re-subscribe backoff, stops on receiver drop — all verified against crates/hya-native/src/events.rs). The transport half stops one step short of usable: the docs name only `HyaNativeTransport`, and `HyaNativeClient` — the exported type alias `pub type HyaNativeClient = ApiClient<HyaNativeTransport>` (transport.rs:96, re-exported at lib.rs:12) — appears nowhere in docs/ (grep: zero hits outside crates/). Since the section explicitly frames itself as 'the supported way to embed hya inside another Rust process', a reader gets a Transport with no documented route to a callable client; they must find `ApiClient` in hya-sdk on their own.

**CRITIC 1 - Native (non-Compat) HTTP route list**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/lib.rs:60`
- Why it matters: docs/project-structure.md:232-239 introduces "The native hya routes are:" and lists only four: `POST /sessions`, `POST /sessions/:id/prompt`, `GET /sessions/:id/events`, `GET /sessions/:id/stream`. docs/architecture/server-client.md:31-36 lists six, additionally `POST /sessions/:id/command` and `POST /sessions/:id/shell` (with full CommandRequest/ShellRequest DTO documentation). The router registers all six, so project-structure.md presents an incomplete list as the complete native surface.

**CRITIC 2 - Permission class of list_agents / roster / channels**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-tool/src/tool.rs:626`
- Why it matters: docs/project-structure.md:129 and :133 put `list_agents` and `send, roster, channels, join, leave` all under permission action `Tool` (which asks by default). docs/architecture/agent-tool-surface.md:627 says "READ, LS, GLOB, FIND, GREP, LSP, SKILL, LIST_AGENTS, ROSTER, and CHANNELS are `ReadOnly`" (allow without prompting), and :132 / :137 repeat that roster and channels are `ToolPermission::ReadOnly`; docs/configuration.md:667 also lists the default auto-allow read-only set as "read, ls, glob, find, grep, lsp, skill, list_agents, roster, and channels". `builtin_permission()` maps exactly that set to `ToolPermission::ReadOnly`, so project-structure.md is the outlier and wrongly implies those three prompt.


### `docs/agent-bundle-authoring.md`

**STILL OPEN 1 - bundle agent fields description, color, model_policy, workdir (items 90,92,94,95)** (`thin`)

- Source: `crates/hya-bundle/src/source.rs:147, crates/hya-bundle/src/model.rs:128`
- Why it is still open: Three of the four fields are now properly documented (description, color, model_policy) and I confirmed color reaches the wire via crates/hya-server/src/compat/bound_agent_metadata.rs:97 and model_policy is applied in crates/hya-core/src/engine.rs:828 and crates/hya-app/src/runtime.rs:2134. `workdir` is not. The table says only '`workdir` | no | Optional working-directory hint on the prepared agent.' I searched every crate for a reader of `PreparedAgent::workdir`: there is none — hya-app/hya-server/hya-backend hits are all on the unrelated `st.agent.workdir` / `turn.agent.workdir` (AgentConfig), and hya-core has zero hits. The field is parsed by SourceAgent, copied at prepare.rs:878, serialized into the prepared catalog, and then never applied to anything. An author who writes `workdir: subdir` gets silent no-op behavior. The doc gives the field the same weight as the working fields; it needs the same 'parsed and stored, currently not applied' treatment the same rewrite correctly gave to SKILL.md `allowed-tools` and `model` in docs/skills.md:58-59.


### `docs/testing/agent-matrix.md`

**CRITIC 1 - Builtin tool count registered by ToolRegistry::builtins()**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-tool/src/tool.rs:313`
- Why it matters: docs/architecture/tools-and-permissions.md:16 and docs/architecture/agent-tool-surface.md:24 both say `ToolRegistry::builtins()` installs **26** canonical schema names; docs/testing/agent-matrix.md:58 says it "registers **25** primary tool names" and splits them 14 covered / 11 not covered. The constructor inserts 19 tools in the loop, then `shell` + a separately named `bash`, then 5 aliased canonicals (apply_patch, webfetch, websearch, todowrite, plan_exit) = 26 distinct registry names. The matrix table simply omits `bash`, so the coverage ledger silently under-reports one untested canonical tool name.


### `AGENTS.md`

**CRITIC 1 - Rust verification gate command (whether hya-e2e is excluded)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/.github/workflows/ci.yml:90`
- Why it matters: AGENTS.md:124 prescribes `cargo test --workspace` as the Rust verification gate. docs/development.md:26 and docs/testing/README.md:30 both prescribe `cargo test --workspace --exclude hya-e2e` and state that the exclusion "matches CI" because Track P spawns real backend processes and must not run multi-threaded under the default suite. CI actually runs `cargo test --workspace --jobs 1 --exclude hya-e2e` and then `cargo test -p hya-e2e -- --test-threads=1` separately, so AGENTS.md's command is the one that diverges and will produce flaky/failing local runs.


### `CONTEXT.md`

**CRITIC 1 - Existence of a well-known "broadcast" channel**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-core/src/engine/mailbox.rs:194`
- Why it matters: CONTEXT.md:195 defines Channel and asserts "**Broadcast** is a well-known channel everyone joins." docs/architecture/agent-tool-surface.md:140-144 says `join` "subscribes the acting agent and **creates** the channel if it does not exist — there is no separate create-channel tool", i.e. channels exist only once an agent explicitly joins. `channel_join` is the only emitter of `Event::ChannelJoined` in the runtime and nothing auto-subscribes members to a broadcast channel, so CONTEXT.md documents a delivery mechanism that does not exist.


### `docs/opencode-feature-inventory.md`

**CRITIC 1 - Hot skill reload implementation status**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-core/src/runtime_registry.rs:378`
- Why it matters: docs/opencode-feature-inventory.md:46 records Hot skill reload as "missing" (and :45 Runtime skill registration as "missing/partial: static discovery and skill tool exist"). docs/adr/0007-hot-skill-reload-visibility.md is Status: accepted and states it is "Implemented by the `0.34.5` `RuntimeRegistry`/`TurnBinding` seam", and docs/architecture/runtime.md:184 says a root turn "refreshes its skill candidate if the logical view changed" before capturing the TurnBinding. `RuntimeRegistry::refresh_skills(workdir)` exists and is driven per root turn, so the inventory row contradicts both the ADR and the runtime doc.


### `docs/hya-pi-compat-comparison.md`

**CRITIC 1 - Whether a "task board" is a live multi-agent runtime primitive**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-store/migrations/0001_init.sql:75`
- Why it matters: docs/hya-pi-compat-comparison.md:16 lists hya's multi-agent support as "Native runtime primitives: `task` tool, child sessions, team evidence projection, mailbox/task board, and optional worktree allocation". docs/architecture/storage.md:54 marks the `task_board` table "**Pre-ADR-0001.** Not on any live read path." The identifier appears only in the migration SQL — no Rust code in `crates/` references `task_board` — so it is a reserved dead table, not a shipped primitive, and the comparison page overstates the feature set.


### `docs/FOLLOWUPS.md`

**CRITIC 1 - Interactive OAuth login (device code / PKCE) implementation status**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-app/src/oauth/mod.rs:149`
- Why it matters: docs/FOLLOWUPS.md:8-14 lists OAuth interactive login under "Deferred (not yet implemented)", saying the remaining piece is "device-authorization request, opening the browser, polling the token endpoint, PKCE code exchange, and refresh-token handling" and that users must "paste a token via `hya-backend login`". docs/configuration.md:310 says "Interactive OAuth is implemented entirely in Rust", and docs/cli.md:392-446 plus README.md:68-77 document the shipped `hya oauth login --provider … --type openai-codex|grok-build`, `--device`/`--loopback`/`--browser` flags, the 1455 loopback port, refresh, and `oauth status`. The source has a full `crates/hya-app/src/oauth/` module (pkce.rs, openai_codex.rs device-code flow, grok_build.rs, callback.rs), so FOLLOWUPS.md is stale.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
