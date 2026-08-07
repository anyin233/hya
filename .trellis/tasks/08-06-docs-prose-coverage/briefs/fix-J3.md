# Fix batch J3 - runtime.md, storage.md, providers.md, server-client.md, agent-tool-surface.md, tools-and-permissions.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/architecture/runtime.md`
- `docs/architecture/storage.md`
- `docs/architecture/providers.md`
- `docs/architecture/server-client.md`
- `docs/architecture/agent-tool-surface.md`
- `docs/architecture/tools-and-permissions.md`

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


### `docs/architecture/runtime.md`

**CONTRADICTION 1**

- The doc claims: "A completed depth-0 turn calls finalize_root_spawn_admissions(root), which: cancels every live governor operation for that root; cancel-finalizes every nonterminal admission journal row; releases the root's per-run subagent budget entry." (runtime.md:267-276, mirrored at admission-and-governor.md:196-205)
- Reality: The call is conditional on a governor being installed: `if self.governor.is_some() && let Ok((root, 0)) = self.session_lineage(session).await { self.finalize_root_spawn_admissions(root).await?; }`. On an engine built without a `SubagentGovernor` the cleanup is skipped entirely, even though durable admission rows can still be written (claim_admission/start_admission run governor-free per admission-and-governor.md's own step list).
- Source: `crates/hya-core/src/engine/turn.rs:506-512`

**STILL OPEN 1 - Root-turn admission cleanup (finalize_root_spawn_admissions)** (`contradicted`)

- Source: `crates/hya-core/src/engine/turn.rs:506`
- Why it is still open: runtime.md:269 states flatly "A completed depth-0 turn calls finalize_root_spawn_admissions(root)", and docs/architecture/admission-and-governor.md:196-205 repeats it as "Root-turn cleanup (depth-0 turn completion)". The real call site is guarded: `if self.governor.is_some() && let Ok((root, 0)) = self.session_lineage(session).await { self.finalize_root_spawn_admissions(root).await?; }`. With no governor installed the cleanup never runs at all. That matters because admission-and-governor.md:184 itself documents that `begin_spawn_admission` still performs the durable `claim_admission` + `start_admission` without a governor ("Without a governor, steps 3-4 are skipped; start still runs after cancel check"), so journal rows are created but never cancel-finalized on a governor-less engine. Both docs need the guard stated, or the claim narrowed to "a completed depth-0 turn on a governor-backed engine".


### `docs/architecture/storage.md`

**CONTRADICTION 1**

- The doc claims: "The CLI uses in-memory stores for goal mode and `rpc`; `exec`, `run`, the TUI, `serve`, `tail-session`, and `sessions` use file-backed SQLite when `--db <PATH>` is supplied, otherwise they use in-memory stores where the command supports an empty database path." This sentence was rewritten in the same docs push (commit 11a28f01 "docs(cli): document session persistence semantics").
- Reality: For the interactive TUI path, `sessions`, and `tail-session`, an empty/default `--db` is NOT in-memory: `resolve_interactive_db` (crates/hya-backend/src/main.rs:51, called at lines 319, 355, 373) remaps it to $XDG_STATE_HOME/hya/sessions.db (falling back to $HOME/.local/state/hya/sessions.db, then ./.local/state/hya/sessions.db) and creates the directory. Only `exec`, `run`, and `serve` fall back to `SessionStore::connect_memory`. The trailing hedge "where the command supports an empty database path" is the only thing keeping the sentence from being flatly false; it never states the XDG remap, so a reader reaches exactly the wrong conclusion that gap entry 24 existed to correct. docs/cli.md itself was fixed properly; this sibling doc was not.
- Source: `crates/hya-backend/src/main.rs:45-67,319,355,373`


### `docs/architecture/providers.md`

**STILL OPEN 1 - configured_identity_v1 routing fingerprint — HTTP identity contents** (`thin`)

- Source: `crates/hya-provider/src/http.rs:410`
- Why it is still open: The `### HTTP fingerprint contents` list (providers.md:442-455) enumerates 11 components — tag, crate version, provider id, kind tag, endpoint, google base, alias markers, sorted model set, per-model reasoning variants, Capabilities bits, auth shape — but omits the last component the code actually appends: `crates/hya-provider/src/http.rs:410-417` writes the literal `bearer-resolver-slot` followed by a presence byte and (when a resolver is installed) the provider id. The gap entry's own remediation asked for exactly this ("and whether a bearer resolver is installed"). Consequence: a reader asking whether wiring OAuth bearer resolution onto a route flips the TurnBinding identity gets no answer from the list, and the `**Deliberately excluded:**` paragraph immediately after invites the list to be read as complete. The wording "identity bytes include" is the only thing keeping this from being an outright false statement.


### `docs/architecture/server-client.md`

**CRITIC 1 - GET /tui/bootstrap — single-round-trip startup aggregate endpoint**

- Source: `crates/hya-server/src/compat/tui.rs:17 (route), :37-155 (handler); consumed by packages/hya-tui-ts/src/upstream/context/sync.tsx:429-467; dedicated test suite at crates/hya-server/tests/tui_bootstrap_api.rs`
- Why it matters: This is the endpoint the shipped TUI calls first, and it returns 14 keys in one request: config, providers, provider_list, capabilities, agents, sessions (up to 100 hydrated session infos, empty/unnamed filtered), commands, lsp, mcp, mcp_resource, formatter, session_status, vcs (branch + default_branch), path, project. The frontend comment states the intent explicitly: "Prefer single-RTT /tui/bootstrap (hya servers). Fall back to multi-call for older backends." Anyone building an alternative frontend, a dashboard, or any integration against `hya-backend serve` currently has no way to discover it and would instead issue roughly a dozen separate calls to /global/config, /provider, /api/agent, /api/session, /api/command, /lsp, /mcp, /experimental/resource, /formatter, /vcs, /path and /project — every one of which IS documented, which makes the omission actively misleading about the cheapest way to bootstrap a client. It is not a stub: it has its own integration test file and its own bundle-catalog-vs-legacy-project agent-resolution behavior.


### `docs/architecture/agent-tool-surface.md`

**CONTRADICTION 1**

- The doc claims: The `task` parameter table lists `inline_agent` as "Request-scoped agent overlay (`name`, `prompt`, `description`, `category`, `model`, `resident`). Unsupported fields fail with `unsupported_inline_agent_field`" - presenting `description` as one of the supported overlay fields.
- Reality: `description` is the ONE inline_agent field the runtime rejects. `validate_unsupported_inline_agent_fields` returns `SpawnError::UnsupportedInlineAgentField { field: "description" }` whenever `inline.description.is_some()`, and it is called on every spawn path. A caller that follows the doc and sends `inline_agent.description` gets a tool error with wire type `unsupported_inline_agent_field`.
- Source: `crates/hya-app/src/runtime.rs:1961-1971 (called at 2033 and 2092); schema advertising the field at crates/hya-tool/src/task.rs:155`


### `docs/architecture/tools-and-permissions.md`

**CONTRADICTION 1**

- The doc claims: "For any resolved path outside the session workdir, tools run an `Action::ExternalDirectory` assert on the containing directory's `<dir>/*` pattern before the normal Read/Edit/Lsp/... check", and the enforcement-points table names `find` as the single exception ("**Does not** perform the external-directory check (deliberate compatibility gap)").
- Reality: `ls` also performs no external-directory check and is absent from the table entirely. `LsTool::execute` asserts only `Action::Read` on the raw string, and - like `find` - builds its directory with `PathBuf::from(path)` rather than `resolve_file(&ctx.workdir, path)`, so a relative path is not even resolved against the session workdir. `ls /etc` outside the workdir therefore never raises `Action::ExternalDirectory`, contradicting both the universal claim and the "find is the deliberate gap" framing.
- Source: `crates/hya-tool/src/tool.rs:951-1000 (LsTool; permission assert at 972-977), compare crates/hya-tool/src/tool.rs:1014-1032 (FindTool)`

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
