# Architecture audit closure and next-step roadmap

## 2026-07-31 controlling `0.34.4` execution ruling

Release `0.34.3` is committed at
`b8c21deeb5004e1f703b199a40de196902fadf35`, pushed to the existing branch and
draft PR #24, and its remote CI run is green. Release `0.34.4` is now the only
active implementation stage.

- Derive a strong internal `OperationId` by fixed-domain UUIDv5 from the
  already-persisted UUID-backed `ToolCallId`; no independent random ID and no
  HTTP/Event/CLI exposure.
- Add one narrow additive `SessionStore` admission journal. Its immutable
  request identity is claimed before governor debit or child/effect creation.
- Use only `accepted -> started -> completed|cancelled|aborted`. Terminals are
  irreversible; identical terminal replay is idempotent and conflicting
  terminalization fails typed-closed.
- Same operation and request returns its existing state without another debit
  or dispatch. A changed fingerprint/units/source returns typed
  `OPERATION_ID_CONFLICT` without mutation.
- Overload terminalizes an accepted claim without releasing capacity. A
  started/debited operation releases exactly once through the common
  completion/cancel/create-failure/root-cleanup finalizer.
- Startup atomically aborts every nonterminal admission before any spawn
  supervisor becomes ready. It never resumes, retries, dispatches, creates a
  child, emits an admission event, or credits the fresh governor.
- Do not add `operation_child`, member/effect/queue/scheduler journals, a
  create-with-ID seam, Bundle work, generation/binding, reconciliation,
  resident epoch/fencing, or 100/256 certification.
- `0.34.4` must update exact release metadata, pass all focused/workspace/build
  gates, produce one atomic commit on the existing branch/PR, and reach green
  remote CI before `0.34.5` preparation.

The detailed main-agent merge is appended to
`research/parallel-planning-synthesis.md`; consultation provenance is
`CONSULT-2026-07-31-OPERATION-ADMISSION-06`.

## 2026-07-31 controlling native-only supersession

This block is authoritative over every older release table and phase mapping
below.

- `0.34.3` delivered pre-create background transient/resident admission,
  bounded spawn transport, and typed overload. `0.34.4` is active under the
  preceding ruling.
- The owner has dropped all old agent-file support. There will be no adapter,
  synthetic representation, old-source bundle list/info, or parser/discovery/
  execution fallback.
- All built-in agents must migrate to native AgentBundles. The old agent
  parser/discovery/execution branch is deleted atomically with that cutover.
- `research/agent-capability-parity-matrix.md` proves native Bundle capability
  coverage and built-in migration; it is not a compatibility adapter plan.
- Historical event/session agent IDs must remain replayable. That data-protocol
  promise does not make an old agent file executable.
- Bundle `role: main|subagent` controls visibility and
  `spawn_lifecycle: transient|resident` controls native-spawn behavior; Harness
  owns the root Session lifecycle.
- Pro round six was returned on 2026-07-31. The MacBook Air coordinator adopts
  native-Bundle-only, build-time prepared immutable built-ins, stable IDs, and
  replay fixtures, while rejecting Pro's premature `0.34.4` cutover and
  temporary old-file detector.
- Built-in sources live under `bundles/builtin/<bundle-id>/`; one deterministic
  preparer emits embedded authoritative package bytes plus a digest-bound
  read-only index. Startup never invokes `hya bundle install`.
- All prepared sources flow through
  `AgentBundleIR -> immutable Generation/catalog -> TurnBinding -> AgentSpec ->
  SessionEngine`. TUI and spawn consume the same generation snapshot.
- The final patch map below is controlling planning. Only `0.34.4` is active;
  every arrow requires the preceding patch's commit/push and full remote CI
  green.

The native-only future plan does not delay or broaden `0.34.4`.

## Status

This roadmap is executable planning for the existing Trellis task
`modular-harness-native-swarm-runtime-refresh`. The user has authorized
`0.34.4` after the committed/pushed/remote-green `0.34.3` gate. Every later
patch remains planning-only and cannot begin merely because it appears here.

The roadmap is anchored to:

- audited `main` HEAD
  `267bfc3c6c66e46fe8514e2e70657489f853b7f0`;
- current fetched `origin/main` and isolated implementation base
  `156d0ad3c50aea67dfac0054485eb6991e77308b`; the only intervening change is
  the README icon reference, so no audited product-source finding changed;
- workspace/root changelog version `0.34.2` at the audit, delivered isolated
  release version `0.34.3`, and active target version `0.34.4`;
- 19 protected pre-existing user-owned dirty entries plus this task directory.

The authoritative evidence and detailed defects are:

- `research/head-architecture-audit.md`;
- `research/defect-register.md`;
- `research/parallel-planning-synthesis.md`;
- `research/fuji1-sync-preflight.md`;
- `research/browser-pro-escalation-protocol.md`.

The protocol ledger records the consultation/ruling chain through
`CONSULT-2026-07-31-OPERATION-ADMISSION-06`. Its Pro output is advisory only.
The MacBook Air rulings control plan placement; current source inspection
independently verifies only the claims explicitly marked source-confirmed.

## Main-agent synthesis

Four read-only planning views were run with the same updated evidence packet:
rapid-roadmap, balanced/reversible delivery, risk/fail-closed review, and
frontier/deep-module consolidation. The main-agent merge makes these decisions:

1. `0.34.3` is the tiny admission-before-allocation tracer, not the full
   100/256 scheduler and not an end-to-end harness rewrite.
2. Current `SubagentGovernor::reserve` is a per-root budget counter, not a
   durable lease. This patch moves that existing decision before child creation
   without inventing the cancel/finalize/recovery machinery reserved for
   `0.34.4`.
3. Background transient and resident work share the same pre-create decision;
   the current depth-greater-than-zero provider-stream semaphore remains a
   separate limit.
4. `SpawnerPlane` becomes a bounded in-memory transport with non-blocking,
   typed overload. Its explicit capacity is derived from existing configured
   subagent limits and is not a new `100`, `128`, or `256` default.
5. No sandbox, seccomp/container research, capability broker, escrow
   delegation, or independent `SecurityEpoch` is in the current patch
   sequence. Explicitly installed same-UID JS/Rust bundle and plugin code is
   trusted; the harness does not isolate malicious plugins.
6. The existing `PermissionPlane`/dispatch remains the final authorization
   path. Future plugin policy propagation reuses it; admission is
   correctness/resource control, not another permission framework.
7. AgentBundle is deferred beyond `0.34.4`. The third-round ruling fixes the
   flat ABI-neutral manifest/catalog, namespace, routing, resource-view, and
   PermissionPlane boundary. Execution option A/B/C, context transfer, and
   resident idle/turn semantics remain deliberately owner-unselected.
8. SQLite and full replay remain authoritative until the `0.34.12` frozen
   benchmark fails. Storage optimization is conditional.
9. `0.34.8` is the one atomic native built-in cutover and old agent-file code
   removal. There is no adapter, conversion/migration, temporary detector, or
   later format-deletion release.
10. Package distribution, external transient execution, resident execution,
    certification, and updater delivery remain separate patches so no release
    silently locks an owner-gated ABI or widens rollback scope.

## Target authority flow

```text
source/config/process observations
  -> immutable ConfigGeneration candidate
  -> desired / observed / validated / effective reconciliation
  -> immutable BindingSet + activation_epoch
  -> TurnAttempt pins one BindingSet
  -> existing PermissionPlane/dispatch authorization
  -> durable admission and cancellation/recovery records
  -> actor_epoch + stable operation_id
  -> correctness EffectGate at the linearization point
  -> durable outcome / explicit indeterminate remote outcome

independent signed release metadata
  -> updater verifier outside runtime/plugin/MCP/bundle authority
  -> immutable staged release
  -> crash journal + atomic selector
  -> accepted release floor / higher-sequence recovery release
```

No adapter in this flow may become an additional effective authority.

Agent sources use this single subflow:

```text
repo-native built-in sources + installed package artifacts
  -> deterministic package preparer
  -> immutable prepared package bytes + digest-bound index
  -> AgentBundleIR
  -> immutable Generation/catalog snapshot
  -> TurnBinding
  -> AgentSpec execution projection
  -> SessionEngine
```

## Owner-corrected patch dependency graph

```text
0.34.3 pre-create admission + bounded transport + typed overload
  -> remote CI green
    -> 0.34.4 OperationId + minimal durable admission/cancel/finalize/recovery
      -> remote CI green
        -> 0.34.5 immutable generation + TurnBinding + atomic registry refresh
          -> remote CI green
            -> 0.34.6 MCP/plugin desired-observed-effective reconciliation
              + respawn declarations + generation binding
              + existing PermissionPlane propagation
              + generic stable-ID/namespace seams only
              -> remote CI green
                -> 0.34.7 resident recovery + actor lease/epoch
                  + minimal correctness effect fencing
                  -> remote CI green
                    -> 0.34.8 atomic native built-in Bundle cutover
                      + capability/replay fixtures + one catalog/runtime path
                      + old agent-file code removed in the same release
                      -> remote CI green
                        -> 0.34.9 .hyabundle distribution + four-command CLI
                          + registry/atomic activation + public/private inspect
                          -> remote CI green
                            -> 0.34.10 owner-gated shared stdio runner
                              + external main/transient Bundles + examples/skill
                              -> remote CI green
                                -> 0.34.11 resident Bundle integration
                                  + Hybrid send/wait/recovery/fencing
                                  -> remote CI green
                                    -> 0.34.12 100/256 certification
                                      + benchmark-triggered optimization only
                                      -> remote CI green
                                        -> 0.34.13 independent updater
                                          + verifier/activator/rollback
                                          + self-update example/skill
```

| Patch | Primary output | Hard exclusions / entry condition |
| --- | --- | --- |
| `0.34.3` | Existing-governor pre-create admission for background transient/resident, explicit bounded spawn transport, typed fail-fast overload | Delivered at `b8c21dee` with remote CI green; no durability, permit/lease, OperationId, 100/256 default, bundle, refresh, updater, or deletion |
| `0.34.4` | `OperationId` and minimal durable admission/cancel/finalize/recovery | Requires `0.34.3` remote CI green |
| `0.34.5` | Immutable config generation, `TurnBinding`, source-owned atomic registry refresh | Requires durable identity/recovery seam |
| `0.34.6` | MCP/plugin desired-observed-effective state, incarnation declarations, generation binding, current `PermissionPlane` propagation, and generic stable-ID/namespace seams | No Bundle loader, manifest, catalog, ABI, TUI selection, spawn, sandbox, or new permission framework |
| `0.34.7` | Resident durable recovery, actor lease/epoch, minimal effect fencing | Correctness/fault scope; no independent `SecurityEpoch` |
| `0.34.8` | Capability matrix/fixtures, minimal Bundle IR/catalog/namespace/resource view, deterministic embedded built-ins, startup/TUI/spawn cutover, all old agent-file code removed, simple repo-native Markdown example and authoring skill | One atomic native built-in cutover; boots without install; preserves stable IDs/event replay; no adapter/migration/detector |
| `0.34.9` | `.hyabundle` public 7z/private envelope inspection, four-command CLI, authoritative registry, single-active version, atomic activation, built-in list/info/immutable semantics | No external Bundle execution or private decrypt/activation |
| `0.34.10` | Existing JSON-RPC/stdio transport extended for owner-selected external main/transient execution; runnable Markdown/JS/Rust examples and authoring skill update | Private key and final external execution/context gates must be recorded; Harness retains spawn/send/wait |
| `0.34.11` | Resident Bundle integration through the same actor/mailbox/admission/event/fencing runtime | Requires owner-selected resident idle/turn semantics and `0.34.10` remote CI green |
| `0.34.12` | 100/256 workload, capacity, and fault certification | SQLite/other changes only for one predeclared failed benchmark, one minimal optimization at a time |
| `0.34.13` | Independent updater/verifier/activator, stage/build/verify/activate/rollback, self-update skill/example | Owner activation/key/TCB gates remain; never reintroduce old agent formats |

The older `R0`–`R12` sections below remain audit-navigation detail only. Where
they conflict with this patch table or current owner boundary, this table
controls and the older text must not be executed.

## Non-skippable dependency rules

1. **Characterization before correction claims.** A `source-inferred` or
   `benchmark-unconfirmed` entry cannot become a confirmed defect without its
   named deterministic gate.
2. **IDs/journals before recovery.** Durable admission, operation
   deduplication, actor reconstruction, and forward-only rollback cannot be
   built on process-local names.
3. **Generation before binding; binding before permission/resource views.**
   Existing PermissionPlane dispatch and narrowing views cannot resolve a
   structure that was never identified and pinned.
4. **Security plus admission before recovered execution.** A reconstructed
   actor must not wake until it has a valid lease and effect fence.
5. **No executable broadening before policy/containment.** Markdown data may be
   effective earlier; JS/Rust execution waits for dependency, capability, and
   trust gates.
6. **No adapter-owned activation.** MCP/plugin work consumes the common
   generation/binding protocol; it cannot add another mutable registry.
7. **No storage redesign before failure evidence.** A passing benchmark closes
   R9 with “no change.”
8. **No capacity claim before integrated recovery.** A one-process throughput
   run is not 100/256 certification.
9. **No updater activation on signatures alone.** Independence, anti-rollback,
   crash recovery, key custody, and owner authorization are all required.
10. **One native agent cutover.** Freeze capability/replay fixtures before
    `0.34.8`, then activate embedded built-ins and remove all old agent-file
    production paths in the same patch. Rollback may select only retained
    verified native generations, never an old parser or adapter.

## R0 — immediate containment and audit closure

**Purpose:** prevent planning gaps from being advertised or activated while
preserving the current checkout exactly.

**Entry gate**

- frozen HEAD/branch/version/dirty inventory is recorded;
- the audit record proves the task was the one existing `planning` task with
  current source `none`; the active isolated-worktree record now proves the
  same task is `in_progress` with the canonical Codex session source;
- no Browser/Pro consultation is attributed.

**Actions**

- adopt `research/defect-register.md` IDs and evidence vocabulary;
- explicitly withhold certification for 100/256, atomic whole-Turn refresh,
  resident recovery, executable AgentBundle behavior, effect fencing, and
  built-in self-update;
- label same-UID executable code as trusted operator code, not sandboxed;
- forbid automatic retry of an indeterminate non-idempotent remote effect;
- keep declaration additions next-Turn-only in the target contract and fail
  closed on uncertain security/admission/update state;
- preserve all 19 user-owned changes; all future source work remains on the
  `fuji1 remote worker`;
- retain the MacBook Air coordination and one-way, non-deleting mirror boundary
  without installing or starting a synchronizer.

**Exit gate**

- every audit claim is implementation, ADR/document, inference,
  benchmark-needed, or target—not a mixture;
- every P0 entry has a future deterministic closure gate;
- no product/source/build/test/release action occurred.

**Parallel work:** none required; documentation-only closure is reversible.

**Stop/rollback:** revert only task-owned documentation if the evidence anchor
is found wrong; never touch the protected baseline.

## R1 — P0 characterization and deterministic harness

**Purpose:** reproduce current behavior and freeze decision criteria before
authority changes.

**Entry gate**

- R0 complete;
- relevant Trellis backend/spec guides loaded;
- current source and nearest tests re-queried at the then-current HEAD.

**Actions**

- add deterministic fake-provider, effect, process, MCP/plugin declaration,
  clock/barrier, crash-point, and real temporary-SQLite fixtures only as needed
  by one RED test at a time;
- enumerate all resident/transient and foreground/background spawn entrypoints;
- characterize child-session allocation relative to governor reservation;
- pause between schema capture, refresh, authorization, resolution, and effect
  boundaries;
- characterize resident startup, mail cursor, broadcast lag, and sequence-zero
  live notifications;
- freeze workload descriptions and owner-ratified correctness/resource/SLO
  thresholds before interpreting measurements;
- characterize updater interruption points without changing updater behavior.

**Exit gate**

- all P0 current behavior is reproducible without correctness depending on
  wall-clock sleeps;
- every `source-inferred` risk has a named deterministic fault test;
- every `benchmark-unconfirmed` claim has a workload, metric, environment
  record, and predeclared threshold;
- Turn vs Turn-attempt/retry, work-item counting, queue/fairness, and remote
  effect outcome terms are unambiguous or remain explicitly owner-blocked.

**Parallel work**

- storage benchmark fixture and updater signing/crash fixtures may be prepared
  independently;
- no performance optimization or updater activation follows from fixture work.

**Forbidden skip:** do not begin a storage redesign, claim stale effects, or
choose a capacity interpretation from intuition.

## First minimal TDD tracer slice

This is the first future production-facing slice after the R1 harness exists;
it is not executed by this audit.

**Invariant:** an admission denial happens before every downstream allocation.

**Atomic slice A — background transient path**

1. Add one RED integration test, likely under `crates/hya-app/tests`, with a
   deterministic deny-all/zero-capacity admission fixture.
2. Submit one background transient member without an existing task/session ID.
3. Expect one typed overload/admission result.
4. Assert that the rejected member produced:
   - no `SessionCreated` event;
   - no request-owned Tokio worker;
   - no provider request;
   - no resident/roster entry;
   - no tool/effect intent.
5. Confirm RED fails because current
   `spawn_team_supervisor` creates the child session before `run_team` reserve,
   not because the fixture is broken.
6. Implement only the smallest admission-before-`SessionEngine::create` seam
   needed to pass this case; do not add queueing, 100/256 constants, or a broad
   scheduler rewrite in the same slice.
7. Run focused tests and the required touched-area/full gates before any future
   commit.

**Atomic slice B — resident parity, immediately next**

- reuse the exact same admission contract for one resident request;
- prove denial occurs before `ensure_main`, `spawn_resident`, team-slot
  registration, or resident task creation;
- do not introduce a resident-specific admission rule.

After both cases pass, expand the test-only capacity to `1 active / 0 queued`
and exercise each mode in both directions. The full 100/156/257 proof remains
R10.

## R2 — stable IDs, epochs, and additive control journals

**Purpose:** establish durable vocabulary before transferring authority.

**Entry gate**

- R1 invariant definitions and compatibility matrix are frozen;
- owner has identified the Turn-attempt/retry and remote-effect boundaries;
- any wire/event migration strategy is explicitly additive or blocked.

**Actions**

- add stable logical/source/declaration/artifact/bundle/binding/Turn-attempt/
  admission/operation/actor IDs in dependency-light domain types;
- add monotonic, explicitly scoped activation, security, and actor epochs;
- add additive durable activation, admission/lease, actor/cursor, and
  operation-intent/outcome journals;
- allocate an epoch and its guarded transition atomically;
- preserve old event replay; prefer additive tables/records until old-reader
  behavior is proven.

**Exit gate**

- stable IDs serialize/replay and are collision-tested;
- equal/lower epoch transitions reject;
- crash at every journal boundary recovers deterministically;
- old logs replay to identical projections;
- retry/recovery of one semantic local operation reuses one `OperationId`;
- selecting old content creates a newer activation epoch, never an epoch
  decrement.

**Parallel work**

- admission journal and generation identity implementations may split after the
  shared ID contracts pass;
- updater metadata/journal format work may begin after its owner decisions;
- no lane becomes effective yet.

**Rollback seam:** stop new shadow writes while retaining additive records.
Never down-migrate or erase durable evidence to roll back.

## R3 — immutable generation and Turn-pinned binding

**Purpose:** make structural state deterministic and atomically visible.

**Entry gate**

- R2 IDs/epochs/journals pass replay and compatibility gates;
- config source precedence, collision ownership, and secret-reference rules are
  decided or fail closed.

**Actions**

- compile canonical source/config/agent/skill/instruction/tool/MCP/plugin/bundle
  declarations into immutable candidates in shadow mode;
- reject missing dependencies, cycles, hash/lock mismatch, alias/tool
  collisions, partial protocol observations, and unexpected capability
  broadening as a whole candidate;
- reconcile desired and observed state into a validated effective
  `BindingSet`;
- persist one binding/activation identity at Turn-attempt admission;
- use that binding for every provider schema, alias, declaration, model/skill
  view, resolver, and executor across all rounds;
- implement lifecycle
  `prepare -> verify -> activate -> quiesce -> drain -> retire`.

**Exit gate**

- identical canonical inputs produce identical content identities;
- invalid/partial candidates leave the effective generation unchanged;
- refresh between schema exposure and dispatch cannot change the current
  attempt;
- the next admitted attempt sees the new generation;
- old pinned process bindings fail typed-unavailable rather than silently
  dispatching to incompatible new declarations;
- crash recovery selects one complete activation.

**Parallel work**

- the flat ABI-neutral AgentBundle parser/read-only catalog and namespace
  resolver may begin in shadow, with no TUI exposure or spawn;
- MCP and plugin adapters may implement observation against the frozen
  candidate interface;
- R5 admission implementation may proceed against stable IDs, but integrated
  activation waits for R4.

**Rollback seam:** retain immutable prior content and reactivate it only under a
new higher epoch. Never restore per-name mutation or an old agent-file
authority.

## R4 — current PermissionPlane propagation and correctness effect gate

**Purpose:** combine immutable Turn structure with the existing permission
authority and actor/effect correctness fencing.

**Entry gate**

- R3 whole-attempt binding is authoritative;
- every effect class and linearization point is enumerated.

**Actions**

- keep Harness config/current `PermissionPlane` as the sole ask/allow/deny
  authority;
- intersect bundle/agent/skill views only as narrowing input to Harness policy;
- reuse current protocol validation, interaction, logging, and fail-closed
  dispatch errors;
- at each effect linearization point revalidate actor epoch,
  binding/declaration, operation ID, current lease/admission state, and the
  existing permission result required by dispatch;
- do not add a capability broker, escrow, independent `SecurityEpoch`, sandbox,
  or second permission framework.

**Exit gate**

- bundle/plugin declarations and overlays never broaden Harness policy;
- direct invocation cannot bypass schema filtering or existing permission
  checks;
- unavailable permission/binding/lease state fails closed;
- recovery/retry preserves operation identity;
- unsupported remote outcomes become explicit `indeterminate`/reconcile state.

**Parallel work:** R5 admission internals may proceed, but resident wake/recovery
does not activate before R4 and R5 are both effective.

**Rollback seam:** current `PermissionPlane` remains authoritative throughout;
bundle/plugin policy can be removed without replacing it.

## R5 — durable unified multi-resource admission

**Purpose:** make all work bounded, durable, fair, and admitted before
allocation.

**Entry gate**

- R1 capacity semantics and tiny tracer are accepted;
- R2 durable ticket/lease identity exists;
- work items can reference an R3 binding identity;
- owner has selected root accounting, fairness, cancellation/promotion, and
  multi-member partial/atomic semantics.

**Actions**

- route foreground/background and transient/resident members through one
  process-wide admission state machine before child session, Tokio task, actor,
  provider, tool, process, storage lease, or effect allocation;
- persist acceptance before returning it;
- model bounded states such as
  `queued -> active -> suspended/waiting -> queued -> terminal`;
- grant multi-resource vectors transactionally with no hold-and-wait;
- allow parents waiting on children to release active execution capacity
  without losing durable ownership;
- keep the depth-greater-than-zero provider semaphore, per-team Turn/message
  budgets, root/control/recovery reserves, and storage/process limits as
  distinct resource dimensions.

**Exit gate**

- tiny-cap tests pass for all four lifecycle modes;
- no queued item owns a Tokio task;
- cancellation promotes exactly one eligible item transactionally;
- restart restores exact accepted/queued/active/lease counts;
- item/batch outcomes are deterministic and typed;
- no resident or alternate path bypasses the controller.

**Parallel work:** R7 manifest/policy and R8 adapter work may continue against
R3/R4 interfaces. Capacity certification waits for R6–R9.

**Rollback seam:** begin in decision-shadow mode. After enforcement, stop new
admission and drain/recover to zero queued work and zero active leases before
selecting retained verified content.

## R6 — resident reconstruction, actor epochs, and effect fencing

**Purpose:** make restart recovery safe rather than merely replayable.

**Entry gate**

- R4 EffectGate is effective;
- R5 leases/queue are authoritative;
- actor/cursor/operation journals from R2 are replayable.

**Actions**

- reconstruct resident identities, queued/suspended work, mail ranges/cursors,
  acknowledgements, dependencies, and leases before new mail consumption;
- durably advance the actor epoch before starting a recovered incarnation;
- attach actor epoch and stable operation ID to claims, mail/cursor transitions,
  results, and effects;
- treat event-bus/broadcast delivery as a wake optimization, not recovery
  authority;
- reconcile indeterminate remote effects instead of automatic exactly-once
  claims.

**Exit gate**

- crash at registration-before-spawn, active Turn, suspension, cursor,
  operation intent, local effect, result, and promotion boundaries yields one
  effective actor incarnation;
- an older actor epoch cannot claim work, send/ack mail, publish terminal state,
  or linearize an effect;
- local operation deduplication is conclusive by `OperationId`;
- unsupported remote outcomes remain visible and do not auto-retry.

**Parallel work:** R7 and R8 can finish independently; R10 waits for all.

**Rollback seam:** rehydration may be disabled only from a quiescent state after
fencing all actors and reconciling every lease/outcome.

## R7 — Markdown/JS/Rust `AgentBundle` and policy enforcement

**Status:** native-only staged delivery in `0.34.8`–`0.34.11`. Pro rounds
three through six are advisory provenance; the MacBook Air coordinator's
native-only sequencing controls.

**Owner-established boundary**

- AgentBundle is an extension/catalog attached to the Harness, not an
  independently callable tool.
- One bundle may export one or more agents.
- A main agent may be selected in the hya TUI. Bundle subagents are not
  directly TUI-selectable and are reached only through a main-agent spawn.
- The flat top level is `identity`, `extensions`, `resources`, and `agents[]`.
  `identity` holds namespaced ID/version/publisher; `extensions` holds JS and
  Rust-sidecar references; `resources` defines tools/skills/MCP/hooks.
- Each agent entry contains `id`, `role: main|subagent`,
  an explicit stable agent ID, `spawn_lifecycle: transient|resident`, prompt,
  model policy,
  `resource_view`, a declarative/narrowing `permission_overlay`,
  `resource_profile`, and optional `can_spawn`/hooks.
- `role` controls TUI visibility only; `spawn_lifecycle` controls behavior only
  when Harness native spawn invokes that definition. Harness owns root Session
  lifetime. Bundles define resources and agents only reference them; no
  inheritance or nested overlay is allowed.
- Bundle-local tool/skill/MCP identifiers are namespaced; an unqualified short
  name resolves bundle-local before global.
- Harness access uses the owner vocabulary `none | basic | full`.
- A bundle does not enforce permissions. Harness configuration and the
  existing `PermissionPlane` make the final ask/allow/deny decision.
- Explicitly installed JS/Rust code is trusted same-UID extension code. This
  project does not claim to isolate a malicious plugin or bundle.

**Namespace and resolution**

- Harness stable IDs are `harness:<kind>/<id>`.
- Bundle stable IDs are
  `bundle:<bundle-id>/<kind>/<id>`; within one bundle,
  `bundle:<kind>/<id>` is expanded with the current bundle ID.
- Version is pinned by the active binding/generation, never embedded in the
  logical short ID.
- Resolution order is: explicit qualified ID; per-agent alias (qualified
  target only and cannot occupy a bundle-local short name); bundle-local short
  name; unique harness short name.
- Missing, ambiguous, conflicting, or invalid resolution fails closed. The
  rule is identical for tool/skill/MCP; hooks require explicit bundle-qualified
  references.

**Resource views**

- `none`: bundle-local tool/skill/MCP only.
- `basic`: bundle-local plus the Harness-defined builtin basic set.
- `full`: bundle-local plus the complete tool/skill/MCP set in the current
  active binding.
- The effective view is the narrowing intersection of requested
  view/allow-deny declarations and Harness policy. Bundle input never expands
  Harness policy.

**Owner-blocked external execution decisions**

- A: transient-only `spawn(agent_id,input) -> handle; wait(handle)`;
- B: resident-only `spawn; send; wait`, dependent on mailbox/recovery;
- C: recommended hybrid with one `spawn`, catalog lifecycle selecting
  transient/resident, resident-only `send`, and one handle/admission/event path;
- context transfer: input-only, input+summary, or full context;
- resident idle/turn lifecycle.

**Native built-in cutover — `0.34.8`**

- Entry requires `0.34.7` remote CI green, a complete source-backed capability
  matrix, and frozen current behavior/replay fixtures.
- A deterministic package preparer validates repo-native built-ins, emits
  authoritative embedded package bytes plus a digest-bound index, and lowers
  them through one `AgentBundleIR -> immutable Generation/catalog ->
  TurnBinding -> AgentSpec -> SessionEngine` path.
- Startup merges `origin=builtin, immutable=true` packages with the installed
  registry, rejects built-in bundle/stable-agent-ID collisions, and gives TUI
  and spawn the same snapshot.
- Built-ins retain every current public stable agent ID as explicit manifest
  data. Event/session IDs are not rewritten.
- Startup/TUI/spawn switch and deletion of every old agent-file
  loader/parser/discovery/execution branch happen in this same release. There
  is no adapter, migration, conversion, scanner, or rollback fallback.
- Exit requires all A rows mapped/tested, all B/C rows implemented or typed
  rejected, all built-ins native, unknown fields fail-closed, historical replay
  green without old loaders, and boot without external install.
- Deliver a simple repo-native Markdown Bundle example, authoring docs, and the
  built-in `agent-bundle-authoring` skill.

**Distribution — `0.34.9`**

- Add only the four approved CLI commands, magic/version format detection,
  safe public 7z staging, private envelope metadata/auth inspection,
  authoritative single-active registry generations, atomic activation, and
  built-in list/info/immutable behavior.
- Do not execute external packages or persist private plaintext. Private
  decrypt/activation stays owner-blocked.

**External execution — `0.34.10` and `0.34.11`**

- `0.34.10` reuses/extends the existing plugin JSON-RPC/stdio transport for
  owner-selected external main/transient execution. Harness retains
  spawn/send/wait, admission, cancellation, lifecycle, events, replay, and the
  existing `PermissionPlane`.
- `0.34.11` adds resident execution through the same Harness actor/mailbox/
  recovery/fencing path and owner-selected idle/turn semantics.
- Runnable Markdown/JS/Rust examples use a main/full lead, transient/basic
  fact-checker, resident/none monitor, bundle-local JS tool, Markdown skill,
  MCP config, Rust hook, and the one Harness runtime.
- Documentation must continue to say that malicious same-UID extension code is
  not isolated.

**Parallel work:** R8 MCP and plugin adapters can proceed independently after
their shared binding contract; `0.34.6` must not prebuild a Bundle loader or
ABI. The updater remains a separate TCB and may not consume bundle code.

**Rollback seam:** activate only a retained verified native generation at a
higher epoch. Never restore removed agent-file loaders or create a second
catalog/runtime authority.

## R8 — MCP/plugin desired-observed-effective reconciliation

**Purpose:** eliminate split activation authorities and stale process
declarations.

**Entry gate**

- R3 candidate/binding protocol and R4 security broadening/removal rules pass;
- official MCP/plugin protocol constraints and conformance fixtures are known;
- executable trust policy is explicit.

**Actions**

- feed static, deferred, and Compat dynamic MCP into one desired state;
- treat server/process declarations as untrusted observed state;
- re-run plugin initialize/protocol/declaration processing for every
  incarnation;
- validate collisions, compatibility, permissions/capability broadening,
  resource identity, and health before effective activation;
- atomically publish a complete generation or retain the previous effective
  generation;
- make removals/revocations fail closed and additions next-Turn-only.

**Exit gate**

- Compat dynamic MCP status and engine callability converge through one
  reconciler;
- failed/partial MCP observation never becomes effective;
- plugin restart removal/change/broadening is processed as a new candidate;
- old pinned Turns return typed-unavailable if their process generation dies;
- reconnect, timeout, crash, collision, protocol mismatch, removal, and
  broadening fault tests pass.

**Parallel work:** MCP and plugin adapters can be implemented in parallel once
the common observation/candidate interfaces are frozen.

**Rollback seam:** retain the last validated effective immutable generation;
never restore a stale process declaration into a new incarnation by name.

## R9 — conditional storage optimization

**Purpose:** change storage only when measured evidence shows it blocks the
contract.

**Entry gate**

- R1 workloads/thresholds are frozen;
- representative R5 admission and R6 recovery traffic exists;
- full replay remains the correctness oracle.

**Decision**

```text
benchmark passes
  -> record measurements and "no storage change"

benchmark fails a predeclared threshold
  -> identify the single measured path
  -> add the smallest reversible index/checkpoint/batch/tuning slice
  -> prove replay/crash equivalence
  -> rerun the same benchmark and R10 matrix
```

**Required workload**

- 1k/10k/100k-event histories;
- concurrent append/read and pool saturation;
- 100 active / 156 queued recovery;
- resident mail/cursor load and broadcast lag;
- refresh/security/actor fault churn;
- CPU, RSS, disk growth, p50/p95/p99 latency, queue age, and recovery time.

**Exit gate**

- measured results and environment are retained;
- a passing path has no speculative storage change;
- any failing path meets the original threshold after a replay-equivalent,
  crash-safe change;
- the append-only event log and shared reducer remain authoritative.

**Parallel work:** benchmark preparation can start at R1; optimization cannot
start until this decision point.

**Rollback seam:** disable/drop only derived acceleration state; rebuild it from
the event log. Never make a checkpoint an independent truth.

## R10 — `0.34.12` integrated 100/256 workload, capacity, and fault certification

**Purpose:** certify the combined system, not isolated throughput.

**Entry gate**

- R3 binding, R4 security, R5 admission, R6 recovery, R7 policy/bundle, R8
  reconciliation, and the R9 decision are complete;
- exact owner-approved definitions for “active,” root/control reserve,
  fairness, and overload are frozen;
- no unresolved escalation or owner gate affects the matrix.

**Required matrix**

| Dimension | Required cases |
| --- | --- |
| capacity | exactly 100 active, 156 durably non-active, item 257 typed-overloaded |
| lifecycle | foreground/background × resident/transient × root/nested |
| allocation | no session/task/actor/provider/tool/effect before admission |
| resources | provider streams, root/control/recovery reserve, processes, tools, storage, memory |
| fairness | owner-selected policy, cancellation, promotion, nested-parent progress, no starvation/hold-and-wait |
| binding | refresh during every provider round and dispatch boundary |
| security | revoke/grant races and unavailable authority state |
| recovery | kill at every admission, actor, cursor, operation, binding, and process transition |
| processes | MCP/plugin disconnect/reconnect/declaration drift |
| bundles | Markdown-only, JS, Rust, missing/revoked/tampered dependency |
| storage | long history, contention, lag, bounded growth, restart recovery |
| duration | burst, steady state, sustained soak, repeated crash/restart |

**Exit gate**

- active count never exceeds the frozen 100 definition;
- durable non-active count reaches exactly 156;
- item 257 returns the typed result atomically and has zero downstream
  allocation;
- the 128 provider-stream limit remains separately enforced/observable;
- restart reconstructs the exact accepted state and safely resumes/drains;
- no stale actor, binding, grant, declaration, or operation crosses its fence;
- measurements meet frozen correctness/resource/SLO thresholds.

**Parallel work:** no certification lane may omit R7/R8 because policy/process
faults are part of the target workload.

**Rollback seam:** stop new admission, preserve durable queue/leases, fence
actors, and drain/recover under the new authority. Never fall back while durable
work remains outstanding.

## R11 — `0.34.13` independent updater implementation and production activation

**Purpose:** add a small update trust boundary without making runtime extensions
part of it.

**Entry gate for design/test implementation**

- R1 failure model and fixtures exist;
- R2 stable release/transaction identities are available where shared;
- owner has chosen trust boundary, root-key custody, CI signing authority,
  rotation/revocation, freshness/freeze policy, anti-rollback floor,
  filesystem guarantees, and emergency recovery semantics.

**Actions**

- reuse current locked build, staged install, smoke, backup/restore, checksums,
  provenance, and installer fallback;
- define canonical signed release metadata with artifact/platform hashes and a
  monotonic signed sequence;
- keep verifier code, trust roots, accepted floor, journal, and selector outside
  plugin/MCP/bundle/provider/candidate authority;
- stage immutable versioned releases and use a durable crash journal plus atomic
  selector;
- permit old content only through a newly signed higher-sequence recovery
  release after the accepted floor advances.

**Entry gate for production activation**

- R10 passes;
- key custody and independent modification boundary are proven on the supported
  platform;
- owner explicitly authorizes activation;
- no unresolved Browser/Pro or owner hold covers updater semantics.

**Exit gate**

- wrong signature, digest, platform, freshness, and non-increasing sequence
  reject;
- tampered runtime/plugin/MCP cannot affect verification;
- disk-full, failed smoke, and termination at every
  download/write/fsync/rename/journal/selector boundary leave one complete
  bootable trusted release;
- recovery never produces a mixed four-resource installation;
- rollback does not decrement activation/security/actor epochs or the trusted
  release floor.

**Parallel work:** updater design/test work can proceed after its entry gate,
independently of R3–R8. Production activation cannot.

**Rollback seam:** keep current installer as bootstrap/break-glass until the
activation matrix passes. After the floor advances, use a higher-sequence signed
recovery release, not a silent downgrade.

## R12 — per-patch authority convergence and release reconciliation

**Purpose:** prove each patch leaves one owner per state transition and removes
its obsolete bypasses immediately rather than reserving a later agent-format
cleanup release.

**Entry gate**

- the owning patch's RED/GREEN, replay/fault, rollback, and remote-CI gates are
  green;
- a call-path/no-bypass audit names the old path being removed;
- owner authorization exists for irreversible migration, production
  activation, or release publication where applicable.

**Actions**

- compare shadow and effective decisions before activation;
- drain/fence pinned Turns and actors where the owning patch requires it;
- remove pre-admission allocation in `0.34.3`, superseded mutable/reconciliation
  paths in their owning patches, and every old agent-file path in `0.34.8`;
- align each patch's workspace version, newest-only root changelog, archived
  previous changelog, and published metadata;
- stage only the atomic patch, commit/push after local gates, and wait for
  remote CI before starting the next patch.

**Exit gate**

- one generation/binding owner, existing permission authority, admission owner,
  actor/effect owner, catalog snapshot, and updater authority exist at their
  respective completed stages;
- no removed path can be selected as a fallback;
- release metadata agrees and protected user changes remain untouched.

**Rollback seam:** select only retained verified content under a higher epoch
after quiescence/fencing. A removed old agent format is never restored; reversal
is a new verified forward change without down-migrating journals.

## Owner-only decisions and fail-closed defaults

Pro/planner advice cannot authorize any row.

| Decision | Earliest blocking stage | Fail-closed default |
| --- | --- | --- |
| Does the 100-active count include the lead/root, and what is reserved for control/recovery? | R1/R5, required for R10 | Do not certify 100/256 until explicit. Keep current provider limit separate. |
| Fairness, cancellation/promotion, parent suspension, and multi-member partial vs atomic admission | R1/R5 | Deterministic per-member rejection; no overcommit or hold-and-wait. |
| Turn-attempt boundary across provider/transport/crash retry | R1/R2 | Same attempt keeps its binding; terminal restart requires a new attempt. |
| Config precedence, collision namespaces, and secret-reference handling | R1/R3 | Reject ambiguous/colliding candidates; do not copy secrets into manifests/events. |
| Final external AgentBundle execution semantics, context transfer, and resident idle/turn lifecycle | `0.34.10`/`0.34.11` | No installed-bundle execution; native built-ins remain available through `0.34.8` semantics. |
| Private Bundle key ownership (offline OS/device-bound vs online publisher/license unwrap) | `0.34.9` inspection / `0.34.10` activation | Inspect authenticated public metadata only; do not decrypt/activate or persist plaintext. |
| Remote non-idempotent effect retry/reconciliation | R1/R4/R6 | Do not auto-retry an indeterminate outcome without the same remote idempotency key or reconciler proof. |
| Additive event/wire/storage compatibility and old-reader support | R2 | Keep new control journals additive/outside shared event tags until proven. |
| Performance/resource/recovery thresholds | R1/R9/R10 (`0.34.12`) | Report measurements; do not claim scale or optimize against moving thresholds. |
| Updater trust boundary, keys, signing authority, rotation/revocation, floor, authorized rollback | R1/R11 (`0.34.13`) | No built-in production update activation. |
| Whether the checkout's `0.34.2` state represents an intended unpublished release | R0/R12 | Record only checkout facts; do not tag or publish. |
| Production activation, permission expansion, or irreversible migration | each activation / R12 | Remain shadow/blocked until explicit owner authorization and all gates pass. |

## Parallel execution boundaries

After R2, these packages can make bounded progress without violating the
critical path:

- generation/binding core and admission journals can proceed against shared ID
  contracts;
- Markdown bundle manifest compilation can proceed in shadow after generation
  identity, while JS/Rust activation waits;
- native/Compat MCP and plugin adapters can proceed independently after the
  desired/observed candidate interface freezes;
- updater verifier/journal fixtures can proceed after updater owner decisions,
  with no dependency on runtime/plugin/MCP/bundle code;
- storage benchmark collection can run throughout, but storage implementation
  waits for R9.

A package must pause when it would encode, consume, certify, or activate an
unresolved high-impact contract. Other dependency-independent packages may
continue.

## Browser/Pro checkpoint

No uncertainty packet is emitted for this audit closure. If a future stage hits
one of the mandatory triggers in
`research/browser-pro-escalation-protocol.md` and authoritative source,
official protocol fixtures, and safe bounded experiments cannot discriminate
the viable designs:

1. the `fuji1 remote worker` creates the minimal redacted uncertainty packet;
2. only the `MacBook Air coordinator` submits it through in-app Browser/Pro;
3. the MacBook Air ruling is explicitly returned to the same canonical
   `fuji1 remote worker` session;
4. the determination is tagged at claim level and independently source/TDD/
   benchmark/fault verified;
5. Browser unavailability leaves the affected dependency closure blocked; the
   `fuji1 remote worker` may not invoke, simulate, proxy, or substitute a
   browser or model.

`Pro-advised` alone never closes generation/fencing, AgentBundle trust,
MCP/plugin compatibility, 100/256 certification, updater activation, owner
authorization, permission expansion, irreversible migration, or rollback
gates.
