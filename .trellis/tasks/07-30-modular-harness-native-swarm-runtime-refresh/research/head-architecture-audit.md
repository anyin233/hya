# HEAD architecture audit

## Audit anchor and evidence rules

- **Authoritative source snapshot:** `267bfc3c6c66e46fe8514e2e70657489f853b7f0`
  on `main`, equal to the `origin/main` tracking ref in the
  `fuji1 remote worker` checkout (`0 0` ahead/behind).
- **Post-audit source accounting:** on 2026-07-31 `origin/main` advanced to
  `156d0ad3c50aea67dfac0054485eb6991e77308b`; the intervening commit changes
  only the README icon reference. The isolated feature branch is rebased to
  that commit, and no source finding below changed.
- **Workspace package version:** `0.34.2` from `[workspace.package].version`.
- **Working-tree rule:** the pre-existing dirty files listed in
  `research/fuji1-sync-preflight.md` are user work and are outside this task.
  Dirty `hya-sdk` and startup-benchmark files were excluded from HEAD claims.
- **Implementation** means behavior directly present in the anchored source.
- **ADR/document** means an accepted or descriptive statement that may not match
  implementation.
- **Inference** means a risk or absence inferred from the inspected inventory;
  it requires a test, benchmark, or fault injection before being promoted to a
  measured fact.
- **Target** means the proposed contract for future work, not current behavior.

## 1. Process and crate boundaries

| Classification | Finding | Source and symbol |
| --- | --- | --- |
| Implementation | The public `hya` Unix entrypoint replaces itself with the adjacent `hya-ts` executable and has no backend/TUI fallback. | `crates/hya/src/main.rs:4-17`, `main` |
| Implementation | `hya-ts` resolves the prepared Bun runtime and backend, starts or attaches to `hya-backend`, gives the TUI its own process group, hands off the terminal, and cleans up an owned backend. | `crates/hya-ts/src/main.rs:27-131`, `main` |
| Implementation | `hya-backend` owns interactive/exec/goal/serve routing but delegates interactive rendering to the TypeScript frontend. | `crates/hya-backend/src/main.rs:287-358`, `main` |
| Implementation | `hya-app` is the composition root for `ToolRegistry`, `PluginHost`, MCP, permissions, event bus, governor, `SessionEngine`, resident supervision, the spawn supervisor, and mailbox service. | `crates/hya-app/src/runtime.rs:484-595`, `build_session_engine` |
| Implementation | Native and Compat plugins are ordinary child processes connected through newline-delimited JSON-RPC stdio with `kill_on_drop`; this is crash/ABI isolation, not a security sandbox. | `crates/hya-plugin/src/client.rs:99-127`, `PluginClient::spawn`; `docs/adr/0009-rust-plugin-binaries-over-in-process-libraries.md`, accepted boundary |

## 2. Event store, projection, bus, and turn lifecycle

| Classification | Finding | Source and symbol |
| --- | --- | --- |
| Implementation | SQLite is opened in WAL mode with `synchronous=NORMAL`, a five-second busy timeout, foreign keys, and a pool maximum of eight connections. | `crates/hya-store/src/lib.rs:47-59`, `SessionStore::connect` |
| Implementation | Each durable event is one `INSERT ... RETURNING seq`; `seq` is a global autoincrement key. | `crates/hya-store/src/lib.rs:79-96`, `SessionStore::append_event`; `crates/hya-store/migrations/0001_init.sql`, `event_log` table |
| Implementation | `replay` selects and deserializes every event for a session in sequence order; `read_projection` folds that complete vector with the shared reducer. There is no persisted projection snapshot in this path. | `crates/hya-store/src/lib.rs:98-118`, `SessionStore::replay`; `crates/hya-store/src/lib.rs:135-137`, `SessionStore::read_projection`; `crates/hya-proto/src/projection.rs:168-202`, `Projection::from_events` / `Projection::apply` |
| Implementation | `SessionEngine::emit` waits for the store append before publishing the durable envelope. Streaming-only deltas use sequence zero and are not durable projection state. | `crates/hya-core/src/engine.rs:272-288`, `SessionEngine::emit` / `SessionEngine::publish_live` |
| Implementation | The event bus is a bounded Tokio broadcast channel (default capacity 8192); lag is an explicit receiver outcome and send failures are not surfaced to the emitter. | `crates/hya-core/src/bus.rs:4-45`, `EventBus::new` / `EventBus::publish` |
| Implementation | Every model/tool round reads a fresh full projection, derives an effective agent, builds messages and current tool schemas, streams a model response, then resolves and authorizes tool calls. | `crates/hya-core/src/engine/turn.rs:86-292`, `SessionEngine::run_turn_rounds`; `crates/hya-core/src/engine/turn/messages.rs:38-54`, `request_from_messages` |
| Implementation | Durable streamed text is coalesced into start/replace/end events rather than persisting every token delta, while other emitted lifecycle/tool events remain individual appends. | `crates/hya-core/src/engine/turn/stream_round.rs:23-128`, `collect_stream_round` |
| Inference | Full replay work grows with session history and SQLite permits only one writer at a time, so replay CPU/allocations and write serialization are credible scale risks. They are **not yet demonstrated bottlenecks** at 100 or 256 agents. | Mechanisms above; measurement is required by the capacity phase |

## 3. Subagent admission, residents, mailbox, and quiescence

| Classification | Finding | Source and symbol |
| --- | --- | --- |
| Implementation | Defaults are depth 5, 128 concurrent subagent provider streams, and 1024 per-run spawn, per-team turn, and per-team message budgets. | `crates/hya-core/src/orchestrator.rs:33-51`, `SubagentLimits::default` |
| Implementation | The 128 semaphore is acquired only around depth-greater-than-zero provider streaming and is dropped before tool execution. It does not cap resident count, queued spawn requests, tool work, event writes, or root-agent streams. | `crates/hya-core/src/orchestrator.rs:76-132`, `SubagentGovernor::acquire_stream` / `reserve`; `crates/hya-core/src/engine/turn.rs:185-214`, `SessionEngine::run_turn_rounds` |
| Implementation | `SpawnerPlane` uses `mpsc::unbounded_channel`; the supervisor starts a Tokio task for every received request. | `crates/hya-tool/src/spawn.rs:71-123`, `SpawnerPlane::new` / `SpawnerPlane::spawn`; `crates/hya-app/src/runtime.rs:262-278`, `spawn_team_supervisor` |
| Implementation | For a background transient without an existing task/session ID, the supervisor calls `SessionEngine::create` while constructing `MemberSpec`, then calls `run_team`; governor reservation therefore occurs after child-session allocation on this path. | `crates/hya-app/src/runtime.rs:356-403,425-430`, `spawn_team_supervisor`; `crates/hya-core/src/subagent.rs`, `run_team` |
| Implementation | Transient work reaches `run_team` and its governor reservation/depth path. Resident work calls `ResidentSupervisor::spawn_resident` directly, so it bypasses spawn reservation, max-depth rejection, and per-run spawn charging. | `crates/hya-app/src/runtime.rs:288-354,425-443`, `spawn_team_supervisor`; `crates/hya-core/src/resident.rs:641-675`, `ResidentSupervisor::spawn_resident`; `crates/hya-core/src/subagent.rs`, `run_team` |
| Implementation | A resident turn still uses the depth-greater-than-zero provider-stream semaphore and is charged against per-team turn/message budgets; “resident bypasses the governor” is therefore only partially true. | `crates/hya-core/src/resident.rs:140-190`, `TeamActor::next_action`; `crates/hya-core/src/resident.rs:385-409`, `TeamActor::on_mail`; `crates/hya-core/src/resident.rs:268-341`, `TeamActor::run_one_turn` |
| Implementation | Mail, channel membership, registration, and roster state are events in the team-root log and fold into the shared projection. | `crates/hya-proto/src/event.rs:242-281`, mailbox/team event variants; `crates/hya-proto/src/projection.rs:101-166`, team projection reducer; `docs/adr/0001-event-sourced-mailbox-and-channels.md` |
| Implementation | Quiescence is an in-memory locked decision: when busy reaches zero and `work_seq` advanced, main is woken once; `last_synth_work_seq` prevents a no-new-work synthesis loop. | `crates/hya-core/src/resident.rs:193-213`, `TeamActor::maybe_fire_quiescence` |
| Implementation | Broadcast-lag recovery replays a root projection and re-arms only already registered in-memory slots whose inbox length exceeds their cursor. | `crates/hya-core/src/resident.rs:411-435`, `TeamActor::recover`; `crates/hya-core/src/resident.rs:536-565`, `ResidentSupervisor::run_bus` |
| Implementation gap | Runtime startup creates `ResidentSupervisor` with an empty team map. Production registration reaches `register_existing_resident` only from a new resident spawn; no startup caller rehydrates prior resident slots, cursors, tasks, or quiescence counters from the event log. Durable mail survives restart, but autonomous resident execution does not resume by itself. | `crates/hya-core/src/resident.rs:501-523`, `ResidentSupervisor::start`; `crates/hya-core/src/resident.rs:567-587`, `ResidentSupervisor::team_for`; `crates/hya-core/src/resident.rs:678-731`, `ResidentSupervisor::register_existing_resident`; `crates/hya-app/src/runtime.rs:580-590`, `build_session_engine` |
| ADR/document | ADR 0002 describes zero-cost parked resident actors and natural quiescence, but does not define process-restart rehydration. | `docs/adr/0002-resident-actor-model-and-autonomous-main-agent.md` |

## 4. Tools, MCP, plugins, agents, and skills

| Classification | Finding | Source and symbol |
| --- | --- | --- |
| Implementation | `ToolRegistry` is an `RwLock<HashMap>` plus aliases. Registration/removal mutates one tool name at a time; there is no generation, stable declaration ID, immutable snapshot handle, or atomic batch swap. | `crates/hya-tool/src/tool.rs:107-301`, `ToolRegistry` / `register` / `remove` / `schemas` / `resolve` |
| Implementation | A completion request reads current schemas each round, while execution resolves the then-current registry after the provider returns. A binding is not pinned across the Turn or even across request/execute. | `crates/hya-core/src/engine/turn.rs:115-166,231-292`, `SessionEngine::run_turn_rounds`; `crates/hya-core/src/engine/turn/messages.rs:38-54`, `request_from_messages` |
| Implementation | Static or deferred MCP connections eventually register tools into the engine registry one by one; deferred registration can occur while the engine is already serving Turns. No corresponding atomic replacement/removal is wired here. | `crates/hya-app/src/runtime.rs:468-481`, `register_mcp_tools`; `crates/hya-app/src/runtime.rs:512-526`, `build_session_engine` |
| Implementation | `McpManager` tracks connection statuses and connected server clients, but has no first-class desired/observed/effective generation or activation transaction. | `crates/hya-mcp/src/manager.rs`, `McpStatus` / `McpManager::pending` / `connect_all_into` / `tools` |
| Implementation gap | Compat HTTP MCP add/connect/disconnect uses a separate `McpHttpState`; it updates configs/status/resources but never receives the engine `ToolRegistry`. Dynamic Compat MCP tools therefore do not become callable engine tools. | `crates/hya-server/src/state.rs:89-125`, `ServerState`; `crates/hya-server/src/compat/mcp_state.rs:8-100`, `McpHttpState::add_config` / `connect` / `disconnect`; `crates/hya-server/src/compat/mcp.rs:27-40`, MCP handlers |
| Implementation | Plugin declarations are captured during initial connection and registered once. On a child restart, `ensure_client` calls `initialize` but discards the returned declarations and protocol result, retaining the original hook/tool/adapter sets. | `crates/hya-plugin/src/host/connection.rs:15-64`, initial connection; `crates/hya-plugin/src/host.rs:125-147`, `PluginConn::ensure_client`; `crates/hya-plugin-compat/adapter/src/initialize.ts:45-83`, `initialize` |
| Implementation | `AgentSpec` contains only name, model, system prompt, workdir, and reasoning. It has no per-agent tool view, capability set, bundle identity, binding set, or extension generation. | `crates/hya-core/src/engine.rs:59-66`, `AgentSpec` |
| Implementation | Disk agent entries can accumulate `permissions` and arbitrary `options`, but the runtime application path copies prompt and resolved reasoning only. `list_agents` also exposes only name/description/category/mode. | `crates/hya-server/src/compat/agent_catalog.rs:7-32,125-153`, `AgentEntry` / `agent_definitions` / `merged_entries`; `crates/hya-server/src/compat/reference.rs:11-28`, `apply_agent_entry`; `crates/hya-tool/src/agents.rs:20-32,91-130`, `AgentDef` / `ListAgentsTool::execute` |
| Implementation | Ancestor `AGENTS.md` files are read once while the root `AgentSpec.system_prompt` is constructed. They are not rediscovered by the turn loop. | `crates/hya-app/src/runtime.rs:38-61`, `discover_context_files`; `crates/hya-app/src/runtime.rs:94-111`, `agent_with_model` |
| Implementation | Agent definitions are rescanned at list/spawn resolution, while skills are synchronously rescanned each round and again when the `skill` tool executes. | `crates/hya-app/src/runtime.rs:552-566`, `AgentCatalogPlane` closure; `crates/hya-server/src/compat/subagent_resolve.rs:73-165`, `resolve_subagent`; `crates/hya-tool/src/skill_catalog.rs:45-114`, `skill_dirs_for_workdir` / `discover_skills`; `crates/hya-core/src/engine.rs:329-344`, `effective_agent_for_projection`; `crates/hya-tool/src/skill.rs:71-133`, `SkillTool::execute` |
| Implementation gap | Skill frontmatter parses `allowed-tools` and `model`, but the effective-agent path uses only the generated name/description section and `SkillTool` returns content; those policy fields are not applied to tool or model binding. | `crates/hya-tool/src/skill_catalog.rs:6-24,132-149`, `ParsedSkill` / `SkillCatalogEntry` / `parse_skill`; `crates/hya-core/src/engine.rs:329-344`, `effective_agent_for_projection`; `crates/hya-tool/src/skill.rs:102-133`, `SkillTool::execute` |
| ADR/document divergence | ADR 0007 and ADR 0008 require an in-flight **Turn** to keep its skill/plugin tool snapshot and expose changes only to the next admitted Turn. HEAD re-reads per round and resolves the current registry at execution, so these accepted ADRs are not implemented as written. | `docs/adr/0007-hot-skill-reload-visibility.md`; `docs/adr/0008-hot-plugin-reload-visibility.md`; turn/registry sources above |

## 5. Permission, capability, operation identity, and effects

| Classification | Finding | Source and symbol |
| --- | --- | --- |
| Implementation | Invocation policy is compiled and captured when the runtime constructs `PermissionPlane`; the base snapshot is immutable and remembered grants live in a separate mutable rules set. | `crates/hya-app/src/runtime.rs:528-540`, `build_session_engine`; `crates/hya-tool/src/permission.rs:182-277`, `InvocationPolicy`; `crates/hya-tool/src/permission.rs:434-458`, `PermissionPlane::new_with_policy` / `new_inner` |
| Implementation | A tool name is resolved, authorization is awaited, `ToolCtx` is built, and the tool executes. There is no security-epoch revalidation immediately before or while committing an effect. | `crates/hya-core/src/engine.rs:37-49`, `authorize_tool_call`; `crates/hya-core/src/engine/turn.rs:265-292`, `SessionEngine::run_turn_rounds` |
| Implementation gap | `ToolCallId` correlates provider calls and events, but there is no stable `operation_id` persisted across retries, idempotency outcome record, actor incarnation fence, binding-set fence, or activation/security epoch on the execution contract. | `crates/hya-proto/src/ids.rs`, `ToolCallId`; `crates/hya-proto/src/event.rs`, tool-call event variants; execution path above |
| Inference | A revocation or actor replacement racing an already authorized asynchronous tool can produce a stale effect unless each mutating adapter participates in a last-moment/commit fence. This must be proven with a deterministic race test. | Absence in execution contract above; required future fault injection |

## 6. Install, release, update, and recovery

| Classification | Finding | Source and symbol |
| --- | --- | --- |
| Implementation | The source installer builds locked Rust binaries, prepares the Bun runtime in temporary locations, backs up the four installed resources, places them, performs post-placement smoke checks, and restores the backup on trapped failure. | `install.sh:76-230`, `build_cmd`, `cleanup_leftovers`, `restore_install`, `on_error`, placement block |
| Implementation | The installer test injects a post-placement smoke failure, asserts all binaries/runtime are restored, and asserts temporary/backup paths are removed. | `tests/install_script.sh:238-257`, rollback fixture |
| Implementation | Release CI checks tag/version/changelog agreement, builds locked artifacts, packages/smokes them, emits `SHA256SUMS`, creates build-provenance attestations, and publishes the release. | `.github/workflows/release.yml:51-205`, `Validate tag version and changelog`, `Build release binaries`, `Package release asset`, `Smoke test packaged binary`, `Attest release assets` |
| Implementation / checkout release state | Root workspace metadata and root changelog agree on `0.34.2`. The `fuji1 remote worker` checkout has no `v0.34.2` tag and no tag points at audited HEAD; its latest present release tag is `v0.33.14`. This is not evidence about remote publication. | `Cargo.toml`, `[workspace.package].version`; `CHANGELOG.md`; `git tag` / `git tag --points-at HEAD` inspection in the authoritative checkout |
| Correction | The initial statement “there is no stage/build/verify/activate/rollback path” is too broad: the installer has a meaningful staged backup/smoke/rollback path and tests. | Installer sources above |
| Inference / gap | No built-in runtime self-updater or independent update binary was found in the HEAD crate/process inventory. The installer does not establish a signed client-verified update manifest, anti-rollback security epoch, crash-journaled single-generation activation, or fences for operations crossing activation. Its four resources are moved sequentially rather than switched through one immutable generation pointer. | `install.sh`; `.github/workflows/release.yml`; process/crate inventory in root `AGENTS.md` |

## 7. Verdict on the preliminary audit

| Preliminary statement | Verdict at HEAD | Precise correction or scope |
| --- | --- | --- |
| 128 only limits provider streams | **Confirmed** | It limits depth-greater-than-zero provider streams, not agents, tasks, tool executions, root streams, or storage work. |
| Resident path may bypass part of the governor | **Confirmed, partial** | Resident spawn bypasses reserve/depth/per-run admission; resident Turns still use the provider semaphore and team turn/message budgets. |
| Spawn channel may be unbounded | **Confirmed** | Both the channel and the task-per-request fan-out are unbounded at this seam. |
| Full replay / SQLite single-write may be a bottleneck | **Mechanisms confirmed; bottleneck unproven** | Full replay per round and serialized SQLite writes are real. Capacity impact needs baseline and contention benchmarks. |
| Skill/deferred MCP can drift within one Turn | **Confirmed** | Skill discovery is per round/tool execution; deferred MCP mutates the registry while serving; requests and execution lack a Turn-pinned binding. |
| Compat MCP state may not enter engine ToolRegistry | **Confirmed for dynamic Compat MCP** | Static/deferred configured MCP does register; Compat HTTP dynamic managers do not. |
| PluginHost hot restart may not update declarations | **Confirmed** | Restart initialization result is discarded; initial declarations remain authoritative. |
| AgentSpec lacks per-agent tool/capability/extension view | **Confirmed and strengthened** | Source entries parse permissions/options, but the execution spec drops them. |
| Out-of-process plugins are not a security sandbox | **Confirmed** | The boundary is a normal child process with inherited host privileges/environment subject to ordinary OS access. |
| There is no trustworthy stage/build/verify/activate/rollback path | **Corrected** | Installer/release staging, smoke, backup, rollback, checksums, and provenance exist. The missing target is an independently trusted, signed, anti-rollback, crash-consistent self-update mechanism with effect fencing. |

## 8. Additional findings beyond the preliminary audit

1. **Accepted ADRs 0007/0008 are ahead of implementation.** The desired Turn
   snapshot rule exists as an architectural decision but is not enforced by the
   current round loop and mutable registry.
2. **Resident durability stops at data, not execution.** Mail/roster replay is
   durable; actor slots, cursors, quiescence counters, and parked tasks are not
   reconstructed at startup.
3. **Parsed policy is not enforced.** Agent permissions/options and skill
   `allowed-tools`/`model` fields do not reach an effective per-agent binding.
4. **Discovery has three visibility regimes.** AGENTS context is startup-static,
   agents are list/spawn-live, and skills are round/tool-live. There is no common
   generation or visibility boundary.
5. **MCP has duplicate control planes.** Static engine MCP and Compat HTTP MCP
   report useful state independently without a shared reconciliation/activation
   authority.
6. **Plugin restart skips more than declarations.** The new initialization
   result, including protocol/declaration validation inputs, is discarded.
7. **Capacity must include more than provider slots.** Spawn intake, Tokio task
   fan-out, file descriptors/processes, plugin queues, event bus lag, projection
   replay, SQLite writer time, memory, and effect concurrency require distinct
   resource budgets.
8. **Background transient allocation precedes reservation.** A newly created
   background child session exists before `run_team` reaches governor reserve;
   admission-before-allocation must therefore be an explicit tracer invariant.

## 9. Required target invariants

These are **target contracts**, not claims about HEAD:

- A stable `tool_id` identifies a declaration across display-name changes and
  process restarts; a generation-specific binding identifies its implementation.
- A stable `operation_id` survives retry and indexes a durable idempotency
  outcome.
- Every admitted Turn attempt pins one immutable `binding_set_id` and
  `activation_epoch` through all of its rounds.
- `security_epoch` is monotonic and live; revocation can invalidate an effect
  even while a Turn retains its immutable functional binding.
- `actor_epoch` is monotonic per logical actor; stale resident incarnations and
  delayed effects cannot write.
- Managed resources expose separate `desired`, `observed`, and `effective`
  states. Only verified/healthy observed state may become effective.
- Refresh follows `prepare -> verify -> activate -> quiesce -> drain -> retire`.
- Rollback may select an older immutable artifact, but activation creates a
  **new, strictly greater** epoch; epochs never move backward.
