# Implementation plan

## 0. Execution guard

The task is `in_progress`. Release `0.34.3` is committed/pushed at
`b8c21deeb5004e1f703b199a40de196902fadf35` and its draft-PR remote CI is
green. Release `0.34.4` is now the only active implementation slice. Source
edits, builds, tests, commit, push, and remote-CI monitoring are authorized only
for that slice. Releases `0.34.5+`, tagging, merge, production activation, and
synchronization remain unauthorized.

Before any future implementation:

1. the implementer reruns the HEAD audit against the then-current source;
2. `git status --porcelain` is compared with the protected baseline in
   `research/fuji1-sync-preflight.md`;
3. the isolated worktree/branch is created from the exact audit HEAD without
   checking out, cleaning, or rewriting dirty `main`;
4. the stable defect ID and its required gates are selected from
   `research/defect-register.md`, and the work enters through the dependency
   stage in `research/next-step-roadmap.md`.

The audit anchor is `267bfc3c6c66e46fe8514e2e70657489f853b7f0`,
workspace version `0.34.2`, with 19 pre-existing user-owned dirty entries
before task creation. Those facts are provenance, not a demand that future
phases remain on that commit. Every phase records its own starting commit and
stops if a planned file overlaps unrecognized user work.

The current `0.34.3` implementation base is fetched `origin/main`
`156d0ad3c50aea67dfac0054485eb6991e77308b`. The sole intervening commit changes
only the README icon reference; the isolated branch was rebased and its task
changes restored without conflict. Dirty `main` remains at the audit anchor.

All implementation, dependency resolution, Rust/JS builds, tests, benchmarks,
release staging, commits, and pushes occur only in the isolated worktree on the
`fuji1 remote worker`, rooted at
`/chivier-disk/yanweiye/Projects/yaca/.worktrees/modular-harness-native-swarm-runtime-refresh`
on branch `codex/modular-harness-native-swarm-runtime-refresh`. The dirty
`main` checkout and its 19 user-owned paths are never switched, cleaned,
staged, or merged. The `MacBook Air coordinator` is not a development
workspace; any separately authorized repository transfer remains
`fuji1 remote worker` to `MacBook Air coordinator`, one-way and non-deleting.

The `MacBook Air coordinator` source session exclusively operates in-app
`[@Browser](plugin://browser@openai-bundled)` and selects ChatGPT Pro Model for
qualifying consultations. The `fuji1 remote worker` canonical session does not
support Browser and must never invoke, simulate, proxy, or fall back to another
browser/model. If Browser is unavailable on the MacBook Air, the affected
dependency closure remains blocked. No Browser call is part of this planning
update. The canonical role/host/session identities and full protocol are in
`research/browser-pro-escalation-protocol.md`.

## 1. Delivery rules for every atomic slice

Each checkbox below is a dependency/gate, not permission to batch a phase into
one oversized change. Every feature/fix slice follows this exact loop:

1. **Preflight:** record HEAD and dirty paths; load the relevant specs and
   current source; name the invariant and rollback seam.
2. **Uncertainty triage:** scan the mandatory Pro triggers. First seek an
   authoritative source, official protocol fixture, or safe bounded experiment.
   If a material trigger remains without a deterministic discriminator, emit a
   redacted uncertainty packet and pause its affected dependency closure.
3. **RED:** add one atomic failing test or deterministic fault case.
4. **Confirm RED:** run only that test and prove it fails for the missing
   behavior, not a fixture/environment error.
5. **GREEN:** implement the smallest change that satisfies that test.
6. **Focused verification:** rerun the focused test plus the nearest
   crate/integration suite.
7. **Compatibility:** replay old fixtures and exercise old-reader/wire behavior
   whenever IDs, events, storage, DTOs, manifests, or protocols change.
8. **Full gate:** run the applicable commands in section 4 and build a runnable
   executable on the `fuji1 remote worker`.
9. **Release metadata:** update `[workspace.package].version` for every
   feature/fix; move the prior root changelog to
   `docs/changes/CHANGELOG_<old-version>.md`, write only the new version in root
   `CHANGELOG.md`, and keep any release tag aligned when publishing.
10. **Atomic delivery:** stage only the slice's files, commit one semantic
   change, and push from the `fuji1 remote worker` only after every required
   gate passes.
11. **Stop on failure:** do not commit or push an unverified slice. Record the
    failed command, evidence, and whether rollback/quiescence is required.

Additive journals and compatibility adapters may exist in shadow mode, but
there must never be two effective writers/authorities for the same transition.

### 1.1 Browser/Pro hold and resumption rule

The escalation predicate is:

```text
material mandatory trigger
AND no authoritative source/authority rule/protocol fixture/bounded experiment
= ESCALATION_BLOCKED
```

The hold covers the owning package plus downstream packages that encode,
consume, certify, or activate the unresolved contract. Independent packages
continue only when they do not depend on that decision. Ordinary
implementation, build, test, and benchmark failures remain
`fuji1 remote worker` work.

The `fuji1 remote worker` canonical session creates and returns the minimal
uncertainty packet to the `MacBook Air coordinator` source session. Only that
`MacBook Air coordinator` source session may submit it with Browser/Pro and
record the session URL/date, exact displayed model label, question summary, Pro
conclusion, and MacBook Air ruling. The `MacBook Air coordinator` source
session explicitly sends the result back to the same `fuji1 remote worker`
canonical session for persistence in this task and source/TDD/benchmark
verification; this exchange does not authorize bidirectional filesystem
synchronization.

Each material determination is marked `Pro-advised`, `source-verified`,
`experimentally-verified`, or `rejected`. `Pro-advised` alone permits at most
bounded reversible verification work and cannot close a source, TDD,
benchmark, compatibility, protocol, migration, rollback, activation,
or owner gate. Reproducible source/experimental evidence wins on conflict.

### 1.2 Current-cycle owner constraints

- Do not add or research sandbox/seccomp/container isolation, a capability
  broker, escrow delegation, or an independent `SecurityEpoch`.
- Explicitly installed JS/Rust AgentBundle and plugin code is trusted same-UID
  extension code. The product does not isolate malicious plugins.
- Plugin policy propagation must reuse current harness configuration and
  `PermissionPlane`/dispatch for minimum protocol validation,
  ask/allow/deny, logging, and fail-closed errors. It must not create a second
  permission framework.
- Admission, cancellation propagation, resource limits, binding consistency,
  crash containment, actor epochs, and effect fencing remain
  correctness/performance work.
- AgentBundle is deferred beyond `0.34.4`. The third-round ruling fixes an
  ABI-neutral Harness-attached catalog/flat manifest, namespace resolver,
  `none | basic | full` resource views, and the existing `PermissionPlane` as
  final authority. It does not authorize execution.
- Do not implement AgentBundle execution until the owner selects transient-only
  A, resident-only B, or recommended hybrid C, plus context-transfer and
  resident idle/turn semantics.
- **Emergency owner override:** drop all old agent-file support. Do not
  implement an adapter, synthetic representation, old-source bundle list/info,
  or parser/discovery/execution fallback. Move all
  built-in agents to native AgentBundles and delete the old execution branch in
  that same future cutover.
- The capability matrix proves native AgentBundle coverage and built-in
  migration only. Historical event/session agent IDs remain replayable, but
  replay compatibility never loads an old agent file.
- Bundle `role` controls visibility; `spawn_lifecycle` controls
  transient/resident native-spawn behavior. Harness owns the root Session
  lifecycle for a TUI-selected main definition.

## 2. Owner-corrected patch dependency graph

```text
0.34.3 minimal pre-create admission + bounded spawn transport + typed overload
  -> remote CI green
    -> 0.34.4 OperationId + minimal durable admission/cancel/finalize/recovery
      -> remote CI green
        -> 0.34.5 immutable config generation + TurnBinding
          + source-owned atomic registry snapshot/refresh
          -> remote CI green
            -> 0.34.6 MCP/plugin desired-observed-effective reconciliation
              + respawn declarations + generation binding
              + current PermissionPlane propagation
              + generic stable-ID/namespace seams only
              -> remote CI green
                -> 0.34.7 resident durable recovery + actor lease/epoch
                  + minimal effect fencing/reconciliation
                  -> remote CI green
                    -> 0.34.8 atomic native built-in Bundle cutover
                      + capability/replay fixtures + one catalog/runtime path
                      + old agent-file code removed in the same release
                      -> remote CI green
                        -> 0.34.9 .hyabundle distribution
                          + four-command CLI/registry + atomic activation
                          + public 7z/private envelope inspection
                          -> remote CI green
                            -> 0.34.10 owner-gated fixed stdio runner
                              + external main/transient Bundles + examples/skill
                              -> remote CI green
                                -> 0.34.11 resident Bundle integration
                                  + Hybrid send/wait/recovery/fencing
                                  -> remote CI green
                                    -> 0.34.12 100/256 certification
                                      + measured optimization only
                                      -> remote CI green
                                        -> 0.34.13 independent updater
                                          + verifier/activator/rollback
                                          + self-update example/skill
```

No later patch may be preimplemented in `0.34.4`. Each arrow requires the
preceding patch's full gate, atomic commit/push, and remote CI green. Pro round
six is advisory provenance; the MacBook Air coordinator's corrections above
control. Final external execution/context/resident semantics, private Bundle
key ownership, irreversible migration, production activation, and
permission-expansion gates remain independently owner-controlled.

## 3. Planned component ownership

This is a starting map to revalidate with CodeGraph before each edit.

| Concern | Likely owning components | Constraint |
| --- | --- | --- |
| IDs, wire/domain records | `hya-proto` | Keep dependency-light; preserve tagged event replay and old readers. |
| Append-only control journals, transactions, recovery indexes | `hya-store` | Journals/reducers are authoritative; indexes/checkpoints are derivable. |
| Generation, binding, security, admission, actors/effects | deep modules under `hya-core` | One state-transition owner per lane; avoid a shallow cross-cutting “manager” API. |
| Tool declarations and effect adapters | `hya-tool` | No mutable effective registry authority or adapter-scoped security policy. |
| MCP/plugin desired and observed state | `hya-mcp`, `hya-plugin`, `hya-plugin-compat` | Protocol output is untrusted observation; adapters cannot activate themselves. |
| Composition and package/source adapters | `hya-app` | Wire authorities only; no duplicate transition logic or old agent-file fallback. |
| Typed APIs/observability | `hya-server`, `hya-client`, `hya-sdk`, optionally TypeScript TUI | Shared DTOs/projections; preserve the event-sourced source of truth. |
| Harness/fault/capacity commands | `xtask` plus focused crate/integration fixtures | Deterministic seeds, barriers, virtual clocks, and crash points; no external provider dependency. |
| Independent update verifier/activator | a minimal dedicated Rust crate/binary and packaging boundary | No dependency on runtime plugins, MCP, provider, bundle execution, or session secrets. |

## 4. Verification commands

These are the required final gates for the active `0.34.4` slice.

### 4.1 Rust and executable gate

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --bins
```

### 4.2 Compat adapter gate when touched

From `crates/hya-plugin-compat/adapter`:

```sh
bun run typecheck
bun test
```

### 4.3 TypeScript TUI gate when touched

From `packages/hya-tui-ts`:

```sh
bun run typecheck
bun test
```

### 4.4 Installer/update gate when touched

```sh
bash tests/install_script.sh
```

The canonical deterministic harness and these profiles are deferred to their
mapped later patches; they are not `0.34.3` deliverables:

```text
characterize
steady-100
burst-256
restart-100-156
refresh-churn
security-race
actor-effect-faults
bundle-containment
update-crash-matrix
```

The exact CLI syntax is frozen by its first RED integration test; later phases
must reuse it rather than inventing independent benchmark scripts.

### 4.5 Consultation evidence gate

Before a consequential gate closes, record one of:

```text
not-applicable-with-reason
resolved-deterministically
escalation-pending
Pro-advised-pending-verification
verified
rejected
```

`escalation-pending` blocks the affected dependency closure.
`Pro-advised-pending-verification` does not satisfy any verification or
activation gate. A `rejected` record disposes of the advice but does not resolve
the underlying decision unless a verified alternative is recorded. A relevant
HEAD/source/spec/protocol/assumption change marks the packet and ruling stale
until the `fuji1 remote worker` revalidates them.

Browser being unavailable on the MacBook Air, the required displayed model
being unavailable, a failed redaction review, or an unsafe session URL leaves
the scope blocked. The `fuji1 remote worker` may not choose a substitute
browser/model or silently stand in. At most one narrow follow-up is allowed
before the unresolved decision returns to its owner/user.

## 5. Delivered release — `0.34.3`

### 5.1 Source-confirmed starting point

- `crates/hya-tool/src/spawn.rs::{SpawnerPlane,spawn_inner}` uses
  `mpsc::unbounded_channel`; `SpawnError` currently contains only
  `Unavailable`.
- `crates/hya-app/src/runtime.rs::spawn_team_supervisor` creates one Tokio task
  per received request. Its background transient branch calls
  `SessionEngine::create` before `hya_core::run_team`.
- `crates/hya-core/src/subagent.rs::run_team` performs depth and
  `SubagentGovernor::reserve` checks before per-member Tokio tasks, but after
  the background pre-creation above.
- `crates/hya-core/src/resident.rs::ResidentSupervisor::spawn_resident`
  creates/registers a child without the transient reserve/depth check.
- `SubagentGovernor::reserve` is a per-root budget counter; it is not a
  lifetime permit. The `max_concurrency = 128` default controls provider
  streaming, not spawn-queue capacity or the future 100/256 contract.

These facts were re-read from the source at
`267bfc3c6c66e46fe8514e2e70657489f853b7f0`. They resolve the current
implementation seam without a Browser escalation.

### 5.2 Atomic TDD sequence

1. **RED — bounded transport.** Construct `SpawnerPlane` with injected capacity
   one, hold the receiver, enqueue one request, then prove the next call fails
   immediately with `SpawnError::Overloaded`. No supervisor exists, so the
   rejected request cannot create a request-owned task, session, or event.
2. **GREEN — bounded transport.** Replace the unbounded channel with a bounded
   channel and `try_send`. Map `Full` to the new typed `Overloaded` variant and
   `Closed` to existing `Unavailable`. Update all constructors explicitly; at
   runtime derive capacity from the existing resolved per-run budget with a
   valid range of `1..=tokio::sync::Semaphore::MAX_PERMITS`, never from `100`,
   `128`, or `256`.
3. **RED — background transient pre-create denial.** With
   `per_run_budget = 0`, submit one background transient. Prove the result is
   typed overload and that no new child session, child event, provider call,
   resident slot, or request-owned execution task exists.
4. **GREEN — shared pre-create decision.** Extract/reuse the existing
   depth/budget decision at the earliest common request boundary, before the
   request-owned Tokio task and before member resolution can create runtime
   state. Preserve `run_team` foreground partial-batch evidence and prevent
   double charging for the pre-admitted background path.
5. **RED/GREEN — resident parity.** Repeat the denial with `resident = true`.
   It must use the same decision and return the same typed overload before
   `ensure_main`, `spawn_resident`, child creation, roster registration, or
   resident task creation.
6. **Regression.** Prove an admitted background transient is counted exactly
   once; foreground batch partial grants/depth rejection remain compatible;
   accepted resident behavior is unchanged; closed transport remains
   `Unavailable`.

“No event” means no child-session/member/roster event caused by the rejected
spawn. The normal parent Turn may still record its caller-visible typed tool
failure through the existing event-sourced path. “Deny-all” in these tests
means zero admission budget, not `PermissionPlane` denial.

### 5.3 Hard exclusions

Do not add a durable queue/scheduler, active permit/lease, cancellation/refund
journal, `OperationId`, generation/binding code, MCP/plugin changes,
AgentBundle ABI/manifest fields or cutover work, 100/256 capacity
defaults/certification, updater work, or a sandbox/security framework.

### 5.4 Release and exit gate

- Change workspace version `0.34.2 -> 0.34.3`.
- If absent, move the exact current root `CHANGELOG.md` to
  `docs/changes/CHANGELOG_0.34.2.md`; never overwrite a conflicting archive.
- Root `CHANGELOG.md` contains only `0.34.3` admission/queue/typed-overload
  notes.
- Run each focused RED and record the expected failure before GREEN.
- Run focused/nearest suites and all commands in section 4.1.
- Run Trellis checks, independent Standards/Spec review, staged diff/accounting
  review, then one semantic commit and push.
- Wait for every required remote check on the pushed SHA to become green.
- Keep this task `in_progress`; do not start `0.34.4`.

### 5.5 Current `0.34.3` evidence and release verification

The isolated worktree remains on
`codex/modular-harness-native-swarm-runtime-refresh`, based on
`156d0ad3c50aea67dfac0054485eb6991e77308b`. The original source audit remains
anchored at `267bfc3c6c66e46fe8514e2e70657489f853b7f0`; the only intervening
upstream change is the README icon reference. The dirty main checkout and its
19 protected paths have not been switched, cleaned, staged, or edited.

RED evidence captured before GREEN:

- unbounded transport made the second request wait instead of returning a
  typed overload;
- a zero-budget background transient created a child session before the
  governor rejected execution;
- the equivalent resident request created child/main-resident state before
  rejection;
- the typed tool boundary could not compile an overload assertion before
  `SpawnError`/`ToolError` gained the minimal variant;
- the authorized `xtask` gate-repair RED reproduced exactly five rustfmt
  differences and two `collapsible_if` warnings; after closing those two
  warnings, the next Clippy run exposed the single test `expect_used` warning.

GREEN/focused evidence:

- `SpawnerPlane` uses a bounded Tokio channel and `try_send`;
- background transient/resident requests share one all-or-none pre-admission
  before the request-owned Tokio task and child state;
- the pre-admitted transient continuation does not double charge;
- queue full and admission denial return typed overload;
- exact reservation has a direct all-or-none unit test;
- independent review found and TDD-closed the public-constructor extreme
  capacity panic (`usize::MAX` now clamps to Tokio's supported maximum) and
  added a bound-plane receiver-closed regression;
- denial integration snapshots now prove no parent event/projection,
  resident-supervisor state, or provider call is created, and an admitted
  resident regression proves roster registration plus provider execution;
- independent Standards and Spec closure reviews report zero remaining hard
  findings; their earlier findings are covered by the regressions above;
- the owner authorized the smallest isolated-branch repair to the
  source-verified `xtask` baseline. The repair applies only rustfmt output,
  collapses the two equivalent nested conditions, and converts the parsing test
  to an `anyhow::Result` instead of `expect`;
- focused `cargo test -p xtask`, `cargo fmt --all --check`, and
  `cargo clippy -p xtask --all-targets -- -D warnings` now pass;
- focused suites passed: `hya-tool` library 29, task integration 10,
  `hya-core` subagent 10, `hya-app` admission 4, and nested spawn tree 3.

Release metadata is aligned locally at `0.34.3`: workspace manifest and lock,
README, TypeScript TUI package manifest, newest-only root changelog, and
`docs/changes/CHANGELOG_0.34.2.md`. The metadata regression passes.

Current verification:

- `cargo fmt --all --check` — passed;
- `cargo clippy --workspace --all-targets -- -D warnings` — passed;
- `cargo test --workspace` — passed;
- `cargo build --workspace --bins` — passed;
- TypeScript TUI typecheck — passed;
- the complete Bun suite — passed 43/43. Two earlier runs timed out at
  different fixed 20-second PTY waits in the unchanged 80/140-column smoke
  tests; the failed 140-column case then passed focused and the complete suite
  passed without a TUI or timeout change, recording the non-deterministic
  baseline without weakening the gate;
- the previously blocking `xtask` formatter/Clippy baseline is now repaired
  only in the isolated branch under explicit owner authorization. The dirty
  main copy remains untouched.

All required local gates passed after the repair. Commit `b8c21dee` was pushed
to the existing draft PR #24 and remote CI run `30598676183` completed green.
This closes the `0.34.3` entry gate for `0.34.4`.

## 5A. Active release — `0.34.4`

### 5A.1 Controlling identity and journal contract

- Add a strong `OperationId` with no random constructor. Derive it only through
  fixed-namespace UUIDv5 from the persisted UUID-backed `ToolCallId`.
- Preserve the derived ID non-optionally in every production `ToolCtx`,
  including normal provider tool calls and direct shell dispatch. Carry the
  source call and derived operation through
  `TaskTool -> SpawnerPlane -> SpawnRequest`.
- Do not add OperationId to public events, projections, HTTP/API DTOs, provider
  schemas, CLI arguments/output, or TUI payloads.
- Add exactly one narrow additive `SessionStore` admission table. Do not reuse
  dormant `session`, `team_run`, or `team_member`; do not add
  `operation_child`, queue, scheduler, member, effect, actor, lease, epoch, or
  generic resource tables.
- Persist immutable `{operation, source tool call, source/root session,
  fingerprint, units}` before debit or downstream allocation.
- Legal states are `accepted -> started -> completed|cancelled|aborted` plus
  accepted-only `cancelled|aborted` for no-debit rejection. Terminals are
  irreversible; identical terminal replay is idempotent; a conflicting
  terminal transition fails typed-closed.
- Same operation plus identical immutable request returns the existing state
  without debit or dispatch. Any immutable mismatch returns
  `OPERATION_ID_CONFLICT` without mutation.
- Overload terminalizes `accepted` without governor release. A successfully
  debited operation first persists `started`; all terminal paths then converge
  on one store-CAS finalizer. Only the winning `started` finalizer removes the
  operation-keyed governor debit, using governor-owned units.
- Startup recovery atomically aborts every nonterminal row before
  `ResidentSupervisor::start`, `spawn_team_supervisor`, mailbox readiness, or a
  returned runtime. It never dispatches/resumes/retries, emits no admission
  event, and never credits a fresh governor.

### 5A.2 Main-agent merged TDD sequence

1. **RED/GREEN identity:** fixed ToolCallId vector,
   deterministic/domain-separated OperationId, different-call separation, and
   absence of random mint/default.
2. **RED/GREEN propagation:** the exact persisted normal/direct-shell tool call
   reaches non-optional `ToolCtx`; task transport preserves both IDs.
3. **RED/GREEN claim:** first claim is `accepted`; serial and concurrent
   identical retry returns existing; changed fingerprint/source/units is exact
   typed conflict with no mutation.
4. **RED/GREEN transitions:** first `accepted -> started`, legal terminals,
   same-terminal idempotency, conflicting-terminal fail-closed, and immutable
   request fields.
5. **RED/GREEN debit/finalize:** one operation-keyed debit; overload has no
   debit/release; completion, cancellation, child/create failure, and root
   cleanup race through one finalizer and release at most once.
6. **RED/GREEN dispatch:** duplicate accepted/started/terminal requests never
   call `SessionEngine::create` or dispatch resident/transient work again.
7. **RED/GREEN recovery:** file-backed restart atomically aborts accepted and
   started, preserves terminals, dispatches nothing, emits no public Event,
   gives no fresh-governor credit, and is repeatable.
8. **Replay independence:** journal mutations do not alter event replay or
   projection.

### 5A.3 Source ownership and deliberate exclusions

- `hya-proto`: internal strong ID and fixed derivation only.
- `hya-store`: schema, immutable claim comparison, transition CAS, terminal
  disposition, and startup recovery.
- `hya-core::SessionEngine`/governor: begin/finalize facade and process-local
  operation debit ownership.
- `hya-tool`: mandatory context and typed transport errors.
- `hya-app`: versioned canonical SHA-256 request fingerprint and supervisor
  orchestration after durable start.

Exact previous-result replay is not promised. An identical duplicate receives
the existing admission state and remains non-dispatchable. The current patch
does not prebuild `0.34.5` generation/TurnBinding, `0.34.6` reconciliation,
`0.34.7` actor fencing, any Bundle work, 100/256 certification, or updater
work.

### 5A.4 Release and exit gate

- Workspace/TUI/README version `0.34.3 -> 0.34.4`.
- Move the exact prior root changelog to
  `docs/changes/CHANGELOG_0.34.3.md`; root `CHANGELOG.md` contains only
  `0.34.4`.
- Run focused crate/integration tests after each RED/GREEN, then
  `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and `cargo build --workspace --bins`.
- Run existing CI-required TUI coverage because the package version changes;
  adapter gates remain conditional on adapter changes.
- Run Trellis/spec/replay/no-public-exposure checks, stage only `0.34.4`, make
  one semantic one-line commit, push the existing branch, update only draft PR
  #24, and wait for every required remote check to become green.
- Do not merge and do not prepare `0.34.5` before the remote gate closes.

### 5A.5 `0.34.4` implementation and local verification evidence

The implementation remains isolated on
`codex/modular-harness-native-swarm-runtime-refresh`, starting from the green
`0.34.3` commit `b8c21deeb5004e1f703b199a40de196902fadf35`.
The protected main checkout remains at `267bfc3c6c66e46fe8514e2e70657489f853b7f0`
with its 19 user-owned paths and three stashes untouched.

Focused RED evidence was captured before each GREEN:

- the fixed-vector `OperationId` test initially failed because no internal
  operation identity existed;
- `ToolCtx`/task transport tests initially failed because the persisted
  `ToolCallId` had no mandatory derived operation companion;
- first-claim, concurrent-start, terminal-CAS, startup-recovery, and exact
  governor-debit tests initially failed on their missing store/engine seams;
- duplicate/concurrent spawn tests initially demonstrated the missing durable
  no-redispatch guard;
- file-backed restart initially left `started` instead of fail-closed
  `aborted`;
- overload/cancel-before-debit and root-cleanup tests established zero
  over-release and one exact release for actually started operations;
- a controlled `cancelled -> completed` terminal race initially logged the
  conflict but incorrectly returned foreground success; the final GREEN
  propagates `SpawnError::Unavailable` and leaves the durable terminal
  unchanged;
- the read-only `tail-session` command initially reused runtime startup and
  changed a live journal row from `started` to `aborted`; the final GREEN
  replays directly from `SessionStore`, so only a spawn-owning runtime invokes
  startup recovery.

The resulting implementation uses fixed-namespace UUIDv5 identity, one narrow
admission journal, SHA-256 request fingerprints, monotonic
`accepted -> started -> terminal` transitions, operation-keyed governor debit,
one idempotent finalizer, and startup abort-before-spawn-readiness. It adds no
public Event/API identity, child/effect journal, durable scheduler, or later
release work.

Local exit gates after the last GREEN:

- focused `hya-store`, `hya-core`, `hya-tool`, and `hya-app` admission suites
  passed, including all nine spawn-admission integration tests;
- `cargo fmt --all --check` passed;
- `cargo clippy --workspace --all-targets -- -D warnings` passed;
- `cargo test --workspace` passed;
- `cargo build --workspace --bins` passed;
- `bun run typecheck` passed in `packages/hya-tui-ts`;
- `bun test` passed 43/43 in `packages/hya-tui-ts`;
- Trellis JSON/JSONL parsing and `git diff --check` passed before staging.

Commit, push, and draft PR #24 remote-CI results are the remaining release
gate; `0.34.5` remains blocked until they are green.

## 6. Deferred semantic audit lanes (`P0`–`P9`)

The sections below preserve the original audit decomposition. They are not
independent releases and must be scheduled only through the patch map in
section 2. They describe gates, not alternate sequencing or permission to
retain old agent-file code.

### P0 — contracts, characterization, and modular harness

**Purpose:** turn every inference into a measurable question before production
architecture changes.

### Slices

- [ ] **P0.1 Evidence/conformance taxonomy.** Add an executable inventory that
      reports which assertions are HEAD implementation, accepted ADR,
      inference, or target. Keep the corrected facts: 128 is a depth>0
      provider-stream semaphore; resident data replay is not actor
      rehydration; a new background transient session is allocated before
      `run_team` reservation; installer/release staging exists.
- [ ] **P0.2 Deterministic fixtures.** Reuse current fake providers and extract
      only shared helpers proven necessary for barriers, virtual time,
      temporary SQLite, plugin/MCP declaration drift, bundle processes,
      revocation races, disk-full/lock injection, and updater signing/crash
      fixtures.
- [ ] **P0.3 Capacity accounting contract.** Ratify that the initial public test
      is 100 active subagent phase leases plus 156 durable non-active positions,
      while root/control/recovery uses a separately capped class and provider
      streams are 100 general + 28 reserved. Define idle-resident, waiting
      parent, tool/effect, and recovery accounting.
- [ ] **P0.4 Scheduling contract.** Freeze acknowledgement, cancellation,
      eligible-item bypass/aging, per-root fairness, parent suspension, batch
      partial-failure, typed overload, and recovery semantics.
- [ ] **P0.5 Threat and updater decisions.** Record same-UID executable trust
      scope, updater OS ownership boundary, signing custody/rotation/revocation,
      authorized recovery policy, and break-glass owner.
- [ ] **P0.6 Baselines.** Measure 1k/10k/100k histories, SQLite contention,
      event-bus lag, CPU/RSS/FD/process/storage, acknowledgement/append/drain
      latency, and recovery time on the `fuji1 remote worker`. Ratify or revise
      the provisional PRD thresholds through a recorded decision.
- [ ] **P0.7 Pro escalation drill.** Walk the protocol without submitting to
      Browser: prove no-trigger and deterministically-resolved cases stay on
      the `fuji1 remote worker`; prove an unresolved trigger yields a minimal
      redacted packet and dependency-scoped hold; validate staleness,
      one-follow-up, evidence-tag, safe-URL, and owner-gate behavior.

### First RED tests

- **First atomic tracer:** with a deterministic deny-all/zero-capacity admission
  fixture, one background transient request must return a typed overload before
  `SessionEngine::create` and produce no child session, request-owned task,
  provider request, resident slot, or effect. Confirm RED is caused by the
  current pre-reservation allocation, then make only that seam pass. The next
  atomic slice applies the same contract to resident work before
  `ensure_main`/`spawn_resident`. Full details are in
  `research/next-step-roadmap.md`.
- Current schema exposure followed by registry mutation can resolve a different
  implementation: characterization reproduces the drift without calling it the
  target.
- Item 257 can enter the current unbounded spawn plane: characterization proves
  the missing overload fence.
- A restarted runtime does not autonomously rebuild an existing resident actor:
  characterization separates durable mail from live execution.
- A plugin restart returns changed declarations that the current host discards.
- A table-driven protocol fixture covers architecture-invariant conflict,
  irreversible DB/event migration, capability/revocation/fencing, capacity
  interpretation, plugin/MCP compatibility, updater TCB/rollback, and
  source-undecidable viable-design triggers without invoking Browser.

### Exit gate

- One reproducible harness command owns every workload/fault profile.
- No correctness test relies on wall-clock sleep or an external provider.
- The active-count, fairness, bundle trust, and updater ownership decisions are
  recorded.
- Every P1-P9 invariant has a named RED test and measurable signal.
- Every mandatory trigger has a named package hold/resumption checkpoint, and
  no checkpoint accepts `Pro-advised` as verification.

### Rollback/stop

P0 is test/measurement infrastructure only. Remove unused helpers if they
cannot be deterministic. Stop if the 19 protected dirty entries overlap a
required harness file; do not “clean” or absorb them.

## 6. P1 — stable IDs, epochs, and additive control journals

**Dependencies:** P0 vocabulary and compatibility matrix.

### Slices

- [ ] **P1.1 ID types.** Add `SourceId`, `ToolId`, `DeclarationId`,
      `ArtifactId`, `BundleId`, `BundleRevisionId`, `BindingId`,
      `BindingSetId`, `TurnId`, `TurnAttemptId`, `OperationId`, and `ActorId`
      with canonical serialization, collision/domain tests, and no display-name
      authority.
- [ ] **P1.2 Epoch authority.** Add transactional monotonic
      `activation_epoch` and per-actor `actor_epoch` allocation. Reject
      equal/lower transitions and overflow/corrupt state. Do not introduce an
      independent `SecurityEpoch`.
- [ ] **P1.3 Append-only journals.** Add versioned activation, admission,
      actor, and operation journal records plus deterministic reducers and
      derivable indexes. Keep session events unchanged until old-reader
      tolerance is proven.
- [ ] **P1.4 Compatibility/observability.** Expose optional IDs/epochs in typed
      APIs without fabricating them for historical sessions. Historical replay
      remains independent of removed source loaders and performs no agent-file
      import or event rewrite.

### RED/gates

- Same semantic tool/source yields the same stable ID across aliases,
  registration order, process restart, and artifact revision; namespace
  collision rejects.
- Concurrent epoch allocations are unique/ordered; selecting old content cannot
  lower any epoch.
- Crash before/after every journal append/transaction recovers one valid state.
- Existing fixtures replay to identical projections; an old reader either
  tolerates additive data or the new record remains in the separate control
  journal.
- A duplicate journaled `OperationId` cannot produce two committed outcomes.
- **Pro checkpoint:** before freezing conflicting ID/epoch invariants or any
  irreversible DB/event migration, either cite a deterministic current-source
  and compatibility result or complete the escalation protocol. Pro advice
  cannot authorize the migration or replace replay/crash evidence.

### Activation/rollback

All P1 state is shadow-only. Readers tolerate absence. Rollback disables new
writes but keeps additive durable records; never down-migrate or decrement.

## 7. P2 — deterministic generation and generic namespace seams

**Dependencies:** P1 IDs and activation journal.

### Slices

- [ ] **P2.1 Canonical generation inputs.** Define source precedence,
      secret-reference rules, compatibility fields, stable declarations, and
      canonical bytes.
- [ ] **P2.2 Generic stable-ID/namespace seam.** Define source-owned logical
      identities, collision behavior, and qualified-name plumbing needed by
      later resource catalogs without adding an AgentBundle loader, manifest,
      registry, runner, or TUI/spawn surface.
- [ ] **P2.3 Candidate compiler.** Compile current runtime sources, skills,
      AGENTS, model routing, and MCP/plugin desired declarations into an
      immutable `ConfigGeneration` in shadow mode.
- [ ] **P2.4 TurnBinding.** Pin the complete generation/registry snapshot before
      the first provider round and use it for schema exposure and dispatch.
- [ ] **P2.5 Candidate audit.** Persist source/config digests and rejection
      reasons without secrets; coalesce change storms deterministically.

### RED/gates

- Identical inputs in different discovery order produce identical canonical
  bytes and IDs.
- Stable-ID collision, ambiguous qualified identity, or secret material rejects
  the entire candidate.
- A bad candidate leaves the effective runtime unchanged.
- Provider-visible schemas and dispatch resolve from one TurnBinding across all
  rounds; refresh is next-Turn only.
- Tests prove this phase has no Bundle parser/catalog/TUI/spawn/runner entry.

### Activation/rollback

Compiler is shadow-only until its output matches characterized HEAD inputs and
TurnBinding can activate atomically. Disable candidate generation to roll back;
no Bundle behavior exists in this phase.

## 8. P3 — reconciliation, immutable bindings, and next-Turn refresh

**Dependencies:** P1 journals/IDs and P2 generations.

### Slices

- [ ] **P3.1 Desired/observed/effective records.** Make builtin, static/deferred
      MCP, dynamic Compat MCP, native/Compat plugin, agent, skill, and AGENTS
      adapters publish desired/observed data only.
- [ ] **P3.2 Binding compiler.** Verify artifact/protocol/stable identity,
      schemas, aliases, health, policy ceiling, and executor; compile one
      immutable `BindingSetId`. Reject partial candidates and collisions.
- [ ] **P3.3 Turn attempt pin.** Persist `{TurnAttemptId, BindingSetId,
      activation_epoch}` before the first round. Provider schemas, model,
      prompt/skills, aliases, and execution use that snapshot through every
      round and retry.
- [ ] **P3.4 Activation lifecycle.** Implement
      `prepare -> verify -> activate -> quiesce -> drain -> retire` with one
      journaled atomic publication and retained generations.
- [ ] **P3.5 Process restart semantics.** Consume plugin reinitialize results.
      Reattach a process to an existing binding only on exact artifact,
      protocol, source/declaration identity, and digest match; otherwise return
      typed unavailable/quarantine and prepare a new activation.

### RED/gates

- Refresh precisely between schema exposure and tool invocation: the current
  attempt executes the old pinned binding; the next attempt sees the complete
  new generation.
- Multiple provider rounds never mix generation/model/skill/tool IDs.
- Dynamic Compat MCP becomes callable only after verified activation; failed
  connect/disconnect/handshake never partially changes effective tools.
- Plugin declaration removal can subtract authority; broadening cannot become
  visible until verification, policy approval, activation, and a new attempt.
- Crash at every lifecycle step recovers the old or new complete generation,
  never a mixture.
- **Pro checkpoint:** plugin/MCP protocol ambiguity must first use official
  sources and conformance fixtures. If compatibility remains undecidable,
  pause the affected adapter/binding consumers, consult through the
  `MacBook Air coordinator`, then require `fuji1 remote worker` source or
  experimental verification before compatibility sign-off.

### Activation/rollback

Begin with decision shadowing. Once activated, rollback republishes the last
retained verified generation at a higher epoch after quiescence; it never
restores per-name mutation or an old agent-file authority.

## 9. P4 — current PermissionPlane propagation and correctness EffectGate

**Dependencies:** P3 immutable binding and P1 operation/actor journals.

### Slices

- [ ] **P4.1 Narrowing resource view.** Resolve Harness policy, active binding,
      AgentBundle requested `none|basic|full`, aliases, allow/deny, and
      `permission_overlay` so bundle/plugin input can only narrow.
- [ ] **P4.2 Existing PermissionPlane propagation.** Reuse current
      ask/allow/deny interaction, logging, protocol validation, and fail-closed
      dispatch errors; do not add a second framework.
- [ ] **P4.3 Correctness `EffectGate`.** Revalidate `actor_epoch`,
      binding/declaration, admission/lease state, `OperationId`, and the
      existing PermissionPlane result required by dispatch.

### RED/gates

- A forbidden resource is absent from schemas and rejected on direct
  invocation.
- Bundle/plugin declarations and overlays cannot broaden Harness policy.
- Missing/ambiguous binding, unavailable permission evaluation, stale actor,
  invalid lease, or duplicate operation fails closed.
- Retry/recovery of the same semantic effect uses the same `OperationId`.
- No test or documentation claims an independent `SecurityEpoch`, capability
  broker, escrow, sandbox, or malicious-plugin isolation.

### Activation/rollback

Rollback retains the current `PermissionPlane` and must never replace it with
bundle/plugin policy. The parser/catalog remains inert until its mapped
execution patch.

## 10. P5 — durable unified multi-resource admission

**Dependencies:** P1 journal, P3 binding identity, P0 accounting/fairness
decision.

### Slices

- [ ] **P5.1 Durable intake.** Replace unbounded acknowledgement with a bounded
      persisted `queued` record before session/task/process creation. Queued
      items own no Tokio task.
- [ ] **P5.2 Transactional vectors.** Implement all-or-nothing phase leases for
      active worker, provider class, process/FD/memory/storage/event/effect/team
      budgets, plus deterministic release/reconciliation.
- [ ] **P5.3 Scheduler.** Add reserved control/recovery service, per-root
      fairness, oldest-eligible selection, bounded bypass/aging, cancellation,
      and parent `waiting` suspension that releases active resources.
- [ ] **P5.4 Route every entry.** Foreground/background, single/batch,
      transient/resident wakes, root/control, and recovery enter the same
      authority before downstream allocation. Preserve existing external await
      semantics through adapters.
- [ ] **P5.5 Recovery.** Reconcile uncertain leases before new promotion; return
      typed overload/durability/cancellation outcomes through shared DTOs.

### RED/gates

- Under a barrier: exactly 100 subagent items active, 156 durable
  queued/waiting/recovering, and item 257 typed-overloaded with no created
  session, task, stream, process, or effect.
- Restart at 100/156 restores the accepted set and counts; cancellation
  promotes exactly one eligible item transactionally.
- 100 parents that spawn children complete because waiting parents release
  active leases.
- Resident wakes and transient spawns consume the same resource model; no
  bypass survives.
- General provider work never uses the 28 non-borrowing reserved slots; total
  streams never exceed 128.
- Disk-full, SQLite busy, cancellation, partial batch, resource exhaustion, and
  race injection return typed outcomes without leaked reservations.
- **Pro checkpoint:** disagreement over what counts as active/non-active,
  resident/transient, root/control, or admitted work triggers escalation only
  after current contracts and a bounded capacity fixture fail to decide it.
  Final certification still requires deterministic proof of exactly 100
  active, 156 durable non-active, 256 admitted total, and typed no-allocation
  overload at item 257.

### Activation/rollback

Start in decision shadow. After enforcing admission, any rollback is allowed
only with zero durable queue and zero active lease. Otherwise stop new intake
and drain/recover under the same authority before selecting retained verified
content.

## 11. P6 — resident reconstruction, actor epochs, and effect outcomes

**Dependencies:** P5 authoritative leases, P4 `EffectGate`, P1 actor/operation
journals.

### Slices

- [ ] **P6.1 Shadow reconstruction.** Rebuild desired actors, cursor/mail
      ranges, wake reasons, parent dependencies, quiescence sequence, and
      budgets from journal/projection without waking them.
- [ ] **P6.2 Actor incarnation fence.** Transactionally increment
      `actor_epoch`; attach it to every lease, mail acknowledgement, result, and
      effect completion; reject stale writers.
- [ ] **P6.3 Operation state machine.** Persist
      `planned -> authorized -> started -> committed|failed|indeterminate` and
      classify adapters as pure, idempotent, reversible, reconcilable, or
      non-idempotent.
- [ ] **P6.4 Bounded activation.** After reconciliation and epoch commit, create
      tasks only for active admitted phases and enqueue pending resident wakes
      through P5. Broadcast/notify becomes a cache/wake optimization only.
- [ ] **P6.5 Quiescence/recovery.** Reconstruct main/resident actors and
      termination counters without duplicate synthesis or mail reinjection.

### RED/gates

- Kill after registration, cursor read, admission promotion, effect start,
  effect commit, and before/after actor-epoch commit.
- Recovery produces one current incarnation, preserves pending mail exactly
  once at the cursor contract, and rejects delayed old-epoch mail/results/effects.
- Journaled idempotent effects deduplicate by `OperationId`; reversible
  compensation and reconcilers are exercised.
- An indeterminate non-idempotent remote effect is visible and not blindly
  retried.
- Broadcast lag/restart converges from durable state even when no in-memory
  actor slot existed.
- **Pro checkpoint:** unresolved actor-incarnation, operation-outcome, or
  effect-fencing semantics remain blocked across the owning package and
  effect-consuming adapters. Pro cannot turn an indeterminate remote effect
  into an exactly-once claim or replace stale-epoch/fault-injection proof.

### Activation/rollback

Do not wake reconstructed actors before fencing tests pass. Disable rehydration
only from a quiescent state with no live/queued resident work; fence newer actor
epochs before selecting retained runtime content.

## 12. P7 — native AgentBundle cutover, distribution, and external execution

**Dependencies:** `0.34.4` OperationId/durable admission, `0.34.5` immutable
generation/TurnBinding, `0.34.6` reconciliation/namespace seams, and `0.34.7`
resident recovery/fencing. Each numbered substage below is a separate patch and
may start only after the previous patch's remote CI is green.

### `0.34.8` — atomic native built-in cutover

- [ ] Complete the field-level A/B/C capability matrix from current source and
      freeze characterization, event, projection, replay, fork, restore, and
      every-built-in-ID fixtures before deleting old code.
- [ ] Add one deterministic package preparer used for repo-native built-ins and
      later installed packages. Validate references/unknown fields,
      canonicalize, digest, and emit authoritative embedded package bytes plus
      a digest-bound read-only index.
- [ ] Add the minimal flat `AgentBundleIR`, catalog, stable-ID/namespace
      resolver, `none|basic|full` views, aliases, `can_spawn`, and hook
      references required by current built-ins.
- [ ] Put built-ins under `bundles/builtin/<bundle-id>/`; preserve each current
      public stable agent ID as an explicit manifest field and mark catalog
      entries `origin=builtin, immutable=true`.
- [ ] Merge embedded built-ins and the installed registry into one immutable
      generation, reject built-in bundle/stable-agent-ID collisions, and feed
      startup, TUI, and spawn resolution from that same snapshot.
- [ ] In the same cutover release, delete every old agent-file
      loader/parser/discovery/execution/TUI/spawn branch. Do not add an adapter,
      conversion, migration, scanner, or rollback fallback.
- [ ] Add built-in authoring documentation, a simple repo-native Markdown
      Bundle example, and `agent-bundle-authoring`; the executable must boot
      without calling `hya bundle install`.

Exit requires every A item mapped/tested, every B/C item implemented or typed
rejected, all seven built-in IDs and replay fixtures green, unknown fields
fail-closed, no old agent-file caller remains, and missing historical
definitions never silently execute another agent. The source-tested
unknown-new-spawn fallback remains an explicit owner decision if the matrix
cannot reconcile it with fail-closed resolution.

### `0.34.9` — package distribution and registry

- [ ] Implement only `hya bundle install <path>`, `list`, `uninstall <name>`,
      and `info <name>` / `info -f <path>`, with `.hyabundle` magic/version
      detection rather than suffix dispatch.
- [ ] Public v1 is safe-staged standard 7z with traversal, absolute-path,
      symlink, entry-count, and expanded-size limits plus locked prepare/build.
- [ ] Private v1 is the `HYABNDL` authenticated envelope with public metadata
      inspection and no persistent plaintext. Decrypt/activation remains
      blocked on the private-key owner decision.
- [ ] Keep the package store an artifact cache and the registry/generation the
      sole state source. Use single-active version semantics, idempotent
      same-digest install, typed conflict/in-use/immutable/not-managed errors,
      and atomic activation.
- [ ] Built-ins appear in list/info as immutable and uninstall returns
      `BUNDLE_IMMUTABLE`.

### `0.34.10` — owner-gated external main/transient execution

- [ ] Extend/reuse the existing out-of-process plugin JSON-RPC/stdio protocol;
      do not add Rust dylib ABI or a second transport.
- [ ] Keep the Bundle ABI minimal: initialize returning descriptor/catalog,
      prompt rendering, tool invocation, existing hook dispatch, and shutdown.
      Harness retains spawn/send/wait and all SessionEngine lifecycle.
- [ ] After the owner selects final external execution/context and private-key
      semantics, activate public/private main/transient packages through the
      same generation, admission, OperationId, event, and PermissionPlane path.
- [ ] Add runnable Markdown/JS/Rust examples and update the authoring skill.

### `0.34.11` — resident Bundle integration

- [ ] Add resident spawn/send/wait through the same Harness handle, mailbox,
      recovery, actor epoch, admission, event, and effect-fencing path.
- [ ] Apply the owner-selected idle/turn reclaim rule and add a resident
      example/skill update. Do not build a second actor runtime.

### Shared RED/gates and rollback

- Unknown field/reference, stable-ID collision, alias ambiguity, resource-view
  expansion, disallowed `can_spawn`, subagent TUI exposure, invalid `send`,
  runner crash/timeout/cancellation, and resource exhaustion fail closed.
- Role controls visibility only; `spawn_lifecycle` controls only Harness
  transient/resident spawn. Root Session lifetime remains Harness-owned.
- Bundle input never broadens Harness policy; same-UID executable code is
  trusted and no sandbox/malicious-code-isolation claim is made.
- Rollback activates only a retained verified generation at a higher epoch.
  It never restores deleted agent-file paths or a second catalog authority.

## 13. P8 — independently protected updater/activator (`0.34.13`)

**Dependencies:** P0 threat/custody decision and P1 IDs/journal. Production
activation additionally requires P3 generation/drain, P4 security, P6
effect-fence contracts, and `0.34.12` remote CI green.

### Slices

- [ ] **P8.1 Canonical signed metadata.** Verify trust root, release sequence,
      freshness, platform/compatibility, hashes/sizes, and explicit recovery
      intent without loading candidate code.
- [ ] **P8.2 Immutable staging.** Download/copy into a versioned staging
      directory, verify locked artifacts, fsync supported filesystem boundaries,
      and smoke in a dedicated subprocess without claiming sandbox isolation.
- [ ] **P8.3 Activation journal/selector.** Journal prepare, quiesce/drain/fence
      the runtime, atomically switch one selector, fsync the parent, and journal
      commit/accepted floor.
- [ ] **P8.4 Independent ownership.** Package verifier, trust roots, accepted
      floor, journal, and selector so candidate/runtime/extensions cannot
      modify them. The updater cannot read session databases or runtime secrets.
- [ ] **P8.5 Recovery and break-glass.** Recover every interrupted state to one
      complete verified generation. Retain `install.sh` as manual/bootstrap
      recovery; an older release requires a newly authorized higher-sequence
      recovery activation.

### RED/gates

- Reject wrong signature/hash/size/platform, expired/frozen/replayed metadata,
  non-increasing sequence, unauthorized key, and artifact substitution.
- Inject disk-full and termination before/after every write, fsync, rename,
  selector, smoke, quiescence, and journal transition.
- Recovery always boots the old or new complete verified generation, never a
  mixed set; activation/update/actor floors do not decrement.
- A malicious runtime/plugin/MCP/bundle fixture cannot change verifier code,
  trust roots, accepted floor, selector, or journal.
- Installer rollback tests continue to pass.
- **Pro checkpoint:** an unresolved updater TCB, anti-rollback, recovery
  rollback, signing-custody, or activation decision blocks the updater
  dependency closure. Consultation cannot approve production activation,
  accept a key, expand privilege, or waive signature/ownership/crash-recovery
  evidence and the existing owner authorization.

### Activation/rollback

Updater remains opt-in until the ownership and crash matrices pass. A failed
not-yet-committed candidate may be discarded/restored without advancing the
accepted floor. After acceptance, old bits activate only through a newly signed
higher-sequence recovery release.

## 14. P9 — `0.34.12` integrated capacity/fault certification

**Dependencies:** `0.34.11` remote CI green and every runtime authority's exit
gate. Updater certification is not included because `0.34.13` follows this
release.

### Slices

- [ ] **P9.1 Combined matrix.** Run steady, burst, parent/child, history/storage,
      restart, refresh, security, actor/effect, bundle, and bus cases
      from `design.md` together with deterministic seeds and saved results.
- [ ] **P9.2 Soak/resources.** Hold 100 active for at least 30 minutes and
      10,000 completions, whichever is longer; confirm the resource/latency
      model, zero tasks for queued work, bounded active tasks/processes/FDs, and
      post-burst reclamation.
- [ ] **P9.3 Replay/wire/UI/API.** Prove old sessions replay unchanged, typed
      overload/recovery/indeterminate states cross server/client/SDK and TUI if
      touched, and no consumer invents a parallel reducer.
- [ ] **P9.4 Conditional storage decision.** If the frozen benchmark passes,
      record “no storage change.” If one predeclared path fails, make one
      minimal reversible replay-equivalent optimization and rerun the identical
      workload; do not batch speculative SQLite changes.
- [ ] **P9.5 Authority/bypass audit.** Prove unbounded spawn intake,
      independent resident/transient admission, mutable per-name effective
      registry, duplicate MCP effectiveness paths, in-memory cursor authority,
      and name-only dispatch are absent. Any remaining obsolete bypass is fixed
      in its owning patch, not deferred as an agent-format cleanup release.
- [ ] **P9.6 Final release gate.** Run every command in section 4, verify the
      executable, align version/changelog/tag, commit/push atomic verified
      slices, and archive evidence only after the task is actually complete.
- [ ] **P9.7 Consultation closure audit.** Enumerate every escalation record,
      confirm its packet/ruling applies to the final source state, and require
      `source-verified`, `experimentally-verified`, or a documented `rejected`
      branch with a separately verified replacement. No unresolved hold or
      advisory-only claim may reach certification, activation, or release.

### Final acceptance

- Exactly 100 active + 156 durable non-active subagent items; item 257 is typed
  overload with no unbounded allocation.
- Every Turn attempt and effect is traceable to stable IDs and one binding;
  activation and actor epochs never decrease.
- No resident/transient/root/control/recovery entry bypasses its declared
  admission class or global resource accounting.
- Registry refresh, revocation, actor recovery, effect ambiguity, and
  executable trust satisfy their fault matrices.
- Existing sessions replay; pre-cutover Turns drain without rebinding;
  rollback reaches a runnable verified generation under higher epochs.
- Generation/fencing, AgentBundle containment, plugin/MCP compatibility,
  100/156/257 certification, and self-update activation contain no unresolved
  escalation hold and no gate closed on `Pro-advised` alone.
- The 19 protected baseline entries remain untouched unless their owner
  separately changes them.

### Rollback/optimization stop

Capture a quiescent recovery point and prove retained-content rollback before
certification. A storage optimization that does not improve the frozen failing
benchmark or preserve replay/crash equivalence is reverted; do not resurrect an
older mutable authority or down-migrate journals.

## 15. Phase gate matrix

| Phase | First durable authority | Activation gate | Rollback seam |
| --- | --- | --- | --- |
| P0 | None | Owner decisions + deterministic harness | Remove unused fixtures |
| P1 | ID/epoch/control journal | Compatibility and crash replay | Stop new shadow writes; retain data |
| P2 | Immutable candidates | Deterministic complete generation | Disable compiler |
| P3 | Binding/activation | Whole-attempt pin + atomic refresh/recovery | Higher-epoch retained verified generation |
| P4 | Security/effect gate | Revocation race + direct-dispatch denial | Equal-or-stricter policy only |
| P5 | Admission | 100/156/257 + restart/fairness/resource faults | Drain to zero before retained-generation rollback |
| P6 | Actor/effect journal | Rehydration and stale-epoch/operation proof | Quiesce/fence actors |
| P7 | Native Bundle catalog/packages/execution | `0.34.8` capability/replay cutover, `0.34.9` packaging, `0.34.10`/`0.34.11` owner-gated external execution | Higher-epoch retained generation; never restore old agent-file code |
| P8 | Updater TCB | `0.34.13` ownership + signature + crash matrix | Old/new complete generation under forward sequence |
| P9 | Runtime certification | `0.34.12` combined proof and fixed benchmark matrix | Revert only failed derived optimization |

Every row also applies section 4.5. In particular, generation/fencing (P1-P4
and P6), AgentBundle dependency/containment (P2/P7), 100/256 certification
(P5/P9), plugin/MCP compatibility (P3/P9), and self-update activation (P8/P9)
must be `resolved-deterministically` or independently `verified`; an
`escalation-pending` or `Pro-advised-pending-verification` record blocks the
affected row. Existing owner gates remain additional, never substitutable,
conditions.

## 16. Context and review checklist

The JSONL manifests intentionally contain only specs, ADRs, and research. Trellis
injects `prd.md`, `design.md`, and `implement.md` automatically, and its workflow
forbids code paths in context manifests.

Before a future implementation/check agent starts:

- [ ] read every real JSONL entry and the automatically injected task artifacts;
- [ ] select a stable finding from `research/defect-register.md` and satisfy its
      evidence-specific characterization/TDD/benchmark/fault requirements;
- [ ] enter through the non-skippable dependencies and rollback seam in
      `research/next-step-roadmap.md`;
- [ ] re-query current source/call paths rather than trusting file:line anchors
      from `267bfc3`;
- [ ] list affected packages and load each spec index's pre-development or
      quality section;
- [ ] compare dirty paths with the protected baseline and stop on overlap;
- [ ] identify the exact RED test, activation gate, and rollback seam;
- [ ] read `research/browser-pro-escalation-protocol.md`, scan mandatory
      triggers, and record the applicable consultation evidence state;
- [ ] confirm each adopted Pro determination is independently
      `source-verified` or `experimentally-verified`, and each rejected
      determination has a reason and a verified replacement or continuing
      hold;
- [ ] verify no source adapter, compatibility path, or UI projection becomes a
      second authority;
- [ ] keep all development and verification on the `fuji1 remote worker` and
      perform no mirror/synchronizer action.
