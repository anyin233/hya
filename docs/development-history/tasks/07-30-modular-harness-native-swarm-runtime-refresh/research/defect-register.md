# Architecture defect register

## Audit identity and scope

This register closes the source audit for the existing Trellis task
`modular-harness-native-swarm-runtime-refresh`. The audit classifications remain
historical HEAD evidence; releases `0.34.3` through `0.34.5` are remote-green
on the isolated branch and the user has authorized only `0.34.6` now.

- Authoritative workspace: the saved project on the `fuji1 remote worker`.
- Branch: `main`.
- Audited HEAD:
  `267bfc3c6c66e46fe8514e2e70657489f853b7f0`.
- Locally tracked `origin/main`: the same commit, with `0/0` divergence at the
  audit. No network fetch is implied.
- Workspace version and root changelog heading: `0.34.2`.
- Checkout tag observation: HEAD has no tag and the `fuji1 remote worker`
  checkout tag set has no `v0.34.2`; its latest present release tag is
  `v0.33.14`. This says nothing about remote publication state.
- Dirty baseline: 19 pre-existing user-owned status entries plus the untracked
  task directory (20 status entries total) and three protected stashes. The 19 entries are frozen in
  `research/fuji1-sync-preflight.md` and are outside this audit's write scope.
- Task state at audit: `planning`; `.trellis/scripts/task.py current --source`
  reported `none`. Implementation is isolated on
  `codex/modular-harness-native-swarm-runtime-refresh` before task start.
- Current implementation/remote base:
  `156d0ad3c50aea67dfac0054485eb6991e77308b`. The only commit after the audit
  anchor changes the README icon reference, so the source classifications in
  this register are unchanged. Dirty `main` remains on the audit anchor.

The role identities and host/session IDs are defined once in
`research/browser-pro-escalation-protocol.md`. This document uses only
`MacBook Air coordinator` and `fuji1 remote worker`.

The Browser protocol ledger now also records the MacBook Air runtime-generation
and runtime-reconciliation consultations. They are advisory provenance only;
the coordinator/owner corrections and
source/TDD/benchmark/CI gates remain authoritative.

Current-cycle owner disposition:

- `MSR-SEC-001` records an unimplemented earlier target, not an authorized
  defect-remediation phase. No capability broker, escrow, or independent
  `SecurityEpoch` will be built; current `PermissionPlane` remains authority.
- `MSR-TRU-001` is an accepted trust-boundary fact. Explicitly installed
  same-UID code is trusted and malicious-plugin isolation is not promised.
- `MSR-BDL-001` is native-only. There is no old-agent adapter, synthetic
  representation, agent-file execution, or old-source Bundle CLI behavior.
  Built-ins migrate to native AgentBundles and the old
  parser/discovery/execution branch is deleted in the same `0.34.8` cutover.
  **Commit 2 WIP (this worktree):** focused cutover contracts are
  source+test verified; local workspace/TUI/bin/zero-INET and product goldens
  are `LOCAL-GATES-GREEN`; exact staging, commit, push, and remote CI remain
  `PENDING-COMMIT-PUSH-REMOTE-CI` (unclaimed). Build-time
  embedded immutable built-ins avoid installer bootstrap and old-file detectors.

## Evidence and severity rules

Every entry has one primary evidence status from the required vocabulary:

| Evidence status | Meaning |
| --- | --- |
| `source-confirmed` | The cited source directly establishes the implementation shape or missing connection. It does not prove that a feared production incident occurred. |
| `source-inferred` | The cited source makes the risk plausible, but a deterministic characterization or fault test must demonstrate the behavior. |
| `benchmark-unconfirmed` | The implementation shape is known, but whether it violates the target workload or SLO is unknown until measured. |
| `target-gap` | The requested future invariant or authority is absent from current HEAD. The target is not reported as current behavior. |

Evidence status is independent of severity:

| Severity | Meaning in this task |
| --- | --- |
| P0 | Blocks safe activation or invalidates downstream correctness evidence. Characterize before broad implementation. |
| P1 | Required correctness, integration, security-boundary, or product-capability gap. |
| P2 | Certification, performance-evidence, compatibility, or documentation gap that must not be overclaimed. |
| P3 | Release-state hygiene or obsolete bypass cleanup in its owning patch. |

Stable IDs are never renumbered or reused. Severity and disposition may change
when evidence changes; a closed entry remains in the register with its closure
evidence.

Gate abbreviations:

- `CHAR`: deterministic current-behavior characterization;
- `TDD`: an atomic RED/confirm-RED/GREEN verification slice;
- `BENCH`: a frozen workload and threshold measurement;
- `FAULT`: deterministic crash, race, lag, or failure injection.

## Register index

| Stable ID | Severity | Evidence status | Area | Short finding | Required gates |
| --- | --- | --- | --- | --- | --- |
| `MSR-ADM-001` | P0 | `source-confirmed` | spawn ingress | Unbounded spawn intake creates one Tokio task per request. | CHAR, TDD, BENCH |
| `MSR-ADM-002` | P0 | `source-confirmed` | admission | Background transient sessions are created before reserve; residents bypass the transient reserve/depth/per-run path. | CHAR, TDD, FAULT |
| `MSR-RCV-001` | P0 | `source-confirmed`; `0.34.7` focused closure `experimentally-verified`, remote pending | resident recovery | Audit HEAD did not reconstruct residents; `0.34.7` now fences, replays, terminalizes running work, and recreates queued actors before readiness. | CHAR, TDD, FAULT, remote CI |
| `MSR-BND-001` | P0 | `source-confirmed` | ToolRegistry / Turn | Schemas and resolution read a mutable registry independently; a Turn attempt pins no whole binding. | CHAR, TDD, FAULT |
| `MSR-SEC-001` | P2 | `target-gap` | permission claims | No monotonic live-security view exists; the owner has explicitly removed that target from the current cycle. | documentation, current PermissionPlane TDD |
| `MSR-FEN-001` | P0 | `target-gap`; local operation/actor closure `experimentally-verified`, remote pending | actor/effect fencing | `0.34.4` delivered OperationId/admission and `0.34.7` adds TTL-free actor-epoch canonical-state fences; arbitrary external exactly-once remains explicitly unsupported. | CHAR, TDD, FAULT, remote CI |
| `MSR-HAR-001` | P1 | `target-gap` | modular harness | No deterministic cross-lane harness controls provider, process, clock, crash, and store boundaries. | TDD |
| `MSR-CFG-001` | P1 | `source-confirmed` | configuration | Runtime composition directly makes independently discovered managers and planes effective. | CHAR, TDD |
| `MSR-REF-001` | P1 | `target-gap` | runtime refresh | No one immutable generation and atomic activation lifecycle spans tools, MCP, plugins, agents, skills, and instructions. | TDD, FAULT |
| `MSR-MCP-001` | P1 | `source-confirmed` | MCP control planes | Native/deferred MCP and Compat HTTP MCP have separate lifecycle authorities; dynamic HTTP MCP does not update engine tools. | CHAR, TDD, FAULT |
| `MSR-MCP-002` | P1 | `target-gap` | MCP state model | MCP/plugin resources have no common desired/observed/effective activation model. | TDD, FAULT |
| `MSR-PLG-001` | P1 | `source-confirmed` | plugin restart | Plugin respawn discards the new initialize result while retaining initial declarations. | CHAR, TDD, FAULT |
| `MSR-TRU-001` | P2 | `target-gap` | trust boundary | Same-UID child processes are not an untrusted-code containment boundary; trusted-only is the accepted current-cycle scope. | documentation, protocol/crash TDD |
| `MSR-POL-001` | P1 | `source-confirmed`; `0.34.8` WIP typed-reject of unsupported AgentBundle v1 fields `FOCUSED-VERIFIED`, full product goldens `LOCAL-GATES-GREEN`, exact staging/commit/push/remote CI `PENDING-COMMIT-PUSH-REMOTE-CI` | agent policy | Pre-cutover parsers retained inert policy metadata; native v1 `deny_unknown_fields` / unsupported-feature reject replaces silent AgentBundle ignore. | CHAR, TDD, full gates |
| `MSR-POL-002` | P1 | `source-confirmed`; **not** an AgentBundle v1 GA blocker | skill policy | Global SKILL.md `allowed-tools`/`model`/`license` still parsed in skill_catalog without execution enforcement; separate from Bundle cutover. | CHAR, TDD (skill plane; out of Bundle GA) |
| `MSR-BDL-001` | P1 | `target-gap` → `0.34.8` Commit 2 WIP `FOCUSED-VERIFIED`, local `LOCAL-GATES-GREEN`, `PENDING-COMMIT-PUSH-REMOTE-CI` | AgentBundle | Native prepared catalog + TurnBinding cutover landed in WIP (built-ins, can_spawn, resource view, guidance, legacy deletion); distribution/external execution remain later patches. | TDD focused done; local gates green; commit/push/remote CI open |
| `MSR-CAP-001` | P1 | `benchmark-unconfirmed` | 100/256 certification | Exact 100-active/156-durable-queue/257-overload behavior has not been certified. | CHAR, BENCH, FAULT |
| `MSR-UPD-001` | P1 | `target-gap` | self-update | Existing installer/release protections do not form an independent verifier, anti-rollback authority, or atomic immutable selector. | TDD, FAULT |
| `MSR-EVT-001` | P2 | `source-inferred` | event delivery | Durable envelopes use store sequence numbers while transient live envelopes use sequence zero; an observable ordering defect is not yet proven. | CHAR, FAULT |
| `MSR-STO-001` | P2 | `benchmark-unconfirmed` | SQLite/replay | Full replay and current append/pool settings may constrain the workload, but no bottleneck is established. | BENCH, FAULT |
| `MSR-DOC-001` | P2 | `source-confirmed` | ADR drift | ADR 0007/0008 specify next-Turn snapshots that current HEAD does not implement. | CHAR, TDD |
| `MSR-REL-001` | P3 | `source-confirmed` | version/release state | Version and changelog are `0.34.2`, while checkout tag refs do not identify a `0.34.2` release. | CHAR, owner gate |
| `MSR-LEG-001` | P3 | `target-gap`; agent-file authority removal `0.34.8` WIP `FOCUSED-VERIFIED` | obsolete authority bypasses | Mutable registry and split admission/reconciliation paths still need evidence-gated removal in their owning patches; old agent-file modules and tracked `.hya/agents` are deleted in this Commit 2 WIP (local gates green; commit/push/remote CI still open). | CHAR, TDD, FAULT |

## P0 details

### `MSR-ADM-001` — unbounded spawn ingress and task fan-out

- **Evidence status:** `source-confirmed`.
- **Source and symbols:** `crates/hya-tool/src/spawn.rs::SpawnerPlane::new`
  creates `mpsc::unbounded_channel`; `SpawnerPlane::spawn_inner` sends every
  request without a capacity lease.
  `crates/hya-app/src/runtime.rs::spawn_team_supervisor` receives an
  `UnboundedReceiver<SpawnRequest>` and creates one outer Tokio task plus one
  Tokio task for each accepted request.
- **Trigger:** concurrent or recursively generated spawn requests arrive faster
  than the supervisor completes them.
- **Impact:** queued requests and runnable tasks are not bounded by the target
  admission contract. This establishes an unbounded allocation path, not a
  confirmed production exhaustion event.
- **Current protection:** each request has cancellation and a oneshot reply;
  `run_team` later applies transient depth/per-run controls. The existing
  128-permit governor semaphore limits only depth-greater-than-zero provider
  streams, not request intake or Tokio tasks.
- **Missing invariant:** every member must obtain one durable multi-resource
  admission result before a request-owned task, child session, actor, provider
  stream, or effect is allocated.
- **Dependency / owner phase:** roadmap R1 characterization, then R5 unified
  admission; capacity accounting and fairness are owner decisions.
- **Required gates:** CHAR records queue/task counts under a blocked fake
  provider; the first TDD tracer uses a tiny capacity and proves typed overload
  before allocation; BENCH later proves bounded counts at 100/156/257.

### `MSR-ADM-002` — pre-admission allocation and resident bypass

- **Evidence status:** `source-confirmed`.
- **Source and symbols:** in
  `crates/hya-app/src/runtime.rs::spawn_team_supervisor`, background transient
  members call `SessionEngine::create` while building `MemberSpec` before
  `crates/hya-core/src/subagent.rs::run_team` performs
  `SubagentGovernor::reserve`. Resident members call
  `crates/hya-core/src/resident.rs::ResidentSupervisor::spawn_resident`
  directly and never enter `run_team`.
- **Trigger:** a background transient request later fails reservation, or a
  resident request arrives while transient capacity/depth/per-run limits are
  exhausted.
- **Impact:** rejected work can already own durable/session state, and resident
  work can bypass the accounting used for transient work.
- **Current protection:** `run_team` enforces depth and per-run reservation for
  transient members; resident actors retain Turn/message budgets. Neither
  protects both paths before allocation.
- **Missing invariant:** foreground/background and resident/transient members
  share one admission state machine, and admission is the first side-effecting
  transition.
- **Dependency / owner phase:** R1 contract characterization, R2 durable ticket
  identity/journal, R5 authoritative admission.
- **Required gates:** CHAR covers all four lifecycle combinations; a TDD test
  proves a denied item produces no `SessionCreated`, resident slot, worker
  task, provider request, or effect; FAULT interrupts reserve/promote/release
  boundaries and recovers exact counts.

### `MSR-RCV-001` — resident execution is not rehydrated

- **Evidence status:** `source-confirmed`.
- **`0.34.7` closure status:** `source-verified` and focused
  `experimentally-verified`; full workspace/remote CI pending. Startup now
  advances every active actor claim before readiness, fail-closed aborts old
  admissions/running projections, recreates the existing resident task owner,
  and notifies only durable queued-not-started work. One running marker plus
  the projected inbox cursor distinguishes retryable queue from aborted work.
  Repeated recovery, running child/tool/message terminalization, and unchanged
  transient paths have deterministic tests.
- **Source and symbols:** `crates/hya-core/src/resident.rs::ResidentSupervisor`
  owns an in-memory `teams` map. `ResidentSupervisor::start` begins without
  reconstructed teams, and `ResidentSupervisor::team_for` creates an empty
  `TeamState`; `ResidentSupervisor::ensure_main`,
  `ResidentSupervisor::spawn_resident`, and `resident_task` populate/run only
  in-process slots. Durable roster/mail projection data does not recreate those
  executable slots on startup.
- **Trigger:** the process restarts after resident registration, while resident
  work is pending, or before/after a cursor/effect transition.
- **Impact:** durable logical team state and executable resident state can
  diverge. Duplicate/stale execution is a risk to prove, not a claimed observed
  incident.
- **Current protection:** mailbox/roster events are durable and replayable;
  broadcast-lag handling can replay into slots that already exist in memory;
  team Turn/message budgets bound a live actor.
- **Missing invariant:** startup reconstructs resident identities, durable work,
  cursor/ack state, leases, and a newer actor epoch before consuming new mail.
- **Dependency / owner phase:** R2 actor/admission IDs and journals, R5
  admission, then R6 rehydration/fencing.
- **Required gates:** CHAR freezes startup behavior; TDD reconstructs one
  resident from real temporary SQLite state; FAULT kills at
  registration-before-spawn, wake, Turn, cursor, and local-effect boundaries
  and proves one effective incarnation.

### `MSR-BND-001` — no whole-Turn immutable binding

- **Evidence status:** `source-confirmed`.
- **`0.34.5` closure status:** remote-green `source-verified` and
  `experimentally-verified` at commit
  `95f4fe20b3750d376023384d869a52da1e84201f`, CI run `30612919698`.
  `RuntimeRegistry`/`TurnBinding` now pin prompt skills, schemas,
  resolution, and dispatch for every round; the deterministic mid-turn
  publication tracer proves old/new visibility. This does not close later
  process-death/reconciliation fault cases.
- **Source and symbols:** `crates/hya-tool/src/tool.rs::ToolRegistry` stores
  mutable tool and alias maps. `ToolRegistry::register_with_permission`,
  `ToolRegistry::remove`, `ToolRegistry::schemas`, and
  `ToolRegistry::resolve` operate by name on independently acquired current
  views. `crates/hya-core/src/engine/turn/messages.rs::request_from_messages`
  obtains schemas, while `crates/hya-core/src/engine/turn.rs::run_turn_rounds`
  later resolves/dispatches against the registry; no `BindingSetId` is pinned.
- **Trigger:** a tool/plugin/MCP declaration changes between schema exposure,
  provider rounds, authorization, and dispatch.
- **Impact:** one Turn attempt can describe one declaration and resolve another.
  A concrete wrong-tool execution remains to be reproduced by the race test.
- **Current protection:** duplicate registered names are rejected; current maps
  are lock-protected; ADR 0007/0008 already document next-Turn intent.
- **Missing invariant:** one `TurnAttemptId` pins one immutable binding set whose
  schema, alias, declaration, executor, model, skill, instruction, and
  structural capability view is used for every round and dispatch.
- **Dependency / owner phase:** R2 IDs/epochs, R3 immutable generation and
  Turn-pinned binding.
- **Required gates:** CHAR deterministically pauses between schema capture and
  resolution; TDD proves current Turn identity is unchanged by refresh and the
  next Turn sees the new activation; FAULT covers plugin death and partial
  activation while an old binding is pinned.

### `MSR-SEC-001` — no live subtractive security fence

- **Evidence status:** `target-gap`.
- **Source and symbols:** `crates/hya-app/src/runtime.rs::build_session_engine`
  constructs one `PermissionPlane`; the Turn path in
  `crates/hya-core/src/engine/turn.rs::run_turn_rounds` resolves, authorizes,
  and executes tools without a durable `security_epoch` recheck immediately
  before each effect. `crates/hya-tool/src/permission.rs::PermissionPlane`
  provides current permission decisions but not the proposed epoch/fence
  contract.
- **Current-cycle disposition:** owner-removed target. No independent
  `SecurityEpoch`, capability broker, or escrow delegation is authorized.
- **Trigger:** a future release claims immediate subtractive revocation or a
  bundle/plugin bypasses current Harness config/`PermissionPlane`.
- **Impact:** the product must not claim the removed security semantics; current
  bundle/plugin inputs must never broaden Harness policy.
- **Current protection:** permission rules and remembered decisions gate normal
  invocation; structural tool removal can prevent later name resolution.
- **Missing invariant:** supported resource/permission overlay fields are
  deterministic narrowing inputs to current Harness policy, and unavailable
  evaluation fails closed.
- **Dependency / owner phase:** `0.34.5` binding and `0.34.6` existing
  PermissionPlane propagation; no Bundle parser or separate security phase.
- **Required gates:** documentation forbids the removed claims; TDD proves
  allow/ask/deny, direct dispatch, bundle/plugin narrowing, and failure
  propagation through the current `PermissionPlane`.

### `MSR-FEN-001` — missing stable operation and actor/effect fences

- **Evidence status:** `target-gap`.
- **`0.34.4`/`0.34.7` closure status:** internal `OperationId`, durable
  admission idempotency, one-winner resident claim, monotonic actor epoch,
  actor-bound admission transitions, and claim-aware canonical event/mail/
  child/tool-result commits are `source-verified` and focused
  `experimentally-verified`; full workspace/remote CI pending. The guarantee is
  deliberately limited to canonical local state. Filesystem/network/API
  effects already performed before takeover remain non-reversible and are not
  claimed exactly once.
- **Source and symbols:** current identity types in
  `crates/hya-proto/src/ids.rs` do not include the planned binding, operation,
  or actor-incarnation identities. Resident execution in
  `crates/hya-core/src/resident.rs::{ResidentSupervisor,resident_task}` and
  tool execution through
  `crates/hya-core/src/engine/turn.rs::run_turn_rounds` do not carry and
  revalidate `actor_epoch`, `binding_id`, admission/lease state, and stable
  `operation_id` as one correctness token.
- **Trigger:** retry/recovery overlaps a delayed old actor or an outcome becomes
  uncertain across a crash.
- **Impact:** stale or duplicate effects are possible by inference; this
  register does not claim that one has occurred. Arbitrary remote effects also
  cannot be promised exactly once.
- **Current protection:** event append precedes durable event publication;
  resident cancellation and in-memory state reduce ordinary duplicate work;
  provider tool-call IDs identify protocol calls but are not durable semantic
  operation IDs.
- **Missing invariant:** retries/recovery of one semantic effect reuse one
  stable operation ID; only the current actor epoch may claim work or
  linearize effects; unsupported remote outcomes enter an explicit
  `indeterminate`/reconcile state.
- **Dependency / owner phase:** R2 stable IDs/additive journals, R4 effect gate,
  R5 admission leases, R6 actor recovery/fencing. Remote-effect retry policy is
  owner-only.
- **Required gates:** CHAR inventories every effect class/linearization point;
  TDD proves local operation deduplication; FAULT delays an old incarnation
  across restart and proves it cannot claim, publish, acknowledge, or commit.

## P1 details

### `MSR-HAR-001` — deterministic modular harness gap

- **Evidence status:** `target-gap`.
- **Source and symbols:** runtime behavior is assembled in
  `crates/hya-app/src/runtime.rs::build_session_engine`; the nearest tests cover
  individual spawn/tool/install paths, but CodeGraph found no direct covering
  tests for `build_session_engine`, `run_turn_rounds`,
  `ResidentSupervisor::spawn_resident`, `McpManager::connect_all_into`, or
  `PluginConn::ensure_client`.
- **Trigger:** a concurrency, refresh, crash, or recovery contract needs a
  reproducible RED test.
- **Impact:** later changes could rely on sleeps, mocks that bypass the event
  store, or independent fixtures that cannot prove cross-lane invariants.
- **Current protection:** fake-provider and integration-test support, in-memory
  SQLite, plugin/MCP fixtures, and installer rollback tests already provide
  reusable pieces.
- **Missing invariant:** one test harness can pause provider/effect/process
  boundaries, inject failure, restart against real temporary SQLite, and
  inspect durable state without becoming a second runtime implementation.
- **Dependency / owner phase:** R1. No product activation occurs in this phase.
- **Required gates:** the harness itself is introduced by a TDD slice; its
  determinism gate forbids wall-clock sleep as the correctness discriminator
  and proves each barrier/fault point is observable.

### `MSR-CFG-001` — direct composition and inconsistent discovery visibility

- **Evidence status:** `source-confirmed`.
- **Closure status through `0.34.6`:** `0.34.5` remotely closed the immutable
  initial/deferred publication seam. `0.34.6` locally routes startup, deferred,
  and Compat MCP through one app reconciler and the same `RuntimeRegistry`;
  its full local/remote release gates remain pending.
  Tool/plugin/synchronous-MCP construction now freezes one initial snapshot;
  deferred MCP can only publish through the engine owner; workdir skill
  discovery enters the same snapshot. Agent/AGENTS sources remain future work;
  the bounded MCP/plugin reconciliation scope is the active `0.34.6` closure.
- **Source and symbols:** `crates/hya-app/src/runtime.rs::build_session_engine`
  directly constructs and registers tools, plugin host, deferred/static MCP,
  permission, interaction, spawner, mailbox, agent catalog, governor, resident
  supervisor, and mailbox service. Agent/AGENTS/skill/plugin/MCP discovery has
  startup-static, spawn-live, round-live, and deferred-connect visibility.
- **Trigger:** source/config/process state changes while the runtime is active.
- **Impact:** adapters can mutate or expose effective behavior on different
  schedules, making atomic refresh and provenance difficult to prove.
- **Current protection:** individual managers validate parts of their input;
  initial plugin protocol versions and registry name collisions are checked.
- **Missing invariant:** discovery emits canonical desired candidates only;
  one generation authority validates deterministic, content-addressed input
  before any effective activation.
- **Dependency / owner phase:** R1 precedence characterization, R2 identity,
  R3 generation. Config precedence and secret-reference policy are owner-only.
- **Required gates:** CHAR records every source and current visibility boundary;
  TDD proves identical input yields identical generation identity and malformed,
  cyclic, colliding, or partial input leaves the effective generation unchanged.

### `MSR-REF-001` — no unified atomic runtime refresh lifecycle

- **Evidence status:** `target-gap`.
- **Closure status through `0.34.6`:** the `0.34.5` in-process immutable
  snapshot/publication prerequisite is remote green. `0.34.6` locally verifies
  current-revision all-or-nothing MCP/plugin-source publication, stale-attempt
  rejection, drop-only removal, and retained old owners; full release gates
  remain pending.
  Failure/no-op preservation, unique monotonic concurrent publication, complete
  candidate visibility, and retained old bindings are covered. Durable
  activation, resource incarnation reconciliation, quiesce/drain/retire, and
  forward-only rollback remain target gaps for later stages.
- **Source and symbols:** current effective mutation is split across
  `crates/hya-tool/src/tool.rs::ToolRegistry`,
  `crates/hya-app/src/runtime.rs::{register_mcp_tools,register_plugin_tools}`,
  `crates/hya-core/src/engine.rs::effective_agent_for_projection`,
  `crates/hya-mcp/src/manager.rs::McpManager`, and
  `crates/hya-plugin/src/host.rs::PluginConn`.
- **Trigger:** tools, MCP, plugins, agents, skills, instructions, or bundle
  inputs change during a running session.
- **Impact:** the target cannot prove all-or-nothing generation activation,
  next-Turn visibility, draining, or forward-only rollback.
- **Current protection:** lock-protected local maps and next-Turn ADR intent;
  failed individual connections generally leave that source unavailable.
- **Missing invariant:** `prepare -> verify -> activate -> quiesce -> drain ->
  retire`, with rollback selecting retained older content under a new,
  strictly higher activation epoch.
- **Dependency / owner phase:** R2 IDs/epochs, R3 generation/binding, R8
  adapter reconciliation.
- **Required gates:** TDD rejects a partial candidate atomically and pins old/new
  Turn visibility; FAULT kills at every activation journal transition and
  recovers one complete effective generation.

### `MSR-MCP-001` — split native/deferred and Compat HTTP MCP authorities

- **Evidence status:** `source-confirmed`.
- **`0.34.6` closure status:** locally `source-verified` and
  `experimentally-verified`, pending full release gates. Startup, deferred, and
  Compat mutations enter `hya_app::runtime_reconcile::RuntimeReconciler`;
  `hya-server::McpControl` is a dependency-inverted handle, and the former
  `compat/mcp_state.rs` authority is deleted.
- **Source and symbols:** `crates/hya-mcp/src/manager.rs::McpManager` stores
  server clients, tools/resources, and status; `McpManager::connect_all_into`
  mutates the shared manager as connections finish.
  `crates/hya-app/src/runtime.rs::register_mcp_tools` copies discovered tools
  into the engine registry. Separately,
  `crates/hya-server/src/compat/mcp_state.rs::McpHttpState` owns dynamic HTTP
  configuration/managers/status, while
  `crates/hya-server/src/state.rs::ServerState` holds both MCP states; the HTTP
  lifecycle has no engine `ToolRegistry` activation handle.
- **Trigger:** a Compat MCP server is added, connected, disconnected, or changes
  declarations after engine construction.
- **Impact:** HTTP state/status can change without the same tools becoming
  callable through the engine, and native/deferred paths do not share one
  activation transaction.
- **Current protection:** MCP initialization/initialized handshake, per-server
  statuses, namespaced tool names, permission classification, and held child
  guards.
- **Missing invariant:** all MCP sources publish desired and observed state to
  one reconciler; only a validated effective declaration set enters an atomic
  binding generation.
- **Dependency / owner phase:** R3 candidate/binding protocol, R8 MCP adapter
  convergence.
- **Required gates:** CHAR compares API status with engine callability; TDD
  connects/disconnects a dynamic fixture and proves next-Turn activation/removal;
  FAULT covers partial handshake, declaration collision, crash, and reconnect.

### `MSR-MCP-002` — desired/observed/effective state is absent

- **Evidence status:** `target-gap`.
- **`0.34.6` closure status:** the bounded MCP state model is locally
  `experimentally-verified`, pending full release gates. Desired revision,
  per-source preparation tickets, and typed observed outcomes are app-owned;
  effective source manifest/generation and binding owners exist only in
  `RuntimeRegistry`. The reconciler owns no effective cache or dispatch API.
- **Source and symbols:** `crates/hya-mcp/src/manager.rs::{McpManager,McpStatus}`
  represents connection status and collected resources, not separate desired,
  observed, validated, and effective generations.
  `crates/hya-plugin/src/host/connection.rs::connect_one` similarly turns the
  first observed declaration directly into retained host state.
- **Trigger:** desired configuration differs from process observation, a
  declaration broadens/removes capabilities, or validation fails.
- **Impact:** health, declaration observation, policy acceptance, and effective
  activation cannot be independently audited or rolled back.
- **Current protection:** typed connection statuses, protocol-version validation
  for plugins, and current registry collision rejection.
- **Missing invariant:** desired configuration, untrusted observation, validated
  candidate, and effective binding are separately recorded; adapters cannot
  self-activate.
- **Dependency / owner phase:** R2 source/declaration IDs, R3 common reconciler,
  R8 adapter integration.
- **Required gates:** TDD proves invalid observation never becomes effective and
  a healthy prior generation remains; FAULT covers reordered completion,
  disappearance, reconnect broadening/removal, and stale observations.

### `MSR-PLG-001` — plugin respawn retains stale declarations

- **Evidence status:** `source-confirmed`.
- **`0.34.6` closure status:** locally `source-verified` and
  `experimentally-verified`, pending full release gates. Startup validates
  configured/handshake identity; a respawn compares a deterministic canonical
  encoding of the complete initialize declaration and closes the replacement
  on drift. The claim is limited to tool exports plus their RPC binding; no
  plugin hot add/remove/reload or whole-plugin snapshot is implemented.
- **Source and symbols:** `crates/hya-plugin/src/host/connection.rs::connect_one`
  validates the initial protocol version and stores `init.hooks`, `init.tools`,
  and `init.workspace_adapters`. On restart,
  `crates/hya-plugin/src/host.rs::PluginConn::ensure_client` calls
  `PluginClient::initialize` but discards the returned initialization value and
  leaves the stored declarations unchanged.
- **Trigger:** a plugin process dies and restarts with removed, changed, or
  broadened declarations or a different protocol.
- **Impact:** host declarations can describe the old incarnation while calls
  reach the new process.
- **Current protection:** restart budget/disable state, per-call timeout,
  initial protocol check, `kill_on_drop`, and a bounded event channel of 256
  that drops with a warning rather than growing without bound.
- **Missing invariant:** every incarnation publishes new observed declarations;
  they are protocol/policy validated into a candidate, and activation is atomic
  or the old pinned binding fails typed-unavailable.
- **Dependency / owner phase:** R3 immutable binding, R8 plugin reconciliation.
- **Required gates:** CHAR changes fixture declarations across restart; TDD
  proves removal and compatibility handling; FAULT kills before/after
  initialize, during observation, and while an old Turn binding is pinned.

### `MSR-TRU-001` — same-UID execution is not containment

- **Evidence status:** `target-gap`.
- **Source and symbols:** `crates/hya-plugin/src/client.rs::PluginClient::spawn`
  uses ordinary `tokio::process::Command` with piped stdio and `kill_on_drop`;
  `crates/hya-mcp/src/client.rs::McpClient::spawn` has the same ambient-process
  trust shape. No OS isolation/broker boundary is established by these symbols.
- **Current-cycle disposition:** accepted trusted-code boundary. The Harness
  does not isolate malicious plugins/bundles.
- **Trigger:** documentation or UI describes an executable plugin/bundle as
  untrusted, sandboxed, or malicious-code isolated.
- **Impact:** same-UID code can use ambient user authority outside the logical
  tool/capability policy. This is a trust-boundary statement, not evidence that
  an escape occurred.
- **Current protection:** out-of-process crash containment, stdio protocol,
  timeouts, restart limits, existing PermissionPlane checks on dispatched
  calls, and the
  documented trusted-only default.
- **Missing invariant:** all product/docs surfaces consistently label
  explicitly installed executable code trusted and make no sandbox promise.
- **Dependency / owner phase:** task documentation, `0.34.8` native built-in
  cutover, and `0.34.10`/`0.34.11` external execution.
- **Required gates:** TDD covers protocol validation, crash/cancel/resource
  propagation and current PermissionPlane integration; documentation review
  rejects any malicious-code-isolation claim.

### `MSR-POL-001` — agent policy metadata is only partially effective

- **Evidence status:** historical `source-confirmed` on pre-cutover parsers;
  `0.34.8` Commit 2 WIP: unsupported AgentBundle v1 fields are
  **typed-reject verified** (`deny_unknown_fields`,
  `invalid_schema_references_and_executable_features_fail_typed`); full product
  goldens are `LOCAL-GATES-GREEN`; exact staging, commit, push, and remote CI
  remain `PENDING-COMMIT-PUSH-REMOTE-CI` (unclaimed).
- **Historical source (deleted):** `compat::agent_catalog::AgentEntry` and
  `subagent_resolve` retained inert options/headers/body/permissions.
- **Current source:** `hya-bundle` prepared IR rejects unknown/unsupported
  AgentBundle fields; effective runtime fields flow through prepared agents +
  `AgentSpec` (name/model/prompt/workdir/reasoning) and compiled resource views.
- **Trigger:** a user expects arbitrary pre-cutover agent-file policy keys to
  constrain a spawned agent silently.
- **Impact (historical):** accepted configuration could be compatibility-only.
- **Current protection:** native v1 does not silently ignore unsupported Bundle
  fields; model/prompt/reasoning/spawn lifecycle and resource views have focused
  tests (see matrix).
- **Missing invariant for release:** exact staging, commit, push, and remote CI
  for Commit 2; any future typed permission-overlay consumer remains a later
  feature.
- **Dependency / owner phase:** `0.34.8` cutover WIP; later patches for
  distribution/external execution.
- **Required gates:** focused typed-reject + cutover suites (present); local
  gates green (`LOCAL-GATES-GREEN`); commit/push/remote CI still open
  (`PENDING-COMMIT-PUSH-REMOTE-CI`).

### `MSR-POL-002` — skill policy metadata is parsed but not enforced

- **Evidence status:** `source-confirmed` on the **global skill catalog**, not
  on AgentBundle v1. **Must not** be labeled a Bundle GA blocker for `0.34.8`.
- **Source and symbols:**
  `crates/hya-tool/src/skill_catalog.rs::{SkillFrontmatter,parse_skill}` parses
  and retains `allowed_tools` and `model` (and `license`);
  `skills_section` emits only names/descriptions into the skill index path.
  Current prompt/tool dispatch does not consume those retained skill policy
  fields as an effective restriction.
- **Trigger:** an activated global skill declares a model restriction or tool
  allowlist.
- **Impact:** skill-declared allowlists/models are not execution fences.
- **Current protection:** malformed/disabled skills skipped; global
  PermissionPlane still applies to tool calls.
- **Missing invariant:** skill-plane enforcement or typed reject of those
  SKILL.md fields (separate track from AgentBundle cutover).
- **Dependency / owner phase:** skill plane / later binding work — **not**
  `0.34.8` Bundle GA.
- **Required gates:** skill-plane CHAR/TDD; do not gate Bundle cutover on this.

### `MSR-BDL-001` — no first-class Markdown/JS/Rust `AgentBundle`

- **Evidence status:** historical `target-gap`; **`0.34.8` Commit 2 WIP focused
  cutover `FOCUSED-VERIFIED`**, local **`LOCAL-GATES-GREEN`**, exact staging/
  commit/push/remote CI **`PENDING-COMMIT-PUSH-REMOTE-CI`** (unclaimed).
  Distribution (`.hyabundle`) and external JS/Rust runners remain later
  patches (`0.34.9+` / `0.34.10+`).
- **Current source and symbols:** `hya-bundle` prepare/catalog IR;
  `RuntimeSnapshot`/`TurnBinding` single `Arc<BundleCatalog>`;
  `hya_app::runtime::builtin_catalog`; compiled resource views; deleted legacy
  agent-file modules and tracked `.hya/agents`. Focused tests indexed in
  `research/agent-capability-parity-matrix.md` and `implement.md` §18.3.
- **Trigger:** product requires immutable prepared agent definitions without
  dual catalog authority.
- **Impact (historical):** no first-class bundle unit; cutover WIP closes the
  built-in native path only.
- **Current protection:** build-time embedded prepared built-ins; fail-closed
  decode; `can_spawn`; resource view narrowing; guidance composition; docs
  example + authoring skill (prepare-valid only).
- **Still open for this defect's full original scope:** package distribution,
  external runners, exact staging/commit/push/remote CI, version `0.34.8`
  publish (local gates already green; no remote/GA claim).
- **Dependency / owner phase:** `0.34.8` WIP cutover (this worktree);
  `0.34.9` distribution; `0.34.10`/`0.34.11` external/resident execution.
- **Required gates:** focused cutover TDD (present); local workspace/TUI/CI
  green (`LOCAL-GATES-GREEN`); exact staging/commit/push/remote CI open
  (`PENDING-COMMIT-PUSH-REMOTE-CI`); later FAULT for external runners.

### `MSR-CAP-001` — 100/256 capacity contract is uncertified

- **Evidence status:** `benchmark-unconfirmed`.
- **Source and symbols:** `crates/hya-core/src/subagent.rs::{run_team,SubagentGovernor}`
  supplies current transient limits; `crates/hya-app/src/runtime.rs::spawn_team_supervisor`
  and `crates/hya-core/src/resident.rs::ResidentSupervisor` expose bypasses
  described above. The existing 128 semaphore is a provider-stream dimension,
  not proof of total work-item capacity.
- **Trigger:** a release or product claim states native support for a 100-active
  steady workload or a 256-request burst.
- **Impact:** capacity, fairness, memory, queue age, storage pressure, and
  recovery behavior are unknown at the requested boundary.
- **Current protection:** transient depth/per-run reserve, provider-stream
  semaphore, resident Turn/message budgets, cancellation, and SQLite busy
  timeout.
- **Missing invariant:** under a barrier workload, exactly 100 admitted work
  items are active, 156 are durably non-active, and item 257 receives a typed
  overload with no downstream allocation. Provider/root/tool/storage resource
  dimensions remain separately observable.
- **Dependency / owner phase:** R1 accounting/SLO decision, R5 admission, R6
  recovery, R10 integrated certification. Whether the root consumes the 100
  and the exact fairness policy are owner-only.
- **Required gates:** CHAR defines each counted state; BENCH covers 100 steady,
  256 burst, nested spawn, resource vectors, memory/latency/queue age and soak;
  FAULT repeats the exact 100/156 state across restart, cancellation, promotion,
  provider failure, broadcast lag, refresh churn, and storage contention.

### `MSR-UPD-001` — independent updater TCB and atomic activation gap

- **Evidence status:** `target-gap`.
- **Source and symbols:** `install.sh::{preflight_path,restore_install,on_error}`
  stages binaries/runtime, retains backups, runs smoke checks, and restores on
  handled failure; `tests/install_script.sh` contains rollback coverage.
  `.github/workflows/release.yml` validates tag/version/changelog, packages and
  smokes artifacts, emits `SHA256SUMS`, and attests provenance. No current
  source symbol forms a built-in verifier protected independently from the
  candidate runtime, monotonic anti-rollback state, durable activation journal,
  or immutable atomic selector.
- **Trigger:** automatic/self-directed update, power/process failure during
  placement, malicious/stale metadata, or requested rollback to old content.
- **Impact:** current protections do not establish the target independent TCB
  or prove that every abrupt interruption leaves one complete trusted
  generation. This does not mean the project lacks an installer or rollback.
- **Current protection:** locked release builds, staged paths, backup/restore,
  post-placement smoke, tag/version/changelog CI checks, hashes, and provenance.
- **Missing invariant:** a minimal independently protected verifier validates
  canonical signed metadata, artifact/platform digest and monotonic release
  sequence; immutable old/new generations plus a crash journal/selector recover
  one bootable trusted release. Older content returns only through a newly
  authorized higher sequence/epoch.
- **Dependency / owner phase:** R1 failure model; design may proceed after R2,
  but R11 activation needs owner-approved key custody, trust boundary, rotation,
  emergency rollback, filesystem guarantees, and production authorization.
- **Required gates:** TDD rejects wrong signature/digest/platform/freshness/
  sequence; FAULT terminates at every download/write/fsync/rename/smoke/selector
  transition, including disk-full and damaged candidate/runtime, and proves
  verification does not depend on plugins, MCP, provider, or candidate code.

## P2 details

### `MSR-EVT-001` — transient sequence-zero semantics need characterization

- **Evidence status:** `source-inferred`.
- **Source and symbols:** `crates/hya-core/src/engine.rs::SessionEngine::emit`
  appends through `SessionStore::append_event` before publishing the assigned
  sequence; `SessionEngine::publish_live` publishes an envelope with sequence
  zero without appending. `crates/hya-store/src/lib.rs::SessionStore::replay`
  returns only durable globally sequenced rows.
- **Trigger:** consumers combine transient live envelopes with durable replay,
  reconnect after lag, or use sequence ordering as a cursor.
- **Impact:** a consumer may observe an ambiguous ordering/recovery boundary,
  but no user-visible misordering or loss has been demonstrated.
- **Current protection:** durable state changes append before publish; shared
  projection replay remains authoritative; lag handling can reread durable
  state.
- **Missing invariant:** transient notification is explicitly a wake hint with
  no durable cursor semantics, or it receives a separate typed ordering domain;
  recovery never treats sequence zero as durable progress.
- **Dependency / owner phase:** R1 characterization and R6 recovery semantics.
- **Required gates:** CHAR records live/replay consumer behavior; FAULT injects
  lag/reconnect around sequence-zero publication and proves authoritative state
  converges without skipped durable events.

### `MSR-STO-001` — SQLite/replay scaling is unproven, not a confirmed bottleneck

- **Evidence status:** `benchmark-unconfirmed`.
- **Source and symbols:** `crates/hya-store/src/lib.rs::SessionStore::connect`
  configures WAL, `NORMAL`, five-second busy timeout, foreign keys, and a pool
  maximum of eight. `SessionStore::append_event` performs one `INSERT ...
  RETURNING seq`; `SessionStore::replay` fetches/deserializes the full ordered
  session log; `SessionStore::read_projection` folds that full replay through
  the shared reducer.
- **Trigger:** long histories, many concurrent writers/readers, restart at the
  100/156 boundary, or sustained resident workloads miss a frozen resource/SLO
  threshold.
- **Impact:** latency, CPU, memory, or writer contention may block
  certification. No such threshold failure has yet been measured.
- **Current protection:** WAL, busy timeout, bounded pool, append-only log,
  deterministic shared reducer, and no second materialized truth that can
  drift.
- **Missing invariant:** measure before optimizing; any checkpoint/index/batch
  is derivable, replay-equivalent, crash-safe, and cannot replace the append-only
  event authority.
- **Dependency / owner phase:** R1 harness/baselines; R9 conditional
  optimization only after owner-ratified thresholds fail; R10 recertification.
- **Required gates:** BENCH covers 1k/10k/100k histories, append contention,
  pool saturation, replay/recovery latency, CPU/RSS/disk growth and the 100/256
  matrix; FAULT proves any chosen optimization remains replay-equivalent. A
  passing benchmark records “no storage change.”

### `MSR-DOC-001` — next-Turn ADRs are ahead of implementation

- **Evidence status:** `source-confirmed`.
- **Closure status:** `0.34.5` made next-Turn immutable binding remote green;
  `0.34.6` narrows ADR 0008 to the implemented plugin startup/crash
  re-handshake tool-binding guarantee and explicitly disclaims plugin hot
  reload. Final `0.34.6` release gates remain pending.
- **Source and symbols:** `docs/adr/0007-hot-skill-reload-visibility.md` states
  that an in-flight Turn keeps its skill snapshot;
  `docs/adr/0008-hot-plugin-reload-visibility.md` states the same for
  plugin/tool registry visibility. Current
  `crates/hya-core/src/engine.rs::effective_agent_for_projection`,
  `crates/hya-core/src/engine/turn/messages.rs::request_from_messages`, and
  `crates/hya-tool/src/tool.rs::ToolRegistry` do not implement a complete
  whole-Turn immutable snapshot.
- **Trigger:** documentation or release notes imply the accepted ADR behavior
  already exists.
- **Impact:** operators and implementers can confuse a target/accepted decision
  with current runtime semantics.
- **Current protection:** the ADRs are explicit and the task design labels
  accepted documents separately from implementation evidence.
- **Missing invariant:** each contract document carries implementation/validation
  status, and the behavior is claimed only after the R3 race gate passes.
- **Dependency / owner phase:** R0 documentation containment and R3 binding
  implementation.
- **Required gates:** CHAR links the current counterexample; TDD turns the ADR
  wording into next-Turn visibility tests before marking it implemented.

## P3 details

### `MSR-REL-001` — checkout version/tag state needs an explicit owner decision

- **Evidence status:** `source-confirmed`.
- **Source and symbols:** root `Cargo.toml` has
  `[workspace.package].version = "0.34.2"`; root `CHANGELOG.md` begins
  `# 0.34.2`; `.github/workflows/release.yml` rejects mismatched
  tag/version/changelog. Local refs contain no `v0.34.2`, and no tag points at
  audited HEAD.
- **Trigger:** calling `0.34.2` released, tagging/publishing it, or beginning a
  later feature slice without choosing a new aligned version.
- **Impact:** release status can be misstated or metadata can diverge at
  publication. The checkout-ref observation cannot prove remote release state.
- **Current protection:** release CI validates all three values and root
  changelog policy; historical changelogs are archived in `docs/changes`.
- **Missing invariant:** source version, newest-only root changelog, chosen
  release tag, published artifacts, and release intent have one explicit owner
  decision at publication.
- **Dependency / owner phase:** R0 records the observation; R12 release
  reconciliation. Only the owner may decide whether `0.34.2` was intentionally
  unreleased or authorize a tag/release action.
- **Required gates:** CHAR is the exact authoritative-checkout
  ref/version/changelog inspection; no TDD/build/publish action is authorized
  by this audit.

### `MSR-LEG-001` — obsolete authority bypasses require owning-patch removal

- **Evidence status:** `target-gap`.
- **Source and symbols:** the paths to retire include mutable per-name
  `crates/hya-tool/src/tool.rs::ToolRegistry` activation, split
  `crates/hya-app/src/runtime.rs::spawn_team_supervisor` resident/transient
  admission, direct `register_mcp_tools`/`register_plugin_tools`, and separate
  Compat MCP state in
  `crates/hya-server/src/compat/mcp_state.rs::McpHttpState`.
- **Trigger:** a new authority becomes effective while a superseded path can still
  admit, activate, dispatch, or recover the same state transition.
- **Impact:** two writers/authorities can drift; deleting too early can remove
  the only rollback path.
- **Current protection:** the implementation plan keeps new journals and
  candidates shadow/additive until gates pass.
- **Missing invariant:** exactly one effective writer/activator/admission path;
  each obsolete bypass is removed in the patch that activates its replacement.
  Old agent-file code is removed in the atomic `0.34.8` built-in cutover and is
  never retained as a translation or rollback seam.
- **Dependency / owner phase:** each owning patch plus R12's per-patch
  authority audit; there is no later agent-format cleanup release.
- **Required gates:** CHAR/CodeGraph no-callers and authority scan; TDD/full
  behavioral/replay proof before removal; FAULT and rollback rehearsal from a
  quiescent recovery point. After removal, rollback is a new verified
  higher-epoch change, not resurrection of mutable authorities.

## Coverage and claim boundaries

The 23 entries cover the required audit surfaces:

- modular harness and configuration: `MSR-HAR-001`, `MSR-CFG-001`;
- subagent/resident/admission/recovery: `MSR-ADM-001`,
  `MSR-ADM-002`, `MSR-RCV-001`, `MSR-CAP-001`;
- SQLite/event/replay: `MSR-EVT-001`, `MSR-STO-001`;
- ToolRegistry/runtime refresh: `MSR-BND-001`, `MSR-REF-001`,
  `MSR-DOC-001`;
- MCP desired/observed/effective: `MSR-MCP-001`, `MSR-MCP-002`;
- plugin restart/containment: `MSR-PLG-001`, `MSR-TRU-001`;
- agent/skill policy: `MSR-POL-001`, `MSR-POL-002`;
- Markdown/JS/Rust AgentBundle: `MSR-BDL-001`;
- live revocation and actor/effect fencing: `MSR-SEC-001`,
  `MSR-FEN-001`;
- self-update TCB/rollback: `MSR-UPD-001`;
- documentation/version/obsolete-path drift: `MSR-DOC-001`, `MSR-REL-001`,
  `MSR-LEG-001`.

The register intentionally does **not** claim that resource exhaustion, stale
effects, event loss, containment escape, SQLite bottleneck, or remote
non-publication has occurred. Those statements require the gates named above.

## Browser/Pro disposition

No new uncertainty packet is required to close this audit:

- source behavior is directly inspectable for the confirmed entries;
- inferred behavior has a safe deterministic characterization/fault criterion;
- performance questions have a benchmark discriminator;
- containment scope, capacity accounting/fairness, updater trust/key custody,
  remote-effect retry policy, config precedence, release intent, and activation
  authorization are owner decisions with fail-closed defaults.

If a later phase reaches a mandatory trigger and neither authoritative source,
an official protocol fixture, nor a safe bounded experiment can discriminate
two or three viable high-impact designs, the `fuji1 remote worker` must emit the
minimal redacted packet defined in
`research/browser-pro-escalation-protocol.md`. Only the `MacBook Air
coordinator` may submit it. Browser unavailability blocks that dependency
closure; no fallback browser/model is permitted. Pro advice cannot approve an
owner gate or replace source, TDD, benchmark, fault, containment, or activation
evidence.
