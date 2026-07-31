# Architecture defect register

## Audit identity and scope

This register closes the source audit for the existing Trellis task
`modular-harness-native-swarm-runtime-refresh`. The audit classifications remain
evidence; the user has since authorized implementation of `0.34.3` only.

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
  task directory. The 19 entries are frozen in
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

The Browser protocol ledger now records the MacBook Air patch-plan,
AgentBundle, distribution, capability-parity, and native-bootstrap
consultations from the same displayed `Pro` session. They are advisory
provenance only; the coordinator/owner corrections and
source/TDD/benchmark/CI gates remain authoritative.

Current-cycle owner disposition:

- `MSR-SEC-001` records an unimplemented earlier target, not an authorized
  defect-remediation phase. No capability broker, escrow, or independent
  `SecurityEpoch` will be built; current `PermissionPlane` remains authority.
- `MSR-TRU-001` is an accepted trust-boundary fact. Explicitly installed
  same-UID code is trusted and malicious-plugin isolation is not promised.
- `MSR-BDL-001` is native-only. There is no old-agent adapter, synthetic
  representation, agent-file execution, or old-source Bundle CLI behavior. Built-ins
  migrate to native AgentBundles and the old parser/discovery/execution branch
  is deleted atomically in `0.34.8`. Build-time embedded immutable built-ins
  avoid both an installer bootstrap cycle and a temporary old-file detector.

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
| `MSR-RCV-001` | P0 | `source-confirmed` | resident recovery | Durable roster/mail state does not reconstruct executable resident actors after restart. | CHAR, TDD, FAULT |
| `MSR-BND-001` | P0 | `source-confirmed` | ToolRegistry / Turn | Schemas and resolution read a mutable registry independently; a Turn attempt pins no whole binding. | CHAR, TDD, FAULT |
| `MSR-SEC-001` | P2 | `target-gap` | permission claims | No monotonic live-security view exists; the owner has explicitly removed that target from the current cycle. | documentation, current PermissionPlane TDD |
| `MSR-FEN-001` | P0 | `target-gap` | actor/effect fencing | Stable operation identity and binding/actor epoch correctness fences are absent. | CHAR, TDD, FAULT |
| `MSR-HAR-001` | P1 | `target-gap` | modular harness | No deterministic cross-lane harness controls provider, process, clock, crash, and store boundaries. | TDD |
| `MSR-CFG-001` | P1 | `source-confirmed` | configuration | Runtime composition directly makes independently discovered managers and planes effective. | CHAR, TDD |
| `MSR-REF-001` | P1 | `target-gap` | runtime refresh | No one immutable generation and atomic activation lifecycle spans tools, MCP, plugins, agents, skills, and instructions. | TDD, FAULT |
| `MSR-MCP-001` | P1 | `source-confirmed` | MCP control planes | Native/deferred MCP and Compat HTTP MCP have separate lifecycle authorities; dynamic HTTP MCP does not update engine tools. | CHAR, TDD, FAULT |
| `MSR-MCP-002` | P1 | `target-gap` | MCP state model | MCP/plugin resources have no common desired/observed/effective activation model. | TDD, FAULT |
| `MSR-PLG-001` | P1 | `source-confirmed` | plugin restart | Plugin respawn discards the new initialize result while retaining initial declarations. | CHAR, TDD, FAULT |
| `MSR-TRU-001` | P2 | `target-gap` | trust boundary | Same-UID child processes are not an untrusted-code containment boundary; trusted-only is the accepted current-cycle scope. | documentation, protocol/crash TDD |
| `MSR-POL-001` | P1 | `source-confirmed` | agent policy | Parsed agent permission/options/request metadata is not fully represented in effective `AgentSpec`. | CHAR, TDD |
| `MSR-POL-002` | P1 | `source-confirmed` | skill policy | Skill `allowed-tools` and `model` are parsed but not enforced in the prompt/tool execution view. | CHAR, TDD |
| `MSR-BDL-001` | P1 | `target-gap` | AgentBundle | No first-class immutable Markdown/JS/Rust bundle or locked dependency graph exists. | TDD, FAULT |
| `MSR-CAP-001` | P1 | `benchmark-unconfirmed` | 100/256 certification | Exact 100-active/156-durable-queue/257-overload behavior has not been certified. | CHAR, BENCH, FAULT |
| `MSR-UPD-001` | P1 | `target-gap` | self-update | Existing installer/release protections do not form an independent verifier, anti-rollback authority, or atomic immutable selector. | TDD, FAULT |
| `MSR-EVT-001` | P2 | `source-inferred` | event delivery | Durable envelopes use store sequence numbers while transient live envelopes use sequence zero; an observable ordering defect is not yet proven. | CHAR, FAULT |
| `MSR-STO-001` | P2 | `benchmark-unconfirmed` | SQLite/replay | Full replay and current append/pool settings may constrain the workload, but no bottleneck is established. | BENCH, FAULT |
| `MSR-DOC-001` | P2 | `source-confirmed` | ADR drift | ADR 0007/0008 specify next-Turn snapshots that current HEAD does not implement. | CHAR, TDD |
| `MSR-REL-001` | P3 | `source-confirmed` | version/release state | Version and changelog are `0.34.2`, while checkout tag refs do not identify a `0.34.2` release. | CHAR, owner gate |
| `MSR-LEG-001` | P3 | `target-gap` | obsolete authority bypasses | Mutable registry and split admission/reconciliation paths need evidence-gated removal in their owning patches; old agent-file code is removed atomically in `0.34.8`, not later. | CHAR, TDD, FAULT |

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
- **`0.34.5` closure status:** local `source-verified` and
  `experimentally-verified`, pending the patch's full local/remote release
  gates. `RuntimeRegistry`/`TurnBinding` now pin prompt skills, schemas,
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
- **`0.34.5` closure status:** partially closed locally, pending release gates.
  Tool/plugin/synchronous-MCP construction now freezes one initial snapshot;
  deferred MCP can only publish through the engine owner; workdir skill
  discovery enters the same snapshot. Agent/AGENTS sources and `0.34.6`
  desired/observed/effective reconciliation remain open.
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
- **`0.34.5` closure status:** the in-process immutable snapshot/publication
  prerequisite is locally `experimentally-verified`, pending release gates.
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

- **Evidence status:** `source-confirmed`.
- **Source and symbols:**
  `crates/hya-server/src/compat/agent_catalog.rs::AgentEntry` carries model,
  category, resident, options, request headers/body, and permissions.
  `crates/hya-server/src/compat/subagent_resolve.rs::resolve_subagent` and
  `crates/hya-server/src/compat/reference.rs::apply_agent_entry` apply the
  supported subset into `crates/hya-core/src/engine.rs::AgentSpec`, whose
  fields are name, model, system prompt, workdir, and reasoning.
- **Trigger:** a user expects parsed permissions/options/request customization
  to constrain a spawned agent.
- **Impact:** accepted configuration can be silently compatibility-only rather
  than an enforced capability/provider request view.
- **Current protection:** model/category/resident/prompt/reasoning behavior is
  applied; reference tests cover parts of variant/reasoning resolution.
- **Missing invariant:** generation compilation either represents and enforces
  each supported field in the immutable binding or rejects/diagnoses it as
  unsupported; no silent policy metadata.
- **Dependency / owner phase:** R3 binding model, R7 policy/bundle enforcement.
- **Required gates:** CHAR maps every parsed field to an effective consumer or
  explicit diagnostic; TDD covers schema filtering, direct dispatch, model
  selection, and provider request shaping for supported fields.

### `MSR-POL-002` — skill policy metadata is parsed but not enforced

- **Evidence status:** `source-confirmed`.
- **Source and symbols:**
  `crates/hya-tool/src/skill_catalog.rs::{ParsedSkill,parse_skill}` parses and
  retains `allowed_tools` and `model`;
  `crates/hya-tool/src/skill_catalog.rs::skills_section` emits only names and
  descriptions into
  `crates/hya-core/src/engine.rs::effective_agent_for_projection`. Current
  prompt/tool dispatch does not consume the retained policy fields as an
  effective restriction.
- **Trigger:** an activated skill declares a model restriction or tool
  allowlist.
- **Impact:** the model can see or directly invoke authority outside the
  declared skill policy.
- **Current protection:** malformed/disabled skills are skipped; discovery is
  deterministic by sorted paths and first-name wins; tool permission checks
  still apply globally.
- **Missing invariant:** supported bundle/agent/skill views are narrowing
  inputs to Harness config/current `PermissionPlane`, enforced consistently in
  provider-visible schemas and direct invocation.
- **Dependency / owner phase:** `0.34.5` structural binding, `0.34.8` native
  built-in cutover, and `0.34.10`/`0.34.11` external execution.
- **Required gates:** CHAR proves current parse/use distinction; TDD hides and
  rejects a forbidden tool/model, including direct invocation that bypasses
  model-visible schema filtering.

### `MSR-BDL-001` — no first-class Markdown/JS/Rust `AgentBundle`

- **Evidence status:** `target-gap`.
- **Source and symbols:** `crates/hya-core/src/engine.rs::AgentSpec` has no
  bundle identity, dependency graph, artifact identity, capability ceiling, or
  trust state. Existing plugin mechanisms in
  `crates/hya-plugin::{host,client}` are process extensions, not per-agent
  bundle manifests.
- **Trigger:** an agent definition depends on Markdown prompt/data plus a JS or
  Rust executable artifact.
- **Impact:** dependency integrity, provenance, activation order, capability
  intersection, and rollback cannot be bound to the agent as one immutable
  unit.
- **Current protection:** Markdown agent/skill discovery and out-of-process
  plugin protocols provide reusable adapters; Rust extensions are already kept
  out of process by ADR 0009.
- **Missing invariant:** a flat
  `identity/extensions/resources/agents[]` manifest and read-only catalog use
  stable namespaces, visibility `role`, native `spawn_lifecycle`, fail-closed resolution,
  `none|basic|full` narrowing views, qualified hooks, default-deny
  `can_spawn`, and current `PermissionPlane` as the sole permission authority.
- **Dependency / owner phase:** `0.34.8` owns the atomic built-in cutover,
  `0.34.9` owns distribution/registry, `0.34.10` owns owner-gated external
  main/transient execution, and `0.34.11` owns resident integration.
- **Required gates:** build-time preparation/catalog/resolver TDD covers
  reproducible embedded bytes/index, deterministic identity,
  main/subagent visibility and transient/resident spawn-lifecycle rules,
  built-in native Bundle cutover, deletion of old agent-file loaders,
  alias/local/global
  resolution, collision/ambiguity, resource views, permission narrowing,
  boot-without-install, and stable-ID/event/replay fixtures. Future external
  execution TDD/FAULT covers native
  spawn/send/wait, admission/OperationId, crash/cancel/restart, runnable
  main+transient+resident example, and the authoring skill.

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
