# Parallel planning synthesis

## Inputs

The `parallel-planning` workflow supplied the same HEAD-anchored evidence packet
to four read-only planners:

1. `rapid-roadmap` — shortest tracer path and early falsification;
2. `balanced-path-planner` — reversible sequencing and compatibility;
3. `risk-aware-strategist` — threat boundaries, fail-closed semantics, and
   recovery;
4. `frontier-architect` — deep module boundaries, consolidation, and deliberate
   legacy deletion.

The main agent, not a planner, owns the merged decisions below.

## Consensus

All four views converged on these points:

- Separate an **immutable structural plane** (generation, binding, Turn pinning)
  from a **live subtractive plane** (security, admission, actors, effects).
- Establish stable IDs and monotonic epoch domains before changing the runtime
  paths that consume them.
- Treat the current 128 value as a provider-stream constraint, not agent
  capacity.
- Replace unbounded spawn intake and the resident/transient admission split with
  one durable bounded authority.
- Persist actor/operation state and fence stale incarnations/retries; do not
  promise exactly-once arbitrary remote effects.
- Unify static/deferred and dynamic Compat MCP plus plugin/skill/agent discovery
  behind immutable candidate generations and desired/observed/effective
  reconciliation.
- Preserve SQLite and the event log until a benchmark demonstrates a failing
  constraint.
- Keep executable extensions trusted until broker-only effects and OS isolation
  are actually proven.
- Reuse the existing installer/release strengths while introducing a small,
  independent verifier/activator and monotonic rollback semantics.
- Develop, build, benchmark, and fault-test only on the
  `fuji1 remote worker`; the `MacBook Air coordinator` remains a one-way
  mirror/coordination host with strict exclusions.

## Conflicts and main-agent decisions

| Topic | Planner tension | Merged decision |
| --- | --- | --- |
| Workspace isolation | The audit turn originally prohibited a worktree, while implementation must protect a dirty `main`. | Superseded by the implementation authorization: use exactly one isolated fuji1 task worktree at the audited HEAD; never switch, clean, stage, or merge through dirty `main`. |
| Meaning of 100/256 | Suggestions ranged from 256 actor slots or 256 admitted behind 128 streams to a 100-active/156-queued split. | Define the release gate as a **total 256 envelope: exactly 100 active + 156 durably queued** under a barrier workload; request 257 gets a typed overload/defer result. Provider streams remain separately bounded at 128, with 28 target-reserved for root/control/recovery. |
| Persistence optimization | Frontier view proposed hot projections, group commit, and checkpoints as a target; conservative views required evidence first. | Preserve current replay semantics as truth. Phase 0 measures 1k/10k/100k histories and contention. Add verified checkpoints/batching only if the frozen gate fails; do not pre-authorize a storage replacement. |
| Grant visibility | One target allowed configurable immediate grants; others constrained live authority to subtraction. | In a pinned Turn, **revocation is immediate and grants wait for the next Turn**. This prevents live policy from expanding a structurally pinned capability view. |
| Update signing policy | Threshold signing was recommended, but custody/rotation ownership is unknown. | Require canonical signed metadata, rotatable/revocable trust roots, freshness, anti-replay, and explicit key custody. Threshold signing remains a decision for the update-threat-model gate, not an unowned implementation assumption. |
| Updater independence | A separate process alone was sometimes treated as sufficient. | Independence requires that candidate/runtime extensions cannot modify verifier code, trust roots, or activation state. Until OS ownership/permission separation is proven, the feature cannot claim an independent TCB. |
| Executable bundle security | Ambitious view assumed Linux isolation availability. | Treat availability as unproven. JS/Rust bundles may be implemented as trusted out-of-process extensions first; “untrusted/sandboxed” activation is blocked on filesystem/network/environment/process/FD/resource escape tests. |
| Legacy path removal | Frontier view favored early deletion; balanced view retained dual paths. | Use short-lived shadow/compatibility seams with explicit deletion gates. Never maintain two indefinite execution authorities. Capture a legacy generation, drain old Turns, then remove mutable registry/name-only internal lookup after recovery and rollback tests. |
| Performance SLO | Proposed absolute latency values varied. | Use correctness gates as non-negotiable, record provisional latency bounds, and freeze hardware-specific p50/p95/p99 budgets in phase 0. Threshold changes require an explicit recorded decision rather than redefining a failed workload. |

## Merged dependency order

```text
P0 contracts + characterization + capacity baseline
  -> P1 stable IDs, epoch domains, additive journals
    -> P2 immutable config generations and bundle manifests (shadow)
      -> P3 Turn-pinned binding sets + lifecycle + unified resource candidates
        -> P4 live security/revocation authority
          -> P5 durable multi-resource admission
            -> P6 actor rehydration + operation/effect fencing
              -> P7 executable bundle containment and enforced capability views
                -> P8 independent updater/activator
                  -> P9 100/256 proof, fault matrix, cutover, deletion gates
```

Some implementation work may be prepared in parallel, but activation follows
the dependency order:

- security cannot fence an unidentified binding/operation;
- durable actors cannot safely run without admission and security fences;
- untrusted executable bundles cannot activate before broker/containment proof;
- the updater can be designed early but cannot activate before generation,
  drain, fencing, trust-root, and recovery contracts exist.

## First cross-lane integration tracer

The audit-closure source pass confirmed that background transient child-session
allocation occurs before `run_team` reserves capacity. The first **atomic** TDD
slice is therefore the smaller admission-before-allocation tracer in
`next-step-roadmap.md`: deny one background transient before
`SessionEngine::create`, then apply the same interface to resident work.

After that seam and the shared ID/journal prerequisites exist, the first
cross-lane integration tracer should use one built-in fake effect and one
Markdown agent bundle:

1. compile an immutable generation;
2. allocate a stable tool ID and binding set;
3. pin one Turn attempt;
4. durably admit it;
5. allocate actor and operation IDs;
6. authorize against a security epoch;
7. race a revocation against the effect fence;
8. persist the outcome;
9. activate a changed generation for the next Turn only;
10. roll back to the old content at a newer activation epoch.

This slice is intentionally non-production and deterministic. It must make every
required invariant observable before MCP/plugin/process/update breadth is added.

## Audit-closure refresh

The four planning lenses were rerun against the updated source/release evidence.
The main-agent merge is recorded in `defect-register.md` and
`next-step-roadmap.md`:

- 23 stable findings preserve the distinction among confirmed implementation,
  inferred risk, benchmark-needed claims, and target gaps;
- P0 is limited to admission/bypass, resident reconstruction, Turn binding,
  live security, and actor/effect fencing;
- SQLite optimization remains conditional on a frozen failing benchmark;
- same-UID execution remains trusted-only rather than a sandbox claim;
- no Browser/Pro packet is required for audit closure because engineering
  unknowns have deterministic gates and remaining policy choices are
  owner-only with fail-closed defaults;
- checkout workspace/changelog `0.34.2` alignment and the absence of a
  checkout `v0.34.2` tag are recorded only as checkout release-state evidence.

## Main deferrals

- SQLite replacement or partitioned logs, unless measured tuning/checkpoints
  still fail the capacity gates.
- Provider concurrency above 128.
- Distributed/multi-host scheduling.
- Automatic retry of an unknown non-idempotent external effect.
- Strong security claims for ambient same-UID child processes.
- Automatic/bidirectional sync or any sync daemon.
- Deletion of compatibility paths before mixed-version, recovery, and rollback
  gates pass.

## 2026-07-31 `0.34.3` implementation-plan supersession

Four read-only planning lenses were rerun against the exact implementation
authorization and the current source facts: rapid tracer sequencing,
balanced/reversible delivery, risk/fail-closed review, and frontier/deep-seam
review. The main-agent merge supersedes earlier phase/security/worktree
assumptions wherever they conflict:

1. At that supersession boundary only `0.34.3` was active. It contained the pre-create shared admission
   decision, bounded in-memory spawn transport, and typed overload.
2. `SubagentGovernor::reserve` is currently a per-root budget counter, not a
   permit. Reuse it and move its decision earlier; do not import the
   lease/finalize/recovery design from `0.34.4`.
3. Replace `mpsc::unbounded_channel` with an explicit-capacity bounded channel
   and `try_send`; full is typed overload, closed stays unavailable.
4. Admission must occur before the request-owned Tokio task,
   `SessionEngine::create`, `ResidentSupervisor::ensure_main`,
   `spawn_resident`, or child/member/roster events. The normal parent Turn may
   still record its caller-visible typed failure.
5. Background transient and resident paths use one decision and are never
   double charged. Foreground batch partial-grant/depth behavior remains
   compatible.
6. Queue capacity derives from existing resolved limits with a minimum
   transport capacity of one. No `100`, `128`, or `256` default/certification
   enters this patch.
7. The exact-HEAD isolated worktree is mandatory because dirty `main` has 19
   protected user-owned paths. The copied task directory was hash/diff checked;
   only the worktree copy is edited after isolation.
8. Workspace `0.34.2 -> 0.34.3`, changelog archive, focused/full Rust gates,
   semantic commit/push, and remote CI green are one release exit gate.

The planners also surfaced two semantic ambiguities and the main agent resolved
them from the supplied contract and event-sourced architecture:

- “deny-all” means zero admission budget, not a `PermissionPlane` denial;
- “no event” means no rejected child-session/member/roster event, not
  suppression of the parent Turn's normal tool failure record.

Current owner supersession:

- no sandbox/seccomp/container, capability broker, escrow, or independent
  `SecurityEpoch`;
- same-UID installed extensions are trusted and not malicious-code isolated;
- current Harness config/`PermissionPlane` remains the only permission
  authority;
- the third-round Pro ruling fixes the flat ABI-neutral AgentBundle
  manifest/catalog, stable namespace/resolution, and `none|basic|full`
  resource-view design;
- `0.34.6` may implement only the inert parser/catalog/resolver plus MCP/plugin
  reconciliation; `0.34.8` execution waits for owner selection among A/B/C,
  context transfer, and resident idle/turn semantics.

Planner recommendations that assumed a new active RAII permit, waiter
scheduler, cancellation/refund semantics, security epoch, or containment suite
were rejected as later-stage scope rather than silently adopted.

## 2026-07-31 native-only AgentBundle override synthesis

After the fifth-round compatibility proposal was superseded, the same four
read-only planning lenses were rerun with distinct framings: fastest native
cutover, reversible dependency planning, compatibility/replay risk, and
single-runtime architecture. Their main-agent merge is:

1. Native built-in AgentBundles become the sole executable agent-definition
   source. One Bundle IR/compiler feeds the existing `AgentSpec` and
   `SessionEngine`; production execution gets no legacy-vs-bundle branch.
2. The capability matrix is a migration proof. `A_EFFECTIVE` behavior needs
   native differential/characterization coverage; parsed-but-ignored and
   doc-only claims must become implemented behavior or typed rejection rather
   than silent acceptance.
3. Current behavior fixtures must be captured before deleting the old loader,
   but test fixtures do not justify keeping that loader in production.
4. Built-in Bundle cutover and deletion of old agent
   parser/discovery/execution are one atomic future transition. There is no
   `LegacyAgentAdapter`, synthetic bundle, legacy bundle CLI view, or fallback.
5. Historical event/session agent IDs remain a replay concern owned by the
   event protocol. An unavailable historical definition may still display and
   replay; it must never silently execute a different agent definition.
6. `role` is a visibility dimension and `spawn_lifecycle` is a native-spawn
   dimension. Harness owns root Session lifecycle, avoiding the superseded
   `main=session` manifest coupling.
7. Bootstrap authority—embedded, preinstalled, or registry-seeded—and the
   resulting patch order cannot be inferred safely from the current source.
   They remain on the MacBook Air coordinator's sixth-round owner hold.

All four lenses agreed that `0.34.3` is independent of this override and must
continue unchanged. No future version map is adopted in this synthesis.

## 2026-07-31 round-six native-only bootstrap and phase-order synthesis

After the MacBook Air coordinator returned Pro round six, the same four
read-only lenses were rerun against the full owner correction: rapid delivery,
reversible dependency planning, fail-closed/replay risk, and single-authority
architecture. Their merged conclusions are:

1. The exact authority path is
   `prepared package sources -> AgentBundleIR -> immutable
   Generation/catalog -> TurnBinding -> AgentSpec -> SessionEngine`.
   `AgentSpec` is the execution projection; catalog/TUI metadata remains in the
   same immutable generation and cannot become a side-channel source.
2. Repo-native built-in package bytes are authoritative. A deterministic
   preparer validates/canonicalizes/digests them and emits a read-only index in
   the same build action; the index must be digest-bound. The preparer becomes
   the shared library later used by installed packages.
3. Built-ins are `origin=builtin, immutable=true`; startup merges them with the
   installed registry into one snapshot and rejects bundle-ID or stable-agent-ID
   collisions before activation. Every current built-in public ID is explicit,
   never path/version-derived, and remains unchanged in events/replay.
4. The owner correctly rejected Pro's `0.34.4` cutover. Operation identity and
   durable admission (`0.34.4`), generation/TurnBinding (`0.34.5`),
   MCP/plugin reconciliation (`0.34.6`), and resident recovery/fencing
   (`0.34.7`) are prerequisites for the atomic native built-in cutover in
   `0.34.8`.
5. `0.34.8` must freeze capability/replay fixtures, prepare/embed all built-ins,
   switch startup/TUI/spawn to one catalog, and delete all old agent-file
   production paths in the same release. No adapter, scanner, conversion,
   migration, dual runtime, or later agent-format cleanup release is allowed.
6. Distribution (`0.34.9`), owner-gated external main/transient execution
   (`0.34.10`), resident integration (`0.34.11`), capacity certification
   (`0.34.12`), and the independent updater (`0.34.13`) remain separate
   patch releases, each gated by the previous remote CI.
7. If any current built-in has source-confirmed effective resident semantics,
   `0.34.8` must preserve them through the existing native runtime or remain
   blocked; the cutover cannot defer an A-class built-in behavior by relabeling
   it external Bundle work.
8. Historical unknown agent IDs may decode, display, and replay without source
   files. Continuing an unavailable historical definition must never silently
   execute a different definition. Separately, current HEAD's
   unknown-new-spawn fallback to `general` is A-class source-confirmed behavior;
   the round-six material did not state an owner ruling, so that narrow
   `0.34.8` decision remains blocked rather than guessed.
9. `0.34.3` contains none of these future semantics. The complete TUI suite is
   green 43/43 outside the sandbox. Its sole local release hold is the protected
   `xtask` full-workspace fmt/Clippy baseline; no commit/push/remote CI claim is
   made until the required gate is green or the owner explicitly resolves
   scope.

The controlling patch map is therefore:

```text
0.34.3 admission
-> 0.34.4 OperationId/durable admission
-> 0.34.5 generation/TurnBinding
-> 0.34.6 MCP/plugin reconciliation and generic namespace seams
-> 0.34.7 resident recovery/fencing
-> 0.34.8 atomic native built-in cutover
-> 0.34.9 package distribution/CLI/registry
-> 0.34.10 external main/transient runner
-> 0.34.11 resident Bundle integration
-> 0.34.12 capacity/fault certification
-> 0.34.13 independent updater
```

## 2026-07-31 `0.34.4` OperationId/durable-admission synthesis

The four mandatory read-only lenses were rerun after `0.34.3` reached remote
CI green and the MacBook Air coordinator returned the controlling `0.34.4`
ruling. Rapid sequencing, balanced delivery, fail-closed risk, and deep-module
architecture agreed on store-first acceptance, mandatory tool-call identity,
one narrow journal, no redispatch on duplicate/restart, and one terminal
finalizer.

The main-agent merge resolves their remaining differences:

1. `OperationId` is a dependency-light internal domain newtype in
   `hya-proto`, next to `ToolCallId`, because both `hya-tool` and `hya-store`
   consume it. It is not added to any `Event`, HTTP/API DTO, CLI argument or
   output, TUI payload, or provider schema.
2. The derivation uses UUIDv5 with one fixed hya operation namespace and the
   exact persisted `ToolCallId` bytes. This follows the owner-specified fixed
   namespace/domain mechanism and rejects both an independently random UUID
   and Rust/process-local hashing. `OperationId` intentionally has no random
   `new` or `Default`.
3. `hya-app` owns a versioned canonical spawn fingerprint because it already
   owns request resolution and SHA-256. It covers parent identity, background
   mode, ordered normalized members, and every dispatch-affecting inline/model/
   category/resident/task-id field; it excludes cancellation and reply
   channels.
4. `SessionStore` owns the sole durable state machine. A claim stores immutable
   source call, source/root session, fingerprint, and units before a governor
   debit. Conditional SQL distinguishes fresh acceptance, identical replay,
   typed conflict, first start, first terminalization, same-terminal replay,
   and conflicting terminalization.
5. The store CAS is the exactly-once logical-release authority. The governor
   additionally keys process-local debits by `OperationId`, stores its own
   units, and removes them once; callers cannot over-credit by supplying units.
   Root cleanup clears legacy anonymous accounting but preserves or cancels
   operation-owned debits through the same finalization seam.
6. Identical duplicate delivery returns the existing admission state and does
   not debit, create, or dispatch. Exact prior child/tool-result replay is
   deliberately absent; adding an outcome cache or `operation_child` would
   violate the narrow owner scope.
7. Startup atomically changes every `accepted`/`started` row to `aborted`
   before resident/team spawn readiness. It dispatches nothing, emits no
   public event, and never credits the fresh governor. The owner explicitly
   excluded a process owner/lease/epoch schema, so multi-runtime leasing is not
   invented in this patch.
8. The RED order is identity, mandatory context propagation, journal
   claim/conflict/concurrency, legal/terminal transitions, operation-keyed
   debit/release, supervisor no-redispatch/no-child behavior,
   cancel/create-failure/root-cleanup finalization, startup recovery, and event
   replay independence.

The frontier proposal to remove the existing `run_team` admission model
wholesale was rejected as broader than `0.34.4`; only the request-level
tool-call operation path is made durable. The conservative proposal for a
new exclusive database lease was also rejected because it would add the
owner/lease machinery explicitly excluded from this minimal journal. A
source-proven need for cross-process ownership must become a later,
separately-authorized patch rather than hidden scope here.
