# Architect modular hya harness, native 100+ subagent swarm, per-agent Markdown/JS/Rust bundles, atomic runtime refresh, and secure Rust self-update

## Goal

Evolve hya into a highly customizable, Pi-like modular coding harness without
giving up its event-sourced source of truth: one native session must safely
schedule 100+ agents, bind each agent to versioned Markdown/JS/Rust
customizations, atomically refresh tool/MCP/skill/plugin state at Turn
boundaries, revoke authority live, recover resident actors and effects, and
stage/verify/activate/rollback the Rust harness through an independently trusted
update path.

This umbrella task begins from the source audit anchored at
`267bfc3c6c66e46fe8514e2e70657489f853b7f0`. The evidence and corrections are in
`research/head-architecture-audit.md`; stable audit findings and their
dependency-ordered closure path are in `research/defect-register.md` and
`research/next-step-roadmap.md`.

After that audit, `origin/main` advanced to
`156d0ad3c50aea67dfac0054485eb6991e77308b` with one README icon-reference
change and no product-source delta. The isolated implementation branch was
rebased to that commit before release staging. The audit anchor remains
provenance; `156d0ad3c50aea67dfac0054485eb6991e77308b` is the current
implementation/remote base.

### Current-cycle owner scope

Release `0.34.4` is committed/pushed at
`709abafb81ba0f94656254d3ecb51b42e051a89d` with green draft-PR CI run
`30609417298`. The owner has now authorized only release `0.34.5`. The patch sequence does not
build or research sandbox/seccomp/container isolation, a capability broker,
escrow delegation, or an independent `SecurityEpoch`. Explicitly installed
JS/Rust AgentBundle and plugin code is trusted same-UID extension code; the
Harness does not isolate malicious plugins.

Harness config and the existing `PermissionPlane`/dispatch path remain the
only permission authority. Bundle/plugin metadata may declare or narrow a view,
but can never expand Harness policy. Admission, cancellation, resource bounds,
binding consistency, crash containment, actor epochs, `OperationId`, and effect
fencing remain correctness/performance work.

The MacBook Air Pro results are recorded as advisory provenance. The
coordinator's sixth-round ruling fixes native-Bundle-only bootstrapping,
build-time prepared immutable built-ins, and the dependency-ordered patch map
below. Final external execution semantics, private-key ownership, context
transfer, and resident idle/turn semantics still require explicit owner
selection and cannot block or enter `0.34.5`.

### Emergency native-only AgentBundle supersession

The 2026-07-31 owner override drops all legacy agent-file support. It
supersedes every earlier compatibility proposal:

- do not build any old-agent adapter, synthetic representation, old-source
  bundle-list/info behavior, or parser/discovery/execution fallback;
- migrate every built-in agent to a native AgentBundle and delete the old
  agent parser/discovery/execution branch in the same cutover;
- retain the capability matrix only as evidence that native AgentBundle
  execution covers every currently effective agent capability and migrates the
  built-in catalog;
- preserve historical event/session agent IDs for deterministic replay.
  Protocol replay compatibility is not permission to load or execute an old
  agent file.

The MacBook Air coordinator returned the sixth Pro advisory and its controlling
owner correction on 2026-07-31. Built-ins are deterministic repo-native
packages prepared at build time, embedded read-only, and merged with the
installed registry into one immutable generation. This avoids both an install
bootstrap cycle and a temporary old-file detector. The corrected future patch
sequence is recorded in `research/next-step-roadmap.md`; it is planning-only
until each preceding release has green remote CI. Only `0.34.5` is active.

## Product outcomes

1. **Modular harness:** the engine consumes deep, stable interfaces for
   configuration, bindings, security, admission, actors/effects, and updates;
   mutable discovery and process-specific managers remain adapters behind those
   interfaces.
2. **Native swarm:** a single session supports a demonstrated 100-active-agent
   steady profile and deterministic 256-request burst without unbounded queues,
   silent loss, resident bypass, or stale actor effects.
3. **Per-agent bundles:** an agent may bind Markdown prompt/data, an
   out-of-process JS package, and/or a Rust binary/package through one immutable
   manifest and explicit capability view.
4. **Atomic runtime refresh:** tools, MCP, skills, plugins, agents, and AGENTS
   context resolve into a verified generation that becomes visible atomically to
   newly admitted Turn attempts. An in-flight attempt never mixes bindings.
5. **Permission integrity:** structural bindings remain stable for a Turn and
   bundle/plugin declarations only narrow current Harness
   config/`PermissionPlane`; actor/effect fences preserve correctness without
   creating a new permission authority.
6. **Secure self-extension/update:** candidate Rust harness generations are
   built on the `fuji1 remote worker`, verified without executing trusted logic
   from the candidate, activated through a crash-recoverable journal, and roll
   back without decrementing any epoch.

## Functional requirements

### R1. Immutable configuration generation

- Canonicalize all effective harness inputs into a content-addressed,
  immutable `ConfigGeneration`.
- Include agent definitions, ancestor instructions, skills, tool declarations,
  MCP/plugin declarations, model routing, bundle manifests, and requested
  capability ceilings.
- Treat a discovered filesystem/package/process state as a candidate only; it
  cannot mutate an effective generation in place.
- Keep generation activation independent from historical event replay. Never
  import, translate, or execute an old agent file as a compatibility source.

### R2. Versioned runtime binding

- Assign stable declaration identities, including a stable `tool_id` independent
  of display name, registry order, process instance, and artifact version.
- Resolve one immutable `binding_set_id` containing exact artifact, schema,
  protocol, model, skill, instruction, and per-agent capability bindings.
- Durably identify every Turn attempt and pin one binding set and
  `activation_epoch` for all of its model/tool rounds.
- Provider-visible schemas and execution resolution must use the same pinned
  binding.
- Model refresh as `prepare -> verify -> activate -> quiesce -> drain -> retire`.
- Rollback may choose older content but must activate it under a new, strictly
  greater activation epoch.

### R3. Existing permission authority and narrowing resource views

- Keep Harness config/current `PermissionPlane` as the only ask/allow/deny
  authority; do not add a capability broker, escrow, or independent
  `SecurityEpoch`.
- Compute AgentBundle requested views/allow-deny/permission overlays only as
  narrowing input to that authority.
- Provider-visible resources and direct dispatch must agree and fail closed on
  missing/ambiguous binding or unavailable permission evaluation.
- Make currently parsed agent permissions/options and skill
  `allowed-tools`/`model` semantics explicit: enforce supported narrowing
  fields through the existing path or diagnose them as compatibility-only;
  never silently turn ignored metadata into a grant.

### R4. Durable multi-resource admission

- Replace unbounded spawn intake and task-per-request fan-out with a durable,
  bounded admission state machine and bounded workers.
- Complete admission before creating a child session, Tokio worker, resident
  slot, provider stream, process, tool execution, storage lease, or effect
  intent.
- Admit root/control, transient, and resident work through one authority; quota
  classes may reserve capacity but none may bypass global accounting.
- Atomically reserve the required resource vector: active worker, provider
  stream, process, file descriptor, memory budget, event/storage budget, team
  turn/message budget, and bounded queue position.
- Track managed resources as distinct `desired`, `observed`, and `effective`
  states. Effective means verified, healthy, bound, admitted, and currently
  authorized.
- Acknowledged work must be recoverable after restart; overload and durability
  failures must return typed outcomes.

### R5. Actor and effect fencing

- Persist enough resident task, mailbox cursor, lease, binding, and quiescence
  state to rebuild runtime actors after process restart.
- Allocate a durable, monotonic `actor_epoch` for each logical actor
  incarnation. Old tasks/messages/results/effects cannot advance state.
- Allocate a stable `operation_id` before an effect attempt and reuse it across
  retries and recovery.
- Persist operation states at least as planned, authorized, started, committed,
  failed, or indeterminate.
- Classify effect adapters as pure, idempotent, reversible, reconcilable, or
  non-idempotent. Never blindly retry an indeterminate non-idempotent external
  effect.
- Broadcast and in-memory actors/registries are notification/caches only;
  durable state must reconstruct them after lag or restart.

### R6. Per-agent bundle model

- Bundle is a Harness-attached extension/catalog, not an independently callable
  tool. It supplies definitions; Harness executes through native spawn.
- The flat top level is `identity`, `extensions`, `resources`, and `agents[]`.
  Repo-native built-ins live under `bundles/builtin/<bundle-id>/`, are
  deterministically prepared at build time, and are embedded as immutable
  packages plus a digest-bound index.
- `role: main|subagent` controls catalog/TUI visibility. A separate
  `spawn_lifecycle: transient|resident` preserves how the definition behaves
  when Harness native spawn invokes it. Selecting a main agent creates a root
  Session under Harness ownership; `session` is not a Bundle lifecycle value.
- Resources are bundle-owned and referenced by agents; no
  inheritance/nested overlay.
- Stable IDs and resolution follow section 5.2 of `design.md`; missing,
  ambiguous, or conflicting names fail closed.
- Resource views are `none|basic|full`, always narrowed by Harness policy.
  `can_spawn` is default-deny catalog reachability, not permission.
- All prepared package sources lower through one
  `AgentBundleIR -> immutable generation/catalog -> AgentSpec -> SessionEngine`
  path. Catalog/TUI metadata and the execution projection come from that same
  generation; there is no side-channel catalog.
- Release `0.34.8` performs one atomic native built-in cutover: freeze the
  capability/replay fixtures, prepare and embed every built-in Bundle, switch
  startup/TUI/spawn resolution, and delete the old agent
  loader/parser/discovery/runtime paths in the same release.
- Old agent files are not discovered, parsed, converted, migrated, listed, or
  executed. An explicitly supplied old source at a Bundle-only input boundary
  receives a typed unsupported-source/format error.
- Explicitly installed JS/Rust artifacts are trusted same-UID extensions; no
  malicious-code sandbox/isolation claim is made.

### R7. Unified runtime refresh

- Reconcile builtins, static/deferred MCP, dynamic Compat MCP, native/Compat
  plugins, skills, agents, and ancestor instructions through the same generation
  and binding pipeline.
- A manager must expose desired configuration, observed identity/schema/health,
  and effective verified binding separately.
- Plugin/MCP restart may reuse an existing binding only if artifact, protocol,
  identity, and declaration digests match exactly; otherwise quarantine it and
  prepare a new generation.
- Implement the accepted ADR 0007/0008 visibility rule: whole-Turn attempt
  pinning with next-Turn visibility.

### R8. Independent update TCB

- Keep update verification/activation outside the candidate runtime and its
  plugins, MCP servers, and bundles.
- Verify canonical signed metadata, artifact digests, platform/compatibility,
  freshness/anti-replay, and a monotonic update/activation epoch before
  activation.
- Define trust-root custody, rotation/revocation, and recovery. Do not call the
  updater independent until candidate/runtime write access cannot modify its
  verifier, trust roots, or activation authority.
- Stage a complete immutable runtime generation, smoke it in a contained
  process, quiesce/drain/fence the current generation, then switch through one
  atomic selector or a journaled prepare/commit protocol.
- Preserve the current `install.sh` path as break-glass/manual recovery.

### R9. Persisted-data compatibility and cutover

- Preserve deterministic replay of existing sessions.
- Prefer additive tables and optional/versioned fields until old reader
  tolerance is proven; do not fabricate historical binding or operation IDs.
- Keep external tool names as aliases while internal routing migrates to stable
  IDs.
- Pre-cutover active Turns drain or are explicitly interrupted; they are never
  rebound in place.
- Existing resident teams without durable actor state must drain or be
  deliberately restarted at cutover.
- Security revocations and monotonic epochs survive runtime/config rollback.
- The old agent-file format has no runtime migration path. Historical
  event/session IDs remain decodable and replayable without consulting removed
  loaders or rewriting data.

### R10. Observability

- Expose binding/config IDs, lifecycle state, security/actor/activation epochs,
  admission class and queue age, operation state, reconciliation errors, and
  typed overload/indeterminate outcomes without logging secrets.
- Metrics must distinguish requested, durably admitted, reserved, active,
  quiescing, drained, rejected, and recovered work.
- Performance conclusions must identify measured constraints; SQLite/full replay
  are not replaced merely because they are plausible risks.

## Capacity contract

The initial falsifiable capacity model uses deterministic fake
provider/tool/bundle fixtures so external provider limits do not contaminate the
internal harness result:

- **Steady profile:** 100 active Turn attempts for at least 30 minutes and
  10,000 completed attempts, whichever takes longer.
- **Burst profile:** 256 simultaneous admissions into a total envelope of 100
  active and 156 durably queued positions.
- **Provider profile:** no more than 128 simultaneous provider streams; the
  target admission design reserves 28 of those slots for root/control/recovery
  traffic rather than raising the limit without evidence.
- **Full-envelope behavior:** while all 256 positions are held, request 257
  returns the documented typed overload/defer outcome without spawning an
  unbounded task.
- **Correctness:** zero lost acknowledged admissions, duplicate committed local
  effects, mixed binding sets, post-fence stale-security effects, stale-actor
  commits, or silent SQLite/broadcast loss.
- **Recovery:** kill/restart while the exact 100-active/156-queued split is held;
  the accepted set, resource counts, actors, and operations converge without
  duplication.
- **Resource model:** for each resource `i`,
  `required_i = active * per_active_i + queued * per_queued_i + fixed_i`.
  Measured peak must stay within 20% of the phase-0 model prediction and return
  to within 10% of the pre-burst memory baseline within five minutes.
- **Latency model:** freeze p50/p95/p99 baselines in phase 0. Provisional gates
  are admission acknowledgement p99 <= 500 ms, append p99 <= 50 ms steady and
  <= 250 ms in burst, burst drain <= 2x the service-time model, and no p95
  regression above 20% without an explicit reviewed budget change.

Changing a threshold requires a recorded capacity decision; a failing result is
not made green by silently redefining the workload.

## Threat model

### Protected assets

- user source/workspaces and credentials
- event, admission, actor, operation, and projection integrity
- capability policy and revocation history
- immutable manifests, bundle artifacts, stable identities, and binding sets
- update signing roots, manifests, activation journal, and last-known-good
  generations

### Adversaries and failures

- prompt-injected model output or malicious Markdown
- buggy/crashing/drifting JS/Rust/native/Compat plugin or MCP child process;
  deliberately malicious same-UID extension code is outside the isolation
  guarantee
- malicious or drifting MCP/plugin declarations
- duplicate/replayed/delayed operations and stale actor incarnations
- process crash, broadcast lag, SQLite contention/corruption, partial placement,
  or bounded test disk-full
- compromised mirror/update server, replayed signed metadata, one compromised
  signer (if the adopted signing policy claims to tolerate it), or rollback
  attack
- operator misconfiguration that accidentally widens capability

### Trust boundary

- The first target trusts the `fuji1 remote worker` kernel/root operator,
  explicitly installed same-UID extension code, current `PermissionPlane`,
  correctness/effect-fence code, durable activation/actor epochs, and
  independent updater trust root.
- Provider output, bundle contents, MCP/plugin protocol data, and downloaded
  artifacts are untrusted inputs.
- Same-UID executable extensions are trusted; malicious-plugin isolation is not
  a current-cycle product claim.
- Compromised kernel/root and physical destruction are outside the initial
  threat model.
- Current permission decisions use Harness config/`PermissionPlane`. Actor and
  effect fences cannot undo an already completed external effect.

## Development, verification, and release constraints

- The authoritative development/build/test/benchmark/release environment is the
  single isolated task worktree inside the saved project on the
  `fuji1 remote worker`. Never switch, clean, stage, or merge through the dirty
  `main` checkout.
- The `MacBook Air coordinator` is a mirror/coordination host only. Default
  repository sync is one-way `fuji1 remote worker -> MacBook Air coordinator`,
  initially non-deleting, and excludes `.git`, `target`, `node_modules`,
  runtime databases/state, secrets, and all worktrees.
- No synchronizer installation, bidirectional sync, or service modification is
  part of this task without separate authorization.
- Every future feature slice follows repository TDD: one atomic failing test,
  verify the expected failure, smallest passing implementation, then the
  touched-area and CI-equivalent verification gate plus a runnable executable
  build on the `fuji1 remote worker`.
- Every verified feature/fix slice updates `[workspace.package].version` and the
  newest-only root changelog as required, then commits and pushes atomically.
  Only the `0.34.5` slice is currently authorized; its immutable-generation
  scope is fixed by the controlling coordinator ruling and section 5.1.1 of
  `design.md`.

## Non-goals until evidence or prerequisites exist

- Replacing SQLite or full replay before the benchmark matrix proves they block
  the capacity contract.
- Raising provider concurrency above 128 before admission, memory, storage, and
  provider evidence supports it.
- Distributed or multi-host scheduling.
- Claiming arbitrary external effects are exactly-once or reversible.
- Treating a normal child process as a security sandbox.
- Activating the native built-in Bundle cutover before `0.34.8` prerequisites,
  capability/replay fixtures, and one-catalog boot proof pass.
- Executing installed public/private bundles before `0.34.10` runner,
  transport, private-key, and owner execution gates pass.
- Installing or operating a sync daemon on the `MacBook Air coordinator`.

## Acceptance criteria

- [ ] The six lanes in `design.md` have one owner boundary each and no mutable
      registry/discovery path bypasses them.
- [ ] Stable `tool_id`, `operation_id`, `binding_set_id`, Turn-attempt ID, and
      monotonic activation/actor epochs have durable schemas and
      collision/decrease tests.
- [ ] A concurrent refresh test proves one Turn attempt uses one binding across
      every round while the next Turn sees the verified new generation.
- [ ] Permission tests prove bundle/plugin declarations never broaden Harness
      config/current `PermissionPlane`, including direct dispatch.
- [ ] Static/deferred and Compat dynamic MCP plus plugin restart declarations
      converge through one desired/observed/effective binding pipeline.
- [ ] Agent permissions/options and skill policy metadata produce an explicit,
      tested effective capability/model view.
- [ ] Resident and transient work use the same durable bounded admission
      authority; no resident path bypasses it and the 257th held-envelope request
      is explicitly rejected/deferred.
- [ ] Restart tests reconstruct resident tasks/cursors with a newer
      `actor_epoch` and reject delayed work from the old incarnation.
- [ ] Duplicate `operation_id` delivery never starts a second committed local
      effect; non-idempotent uncertain outcomes surface as `indeterminate`.
- [ ] The native built-in cutover lowers deterministic build-time packages
      through one Bundle IR/catalog and preserves every built-in stable ID,
      prompt/model/resource/event behavior required by the capability matrix.
- [ ] The package resolver rejects invalid flat schemas, unknown fields,
      namespace/alias conflicts, ambiguous resources, and immutable built-in
      ID collisions; later external examples exercise main/transient and
      resident agents through the same Harness runtime.
- [ ] The 100-steady, 256-burst, crash-recovery, long-history, SQLite-contention,
      broadcast-lag, refresh-churn, permission-dispatch, and actor/effect matrices pass
      on the `fuji1 remote worker`.
- [ ] Update tests reject tampered/replayed/wrong-platform/lower-epoch content,
      survive termination at every lifecycle transition, and activate an older
      retained artifact only under a newer epoch.
- [ ] Existing sessions replay unchanged; pre-cutover Turns drain without
      rebinding; missing definitions never silently execute another agent; an
      exercised rollback reaches a runnable verified generation.
- [ ] CI-equivalent Rust checks, applicable Bun checks, full workspace tests, and
      canonical executable builds pass for each relevant implementation slice on
      the `fuji1 remote worker`.
- [ ] Workspace version, root newest-only changelog, archived changelog, and any
      release tag remain aligned.
- [ ] The `fuji1 remote worker` / `MacBook Air coordinator` boundary and
      exclusion manifest are reviewed before any sync; no reverse or
      bidirectional path is enabled.

## Planning decisions still requiring an owner

These do not block the active `0.34.5` slice, but their owning phase cannot
activate until decided:

1. update signing-key custody, threshold/rotation/revocation policy, and
   break-glass ownership;
2. final external AgentBundle execution semantics (current recommendation:
   one Harness spawn API with catalog-selected transient/resident behavior),
   context transfer, and resident idle/turn lifecycle;
3. private Bundle key ownership (`offline OS/device-bound` or `online
   publisher/license unwrap`) before private decrypt/activation;
4. additive wire/event compatibility results for older readers;
5. final baseline-derived latency/resource SLO ratification before performance
   optimization.
