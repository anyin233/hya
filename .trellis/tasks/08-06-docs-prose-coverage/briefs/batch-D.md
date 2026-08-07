# Batch D - runtime.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/architecture/runtime.md`

You have **34 gap entries** and **5 stale claims** to resolve.

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

### `docs/architecture/runtime.md`

**1. [behavior] ToolRegistrySnapshot / dispatch identity** — `undocumented` · severity medium

- Source: `crates/hya-tool/src/tool.rs:473-537`
- Evidence: Grep for "dispatch identity", "logically_matches", "ToolRegistrySnapshot" and "snapshot identity" across docs/, README.md, CONTEXT.md, DESIGN.md, AGENTS.md returns nothing. docs/architecture/tools-and-permissions.md:127 only says "Source metadata never becomes a second dispatch registry", which is a different claim.
- Write: In the runtime-registry section, document the per-turn tool snapshot: a turn takes an immutable, lock-free `ToolRegistrySnapshot` of the registry so tool resolution cannot change mid-turn. Each builtin entry carries a SHA-256 dispatch identity computed over the domain string `hya.tool.builtin-dispatch/v1`, the crate version, and the canonical tool name; MCP/plugin sources get a per-source identity instead. `logically_matches` compares a candidate registry against the published one using those identities, which is how a reconciliation can tell a no-op refresh from a real change without diffing schemas.

**2. [hook] permission.ask plugin hook** — `thin` · severity medium

- Source: `crates/hya-plugin/src/permission_bridge.rs:32-116`
- Evidence: docs/architecture/runtime.md:207 mentions "permission asks" among hookable surfaces and docs/configuration.md:636 mentions "permission hooks", but the wire hook name `permission.ask`, the polling order, and the reply vocabulary appear nowhere. (A grep hit on "permission.ask" in docs/architecture/tools-and-permissions.md is a false positive matching the prose "permission asks".)
- Write: In `## Hooks`, add a `permission.ask` entry. Document: `PermissionBridge` implements `PermissionInterceptor` over the plugin host; every plugin that declared the `permission.ask` hook is polled in declaration order; the first non-`defer` reply decides and later plugins are not consulted. Valid replies are `allow_once`, `allow_always`, and `reject` with an optional `feedback` string that is folded into the `PermissionError::Denied` message the model sees. If every plugin replies `defer` (or none declare the hook), the request falls through to the normal user prompt. It is wired at crates/hya-app/src/runtime.rs:3966-3972.

**3. [hook] tool.execute.before hook** — `thin` · severity medium

- Source: `crates/hya-core/src/hooks.rs:25, crates/hya-core/src/engine/turn.rs:716-750`
- Evidence: docs/architecture/runtime.md:207 lists "tool before/after hooks" and docs/agent-bundle-authoring.md:78 names `tool.execute.before` as a selectable bundle hook ID, but no doc says the hook can REWRITE the tool input or VETO the call, nor what a veto looks like to the model.
- Write: In `## Hooks`, spell out `tool.execute.before`: it runs before every model-issued tool call (and also for direct shell turns, crates/hya-core/src/engine/shell.rs:127-157). A hook may return rewritten input JSON, which replaces the model's arguments, or a Veto with a reason. A veto skips execution entirely and emits a `ToolError` whose message is `blocked by plugin: <reason>`. Both the global plugin host and the per-session activation (sidecar) dispatcher run this hook.

**4. [behavior] activation (sidecar) hooks** — `thin` · severity low

- Source: `crates/hya-core/src/hooks.rs:39-65, crates/hya-core/src/engine/turn.rs:709-733`
- Evidence: docs/agent-bundle-authoring.md and docs/architecture/runtime.md:132-146 describe activation-scoped sidecars and their hook selection, but neither states that the activation dispatcher runs ALONGSIDE the global plugin host for the same hooks, nor what happens when it goes unhealthy mid-call.
- Write: In `## Hooks`, add a paragraph on activation hooks: a task-local activation hook dispatcher is scoped to one session and runs in addition to (not instead of) the global plugin host for `tool.execute.before` / `tool.execute.after`. If the activation dispatcher reports unhealthy while a tool call is in flight, the whole turn is cancelled rather than silently continuing without the sidecar's hooks.

**5. [behavior] semantic identity of the permission policy** — `undocumented` · severity low

- Source: `crates/hya-tool/src/permission.rs:474-499`
- Evidence: Grep for "semantic identity", "semantic_identity" and "hya.permission.semantic-identity" across all in-scope docs returns nothing. The function has only a one-line rustdoc ("Returns a stable identity for the immutable permission policy semantics") that does not say what is hashed.
- Write: Document `PermissionPlane::semantic_identity_v1` alongside the other runtime identities: it produces a SHA-256 fingerprint over the domain string `hya.permission.semantic-identity/v1` plus the snapshot resource rules, the invocation model, the compiled invocation rule selectors, and the installed interceptor's own identity. Any change to the effective policy — including swapping the interceptor — changes the fingerprint, which is how a reload can detect that permissions actually changed rather than re-deriving the rule set.

**6. [behavior] deferred MCP startup** — `thin` · severity medium

- Source: `crates/hya-app/src/runtime.rs:4033-4053`
- Evidence: docs/architecture/runtime.md:89 mentions "Startup, deferred MCP, and Compat MCP" in passing as reconciliation entry points, but nothing explains when deferral engages, what it changes about startup latency, or the failure message.
- Write: Document deferred MCP startup: when sideplane deferral is enabled AND MCP servers are configured, plugins are reconciled synchronously while MCP connection is moved to a background task, so a slow or hanging MCP server cannot block hya from starting. The consequence for users is that MCP tools may not be present for the very first turn. A refresh rejected by the reconciler is non-fatal and only prints `hya: MCP runtime refresh rejected` to stderr.

**7. [behavior] MCP resources** — `undocumented` · severity low

- Source: `crates/hya-mcp/src/resource.rs:27-54`
- Evidence: No in-scope doc mentions MCP resources at all. docs/configuration.md and docs/architecture/runtime.md discuss only MCP tools; grep for `resources/list` and `McpManager::resources` across docs/ returns nothing.
- Write: Add a short MCP resources paragraph to the MCP section: beyond tools, hya performs a best-effort `resources/list` per connected server and exposes the result through `McpManager::resources()`. Each entry is keyed `<sanitized server>:<sanitized resource name>`, where sanitizing replaces every non-alphanumeric character with `_`, and carries a `client` field naming the owning server. Be explicit that resources are NOT registered as tools and are not reachable by the model through the tool registry today.

**8. Native `/responses/compact` context compaction (supports_responses_compact, compact_endpoint derivation, request/response shape, engine wiring)** — `stale` · severity high

- Source: `crates/hya-core/src/engine/turn.rs:583, crates/hya-provider/src/http.rs:329`
- Evidence: docs/architecture/runtime.md:193-202 (`## Compaction and Summaries`) describes compaction as `ModelSummarizer` + `compact_context` only — it never mentions that the turn first tries the provider's native `/responses/compact`. Grepping all in-scope docs for `compact_responses`, `responses/compact`, `CompactedWindow`, `compact_if_supported` returns zero hits. The doc therefore describes a compaction path the code no longer takes first.
- Write: Rewrite `## Compaction and Summaries` to describe the two-tier path. Tier 1 (native): when the window is over threshold the turn calls `ProviderRouter::compact_if_supported`, which resolves the model's provider and delegates to `Provider::compact_responses` (router.rs:85, lib.rs:364). Only `openai-response`, `openai-codex` and `grok-build` routes advertise it (http.rs:329); the endpoint is derived by appending `/compact` to an endpoint already ending in `/responses`, otherwise `/responses/compact` to the trimmed endpoint (http.rs:336). The call POSTs `{model, input}` (with an optional system item prepended) and requires an `output` array in the reply, else it fails with `Decode("responses compact reply missing output array")` (http.rs:528). A success yields a `CompactedWindow` (lib.rs:335) whose item array is persisted as a system message prefixed with `<<<RESPONSES_COMPACT_ITEMS>>>` and re-injected verbatim into the next request's `input` (responses.rs:11, :110). Tier 2 (fallback): on `Ok(None)` (no compact endpoint) or ANY error, the turn falls back to the existing local `ModelSummarizer`, which writes a system message carrying the `HYA_COMPACTED_CONTEXT` marker (responses.rs:14) treated as a plain local summary. Keep the existing `/compact` CLI and Compat summarize notes.

**9. ModelGoalEvaluator provider request parameters** — `thin` · severity low

- Source: `crates/hya-core/src/completion.rs:242`
- Evidence: docs/architecture/runtime.md:221 says "`ModelGoalEvaluator` calls a provider with no tools and requests strict JSON" but does not give the sampling parameters, so the cost/behavior of a goal run is not predictable from the docs.
- Write: In `## Goal Mode`, extend the ModelGoalEvaluator paragraph with the concrete request shape (completion.rs:242): a tool-free `CompletionRequest` at `temperature: 0.0` and `max_output_tokens: 256`, asking the model for the JSON verdict. Note that this is a separate provider call per gate evaluation and therefore bills against the same route as the turn itself.

**10. Session title generation provider call** — `undocumented` · severity low

- Source: `crates/hya-core/src/engine/session_title.rs:57`
- Evidence: Grep for `title generation`, `session_title` across all in-scope docs: no hits. docs/architecture/event-model.md only lists session title as an event group; nothing says a separate provider completion is issued.
- Write: Add a short `## Session Titles` subsection (next to Compaction and Summaries): hya issues a separate provider completion to generate a session title, honoring an explicit model and reasoning effort taken from the fixed system agent definition rather than the session's active model (engine/session_title.rs:57). Say that this is an extra billed provider call per session and which route it lands on.

**11. SessionEngine::publish_live and SessionEngine::publish_envelope** — `contradicted` · severity high

- Source: `crates/hya-core/src/engine.rs:557`
- Evidence: docs/architecture/runtime.md:22-23 states 'All runtime events pass through SessionEngine::emit, which appends to the store and publishes the same envelope to the bus', and docs/project-structure.md:181-182 states 'It appends every event through the store and immediately publishes the same envelope on the EventBus'. Both are false: publish_live (engine.rs:557) publishes an Envelope at seq 0 WITHOUT any store write, and it is the path used for every streaming text/reasoning delta. publish_envelope (engine.rs:565) has no rustdoc.
- Write: Replace the 'all runtime events pass through emit' claim with the three real seams: (1) emit — append to SQLite, take the returned seq, THEN publish, so a live observer never sees a non-durable event; (2) publish_live — publish at seq 0 with no store write, used for high-frequency text/reasoning deltas that are re-emitted durably as a TextStart/TextReplace/TextEnd triple at round end; (3) emit_for_actor — routes through commit_resident_mutation when an ActorClaim is present, else plain emit. Also document publish_envelope as the single publish seam: it dispatches to global hooks, then activation (sidecar) hooks, then the bus. Apply the same correction to docs/project-structure.md:181-182.

**12. SessionEngine::emit_for_actor (the fencing seam)** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine.rs:513`
- Evidence: No rustdoc (unlike its callee commit_resident_mutation at engine.rs:541, which is documented). grep for 'emit_for_actor' in all in-scope docs returns zero hits. docs/architecture/runtime.md:170-174 describes claim-aware commits abstractly but never names the function that makes resident writes fenced.
- Write: In the 'Resident recovery and actor fencing' section, name emit_for_actor as the single seam: it takes Option<&ActorClaim>; with Some(claim) it routes to commit_resident_mutation (fenced, publish-after-commit), with None it falls through to plain emit. Every resident-originated event goes through it, which is why transient work performs no actor-claim lookup.

**13. SessionEngine::create_with_id idempotence** — `thin` · severity medium

- Source: `crates/hya-core/src/engine.rs:573`
- Evidence: docs/architecture/runtime.md:27-32 documents `create` and the SessionCreated payload, but says nothing about create_with_id or create_for_actor. No rustdoc on any of the three.
- Write: Under Session Creation, add: create_with_id(id, spec) is IDEMPOTENT — if the supplied id already has events in the log it returns immediately without re-emitting SessionCreated. This is what makes resume/fork/recovery paths safe to call unconditionally. create_for_actor is the ActorClaim-fenced variant.

**14. AgentSpec** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine.rs:95`
- Evidence: No rustdoc. docs/architecture/server-client.md:12 mentions 'process-level AgentSpec' in the AppState list without defining it; docs/architecture/runtime.md never defines it despite the turn loop being built on it.
- Write: Define AgentSpec in the SessionEngine section as the resolved agent for one turn: name, model, system_prompt, workdir, reasoning effort. Note it is what a TurnBinding's resolve_agent produces and what the server holds as its process-level default.

**15. RuntimeCatalogRefresh trait** — `undocumented` · severity low

- Source: `crates/hya-core/src/engine.rs:149`
- Evidence: Zero hits in all in-scope docs; no rustdoc. docs/architecture/runtime.md:103-109 describes bundle-catalog refresh behaviour ('hya-app reads the bundle registry generation before binding each new root turn') but never names the trait that is the hook point.
- Write: Name RuntimeCatalogRefresh as the trait hook bind_root_runtime calls before a ROOT turn binds its snapshot: it lets hya-app refresh the bundle catalog if the registry generation changed. Emphasise it fires only on root binds — child/bound turns reuse the parent's pinned binding and never consult the registry.

**16. TurnActivation (Root | Bound | Resolved) and the six turn entry points** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:94`
- Evidence: Zero hits for 'TurnActivation' in in-scope docs. crates/hya-core/src/engine/turn.rs has only 7 doc-comment lines in 902 lines, and only the `..._and_guidance` variant is documented (turn.rs:191-196). docs/architecture/runtime.md:53-58 describes run_turn as if it were the only entry point.
- Write: Add a subsection under Assistant Turn Loop: TurnActivation has three modes — Root re-binds the runtime (and may start a sidecar), Bound reuses a TurnBinding captured by the parent turn, Resolved additionally reuses pre-resolved agents, resource policy and sidecar tools. Then list the six entry points and what each adds: run_turn (Root), run_turn_with_external_dirs, run_turn_with_external_dirs_and_guidance, run_bound_turn / run_bound_turn_for_actor, run_resolved_turn_with_sidecar_tools / ..._for_actor. State that the _for_actor variants carry an ActorClaim and are the resident path.

**17. Turn round loop — the real per-round sequence** — `stale` · severity high

- Source: `crates/hya-core/src/engine/turn.rs:544`
- Evidence: docs/architecture/runtime.md:60-71 gives an 8-step round list (read projection, build request, stream, append events, collect ToolCallRequested, execute tools, formatter/LSP, append result). The actual loop at turn.rs:544 additionally does, in order: validate the actor claim, check activation-hook health, check the cancel token, maybe compact, run the chat_params hook, acquire a stream permit, emit StepStarted, run collect_stream_round, emit StepFinished, DROP the permit, then run tools. The doc's ordering hides the permit lifetime, which is the deadlock-critical part.
- Write: Rewrite the round list to match turn.rs:544 exactly and in order: validate actor claim → check activation-hook health → check cancel token → read projection → maybe compact → chat_params hook → acquire stream permit → StepStarted → collect_stream_round → StepFinished → DROP stream permit → run tools → repeat until a round yields no tool calls.

**18. Stream permit is dropped before tool execution (nested fan-out deadlock invariant)** — `undocumented` · severity high

- Source: `crates/hya-core/src/engine/turn.rs:689`
- Evidence: Not stated in docs/architecture/runtime.md, docs/adr/0002, or anywhere else in scope. crates/hya-core/src/orchestrator.rs:22-37 documents the permit COUNTS but not the permit LIFETIME. This is the invariant that prevents deadlock when a member blocks in the `task` tool awaiting children.
- Write: Add a call-out box to the Assistant Turn Loop section: the governor stream permit is held ONLY around provider streaming and is dropped BEFORE tool execution. This is load-bearing — a member blocked inside the `task` tool waiting on its children holds no permit, so nested fan-out cannot deadlock the semaphore. Warn that moving tool dispatch inside the permit scope reintroduces the deadlock.

**19. Reserved vs general stream permit class chosen by session depth** — `thin` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:654`
- Evidence: crates/hya-core/src/orchestrator.rs:26-29 documents the 100 general / 28 reserved split, but nothing (rustdoc or prose) says WHICH turns take which class. docs/configuration.md has no subagent-limit section at all (grep for 'max_concurrency', 'max_depth', 'per_run_budget' in docs/configuration.md returns zero hits).
- Write: State the selection rule: depth-0 (root/interactive) turns take a RESERVED permit; depth>0 subagent turns take a GENERAL permit. General work can never borrow from the reserved pool, so root progress never queues behind background subagent work. Give the numbers: DEFAULT_GENERAL_STREAM_PERMITS=100 (max_concurrency normalized to 1..=100) plus a fixed RESERVED_STREAM_PERMITS=28, for a 128-permit live stream budget.

**20. Forced MessageFinished on turn error or sidecar loss** — `thin` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:459`
- Evidence: docs/architecture/runtime.md:185-188 covers only the pre-round cancellation case ('If cancellation is observed before a provider round starts, the engine emits MessageFinished with FinishReason::Cancelled'). It does not cover the non-cancel error path or the sidecar loss-token path, both of which also force a MessageFinished when the assistant message is still open.
- Write: Extend the Cancellation section (rename it 'Turn termination guarantees'): whenever the turn ends with the assistant message still open, the engine force-emits MessageFinished — Error for a non-cancel failure, Cancelled for cancellation or for the sidecar loss token firing. State the contract this buys clients: a UI that has seen MessageStarted is guaranteed to eventually see MessageFinished, so it never spins forever.

**21. Root-turn admission cleanup (finalize_root_spawn_admissions)** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:506`
- Evidence: Zero hits in in-scope docs. crates/hya-core/src/engine/admission.rs has 736 lines and ZERO doc comments. docs/architecture/runtime.md never mentions per-run subagent budget release.
- Write: Document that a completed depth-0 turn calls finalize_root_spawn_admissions(root), which cancels every live operation for that root, cancel-finalizes every nonterminal admission journal row, and releases the root's per-run subagent budget entry. Explain why: without it a long-lived root session leaks budget and never recovers spawn capacity.

**22. Compaction fallback chain and the HYA_COMPACTED_CONTEXT marker** — `thin` · severity high

- Source: `crates/hya-core/src/engine/turn.rs:569`
- Evidence: docs/architecture/runtime.md:193-201 says ModelSummarizer 'asks the configured provider for a summary when token thresholds are exceeded' and compact_context 'records a hya-native system summary and prunes older provider context'. grep for 'HYA_COMPACTED_CONTEXT' across all in-scope docs returns ZERO hits, and the provider-native /responses/compact step is not mentioned at all.
- Write: Document the real three-step chain when needs_compaction fires: resolve the fixed `compaction` agent (FAILS CLOSED if the agent is missing) → try the provider's native /responses/compact → on None or Err fall back to the local ModelSummarizer. Then document the marker: the compacted window is persisted as a system message prefixed with the literal HYA_COMPACTED_CONTEXT, and every subsequent round drops all history before that marker when building the provider request. Also document CompactionConfig{token_threshold, keep_recent} and SummarizeOptions{system, model, reasoning}.

**23. Tool dispatch pipeline (hooks, permission, claim re-validation, output cap)** — `thin` · severity high

- Source: `crates/hya-core/src/engine/turn.rs:707`
- Evidence: docs/architecture/runtime.md:67-69 compresses the whole pipeline into 'Resolves and executes requested tools through the same binding, with permission checks and plugin/MCP bridges'. The hook veto path, the claim re-validation at the dispatch boundary, and cap_tool_output are not described. docs/architecture/tools-and-permissions.md covers rules but not this ordering.
- Write: Document the per-tool-call pipeline in order: tool_execute_before hooks (global first, then activation/sidecar) — a Veto produces a ToolError with message 'blocked by plugin' and the tool never runs → resolve_tool against the bound runtime → permission authorize → RE-VALIDATE the actor claim at the dispatch boundary (a stale resident cannot dispatch a tool even if it passed validation at round start) → execute → tool_execute_after hooks (which may rewrite the outcome) → cap_tool_output → emit ToolResult or ToolError.

**24. TextPartAccumulator and the text_complete plugin hook** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/text_complete.rs:9`
- Evidence: crates/hya-core/src/engine/text_complete.rs has 97 lines and zero doc comments. grep for 'text_complete' across all in-scope docs returns zero hits; docs/architecture/runtime.md:205-209 lists 'text completion' as a hookable surface with no detail.
- Write: Under Hooks, document text_complete: the engine accumulates streaming text per PartId in a TextPartAccumulator; on TextEnd the text_complete hook is offered the accumulated text and may return replacement text. A rewrite publishes a live TextReplace AND changes what is persisted in the durable triple — so this hook can alter the stored transcript, not just the display.

**25. SessionEngine::run_shell synthetic assistant turn (7-event sequence)** — `thin` · severity medium

- Source: `crates/hya-core/src/engine/shell.rs:30`
- Evidence: docs/compat-parity.md:82 says run_shell 'execute[s] the real shell tool and record[s] a synthetic user message plus assistant tool result'; docs/architecture/server-client.md:68 says 'shell runs the shell tool directly and records a synthetic assistant tool-result message'. Neither gives the event sequence. crates/hya-core/src/engine/shell.rs has 269 lines and zero doc comments.
- Write: Add a 'Shell turns' subsection: run_shell admits a shell user message, binds the root runtime, then emits a full synthetic assistant message around one `shell` tool call — MessageStarted → TurnBindingRecorded → ToolInputStart → ToolCallRequested → ToolResult (or ToolError) → MessageFinished. Note there is no provider call, so no StepStarted/StepFinished and no stream permit is taken.

**26. SessionEngine::record_user_prompt_context** — `thin` · severity low

- Source: `crates/hya-core/src/engine/admission.rs:493`
- Evidence: docs/architecture/runtime.md:49-50 says only 'Compat-compatible v2 prompt admission can attach file and agent metadata that is replayed through the projection and provider request builder'. The function name, its short-circuit rule, and the event it emits are absent. admission.rs has zero doc comments.
- Write: Name record_user_prompt_context in the Prompt Admission section: it emits UserPromptContextRecorded{files, agents} but short-circuits to Ok(()) and emits NOTHING when both vectors are empty — so a prompt with no @mentions leaves no context event in the log at all, and consumers must not expect one per user message.

**27. SessionEngine session-state mutators (14 single-event emitters)** — `thin` · severity medium

- Source: `crates/hya-core/src/engine/session_state.rs:7`
- Evidence: docs/project-structure.md:169 gives one table cell: 'Agent/model/session metadata updates'. crates/hya-core/src/engine/session_state.rs has 170 lines and zero doc comments. No doc lists the methods or maps them to events.
- Write: Add a table mapping each mutator to the single event it emits: switch_agent→AgentSwitched, switch_model→ModelSwitched, set_title→SessionTitled, set_workdir→SessionMoved, set_metadata→SessionMetadataSet, set_permission→SessionPermissionSet, set_archived→SessionArchived, set_share→SessionShareSet, clear_share→SessionShareCleared, delete_message→MessageDeleted, delete_part→PartDeleted, replace_text_part→TextReplace, replace_reasoning_part→ReasoningReplace, update_tool_part→ToolPartUpdated. State that each is a thin single-event emit with no side effects beyond the append+publish.

**28. SessionEngine::copy_messages_to_session (fork mechanics)** — `thin` · severity medium

- Source: `crates/hya-core/src/engine/fork.rs:9`
- Evidence: docs/compat-parity.md:106 mentions fork routes and 'metadata/message-copy fork'; docs/architecture/server-client.md:87 lists /fork as a route. Neither explains the replay mechanics. crates/hya-core/src/engine/fork.rs has 179 lines and zero doc comments.
- Write: Add a 'Forking a session' subsection: copy_messages_to_session replays the source SessionProjection into the target session as FRESH events with newly minted MessageId/PartId per copy (ids are never reused), stopping before an optional `before` message id. Tool parts cannot be replayed as their original streaming events, so they are recreated as ToolInputStart followed by ToolPartUpdated carrying the final ToolPartState. Consequence: a forked session's event log is not byte-identical to the source's and its seq numbering is independent.

**29. SessionEngine::auto_title_session** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/session_title.rs:15`
- Evidence: Zero hits for 'auto_title' across all in-scope docs. crates/hya-core/src/engine/session_title.rs has 115 lines and zero doc comments. docs/architecture/event-model.md mentions SessionTitled only via the group name 'title'.
- Write: Document auto_title_session and its three guards: it titles ROOT sessions only (children are skipped), it skips any session that already has a non-default title, and it requires exactly one user message in the projection. It resolves the fixed `title` system agent and calls it at temperature 0 with a 128-token cap, then emits SessionTitled. Cross-reference FixedSystemAgent (compaction | title | summary) as the closed set of Harness system operations.

**30. SessionEngine::cleanup_empty_unnamed_session** — `undocumented` · severity low

- Source: `crates/hya-core/src/engine/session_cleanup.rs:8`
- Evidence: Zero hits in all in-scope docs; zero doc comments in the file. Not mentioned in docs/architecture/storage.md alongside delete_session either.
- Write: Document cleanup_empty_unnamed_session: it deletes a session if and only if title::is_empty_unnamed_session holds (no user content and no assigned title). Note it calls through to SessionStore::delete_session, which removes token_ledger rows then event_log rows in one transaction.

**31. CoreError variants** — `undocumented` · severity medium

- Source: `crates/hya-core/src/error.rs:6`
- Evidence: crates/hya-core/src/error.rs has 35 lines and zero doc comments. docs/project-structure.md:179 says only 'Runtime error wrapper'. grep for 'CoreError' across in-scope docs returns zero hits.
- Write: Add an Errors section enumerating CoreError: Bundle, Provider, Tool, Store, RuntimeRefresh, Cancelled, AgentDefinitionMissing{agent_id}, Invalid(String). Note that hya-server maps every CoreError to 500 except where a compat route translates it, so Cancelled and AgentDefinitionMissing are NOT distinguishable over HTTP today.

**32. HookDispatcher — the five hook points with typed in/out pairs plus dispatch_event** — `contradicted` · severity high

- Source: `crates/hya-core/src/hooks.rs:67`
- Evidence: docs/architecture/runtime.md:205-209 says 'Hookable surfaces include events, command/user message admission, chat params/messages, text completion, permission asks, and tool before/after hooks'. The trait at crates/hya-core/src/hooks.rs:13-27 has exactly: dispatch_event, is_healthy, command_execute_before, text_complete, message_user_before, chat_params, tool_execute_before, tool_execute_after. There is NO permission-ask hook on HookDispatcher — permission callbacks live on PermissionPlane / the hya-plugin bridge. The input/output structs (hooks.rs:67-138) carry zero doc comments.
- Write: Correct the hook list to the six real trait methods and remove 'permission asks' (or explicitly say permission callbacks are a PermissionPlane/hya-plugin bridge concern, not a HookDispatcher method). Then table each hook with its typed in/out pair: command_execute_before(CommandExecuteBeforeInput{session,command,arguments,text}) → Continue{text}; text_complete(TextCompleteInput{session,message,part,text}) → Continue{text}; message_user_before(MessageUserBeforeInput{session,text}) → Continue{text}; chat_params(ChatParamsInput{session,message,request}) → Continue{request}; tool_execute_before → Continue | Veto{reason}; tool_execute_after → ToolOutcomeNative Ok{output,time_ms} | Err{message}. Note dispatch_event fires for EVERY published Envelope including seq-0 live-only ones.

**33. Activation-hook health gate aborts the turn** — `undocumented` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:546`
- Evidence: No in-scope doc mentions is_healthy or the health gate. docs/architecture/runtime.md:141-154 describes sidecar loss behaviour in terms of 'running loss aborts and fences the current item' but never says the turn loop polls dispatcher health at the top of each round and after each hook batch, nor that the failure is surfaced as CoreError::Cancelled.
- Write: In the sidecar section, document the health gate: the turn loop checks the activation hook dispatcher's is_healthy() at the top of every round and again after each before/after hook batch. An unhealthy dispatcher aborts the turn with CoreError::Cancelled (not an error), which is why a lost sidecar shows up to clients as MessageFinished{Cancelled} rather than {Error}.

**34. Sidecar cleanup discipline — shutdown() vs terminate()** — `thin` · severity medium

- Source: `crates/hya-core/src/engine/turn.rs:497`
- Evidence: docs/architecture/runtime.md:117-154 documents the sidecar contract at length ('A transient activation owns one child through its whole activation and then shuts down/reaps it') but never states the branch condition. crates/hya-core/src/sidecar.rs:12 has 8 doc lines covering the trait shape, not the cleanup rule.
- Write: State the exact rule: a turn ending with FinishReason Stop or Length calls SidecarHandle::shutdown() (graceful); EVERY other outcome — error, cancellation, or a round that ended in tool_calls without completing — calls terminate(). Sidecar authors must therefore assume terminate() is the common path on abnormal exit and must not rely on shutdown-time flushing for durability.

**STALE 1.** The document claims: Compaction is described as `ModelSummarizer` asking the configured provider for a summary plus `compact_context` pruning older context — presented as the whole mechanism.

- Reality: crates/hya-core/src/engine/turn.rs:583 now PREFERS the provider's native `POST /responses/compact` when the route supports it (openai-response, openai-codex, grok-build), persists the returned window as a `<<<RESPONSES_COMPACT_ITEMS>>>` system message, and only falls back to the local summarizer on `Ok(None)` or an error. The doc describes the fallback path as if it were the only path.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: "All runtime events pass through `SessionEngine::emit`, which appends to the store and publishes the same envelope to the bus."

- Reality: False. SessionEngine::publish_live (crates/hya-core/src/engine.rs:557) publishes an Envelope at seq 0 with NO store write, and it is the path used for every streaming text/reasoning delta. Resident writes go through emit_for_actor → commit_resident_mutation (engine.rs:513/541), which commits inside a fenced SQLite transaction and publishes only after commit.
- Action: correct or delete. Do not merely supplement.

**STALE 3.** The document claims: "Hookable surfaces include events, command/user message admission, chat params/messages, text completion, permission asks, and tool before/after hooks."

- Reality: The HookDispatcher trait (crates/hya-core/src/hooks.rs:13-27) has exactly dispatch_event, is_healthy, command_execute_before, text_complete, message_user_before, chat_params, tool_execute_before, tool_execute_after. There is NO permission-ask hook — permission callbacks are owned by PermissionPlane and the hya-plugin permission bridge, not by HookDispatcher.
- Action: correct or delete. Do not merely supplement.

**STALE 4.** The document claims: Lists `team.rs` as one of the files team-related code is split across, and states "`TeamControlPlane` models lifecycle transitions, mailbox messages, and task board state."

- Reality: crates/hya-core/src/team.rs does not exist and grep for TeamControlPlane across crates/hya-core/src returns nothing. docs/adr/0001-event-sourced-mailbox-and-channels.md:11-12 records that the dead TeamControlPlane was deleted and replaced by event-sourced MailSent/ChannelJoined/ChannelLeft folded by hya-proto::Projection.
- Action: correct or delete. Do not merely supplement.

**STALE 5.** The document claims: The assistant turn loop is presented as an 8-step round: read projection, build request, stream, append text/reasoning/tool-input events, collect ToolCallRequested, resolve and execute tools, formatter/LSP post-edit, append ToolResult/ToolError.

- Reality: The real loop (crates/hya-core/src/engine/turn.rs:544) also validates the actor claim, checks activation-hook health, checks the cancel token, may compact, runs the chat_params hook, acquires a governor stream permit, emits StepStarted/StepFinished, and DROPS the permit before tool execution. The permit lifetime in particular is the invariant that prevents deadlock in nested fan-out and is absent from the documented sequence.
- Action: correct or delete. Do not merely supplement.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 34 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
