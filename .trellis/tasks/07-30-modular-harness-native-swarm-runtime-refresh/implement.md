# Implementation plan

## 0. Execution guard

The task is `in_progress`. Release `0.34.6` is committed/pushed at
`680f9fb535fc48f71f9aead64cc3d3d30161678a` and draft-PR CI run
`30634501761` attempt 2 is green. Release `0.34.7` is now the only active
implementation slice. Source edits, builds, tests, commit, push, and remote-CI
monitoring are authorized only for that slice. Releases `0.34.8+`, tagging,
merge, production activation, and
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

The isolated implementation base for the delivered stages is fetched `origin/main`
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
- AgentBundle is deferred to its prerequisite-gated `0.34.8+` stages. The third-round ruling fixes an
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
            -> 0.34.6 dynamic MCP desired-observed-effective reconciliation
                      + plugin tool/RPC startup-crash re-handshake consistency
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

No later patch may be preimplemented in the active `0.34.7` slice. Each arrow requires the
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

These are the required final gates for the active `0.34.7` slice.

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

## 5A. Delivered release — `0.34.4`

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

Feature commit `53a76ec12c516c9cef0cf916b83d29492d80eac9` was pushed
to draft PR #24. Remote CI run `30607264119` attempts 1 and 2 both stopped in
the unchanged PTY smoke before any Rust gate: fixed 20-second waits timed out
at different interaction stages on the slower runner, while the 80-column
permission-focus wait was the only repeated location. The repair preserves
every narrow/wide behavior assertion and changes only the test harness:

- CI transcript polling is reduced from every 50 ms to every 100 ms so repeated
  whole-transcript reads do not starve the PTY process;
- local waits remain 20 seconds, while CI waits allow 45 seconds and the
  enclosing CI test allows 120 seconds;
- the two observed keyboard boundaries explicitly flush the Bun child stdin.

After that repair, `bun run typecheck`, the focused `CI=true` PTY suite (3/3),
and the complete `CI=true` Bun suite (43/43) passed locally. The repeated Rust
gate then exposed a separate fixture race: the built-in formatter test
received Linux `ETXTBSY` while executing an asynchronously created
`vendor/bin/pint`. A close-before-exec timing race is the source-supported
inference, not a product formatter defect. Creating that executable fixture
with a synchronous write passed 10/10 focused runs, then the complete workspace
test and binary build gates.

At that intermediate point, a focused follow-up commit/push and fully green
draft-PR rerun were still required. Section 5A.6 records their completion and
the resulting `0.34.5` entry gate.

### 5A.6 Browser/Pro PTY causal repair evidence

Gate-repair commit `d4825a8c35d86c37c19f87800c70a7eebd93a6b7`
was pushed to draft PR #24. Remote run `30607763589` then passed the
80-column case but timed out at the still-ambiguous
`worker-1 focused header` in the 140-column case before every Rust step. The
MacBook Air coordinator returned
`CONSULT-2026-07-31-PTY-HARNESS-07`, rejecting a concurrency/serialization
change and constraining the repair to the existing PTY test helper, proxy, and
observable transcript boundaries.

The resulting TDD and causal evidence is:

- `semantic_input_flushes_before_next_action` uses a test-local `FileSinkLike`
  whose asynchronous write remains pending until awaited. RED received
  `["", "chord-a"]` instead of `["chord-a", "chord-b"]`; GREEN awaits
  `write` and then the immediately following `flush`.
- Every semantic input in this PTY spec now uses that narrow helper. Flush is
  recorded only as delivery-boundary evidence, never as proof that the TUI
  consumed the input.
- The ambiguous waits are now
  `open-by-handle/worker-1-focused-header` and
  `ctrl-x-dot/worker-1-focused-header`.
- The bounded diagnostic contains a stripped last frame, raw transcript tail,
  backend and PTY PID/exit/signal status, and the most recent 64 monotonic
  phase records. The existing proxy records request observations with the
  active case/callsite and request path; no endpoint, event, acknowledgement,
  product hook, or pipe drain was added.
- Roster opening now waits for the existing tree request, visible roster, and
  filtered openable child before Enter/final-render observation. The
  worker-cycle path establishes `researcher-1` as the visible focused
  predecessor before the single `Ctrl+X .` action.
- A diagnostic local run proved the open-by-handle worker path fully reached
  final render, while the old `Ctrl+X .` precondition stably rendered
  `scroll-1`; this rejected missing delivery and exposed the old
  Main-to-first-observation assumption. No TUI focus/product behavior changed.

Verification after the final local GREEN:

- `bun run typecheck` passed;
- the helper regression passed;
- focused CI-mode PTY tests passed 3/3 at 80 and 140 columns;
- the complete CI-mode TUI suite passed 44/44 (the original 43 plus the new
  deterministic helper regression);
- `cargo fmt --all --check` passed;
- `cargo clippy --workspace --all-targets -- -D warnings` passed;
- `cargo test --workspace` passed;
- `cargo build --workspace --bins` passed;
- no `crates/**`, product TUI source, workflow, dependency, timeout, version,
  or changelog file changed in this repair.

The atomic test-harness repair is included in green `0.34.4` HEAD
`709abafb81ba0f94656254d3ecb51b42e051a89d`. Draft-PR run `30609417298`
completed successfully, so the `0.34.5` entry gate is closed.

## 5B. Active release — `0.34.5`

### 5B.1 Exact boundary and baseline

Implementation starts from green isolated HEAD
`709abafb81ba0f94656254d3ecb51b42e051a89d` on the existing branch/worktree
and draft PR #24. The stage implements only immutable runtime generations,
per-turn `TurnBinding`, and source-owned atomic tool/skill/MCP publication.
It does not implement `0.34.6` reconciliation, plugin respawn state, stable
namespace catalogs, resident leases/effect fencing, AgentBundle, updater,
sandbox, or a new permission framework.

### 5B.2 RED evidence

Four deterministic integration tests were written before product code:

- `in_flight_turn_retains_generation_while_post_publish_turn_sees_next`;
- `failed_candidate_refresh_retains_generation_and_exact_registry_view`;
- `one_turn_cannot_mix_tool_skill_or_mcp_members_across_generations`;
- `concurrent_publications_are_unique_monotonic_and_never_publish_a_mixed_candidate`.

Each focused command exited 101 because `RuntimeRegistry`, `TurnBinding`, and
`RuntimeRefreshError` did not exist. A second real-engine tracer,
`admitted_turn_uses_one_binding_for_prompt_schema_skill_and_dispatch`, exited
101 because `SessionEngine::refresh_runtime` and `TurnBindingRecorded` did not
exist. These are expected missing-behavior REDs, not fixture failures.

### 5B.3 Minimal GREEN and authority removal

- `hya-core::RuntimeRegistry` owns one active `Arc<RuntimeSnapshot>` and one
  serialized publication seam. Candidate construction never holds the active
  pointer lock; bound dispatch reads only immutable maps.
- `ToolRegistry` remains a convenient mutable candidate builder. Engine
  construction snapshots it immediately; retaining and mutating that builder
  cannot change the effective view.
- Generation allocation occurs only after the candidate closure and validation
  complete. Failed and logical no-op candidates do not consume a generation.
- A snapshot contains one tool view (builtin/plugin/MCP) plus immutable
  multi-workdir skill views. The same binding supplies skill prompt content and
  the `skill` tool plane.
- `run_turn` and direct shell bind once after admission and before assistant
  behavior, then record exactly one lightweight binding event. All stream
  rounds, schema filtering, resolution, dispatch, and skill execution use that
  retained binding.
- Fork/history copying preserves an existing message's generation audit field;
  subsequent fork turns use the same engine owner and bind the then-current
  snapshot. `text_complete` has no independent entry point or registry lookup;
  it executes only inside the already-bound stream-round path.
- `hya-app` builds plugins and synchronous MCP into one complete initial
  builder, then discards builder authority at engine construction. Deferred MCP
  tools are submitted as one complete candidate to `refresh_runtime`.
- `ConfigGeneration` is dependency-light in `hya-proto`.
  `TurnBindingRecorded` carries only the existing session routing field,
  message ID, and generation; projection stores only the optional message
  generation.

### 5B.4 Focused proof

Focused GREEN currently proves:

- old bindings retain old tool/MCP/skill members and later bindings see the
  next complete generation;
- failed candidates preserve exact generation/view and the next success uses
  `N+1`;
- logically unchanged complete candidates do not advance generation;
- eight concurrent publications receive unique consecutive generations and
  the final active view equals one complete candidate;
- a real multi-round turn keeps old prompt/schema/tool/skill dispatch after a
  mid-turn publication, while the next turn sees the new view;
- direct shell records a binding;
- an app-level retained-builder mutation is invisible, candidate member one is
  invisible during candidate assembly, and both deferred MCP markers appear
  together after publication.

`cargo check --workspace --all-targets` and the focused core/app/deferred-startup
tests pass. Section 5B.6 records the completed local release gate; exact
staging, commit/push, and remote CI remain required before `0.34.5` is complete.

### 5B.5 Release/document boundary

Workspace/TUI/README advance exactly `0.34.4 -> 0.34.5`; the prior root
changelog is archived as `docs/changes/CHANGELOG_0.34.4.md`, and root
`CHANGELOG.md` contains only `0.34.5`. Runtime/event architecture docs and ADR
implementation notes describe the narrow snapshot contract. No new
user-configurable surface is introduced, so this stage deliberately adds no
example or skill.

### 5B.6 Local exit-gate evidence

The final local source passed:

- `cargo fmt --all --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `cargo test --workspace`;
- `cargo build --workspace --bins`;
- the CI-locked launcher/backend build;
- `bash scripts/verify-no-http.sh` with zero INET sockets;
- runnable `hya` and `hya-backend` executables reporting `0.34.5`;
- TUI typecheck/build and the complete `CI=true` Bun suite, 44/44.

One default-20-second local Bun run timed out at the 140-column root-session
frame and a later one at the 80-column root-session frame; the complementary
width passed in each case, an intervening default run passed 44/44, and the
final CI-equivalent 45-second suite passed both widths. No timeout, retry,
concurrency, product-TUI, or PTY-helper change was made.

The first final workspace-test run also reproduced the pre-existing Linux
formatter-fixture `ETXTBSY` at its generated `vendor/bin/pint`. The fixture now
holds an explicit file handle, writes and `sync_all`s the script, and drops the
writer before chmod/exec. The focused test passed 20/20 and the original full
workspace command then passed. This is a test-fixture close/durability repair,
not runtime formatter behavior.

Trellis check and the two-axis local Standards/Spec review found no remaining
hard finding after:

- renaming the old hot-register test and proving a retained candidate builder
  cannot change a frozen snapshot;
- asserting direct shell records exactly one binding event;
- avoiding a full catalog clone when the bound `skill` tool selects one entry;
- adding the executable backend code-spec for the immutable generation
  boundary.

Release `0.34.5` was committed at
`95f4fe20b3750d376023384d869a52da1e84201f`; draft-PR CI run
`30612919698` completed successfully. No `0.34.6` work is included in that
historical release.

## 5C. Delivered release — `0.34.6`

### 5C.1 Exact boundary and authority

Implementation starts from the green `0.34.5` commit and existing draft PR
#24. One app-owned `RuntimeReconciler` records the latest desired MCP/plugin
revision, preparation tickets, and observed outcomes. It has no dispatch API or
effective-tool cache. `hya-core::RuntimeRegistry` remains the sole effective
authority and owns each published source's tool exports, resources,
declaration digest, and client/child handle inside the immutable snapshot.

Stable source identity is `(mcp|plugin, configured_id)`. External tool names
remain unchanged. The complete candidate rejects duplicate source IDs,
same-source exports, configured/handshake plugin-ID mismatch, and any
canonical/alias collision before generation allocation. Current
`PermissionPlane` and hook behavior are unchanged.

### 5C.2 Deterministic RED evidence

The strict sequence produced expected missing-behavior failures for:

- stale preparation success closing without overwriting a newer revision;
- safety-priority drop-only removal despite an unrelated connection failure,
  including retained old/new `TurnBinding` behavior;
- current partial failure preserving generation and closing staged owners;
- duplicate source/export/canonical/alias and plugin handshake-ID rejection;
- one complete mixed MCP/plugin publication rather than partial visibility;
- plugin respawn declaration drift closing the replacement and failing calls
  closed.

An additional hardening RED showed candidate collision status remained
`Connecting`; the GREEN marks every ticket in that failed atomic revision with
a typed observed failure and invalidates the attempt without consuming a
generation.

The focused tests were introduced and run one at a time before their product
seams existed with these commands (each RED failed on the named missing
invariant, rather than setup or compilation unrelated to that invariant):

```sh
cargo test -p hya-app stale_success_is_closed_and_cannot_publish_over_newer_ticket
cargo test -p hya-app explicit_removal_publishes_drop_only_despite_unrelated_connect_failure
cargo test -p hya-app current_failure_keeps_generation_and_closes_partial_successes
cargo test -p hya-core --test runtime_sources
cargo test -p hya-plugin --test configured_id_mismatch
cargo test -p hya-app mixed_mcp_plugin_revision_publishes_exactly_once_only_when_complete
cargo test -p hya-plugin --test respawn_declaration_drift
cargo test -p hya-app candidate_rejection_records_failure_and_invalidates_attempt
```

The corresponding RED observations were, in order: no reconciler/ticket seam;
removal could not publish independently of failed addition; partial owners and
generation were not governed by one attempt; source/canonical/alias collision
validation was absent; configured and handshake plugin IDs could differ; mixed
sources lacked one atomic revision; respawn accepted declaration drift; and a
candidate rejection left observation in `Connecting`.

### 5C.3 Minimal GREEN and lifecycle rules

- Startup, deferred MCP, and Compat MCP control all mutate the same reconciler.
  `hya-server` receives a narrow dependency-inverted `McpControl`; the deleted
  `McpHttpState` no longer owns configs, managers, status, or effective tools.
- Handshake/start work occurs before the reconciler lock. A prepared success
  owns its client/child but is not effective until publication. Stale and
  failed staged owners are dropped after releasing the app state lock.
- Explicit disable/removal first publishes a complete candidate that removes
  exactly those sources. Preparation of unrelated additions follows; its
  failure cannot restore the removed source.
- Additions/replacements publish only when every current ticket succeeds.
  Publication derives from the registry's current snapshot, so it preserves
  intervening skill refreshes rather than overwriting from a stale base.
- Old snapshots retain source owners through old `TurnBinding` Arcs. The next
  binding sees removal/replacement atomically; the owner drains when its last
  snapshot reference is dropped.
- Plugin startup validates configured identity. Respawn compares a canonical
  encoding of the complete initialize declaration: plugin metadata, tools,
  hooks (including command/permission declarations), and workspace adapters.
  Declaration drift closes the new process and makes later calls fail closed.
- This release adds no plugin watcher, hot-add/remove/reload command, dynamic
  hook/control plane, permission interceptor, lease/fence, Bundle behavior, or
  new dependency.

### 5C.4 Focused proof and dependency audit

Focused GREEN covers the six required reconciliation cases, typed candidate
failure, no-generation-on-failure, old-owner lifetime, complete plugin
declaration order independence, configured plugin identity, startup mixed
publication, and Compat MCP add/remove callability through the same registry.
The new explicit test-only defer seam removed an environment-variable race
between synchronous and deferred MCP startup tests without changing production
defaults or test serialization.

GREEN was re-run with the same commands above plus these integration commands:

```sh
cargo test -p hya-app runtime_reconcile::tests
cargo test -p hya-app startup_mixed_mcp_plugin_publishes_one_complete_generation
cargo test -p hya-app compat_mcp_control_publishes_and_removes_through_one_runtime_registry
cargo test -p hya-plugin initialize_declaration_is_order_independent_and_complete
cargo test -p hya-server --test compat_mcp_api
cargo test -p hya-server --test compat_mcp_dynamic_api
cargo test -p hya-server --test compat_experimental_resource_api
```

`crates/hya-plugin/Cargo.toml` and `Cargo.lock` had an empty before/after diff
before the version bump. Declaration hashing uses `hya-app`'s pre-existing
workspace `sha2` dependency; `hya-plugin` only emits deterministic canonical
bytes using its existing `serde_json` dependency. No dependency edge was added.

### 5C.5 Release/document boundary

Workspace/TUI/README advance exactly `0.34.5 -> 0.34.6`; the old root
changelog is archived as `docs/changes/CHANGELOG_0.34.5.md`, and root
`CHANGELOG.md` contains only `0.34.6`. Existing MCP/plugin configuration is
reused, and `docs/configuration.md` contains a minimal runnable local MCP
fixture plus the actual add/status/disconnect/connect route and payload flow.
No new user-configurable field, command, or agent-facing self-operation is
added, so no separate example framework or built-in skill is warranted; no
placeholder skill is added.

The documentation claim is deliberately narrow: dynamic MCP
desired-observed-effective reconciliation is supported. Plugin support is only
startup/crash re-handshake consistency for tool exports plus their RPC binding,
not plugin hot add/remove/reload or whole-plugin snapshotting. The controlling
MacBook Air correction and advisory provenance are recorded in
`research/browser-pro-escalation-protocol.md` as
`CONSULT-2026-07-31-RUNTIME-RECONCILIATION-09`.

### 5C.6 Exit gate

All local release gates are green:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --bins
cargo build --workspace
cargo build --locked -p hya -p hya-backend -p hya-ts --bins
(cd packages/hya-tui-ts && bun run typecheck)
(cd packages/hya-tui-ts && bun run build)
(cd packages/hya-tui-ts && CI=true bun test)
bash scripts/verify-no-http.sh
target/debug/hya --version
target/debug/hya-backend --version
target/debug/hya-ts --version
```

The CI-mode TUI result is 44/44, zero-INET reports `OK`, and all three native
executables report `0.34.6`. The sandboxed first passes could not bind loopback
sockets/use ptrace or download an isolated Bun runtime; the identical commands
passed outside that sandbox. No product exception was added.

The full workspace gate exposed and then closed two test-observer races without
weakening owner-lifetime or candidate-invalidation coverage:

- `cargo test --workspace --test respawn_declaration_drift -- --nocapture`
  deterministically observed the close marker file between create and write;
  the test now waits for the exact `2\n` marker, with the same timeout and
  product shutdown path.
- The mixed-source test used a second `bind_turn` as if it were a read-only
  snapshot accessor. Parallel test HOME changes could legitimately make that
  call publish a skill view. It now compares the reconciliation outcome to the
  effective source manifest and current schemas; the separate explicit-removal
  test still proves old/new `TurnBinding` behavior unchanged.

Metadata/JSON/JSONL and `git diff --check` pass. `Cargo.lock` changes only the
workspace package versions; every dependency list is identical and no crate
`Cargo.toml` adds an edge. The stage closed at
`680f9fb535fc48f71f9aead64cc3d3d30161678a`; draft-PR CI run `30634501761`
attempt 2 is fully green.

## 5D. Active release — `0.34.7`

### 5D.1 Exact boundary and authoritative source corrections

The stage starts from the clean, remote-green `0.34.6` commit above. The
MacBook Air ruling is recorded as
`CONSULT-2026-07-31-RESIDENT-FENCING-10`. Direct source inspection corrected
the abstract proposal as follows:

- the stable resident `ActorId` is the already-persisted
  `AgentRegistered.agent_session`/roster `SessionId`; `MemberId` remains parent
  tree lifecycle metadata;
- `0.34.4` already owns a literal `admission_journal`, so resident operations
  add nullable actor identity/epoch there rather than creating an effect
  journal;
- roster/mail are durable, but the resident cursor and running boundary were
  memory-only, requiring exactly one `ResidentWorkStarted` event;
- `RuntimeSnapshot`/`TurnBinding` lifetime is independent from actor epoch and
  continues to drain through retained `Arc`s.

`OwnerRunId` is generated once per process. The claim table contains only
stable actor ID, monotonic epoch, process owner, and `active|released`; it has
no TTL, time field, heartbeat, background task, or distributed/HA semantics.

### 5D.2 Deterministic RED evidence

The ordered RED cases failed for the expected missing behavior before their
GREEN seams existed:

```sh
cargo test -p hya-store --test resident_claim concurrent_claims_allow_exactly_one_owner
cargo test -p hya-store --test resident_claim restart_recovery_increments_epoch_and_invalidates_old_claim
cargo test -p hya-core --test resident_recovery stale_tool_or_child_completion_cannot_append_or_advance_projection
cargo test -p hya-core --test resident_recovery takeover_aborts_and_refunds_bound_operation_exactly_once
cargo test -p hya-core --test resident_recovery queued_resident_message_resumes_but_running_message_aborts
cargo test -p hya-core --test resident_recovery repeated_startup_recovery_produces_identical_projection_and_no_duplicate_terminal_events
cargo test -p hya-core --test resident_recovery transient_non_resident_paths_do_not_require_actor_claim_or_change_events
```

The first failures respectively showed the missing claim API, recovery fence,
claim-aware event commit, actor-bound admission recovery, queued/running
marker, repeatable recovery composition, and transient characterization. The
final audit added three focused REDs: a recovered running user turn was
incorrectly reclassified as queued on the second startup; a claim-less
finalizer could terminalize an actor-bound admission; and running child/tool/
assistant projection state remained nonterminal after takeover. Each failed
on the named assertion before the narrow fix.

The final authority/lifetime pass added two more deterministic REDs:

```sh
cargo test -p hya-store --test admission startup_recovery_leaves_actor_bound_operation_for_fenced_takeover -- --nocapture
cargo test -p hya-store --test admission release_claim_aborts_bound_operation_before_releasing_actor -- --nocapture
```

The first showed the 0.34.4 global startup abort consuming an actor-bound row
before the recovered claim transaction could see it. The second showed claim
release leaving a bound `started` admission orphaned. Both failed on their
named assertions before the SQL transitions were narrowed.

### 5D.3 Minimal GREEN and recovery order

- Migration `0005_resident_actor_claim.sql` adds the sole claim table and
  resident-only actor columns/index to `admission_journal`.
- `try_claim_new`, `recover_claim`, and `release_claim` are indexed SQLite
  compare-and-set transactions. Ordinary concurrent claims have one winner;
  recovery advances the epoch before any runtime service becomes ready.
  Release atomically aborts nonterminal admissions for that exact actor/epoch
  before marking the full claim tuple released, and returns only first-release
  rows to the existing governor refund seam.
- Startup first fences all active claims, then performs fail-closed 0.34.4
  admission recovery for non-actor operations. Actor-bound operations remain
  for the current recovered-claim transaction; startup then replays roster/
  session projections, aborts old running work, and recreates the existing
  `ResidentSupervisor` slots. Only queued not-started mail is notified.
- `ResidentWorkStarted` commits before provider/tool/child dispatch. Recovery
  terminalizes in-flight tool parts, assistant messages, and child members
  using existing events before clearing the root running marker. Repeated
  recovery observes terminal state and emits no duplicate terminal event.
- Resident event/mailbox/child commits and actor-bound admission transitions
  validate the full claim in the same SQLite transaction as canonical state.
  Tool execution checks immediately before dispatch and after the external
  result before plugin post-processing; result publication is transactionally
  fenced. Root-turn cleanup cannot mutate actor-bound admissions.
- Transient work carries no claim, performs no claim-table lookup, and retains
  its prior event shapes. Actor epoch and runtime generation remain orthogonal.

### 5D.4 Claim boundary and non-goals

The release claims deterministic single-process resident crash recovery and
canonical-state fencing only. Filesystem, network, provider, and third-party
effects that occurred before takeover are not reversible or externally
exactly once. Running/in-flight work is never automatically retried. There is
no TTL/time lease, HA/active-active, generic supervisor/outbox/effect system,
new permission/sandbox behavior, AgentBundle work, or 100/256 capacity proof.

No new user configuration or agent-facing self-operation is introduced, so
`0.34.7` deliberately adds neither a runnable user example nor a built-in
skill. Architecture, storage, event-model, ADR, changelog, and this task's
defect/roadmap evidence are the appropriate documentation surface.

### 5D.5 Focused GREEN and exit gate

Focused GREEN includes the complete store claim/admission suites, all resident
recovery cases, existing resident behavior, and the app startup test that
observes running-work termination and queued-mail scheduling before readiness.
It also includes release-time abort/refund-once and the proof that global
startup recovery leaves actor rows for fenced takeover.

The local exit gate is green:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --bins
(cd packages/hya-tui-ts && bun run typecheck)
(cd packages/hya-tui-ts && bun run build)
(cd packages/hya-tui-ts && CI=true bun test)
bash scripts/verify-no-http.sh
target/debug/hya --version
target/debug/hya-backend --version
target/debug/hya-ts --version
```

The workspace test initially exposed only a stale README release label; after
the required `0.34.7` alignment, the full suite passed. Existing loopback and
strace fixtures required their normal unsandboxed CI permissions. The first
two full TUI attempts reached 43/44 with the known 140-column PTY observation
flake at different callsites; the focused PTY suite then passed 4/4 and the
unchanged full suite passed 44/44 without timeout, retry logic, concurrency, or
TUI product changes. The no-INET gate reports `OK: zero inet sockets`, and all
three local executables report `0.34.7`.

Exact staging, commit/push, and draft-PR remote CI are still required; their
final results are appended here before delivery. `0.34.8` remains unauthorized.

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
      AgentBundle `harness_access: none|basic|full`, and separate
      `resource_view` aliases/allow/deny/namespace so Bundle input can only
      narrow. Until a typed permission-overlay consumer exists, preparation
      rejects that field instead of retaining inert policy metadata.
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

**Controlling release:** `0.34.7`, with the concrete implementation and proof
recorded in section 5D. The older generalized lease/effect-journal design is
superseded for this stage.

### Delivered slices

- [x] one TTL-free actor claim row per stable resident session identity;
- [x] one-winner ordinary claim, monotonic startup recovery, and full-tuple
      idempotent release;
- [x] nullable actor ID/epoch on the existing admission journal only;
- [x] one resident-work-start marker plus projected inbox cursor;
- [x] fence-first startup, abort/no-retry running work, resume queued work, and
      recreate the existing supervisor before readiness;
- [x] claim-aware canonical event, mailbox, child, spawn-admission, tool
      pre-dispatch, and result-commit paths; transient behavior unchanged.

### Required exit gates

The deterministic matrix in section 5D must remain green together with the
full workspace/TUI/no-INET/executable gates and same-PR remote CI. No TTL,
generic effect state machine, remote-effect retry/reconciliation, scheduler,
AgentBundle, or capacity certification is implied. If delivery is rolled back,
the runtime must first be quiescent; a stale/older epoch is never selected as a
recovery mechanism.

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
definitions never silently execute another agent. Consult19 resolves the old
unknown-new-spawn hold: only an omitted agent selects stable ID `general`;
an explicitly supplied unknown ID returns typed `UNKNOWN_AGENT_ID`.

### `0.34.9` — package distribution and registry

Historical planning only: this heading is retained for audit history.
Sections 18.4.1 and 18.4.2 supersede the actual 0.34.9/0.34.10 release split;
external runner work remains deferred with no replacement version assigned.

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

Historical planning only: this heading is retained for audit history.
Sections 18.4.1 and 18.4.2 supersede the actual 0.34.9/0.34.10 release split;
external runner work remains deferred with no replacement version assigned.

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

## 17. `0.34.7` bounded PTY continuous-drain CI repair

Draft-PR run `30643007465` failed its unchanged 80-column PTY fixture twice:
attempt 1 timed out after 58.439 seconds at `grandchild permission in Main`,
while attempt 2 timed out after 51.063 seconds at the earlier
`root session frame`. Both 140-column cases passed and the fixture blob
`1628cedcd840187ac22493a02b7574a658f35e30` is identical to green `0.34.6`.
The MacBook Air Browser/Pro ruling in
`CONSULT-2026-07-31-PTY-CONTINUOUS-DRAIN-11` authorizes exactly one separate,
test-only repair without amending feature commit `6f3402e`.

Execution order:

1. Add `drains_child_stderr_while_waiting_for_stdout_marker` beside the narrow
   PTY input/process helpers. Use a real Bun child that writes a fixed large
   stderr payload ending in a sentinel before stdout `DONE`; prove waiting on
   stdout alone remains blocked because stderr is not drained.
2. Add one test-local, single-reader continuous drain with a 64 KiB byte-bounded
   tail. The server stdout reader must detect readiness incrementally across
   chunk boundaries and keep draining with that same reader.
3. Start drains immediately for server stderr, recovery-PTY stderr, and
   main-PTY stderr. Keep TUI stdout ignored, semantic input write/flush,
   wait durations, assertions, widths, and concurrency unchanged.
4. Bound cleanup and failure diagnostics: wait/callsite, monotonic phase trace,
   last PTY frame, and applicable stream tails. Do not add a process framework,
   product sidechannel, endpoint, retry, dependency, or version/changelog edit.
5. Run focused RED/GREEN, TUI typecheck/build/PTTY/full tests, every existing
   Rust/executable/zero-INET release gate, exact diff/accounting, then commit and
   push one follow-up change and require one fresh full remote run to pass.

Evidence state before RED: `Pro-advised-pending-verification`. A further
event-applied/focus-changed test sidechannel is explicitly unauthorized unless
the new CI trace proves the backend event was emitted, every pipe drained, and
the corresponding UI state/render was still absent.

### 17.1 Controlling corrected RED and claim boundary

The second MacBook Air Browser/Pro ruling withdraws the subprocess
backpressure/root-cause premise above. Experiments from 4 MiB
`process.stderr.write` through 64 MiB Bun FileSink output reached stdout
`DONE` while the parent stderr JS stream remained unread; low-level fd writes
ended in `EAGAIN`/environment errors instead of a blocked-child RED. No larger
payload, fd2 retry/polling, IPC, or subprocess experiment is permitted.

The corrected one-slice loop supersedes steps 1–2 above:

1. RED a pure injected `ReadableStream<Uint8Array>` contract for local
   `startBoundedDrain(stream, readinessMatcher?)`: split readiness across two
   chunks, resolve it before later chunks, consume more than 64 KiB through
   EOF, retain a byte-bounded tail ending in a sentinel, and acquire exactly
   one reader. The pre-GREEN stdout-only/no-tail behavior must fail a behavioral
   assertion, not compilation.
2. GREEN the smallest one-reader drain with streaming UTF-8 decoding,
   cross-chunk readiness, continued EOF consumption, 64 KiB byte tail, and
   explicit cancel/settlement. Wire only already-piped server stdout/stderr and
   recovery/main TUI stderr immediately after spawn.
3. Add bounded diagnostics at the two observed seams only:
   `root-session-frame/80` and `grandchild-permission-in-main/80` (with the
   actual width substituted by the case). Include monotonic phase, last frame,
   relevant bounded stream tails, and child state without changing waits,
   assertions, retries, input, or concurrency.

The delivery claim is only “test-harness lifecycle, byte-bounded buffering,
and diagnostics repair.” Whether it mitigates the CI flake remains
`benchmark-unconfirmed` until the single fresh-SHA full CI run succeeds.

### 17.2 Causal permission-render ordering gate

The corrected drain helper passed its deterministic contract, but the one
local focused integration run remained red at
`grandchild-permission-in-main/80` while the 140-column case passed. Therefore
the drain work is disproven as a sufficient repair and must not survive the
final commit.

The controlling `CONSULT-2026-07-31-PTY-EVENT-ORDER-12` allows one diagnostic
run before any product edit:

1. Immediately before the existing Escape, fetch and lock pending permission
   ID `P`, the proxy request-log cursor, and transcript output cursor.
2. After the existing semantic write/flush, use the existing `waitFor` budget
   to compare first transcript occurrence of `Permission required`, the real
   `POST /permission/{P}/reply`, and whether `permission.list` still contains
   `P`.
3. Only a reply/disappearance while no prompt has ever appeared emits the exact
   RED `ESCAPE_PROPAGATED_TO_NEW_PERMISSION_PROMPT`. A timeout with no reply and
   `P` still present disproves the hypothesis and freezes product code.
4. Only after the exact RED, reorder the two existing observation-Escape
   handlers so propagation is consumed/prevented before `focusMain`. Do not
   change any other key, focus, permission, SSE, layout, or wait behavior.
5. On GREEN, remove every bounded-drain helper/test/wiring/tail/cleanup change
   and restore the original single server-readiness reader. Retain the causal
   regression, narrow callsite/phase/last-frame diagnostics, the minimal proven
   product fix, and Trellis evidence only.

No second focused run, full gate, product edit, commit, push, or CI is allowed
unless the prior conditional gate succeeds exactly as specified.

### 17.3 `confirmMainInput` transcript-oracle gate

The one event-order run did not emit the authorized permission-order RED: its
80-column case completed normally, while 140 columns failed earlier at
`m62d1 Main focus`. `CONSULT-2026-07-31-PTY-TRANSCRIPT-ORACLE-13` identifies
the integration RED as an invalid pre-marker oracle in `confirmMainInput`.

Execute exactly once:

1. Keep one `writeSemanticInput(Escape)` and its awaited flush inside
   `confirmMainInput`; bypass its two calls that wait for an already-existing
   `rootDraft` to be re-emitted before sending input.
2. Immediately send the existing caller-supplied marker once through the same
   semantic write/flush helper, then use the unchanged wait budget to require
   both marker and `rootDraft` in the transcript delta since `start`.
3. Do not alter `focusMain`, any other `waitForMain` callsite, product code,
   wait duration, sleep, retry, key count, marker, width, or concurrency.
4. Run the current five-test focused file once. Marker absence, draft absence,
   a typed permission-order failure, or any other non-green result freezes the
   branch without another run. Only all-green permits final cleanup.
5. If green, remove every bounded-drain experiment by patch while preserving
   this oracle correction, the locked-P reply/render regression, concise
   callsite/phase/last-frame diagnostics, and the full consultation record.
   Then run one full TUI suite plus every required Rust/executable/zero-INET
   gate before an exact atomic test commit and one fresh-SHA CI run.

#### 17.3.1 Local completion evidence

The one permitted intermediate focused run passed 5/5:

```text
semantic_input_flushes_before_next_action                         pass
bounded drain continues after readiness with one reader          pass
Linux PTY renders home, opens a session, and restores terminal   pass
Linux PTY 80-column subagent workspace                            pass (14.893s)
Linux PTY 140-column subagent workspace                           pass (11.877s)
```

This confirmed the transcript-oracle correction without authorizing or
requiring a product change. Cleanup then removed the bounded-drain helper,
contract test, stream wiring/tails, and extra cleanup, and restored both server
readiness paths to the exact original single reader. No standalone focused run
was made after cleanup, as required.

Final local gates:

```sh
cd packages/hya-tui-ts
bun run typecheck                         # pass
bun run build                             # pass
CI=true bun test                          # pass, 44/44

cd ../..
cargo fmt --all --check                   # pass
cargo clippy --workspace --all-targets -- -D warnings  # pass
cargo test --workspace                    # pass outside restricted sandbox
cargo build --workspace --bins            # pass
cargo build --workspace                   # pass
cargo build --locked -p hya -p hya-backend -p hya-ts --bins  # pass
target/debug/hya --version                # hya 0.34.7
target/debug/hya-backend --version        # hya-backend 0.34.7
target/debug/hya-ts --version             # hya-ts 0.34.7
bash scripts/verify-no-http.sh             # OK: zero inet sockets
```

The first sandboxed workspace-test attempt failed only because the existing
OAuth callback fixture could not bind loopback (`Operation not permitted`);
the identical full command passed outside that sandbox. `git diff --check`
passes. The final code scope is `pty-smoke.test.ts` plus the two existing
Trellis evidence files; there is no product, dependency, lock, workflow,
version, or changelog diff. Exact staging, one semantic follow-up commit, push,
and one fresh full remote CI run remain pending.

### 17.4 Consult14–16: `/global/event` pending catch-up

- Consult14 source audit found the PTY proxy was a direct SSE pass-through, so
  byte tracing, a second subscription, and a transforming stream were rejected.
- Consult15 identified the bounded backend gap: pending permission/question asks
  could predate `/global/event` subscription or be skipped after broadcast lag.
- Consult16 approved the final simplification below. Full consultation metadata
  and determination status live only in the Browser/Pro ledger.

#### Final behavior

- Subscribe to engine, permission, and question live streams before taking
  short-lock snapshots; emit `server.connected`, current pending asks, then live.
- On permission/question `BroadcastStream` lag, one private lazy helper snapshots
  current pending exactly once; normal values bypass snapshotting.
- Delivery is at-least-once by stable request ID. There is no sorting, dedup,
  cursor, history replay, second SSE route change, or TUI/bootstrap change.
- Pending owners clone only necessary typed state under lock and serialize after
  release. Fixed production broadcast capacity remains 256.

#### TDD index

- Initial HTTP REDs: pending permission/question each lost before the opposite
  live sentinel; both GREEN after subscribe-before-snapshot publication.
- Historical capacity-one REDs missed stable `perm_...01` / `q_...01`; both
  GREEN after lag snapshot recovery. These fixtures were removed by Consult16.
- Consult16 mutation checkpoint: real `broadcast::channel(1)` produced Lagged;
  `Lagged => []` failed against `[P1]`. Final helper returns P1 with one lazy
  snapshot call; `Ok(P2)` returns P2 with zero additional calls.

#### Narrow verification and status

- Helper contract: pass 1/1; Compat permission/question integration: pass 8/8.
- `cargo fmt --all --check` and `git diff --check`: pass.
- Full release gates, exact staging, commit/push, and one fresh-SHA CI run remain
  pending. Version stays 0.34.7 and 0.34.8 remains blocked.

### 17.5 Consult17–18: sandbox classification and synchronous Main input ownership

- Consult17's exact fixture-equivalent backend probe retained binary SHA-256
  `746e85099073f5f621857156ac0bb537aad641a5621ce15f7df10a9fe855f051`
  and proved the restricted startup failure was loopback-bind EPERM. The first
  non-sandbox focused run then reached an independent 80-column
  `CONFIRM_MAIN_MARKER_MISSING` product seam.
- Consult18 selected synchronous Prompt ownership after `focusMain` dispatch
  when the existing ref is present and no modal owns input. The existing
  reactive focus effect remains the fallback; chords, layout, PTY waits, keys,
  markers, timeouts, and retries are unchanged.
- RED: focused unit target 26/27, expected `focusMain -> focus -> return` but
  observed `focusMain -> return`; absent-ref and modal-active cases passed.
- GREEN: unit 27/27, typecheck and build pass; the one non-sandbox focused PTY
  run passed 4/4, then the unchanged full TUI suite passed 47/47.
- Pending: coordinator diff review, exact staging, commit/push, and one fresh
  full CI run. Version remains 0.34.7; 0.34.8 remains blocked.

## 18. Active release — `0.34.8` native Bundle cutover

### 18.1 Consult19 authority and verified baseline

`CONSULT-2026-07-31-NATIVE-BUNDLE-CUTOVER-19` authorizes exactly two atomic
commits on the existing task/worktree/branch/PR. The preflight baseline is the
clean, upstream-equal `0.34.7` tip
`064ede0b4fe4601b84ccbe912c75980449527d2c`; draft PR #24 and CI run
`30657044177` are green. Protected main remains at
`267bfc3c6c66e46fe8514e2e70657489f853b7f0` with 20 status entries and the
three recorded stashes unchanged.

Controlling corrections supersede older unresolved or overloaded Bundle
language:

- omitted agent alone resolves to stable ID `general`; explicit unknown is
  typed `UNKNOWN_AGENT_ID`; replay preserves historical `AgentName` bytes and
  continuation requiring a removed definition is typed
  `AGENT_DEFINITION_MISSING`;
- `role` controls only TUI selector visibility, while `can_spawn` is the sole
  Bundle reachability graph and internal/model-facing rosters retain eligible
  subagents;
- `harness_access = none|basic|full` chooses the Harness resource candidate
  set, then `resource_view` narrows it with allow/deny/aliases/namespace;
  neither can expand the current PermissionPlane or plugin authority;
- runtime consumes only embedded prepared catalog bytes. It never scans source
  Bundles, examples, ordinary Markdown, old JSON/JSONC/Markdown agents, or an
  installed-package placeholder;
- executable features without an existing 0.34.8 consumer are rejected as
  `UNSUPPORTED_BUNDLE_FEATURE`; external JS/Rust/MCP execution stays in
  `0.34.10`.

### 18.2 Atomic commit 1 — inert preparation

Commit subject: `feat(bundle): prepare native builtin bundles`.

RED order:

1. freeze exact source behavior for the seven product IDs and eight tracked
   development IDs, including stable bytes, prompt/model/reasoning/workdir,
   effective metadata/visibility, and replay/fork identity;
2. identical sources produce identical prepared bytes, digest, and sorted
   index independent of filesystem iteration order;
3. exact `bundle.hya.md` input requires both v1 markers;
4. unknown fields, missing references, duplicate stable IDs, namespace/alias
   collisions, wrong kind, and unsupported executable features fail typed;
5. bundle-local short names win while qualified identities remain exact;
6. `hya/core-agents` and `hya/development` prepared sources match frozen
   fixtures.

GREEN is limited to dependency-light `crates/hya-bundle`, deterministic
built-in sources under `bundles/builtin`, and the `hya-app` build boundary that
prepares canonical embedded bytes without activating them. RuntimeSnapshot,
SessionEngine, server/TUI endpoints, and old loaders remain unchanged. Version
and release metadata remain `0.34.7`. Full Rust/bin/zero-INET gates and green
remote CI are required before commit 2 begins.

Consult20 resolves the source-mapping hold for the three effective hidden
native definitions: `compaction`, `title`, and `summary` prepare as
`role=subagent`, and no ordinary built-in agent's `can_spawn` includes them.
There is no second `hidden` IR field. This intentionally removes their legacy
accidental explicit-spawn reachability while retaining their exact stable IDs,
prompt/model/reasoning characterization, and fixed Harness system use.

#### 18.2.1 Commit 1 RED→GREEN evidence

The inert preparer was developed one missing contract at a time. Each RED was
an intended public seam or behavioral assertion, never a deliberately broken
product sorter:

- missing `BundleSource`/`prepare_builtins`, directory-reader,
  `PreparedCatalog::decode`, and pure `BundleCatalog::from_prepared` seams first
  failed to compile at their consumer tests, then passed with the minimal API;
- deterministic preparation initially lacked full alias/reference validation,
  normalized provenance, content-digest verification, canonical decoded-vector
  checks, and stable-ID-versus-qualified-ID collision checks; each focused test
  failed on the exact accepted-invalid candidate before the corresponding
  validation was added;
- source `resource_profile` initially returned the wrong manifest error, while
  executable tool/MCP/hook/JS/Rust declarations and unknown fields were proven
  rejected rather than retained inertly;
- the first built-in parity run found the native `title` prompt missing the
  current exact `in App` suffix; the corrected source then matched all frozen
  prompt bytes;
- the app build-boundary test first failed because the embedded OUT_DIR
  artifacts did not exist, then passed after deterministic build-time prepare;
- a release-discipline assertion found both inert bundle sources prematurely
  labeled `0.34.8`; they now remain `0.34.7` until the activating commit bumps
  the whole release once.

Current focused GREEN commands:

```sh
cargo test -p hya-bundle
cargo test -p hya-app --test builtin_bundle_build
cargo clippy -p hya-bundle --all-targets -- -D warnings
```

They cover deterministic bytes/digests/index, source-order independence,
Markdown markers, canonical payloads/paths/vectors, outer and inner tamper
rejection, second-pass cross-bundle references, namespace/alias conflicts,
unsupported features, exact current catalog metadata and prompts, Consult20
roles/reachability, and historical AgentName wire/projection bytes. Commit 2
still owns runtime selector/roster/spawn/system-call/resume behavior. The pure
historical fixture clones every projected stable ID into a second
`SessionCreated` fork fixture and proves the wire/projection bytes remain
unchanged without editing the current server runtime.

#### 18.2.2 Commit 1 local release gates

```sh
cargo fmt --all --check                                  # pass
cargo clippy --workspace --all-targets -- -D warnings   # pass
cargo test --workspace                                   # pass outside restricted sandbox
cargo build --workspace --bins                          # pass
cargo build --locked -p hya -p hya-backend -p hya-ts --bins  # pass
target/debug/hya --version                              # hya 0.34.7
target/debug/hya-backend --version                      # hya-backend 0.34.7
target/debug/hya-ts --version                           # hya-ts 0.34.7
bash scripts/verify-no-http.sh                           # OK: zero inet sockets
```

The first sandboxed workspace test failed only at the existing OAuth callback
loopback bind with `Operation not permitted`; the exact command passed once
outside the restricted sandbox. The first sandboxed zero-INET gate was blocked
because `strace` reported `PTRACE_TRACEME: Operation not permitted`; the exact
gate passed once outside the sandbox. No TUI/package/runtime source or version
changed in this inert commit, so no TUI behavior gate, example, or skill is
introduced. JSONL parsing, absence of `context.jsonl`, diff checks, exact scope,
protected-main accounting, atomic staging, push, and remote CI remain the final
Commit 1 gates.

### 18.3 Atomic commit 2 — one-authority cutover (WIP evidence)

Commit subject: `feat(agent): cut over to native bundles`.

**Overall status:** **`LOCAL-GATES-GREEN` / `PENDING-COMMIT-PUSH-REMOTE-CI`**.
Uncommitted `0.34.8` Commit 2 WIP in this worktree. Workspace/release metadata
is `0.34.8`; version/changelog/archive applied. Focused cutover contracts below
remain **FOCUSED-VERIFIED** by direct source + named tests. Local full gates
are green (evidence below). Staging, commit, push, and remote CI are **not**
claimed.
No 0.34.9 work. Capability matrix:
`research/agent-capability-parity-matrix.md`.

#### Implemented authority (source)

| Contract | Primary symbols |
| --- | --- |
| Single catalog Arc | `RuntimeSnapshot.catalog: Arc<BundleCatalog>`; `RuntimeRegistry::new`; `TurnBinding` pins snapshot |
| Bootstrap decode once / fail-closed | `hya_app::runtime::builtin_catalog` (`OnceLock`); `PreparedCatalog::decode`; empty/corrupt/digest fail closed |
| Spawn / roster | `TurnBinding::resolve_spawn` / `spawnable_agents`; empty/`None` → `general`; explicit missing → `UNKNOWN_AGENT_ID` |
| Reserved system | Exact `resolve_agent` for `compaction`/`title`/`summary`; ordinary spawn not on `can_spawn` |
| Inline overlay | `resolve_spawn_member`: authorize base, then request-scoped name/prompt/model/category/resident; `inline.description` → `UnsupportedInlineAgentField` |
| Compiled resource view | `compile_agent_resources` → `Arc<CompiledResourceView>` shared by schema/skill/dispatch |
| Guidance | Server pre-renders → `Option<Arc<str>>`; `agent_with_guidance_layer`; child/resident carry Arc; not in catalog/wire |
| Legacy deletion | `AgentCatalogPlane` and compat agent parsers/discovery/`subagent_resolve` removed; tracked `.hya/agents/*` deleted |
| Docs / skill | `docs/examples/bundle.hya.md`, `docs/agent-bundle-authoring.md`, `agent-bundle-authoring` skill; `plan` description corrected (no “disallows all edit tools”) |

#### Focused RED→GREEN evidence (already in tree)

Concise test evidence only (no re-litigation of Consult21–24 rulings):

| Area | RED (missing behavior) | GREEN (current focused tests) |
| --- | --- | --- |
| Catalog Arc + binding pin | Registry/turn could diverge from catalog authority | `runtime_registry` suite; `runtime_turn_binding::admitted_turn_uses_one_binding_for_prompt_schema_skill_and_dispatch`; `builtin_catalog_initializes_once_and_shares_arc` |
| Fail-closed bootstrap | Empty/corrupt embedded catalog could soft-fail | `zero_bundle_prepared_document_cannot_bootstrap_registry_catalog`; `corrupted_prepared_bytes_or_digest_fail_closed_with_decode_context` |
| Role vs can_spawn | Role incorrectly gated spawn/roster | `role_selector_vs_can_spawn_roster::can_spawn_roster_includes_reachable_subagent_excludes_unlisted_main_and_system`; TUI `agent-visibility.test.ts` |
| Omitted vs unknown | Explicit unknown fell back to general | `task::omitted_subagent_type_selects_general`; `spawn_admission::explicit_unknown_inline_target_creates_no_child` (+ batch zero side-effects) |
| Reserved system exact lookup | Hardcoded prompts / ordinary spawn of system IDs | `fixed_system_agents::*` (title/summary/compaction exact Bundle resolve, missing → `AGENT_DEFINITION_MISSING`, roster exclusion) |
| Historical identity / continue | Replay rewrite or silent general on missing def | `historical_agent_identity::*`; `root_turn_missing_definition_fails_closed_without_general_fallback` |
| Inline overlay | Catalog mutation / unknown child | `authorized_inline_overlay_executes_without_catalog_entry`; `inline_child_spawns_through_its_authorized_base_roster`; unknown path above. Description typed-reject is **FOCUSED-VERIFIED** by `spawn_admission::inline_description_is_unsupported_before_admission_without_side_effects` |
| Resource view | Schema ≠ dispatch; silent allow/deny drift | `agent_resource_view::{harness_access_filters_schema_dispatch_and_skill_prompt,canonical_allow_deny_and_alias_share_schema_and_dispatch,mcp_selected_public_name_dispatches_once_with_canonical_permission}` + `runtime_registry` alias/deny units |
| Guidance composition | RED: guidance in `AgentSpec.system_prompt` wiped by Bundle replace (`available_references` 0) | GREEN: `compat_prompt_bundle_prompt_and_reference_guidance_parity`; `bundle_prompt_replaces_base_but_preserves_guidance_once_and_in_order`; `bundle_prompt_none_preserves_harness_base_then_guidance`; `guidance_captured_once_across_provider_rounds`; spawn `transient_child_uses_triggering_turn_guidance_once_without_child_scan`; `resident_activations_reuse_in_process_triggering_guidance`; `nested_spawn_inherits_same_immutable_guidance`; `resident_guidance_is_ephemeral_not_persisted_in_events` |
| Root Bundle precedence | Base/Bundle/session/skills order wrong | `root_turn_bundle_precedence::*` |
| Docs example prepare | Example invalid for preparer | `docs_example::docs_example_bundle_hya_md_prepares_deterministically` |
| v1 typed reject (historical B fields) | Silent ignore of unsupported manifest keys | `validation::invalid_schema_references_and_executable_features_fail_typed`; `unsupported_resource_profile_is_rejected_as_a_feature_not_ignored` |

#### Local full-gate evidence (final; concise)

- `cargo fmt --all --check` pass
- `cargo clippy --workspace --all-targets -- -D warnings` pass
- `cargo test --workspace` full pass outside restricted sandbox (sandbox-only
  OAuth bind `EPERM`; two real stale `0.34.8` README/builtin parity expectations
  fixed, targeted tests passed, then final full green run)
- `cargo build --workspace --bins` pass
- `cargo build --locked -p hya -p hya-backend -p hya-ts --bins` pass
- All three version probes `0.34.8`
- zero-INET pass outside sandbox after ptrace restriction
- TUI `bun` typecheck/build pass
- One final `CI=true` focused PTY run 4/4
- One full `CI=true` `bun test` run 49/49
- `task.py validate` implement=49 / check=47 pass
- `git diff --check` pass

#### Still pending (do not claim)

- Staging, commit, push, and one green remote CI run remain unclaimed.
- No claim that global SKILL.md `allowed-tools`/`model`/`license` enforcement
  is a Bundle GA item (those are skill-catalog fields, not AgentBundle v1).

#### 18.3.1 Release-prep metadata + formatter fixture gate repair (focused)

**Status:** focused release-prep for activating `0.34.8` is applied in this dirty
worktree (version/changelog/archive/identity alignment + matrix `general` role
fix). Local full gates are green (**`LOCAL-GATES-GREEN`**); staging/commit/push/
remote CI remain **`PENDING-COMMIT-PUSH-REMOTE-CI`** (unclaimed).

**Test-only formatter fixture gate repair (no product behavior change):** the
`pint` vendor executable fixture still hit non-sandbox `ETXTBSY` after
write/`sync`/drop. Replaced the freshly written executable inode with a
`/bin/sh` symlink fixture while preserving the `./vendor/bin/pint` target
invocation path. Exact focused and full `hya-tool` suites green afterward.

**Earlier green focused/targeted suites (superseded by full local envelope above):**

- `hya-tool` focused + full suite
- `hya-server` full suite
- `hya-bundle` / `hya-backend` targeted suites
- TUI typecheck / build / agent-visibility focused

**Still pending (explicit):** staging, commit, push, remote CI. Not claimed here.

#### 18.4 0.34.9 execution model and first RED

Current authority is Sol-max (`gpt-5.6-sol`, max reasoning), the canonical coordinator/reviewer
in session `019fbf81-c41e-7450-8c17-e4036548bc33`, over this same task,
worktree, and branch. Bounded native `luna-worker` agents perform deterministic
file edits, commands, tests, fixture work, and return raw evidence/mechanical
summaries; they never commit, push, create topology, stash, or decide
architecture, safe inheritance, handoff, stage/goal completion.

Sol independently checks raw evidence and exclusively owns architecture/stage
slicing, RED/GREEN acceptance, diff review, risk/invariants/completion
judgments, every session/stage/worker handoff and checkpoint reconstruction,
next-executor prompts, Browser/Pro escalation packets, atomic commit/push,
exact-SHA CI interpretation, and stage transition. Earlier Grok 402, Luna/Terra
implementation turns, and Mac-driven Sol/Terra alternation remain accurate
historical evidence but are superseded as current rules.

The 0.34.9 plan required each atomic commit to receive its own push and unique
green remote CI run. Commit 1 owned package inspection, safe public 7z, private
structural inspection, and the SQLite registry core. The original plan assigned
Commit 2 the four-command CLI, lazy publication, and documentation while
staying on version 0.34.9; section 18.4.2 supersedes that version boundary.

The first TDD slice is one `hya-bundle` integration RED only:
`HYABNDL\0` plus little-endian envelope version 1 must be recognized as
private/v1 solely from bytes magic/version, without a filename or path. No
detector implementation, dependency, version/changelog change, or additional
test belongs in this RED checkpoint.

Current Consult25 public-reader evidence: sevenz-rust 0.6.1 and sevenz-rust2
0.21.3 remain no-go; only exact sevenz-rust2 = "=0.20.2" with
default-features = false is conditional. A bounded /tmp source sync verified
detached 424ebdb8fa98b78b8e1c18f73c9add6972fe5496, Cargo 0.20.2, and
rust-version 1.85 without touching the canonical worktree; the Hya wrapper
owns staging/policy/thread = 1/byte-CRC-callback accounting, while the
source-verified functional boundary is `reader.rs`, `decoder.rs`, `error.rs`,
and `lib.rs`: strict options, typed structural/memory-limit propagation,
re-export, and exact EOF. `archive.rs` remains byte-identical upstream; no new
parser or filesystem extractor is added.

#### 18.4.1 0.34.9 Commit 1 final evidence

Externally observed RED→GREEN results: bytes-only private detect; truncated
private fail-closed; public magic; structurally valid private
`Unverified`/`Opaque` and digest mismatch; strict public root/CRC/header/
property/limits; typed next-header limit; extra decoded COPY byte; duplicate
bundle ID; and empty private target all pass their focused contracts. Expansion
ratio coverage also passes a scalar boundary/zero-semantics RED→GREEN, a real
deterministic 409-byte 7z fixture whose accepted entry declares 320,000 expanded
bytes against 304 referenced PackInfo bytes, and a streaming-before-retain test
that rejects the crossing chunk without changing the retained buffer/count.

Staging contracts pass for single-open/lock/ownership, published final-stage
orphan cleanup, and missing-root no-op. Cleanup excludes building directories:
pre-lock cleanup would race an active install, so crash-before-lock directories
may leak; the attempted building-orphan RED/branch was rejected and reverted
before commit. Registry contracts pass for idempotency, conflict, replacement
rollback, busy, private no mutation, builtin immutable, corrupt BLOB, and
uninstall.

Final local gates: `cargo fmt --all --check` GREEN;
`cargo clippy --workspace --all-targets -- -D warnings` GREEN;
`cargo test --workspace` GREEN in the already-authorized non-sandbox
environment because of the known OAuth/local-bind sandbox restriction; and
`cargo build --workspace --bins` GREEN. Locked `hya`, `hya-backend`, and
`hya-ts` bins are GREEN, with every `--version` reporting 0.34.9.
`bash scripts/verify-no-http.sh` is GREEN outside the sandbox because of the
known ptrace restriction. TUI `bun run typecheck` and `bun run build` are
GREEN; `CI=true bun test` is 49/49 GREEN outside the sandbox because of the
known PTY/local-bind restriction. Locked/offline Cargo metadata and the sevenz
feature tree are GREEN. Trellis validation is GREEN (`implement.jsonl`: 49;
`check.jsonl`: 57), as are JSONL validation and `git diff --check`. Commit 1 was
committed and pushed as `c9055149`; exact-SHA CI run `30712583045` (job
`91402594806`) completed successfully.

#### 18.4.2 0.34.10 Commit 2 local evidence

Commit 1 is closed at `c9055149`, with exact-SHA CI run `30712583045` GREEN.
The earlier plan for Commit 2 to remain on 0.34.9 is superseded. Commit 2 is the
atomic 0.34.10 feature commit for bundle `install`/`list`/`uninstall`/`info`,
lazy atomic installed public-static catalog publication, and documentation/
example/authoring-skill delivery.

Actual backend RED→GREEN families cover parser routing, the full command
workflow, private info-only inspection and activation rejection, built-in
list/info immutability, public `info -f` without mutation, exact lowercase
suffix enforcement, and static agent/skill info output. The hya-ts RED→GREEN
covers all five forwarding shapes exactly once without starting the TUI:
install, list, uninstall, named info, and file info.

Focused verification is GREEN: the hya-backend suite is 31/31; the hya-ts suite
is 17/17 in the authorized non-sandbox environment after two known sandbox
failures caused only by denied loopback binds; the built-in authoring-skill
metadata contract and repository bundle example preparer test are GREEN.
`cargo fmt --all --check`, focused clippy, JSONL validation, and
`git diff --check` are GREEN.

Queued child binding has its own exact RED→GREEN evidence. Before the typed
bound-spawn request was threaded through execution, the exact regression test
exited 101 because the queued child received the NEW prompt. After the change,
the exact test is 1/1 GREEN; the complete `spawn_admission` suite is 23/23, the
focused core suite is 17/17, and the hya-tool spawn tests, focused clippy,
`cargo fmt --all --check`, and `git diff --check` are GREEN. Child replay proves
exactly one `TurnBindingRecorded` event carrying the old generation.

Late review RED→GREEN: startup recovery of an installed resident initially
failed exactly `AGENT_DEFINITION_MISSING: runtime-installed-resident-agent`.
GREEN makes recovery call `bind_root_runtime` once and passes the same
TurnBinding into `register_recovered_resident`; the new test is 1/1 and the
existing recovered-resident test is 3/3 GREEN.

Loop child pinning RED→GREEN: a two-iteration sequence refresh initially failed
the second child with `AgentDefinitionMissing { agent_id: "loop-agent" }`.
GREEN captures one root binding at `run_loop` admission and uses existing
`run_bound_turn` for every child; the focused test is 1/1 and `loop_gate` is
7/7 GREEN.

The built-in `customize-compat` skill RED exposed stale 0.34.8/distribution-
later text; GREEN states 0.34.10 public-static `.hyabundle` info/install and
legacy discovery absence; the focused test is 1/1 GREEN.

Final local gates are GREEN: `cargo fmt --all --check`, workspace clippy with
`-D warnings`, workspace tests (1064 passed, 2 ignored), workspace bins plus
locked `hya`/`hya-backend`/`hya-ts`, all three version probes at 0.34.10,
zero-INET, and TUI typecheck/build. The first full TUI attempt was 48/49 with
only byte-identical 140-column `CONFIRM_MAIN_MARKER_MISSING`; after user
recovery and Sol causal reconstruction, exactly one zero-edit bounded full
rerun was 49/49 with 2286 assertions and PTY 80/140 both GREEN. No
test/product change was made for the flake.

Trellis validation is GREEN (`implement.jsonl`: 49; `check.jsonl`: 74), as are
JSONL validation and `git diff --check`. Exact staging, commit, push, and
exact-SHA remote CI remain pending/unclaimed.

## 18.5 Active release — `0.34.11` public JS/Bun extension sidecars

### 18.5.1 Closed entry baseline and authority

Release 0.34.10 is closed at clean HEAD/upstream/PR head
`63c5c7b0d0267b1e811e9b6a45e0b978362c3115`; exact-SHA CI run `30724050897`
attempt 2 is fully GREEN. Workspace/TUI version is 0.34.10. Protected main
remains `267bfc3c6c66e46fe8514e2e70657489f853b7f0` with exactly 20 user-owned
status entries, and the three preserved stashes remain unchanged.

Consult26 chooses one Harness-native agent runtime with one optional
activation-scoped Bun Compat extension sidecar. Its source-corrected ruling
supersedes the older unresolved owner gate, the old 0.34.10 runner placement,
the transient/resident release split, and every `agent/invoke` or terminal/
artifact-result proposal. Sol owns architecture, decision records, RED/GREEN
acceptance, review, handoff, commit/push, and exact-SHA CI. Bounded
`luna-worker` agents may execute only explicit deterministic edits/tests/gates
and may not interpret the ruling or decide stage completion.

### 18.5.2 Strict TDD implementation order

No item opens before the preceding item is independently GREEN:

1. Plugin duplex method-role lock: initialize carries exactly activation ID and
   lifecycle as a request/reply; `tool/call` and `hook/*` are request/reply;
   `event` is a no-id/no-result notification; no `agent/invoke` exists.
2. Harness ACK/no-delegated-loop gate: a barrier fake proves no running
   transition or model poll before initialize-plus-declaration ACK, the task
   stays in Harness, the sidecar receives no task/result, resident send uses
   the mailbox, and transient send rejects.
3. Real process ownership fixtures: per-activation PID/cwd, bounded stderr,
   explicit shutdown, terminate/reap, drop fallback, exit observation, no
   transparent retry, distinct transient PIDs, and one healthy resident PID.
4. Open the existing schema/prepared decode only for supported JS/Bun extension
   resources, tools, and hooks while preserving paths/content/IDs/aliases/
   digests/references; private, raw Rust, Bundle MCP, and unenforced resource
   profiles remain activation-rejected.
5. Strict public archive closure: root manifest plus exactly the existing-v1
   declared agent prompt/resource/Extension paths; reject missing declared or
   unreferenced archive entries, wrapper/traversal/non-regular/duplicate paths;
   prove directory/archive identity/digest equality and unchanged 7z limits.
   Undeclared directory files stay outside prepared bytes/digests; no helper or
   import closure is introduced.
6. Seam-scoped app materialization: app resolves/materializes from the captured
   `RuntimeSnapshot`/`TurnBinding` catalog and constructs the bound plugin
   implementation; the core-facing start request contains only `activation_id`
   and lifecycle and adds no Bundle/package/`PreparedResource`/source/path/
   digest/`hya-plugin` type. Do not remove the shipped core-to-bundle edge.
7. Atomic executable publication through the existing registry/CLI; unsupported
   or invalid candidates preserve generation and old/new bindings remain
   pinned.
8. Namespace and `PermissionPlane`: canonical Bundle precedence from the
   captured binding, denial before RPC, allowed `tool/call` through the existing
   Harness result path, static skills/host MCP retained, Bundle MCP rejected.
9. Activation-bound hook dispatch and exact one-way event notification, with no
   result, task meaning, or `MemberOutcome` gate.
10. Transient E2E: ACK, Harness model, permission, sidecar tool call, Harness
    result and `MemberOutcome`, graceful shutdown/reap; the child receives no
    task and returns no agent result.
11. Resident E2E: two durable mailbox messages/model loops reuse one healthy
    sidecar PID; no sidecar send, per-message process, pool, or idle reclaim.
12. Cancellation/process-loss recovery: kill/timeout aborts running work under
    epoch fencing, refunds/finalizes once, rejects stale results, replays no
    RPC/event, preserves queued-after work, then uses fresh PID/ACK and the same
    pinned binding; idle replacement is lazy and startup recovery idempotent.
13. Explicit stop and terminal incompatibility: final/idempotent stop rejects
    sends, cancels queued work, removes map/staging, releases claim once;
    replacement declaration drift disables/fails once with no restart loop.
14. Full invariant regression: private/generation preservation, stable
    `AgentName`/event/replay, admission/epoch, permission interception, and
    dependency audit retaining `hya-core -> hya-bundle`, `hya-plugin ->
    hya-core`, and `hya-app ->` both while `hya-bundle` remains independent,
    `hya-core -> hya-plugin` stays absent, and `RuntimeSnapshot` remains the sole
    catalog owner; then Rust/Compat/TUI/executable/strace/zero-INET/full CI.

Item-14 executable-main correction follows five strict slices: (1) root resolver/ACK gate with one captured binding/name and no model poll before ACK; (2) static/coordinator/executable-root-plus-resident ownership matrix; (3) old/new root generation pinning; (4) pre-model failure and exactly-once cleanup table; (5) supported Bun selected-main E2E. It must reuse the single app-injected `BundleSidecarEnvironment::factory_for` responsibility, keep `ensure_main` process-free, and never share root and resident-child handles. This is an additive root activation seam, not a catalog migration or server callback.

### 18.5.3 Delivery boundary

The stage must update runtime/plugin/package/CLI/user documentation, README,
newest-only 0.34.11 changelog with archived 0.34.10 history, the built-in
`agent-bundle-authoring` skill, deterministic transient/resident Bun examples,
the retained static-only example, and invalid closure fixtures. The Rust
example remains explicitly non-activatable. Every executable example uses the
same Harness agent loop and the existing plugin transport.

No private/key work, 0.34.12/0.34.13 work, external model loop, terminal or
artifact ABI, sidecar send/wait, pool/shared process, second catalog/runtime/
loader/manifest/DTO/transport, core-to-plugin dependency, raw Rust/Bundle MCP,
compile-on-activation, arbitrary env/command, cancellation RPC, streaming,
inbound child requests, result replay, PID/context persistence, TTL/heartbeat/
reclaim/watcher, permission expansion, sandbox claim, or stable event/identity
change belongs in 0.34.11.

Item 7 TDD evidence: `public_bun_bundle_install_publishes_resources_atomically` first completed real public 7z inspection/install and generation-1 prepared-BLOB validation, then failed only because `bundle info` omitted executable resource rows. GREEN keeps the existing atomic registry/publication path and extends the shared deterministic info projection to tool/MCP/hook/extension stable IDs; existing `bundle_registry` rollback/idempotency tests and runtime-refresh old/new-binding tests remain the nonduplicated generation/pinning evidence. Focused 1/1, bundle CLI 7/7, touched Clippy/fmt/diff, hya-app 91 unit plus all integration, and hya-core full-crate gates are green.

Item 8 TDD evidence: the compiled-resource RED first exited 101 only because the intended `compile_agent_resources_with_sidecar_tools` seam was absent. GREEN builds one immutable schema/dispatch map from the captured `TurnBinding`, gives Bundle-local tools short-name precedence, retains exact Bundle and Harness qualified names, and keeps existing static-skill and host-MCP behavior. The transient permission RED first exited 101 only because the opaque handle exposed no tool bindings; GREEN captures declarations after initialize, validates them before ACK, and sends the same resolved tools into the existing Harness turn. A denied canonical Bundle tool produces the existing persisted `ToolError` and performs zero sidecar calls. The raw transport RED exited 101 only because `PluginClient::call_tool` was absent; GREEN reuses the existing `tool/call` request/reply path without Bundle-mode retry. The app binding RED exited 101 only because `bind_bundle_sidecar_tools` was absent; GREEN exact-binds initialized declarations to captured prepared resources, checks canonical `PermissionPlane` identity before RPC, and rejects drift before ACK without consulting the live catalog.

Resident propagation has an independent behavioral RED: one durable mailbox message deterministically reached `RosterStatus::Failed` instead of `Idle` because the healthy resident handle's tool bindings were dropped before resource compilation. GREEN captures that Arc once after ACK and reuses it for resolved resident turns; the exact test now observes public `echo`, one allowed canonical dispatch, and the existing persisted `ToolResult`. Focused map/permission/transport/app/resident tests, the 14/14 subagent suite, full hya-core and hya-plugin suites, fmt, touched all-target Clippy, and diff checks are GREEN. The sandboxed hya-app suite was 91/92 with only OAuth loopback bind `Operation not permitted`; the one authorized exact non-sandbox rerun was fully GREEN (92 unit tests plus all integration suites).

Item 9 TDD evidence: the core RED exited 101 only because `SidecarHandle::hook_dispatcher` was absent. GREEN captures one dispatcher after ACK, scopes it task-locally to the child `SessionId`, chains existing global then activation-local tool before/after hooks, forwards only exact matching child envelopes, and reuses the same healthy resident hook Arc; command, message-user, chat-params, and text-complete calls never reach the activation dispatcher. The raw-plugin RED exited 101 only because `ActivationHookDispatcher` was absent. GREEN uses the already-initialized `PluginClient` without respawn/replay, preserves hook request/reply posture semantics, and drains exact `event` notifications through one bounded ordered activation queue with no ID or result. The app RED first failed because initialized hook declarations still yielded `None`; GREEN exposes the dispatcher only after compatibility and declaration validation. Independent behavioral REDs then proved task-context hooks and duplicate hook names were accepted; GREEN permits only `tool.execute.before`, `tool.execute.after`, and `event`, rejects unsupported or duplicate declarations before ACK, terminates the child, and removes staging on failure. Focused core/plugin/app tests, full hya-core and hya-plugin suites, touched all-target Clippy, rustfmt, and diff checks are GREEN. The post-change hya-app lib run passed 93/94; its sole failure was the already-accounted sandbox-only OAuth loopback bind `Operation not permitted`, while all runtime-reconcile tests passed.

Item 10 TDD evidence: the explicit lifecycle RED first failed to compile only because the opaque `SidecarHandle` exposed no `shutdown`; GREEN adds one dependency-free async lifecycle method, keeps raw process ownership in `hya-plugin::ChildGuard`, reaps before returning, and removes activation staging idempotently. The core ordering RED then failed `calls left 0 right 1`; GREEN carries the transient opaque handle through the successful member task, appends the canonical `MemberFinished` exactly once, and only then performs graceful shutdown/reap. Error and cancellation recovery remain item 12.

The Bun Bundle-mode RED ran the real adapter and received only the hostile normal-discovery `leak` tool instead of the explicit `echo` extension. GREEN adds a narrow internal `--bundle-extension` startup path that loads only app-materialized absolute files with empty initialization input; normal Compat discovery remains unchanged and no wire method or second transport was added. The app launch RED reached initialize but failed declaration validation with `expected 1, got 0`; GREEN passes those materialized extension paths to the same bundled adapter command after creating the per-activation cwd.

The final transient composition test uses the real bundled Bun adapter plus a two-round `FakeProvider`: Harness polls the model only after ACK, applies the canonical Bundle-tool permission, sends `tool/call`, persists the normalized existing `ToolResult`, completes the second Harness model round, emits one Done `MemberFinished` with the child session, and removes activation staging after shutdown/reap. The child fixture rejects nonempty initialization or tool input, receives no agent task, and returns no agent outcome. Its final focused result is 1/1 GREEN.

Touched gates are GREEN: core subagent 16/16 and full hya-core; full hya-plugin; non-sandbox hya-app 98 unit tests plus every integration suite; adapter typecheck and an isolated-HOME full adapter run at 64/64 with 153 assertions; rustfmt, touched all-target Clippy, and `git diff --check`. The first sandbox app run also exposed two pre-existing reconcile tests whose generation oracle crossed `bind_turn` HOME-based skill discovery; both passed alone. The test-only correction now measures the reconciler's effective manifest immediately around failure, preserving tool/pinned-binding assertions without changing product concurrency. The only remaining sandbox failure was the known OAuth loopback `Operation not permitted`.

Item 11 composition evidence is deliberately honest: the earlier item 8 resident-propagation RED had already supplied the missing product behavior, so the first correct item 11 invocation was GREEN rather than a fabricated failure. The real Bun fixture starts with no initial directive, receives exactly two durable mailbox messages, and runs two Harness-owned provider/tool loops through one resident activation. Its module-scoped counter returns `pid:1` then `pid:2`; child replay contains exactly two normalized `ToolResult` events, both PIDs are identical and nonempty, no `ToolError` exists, and the resident returns to `Idle` after each event-barrier wait. No sleep, poll, timeout, sidecar send, per-message process, pool, or idle reclaim was introduced.

Item 11 gates are GREEN: focused 1/1; non-sandbox hya-app 99/99 unit tests plus 28/28 integration tests (127 total); rustfmt, hya-app all-target Clippy with `-D warnings`, and `git diff --check`. Explicit resident stop/reap/staging removal remains item 13 and was not claimed by this healthy-reuse proof.

Item 12 TDD evidence: a Bundle-mode tool timeout first left the same client callable; GREEN taints and closes the process with no transparent retry. Independent idle-loss and running-loss tests then proved lazy fresh-process ACK, epoch fencing, once-only abort/refund, stale rejection, queued-after preservation, a fresh PID, and the original pinned TurnBinding. Activation-local before-hook, after-hook, and event transport-loss REDs each crossed their real dispatch seam; GREEN checks the captured dispatcher before provider polling and after each request/reply hook, so a lost sidecar cannot commit a tool result, append later actor state, or trigger replay. Global hooks remain unchanged.

The startup-recovery RED used an installed executable resident plus durable queued mail: current recovery consumed the mail but never initialized its extension because the recreated slot discarded roster/resource/factory context. GREEN exact-resolves the recorded stable AgentName from one current TurnBinding, derives its roster, resource policy, and optional bound factory from that same binding, and stores the existing ResolvedResidentRuntime in the recovered slot; guidance, PID, stdio, and process memory remain non-durable. The real Bun extension ACK marker now precedes queued-mail completion. Focused recovery tests, hya-app 129/129, hya-core 166 passed with one ignored worktree lifecycle test, rustfmt, touched all-target Clippy, and diff checks are GREEN.

Item 13 TDD evidence: the idle-stop RED first exited 101 only because ResidentSupervisor exposed no stop_resident seam. GREEN adds one slot-local cancellation root, one explicit stop request, actor-epoch fencing through the existing recover/terminalize path, exactly one Failed activity transition, full-tuple claim release, graceful idle shutdown, active terminate/reap, slot removal, and pre-append rejection of mail to terminal residents. Repeating a completed stop is projection-idempotent. The running-stop RED then failed only with Invalid("resident stop already in progress"); GREEN treats an already accepted stop request as idempotent without a second fence, release, notification, or cleanup. A blocked provider plus queued-after mail proved one terminate, no queued admission or provider rerun, no late stale append, released claim, and final send rejection.

The existing running-loss regression then produced [Cancelled, Cancelled] instead of [Cancelled, Stop]. Direct source review showed BundleSidecarTool cancels its ToolCtx token on transport loss, while the new slot token had been reused across mailbox turns. GREEN keeps the slot token as the team-derived stop root but gives every RunPlan a fresh child token, so loss cancels only the current turn and explicit stop still cancels the active child. The declaration-drift RED reached the opaque factory ACK failure and failed only because the child claim and resident slot remained active. GREEN preserves one actor-fenced Failed event, releases the current claim once, removes the Busy slot, and exits with no restart loop; app-owned factory tests independently retain real child reap and activation-staging cleanup on shutdown or declaration rejection.

Item 13 gates are GREEN: focused idle/running/drift tests; hya-core 169 passed with one ignored worktree lifecycle test; hya-app 129/129 including bundle_sidecar_handle_shutdown_reaps_child_and_removes_activation_staging and bundle_sidecar_rejects_task_context_hook_before_ack; rustfmt, hya-core+hya-app all-target Clippy with -D warnings, and git diff --check. The first post-change app run exposed the running-loss cancellation regression above plus a secondary poisoned shared EnvGuard; after the product correction, the exact socket-based focused test passed in its one authorized non-sandbox run and the full non-sandbox app suite was clean.

The earlier item-14 concurrency diagnosis that described a stop request preceding split durable recovery is superseded by the implemented C semantics and the source audit below. TeamState installs one shared completion and issues only a local cancellation signal; that signal is not a durable or public linearization point. The leader then calls one bounded SessionStore stop finalizer under BEGIN IMMEDIATE. That transaction fences the exact active claim, appends existing actor cancellation effects, aborts/refunds bound admissions exactly once, appends the existing Failed resident activity, releases the claim last, and commits atomically. Only after that commit does the resident consume its cleanup request, shut down or terminate/reap its activation, remove its slot, and publish the shared completion Result. Duplicate callers wait on that same completion through cleanup; failure is shared and a later retry resumes cleanup against the already-terminal durable state without duplicating terminal/refund writes.

Direct and channel resident sends each acquire the same SQLite writer order with BEGIN IMMEDIATE before replaying roster/claim state, require an active resident claim, append the unchanged MailSent event, and report success only after commit. Therefore send-first mail is visible to the later stop and becomes non-processable through the existing reducer/cursor terminal semantics; stop-first releases the claim before commit and a later sender rechecks and rejects or excludes that resident. Mail remains epoch-independent, channel events keep stable bytes, and SQLite/event replay remains the sole acceptance and restart authority. The single-transaction feasibility audit found no separate authoritative inbox table, so the accepting_mail/stopping two-phase fallback is not activated.

The final focused evidence is GREEN without another product change: resident_stop_finalizer_rolls_back_at_each_write_boundary_and_is_exactly_once; resident_mail_remains_epoch_independent_until_explicit_stop; resident_direct_send_committed_before_stop_is_durably_cancelled; resident_stop_commits_before_stale_direct_send_rechecks_and_rejects; channel_mail_after_stop_excludes_resident_and_replays_for_active_subscriber; duplicate_stop_shares_cleanup_failure_and_later_retries_cleanup_without_reterminalizing; and explicit_running_resident_stop_is_idempotent_fences_and_drops_queued_mail each passed 1/1. cargo fmt --all --check and git diff --check also passed. These tests cover send-first, stop-first, active-plus-stopped channel ordering and replay, shared duplicate failure/cleanup retry, epoch independence, every durable write-boundary rollback, once-only terminal/refund/release, queued-mail cancellation, and post-stop rejection.

Item-14 hook/resource-isolation follow-up is now controlling over the earlier broad all-Bundle materialization behavior. The accepted A+B sequence is: (1) canonicalize local/alias/canonical `hook_refs` to stable Hook IDs, reject wrong-kind/ambiguous/duplicate refs, and admit only exact local IDs `event`, `tool.execute.before`, and `tool.execute.after`; (2) exact-path join selected Tool/Hook resources to exactly one same-owner `extensions.js`, reject unreachable/zero/ambiguous/wrong-owner matches, and stage controlled cross-kind path reuse once; (3) extend the captured `CompiledResourceView` with canonical Hook IDs and prove two agents receive disjoint deterministic closures pinned across N/N+1 publication; (4) validate initialized tool and hook declarations as independent exact sets, including duplicate/missing/extra/unsupported and tool-only/hook-only cases; (5) prove unselected dispatch isolation and `PermissionPlane` denial before RPC; (6) run deterministic split-entrypoint Bun Compat E2E and reject a generic superset module before model polling; (7) regress static process-free, shared selected Extension load-once, private/Rust/Bundle-MCP rejection, and generation pinning.

The implementation must remove all-Bundle tool/Extension binding and derive the activation closure only from the captured agent resource policy/view and `TurnBinding`. No `extension_refs`, implementation/provenance field, free HookName mapping, second DTO/loader/catalog/transport, import graph, or permission plane is authorized. Existing one-way `event` and method-role tests remain valid; this follow-up reopens only identity, entrypoint selection, declaration equality, and dispatch isolation.

The helper-file arbitration is resolved as self-contained-entrypoint-only. No source/IR/runtime field is opened. Pending item-14 evidence must prove: helper/dependency unknown fields reject; undeclared directory files do not enter PreparedBundle/digest while archive extras reject; directory and archive activations omit relative helpers and fail before ACK/model/dispatch with once-only cleanup; a co-path self-contained Extension stages once for aggregate selected declarations; distinct/other-agent/unreachable/misdeclared Extensions preserve selected-entrypoint isolation; and captured PreparedResource bytes remain pinned across source mutation and N+1 publication. The exact normative sentence is: “The 0.34.11 public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists.”

The harness-prefixed hook_ref arbitration is also resolved as option A. Both prepare/validation bypasses must be deleted; all supported-looking, unknown, empty, and minimally malformed harness:hook spellings reject through existing normal-reference errors. Follow with hand-crafted PreparedCatalog rejection, unchanged real Bundle Hook canonicalization/duplicate/name/path regressions, generation/catalog/binding preservation, and no runtime filter/fallback/sidecar/model polling. No new error, compatibility plane, schema migration, or runtime branch is authorized.

Item-14 self-contained-entrypoint evidence is GREEN. `prepared_decode_rejects_unknown_bundle_collections` injects `files`, `helpers`, `dependencies`, and `imports` into digest-rebound prepared bytes and proves decode rejects every invented collection. `public_archive_matches_directory_source_prepared_identity_and_ignores_undeclared_helper` proves an authoring-only helper never enters a `PreparedResource`, prompt source, prepared bytes, or digest while directory/archive declared identity stays identical. `directory_bundle_importing_undeclared_authoring_helper_fails_before_ack` uses a real directory source whose selected entrypoint imports that helper; captured-generation rematerialization omits it, initialization fails before ACK/model/dispatch, and cleanup leaves activation staging empty. Existing co-path shared-Extension, disjoint-agent, generic-superset, unreachable-Extension, declaration-order, and N/N+1 captured-content tests provide the nonduplicated remaining isolation evidence.

The authoring docs/skill contract REDs compiled and failed only because the exact self-contained-JS and Bundle-local-hook wording was absent. GREEN records the normative self-contained sentence, external single-file bundling, authoring-tree isolation, directory/archive asymmetry, pre-ACK helper-import failure, rejection of all `harness:hook/*` spellings, host-hook ownership, and unchanged private/raw-Rust/MCP/no-sandbox/no-permission-expansion boundaries. Focused docs contract 1/1, complete `docs_example` 5/5, built-in skill contract 1/1, rustfmt, and `git diff --check` are GREEN.

Final Standards/Spec review closed two concrete regressions without opening a new product boundary. Source and prepared resource-view validation had retained a second `harness:hook/*` bypass after `hook_refs` itself was corrected. The source RED and hand-crafted prepared RED each compiled and failed only because `harness:hook/event` still resolved; GREEN removed `hook` from both Harness-prefix allow-lists so all such spellings now follow the existing normal-reference error path. Real Bundle Hook canonicalization, exact-path Extension selection, and runtime behavior remain unchanged. The complete hya-bundle suite passed 93 tests, with all-target Clippy, rustfmt, and diff checks GREEN.

The final transient-cancellation RED proved that `run_member` discarded `FinishReason::Cancelled`, emitted `MemberRunStatus::Done`, and selected graceful sidecar shutdown. GREEN captures the finish reason from all four bound/resolved actor/non-actor turn paths, maps only `Cancelled` to the existing `CoreError::Cancelled` path, records Failed evidence plus `MemberRunStatus::Cancelled`, and terminates the activation handle exactly once with no graceful shutdown. The focused test passed 1/1; the complete hya-core suite, hya-core all-target Clippy, rustfmt, and diff checks are GREEN.

The final Compat gate also exposed host-environment leakage in eight subprocess test helpers: an unsandboxed run inherited real HOME/XDG plugin discovery and added unrelated default hooks while every product assertion remained intact. The existing failures served as the RED. GREEN gives each helper its existing temporary HOME/XDG root and disables project discovery without changing product code or expected declarations. Unsandboxed Compat verification passed typecheck and 64/64 tests.

Local 0.34.11 CI-equivalent gates are GREEN. The locked hya/hya-backend/hya-ts binary build, workspace rustfmt, workspace all-target Clippy with `-D warnings`, workspace build, unsandboxed serial workspace tests, and final workspace-bin build all exited zero. The initial sandbox workspace test failed only at prohibited localhost/process operations and consequent shared-lock poisoning; the exact unsandboxed rerun was clean. TypeScript TUI frozen install/typecheck/build and unsandboxed tests passed 49/49; Compat passed 64/64; `scripts/verify-no-http.sh` reported `OK: zero inet sockets`; final diff checks are clean. Workspace/TUI/root changelog remain 0.34.11 with archived 0.34.10 history. Canonical HEAD equals upstream `63c5c7b0d0267b1e811e9b6a45e0b978362c3115`; protected main retains its exact 20 user-owned status paths and the three preserved stash hashes. Current status is `LOCAL-GATES-GREEN / READY-TO-COMMIT-PUSH-CI`; commit, push, and exact-SHA remote CI remain unclaimed.

## 18.6 Active release — `0.34.12` durable admission plus R10 certification

### 18.6.1 Closed entry baseline

Release 0.34.11 is closed at clean HEAD/upstream `9dafca4bc62b60ab4d7a66224f42a15251d6f4cf`; exact-SHA CI run `30795476621` is GREEN. Workspace/TUI version remains 0.34.11 at entry. Consult27 supersedes only the literal certification-only wording and authorizes the minimum missing P5 durable capacity authority before R10; 0.34.13 remains the updater.

### 18.6.2 Strict TDD order

1. Migration/state machine: `Queued`, `Accepted`, `Started`, `Waiting`, terminal transitions, row-derived active/non-active counts, stale `Accepted -> Queued`, stale `Started -> Aborted`, and idempotent recovery.
2. Atomic capacity: exactly 100 active plus 156 non-active; item 257 typed overload with zero downstream allocation; any batch crossing either boundary rolls back every member.
3. Zero-task queue/promotion: `Queued` allocates nothing; `Queued -> Accepted` returns one launch instruction; execution remains blocked until `Started` commits; pre-start failure cancels the suspended task and recovery requeues.
4. Durable fairness: oldest eligible within a root, one-per-root bounded rotation, deterministic ties/skips, bounded progress, and restart-identical order.
5. Parent suspension: child batch plus `Started -> Waiting` is one capacity-valid transaction; dependency completion uses `Waiting -> Queued` and fair reacquisition; failure leaves parent and batch unchanged.
6. Release accounting: one active release promotes at most one; N releases promote at most N; cancelling non-active work promotes none; terminal/refund/budget/lease effects are exactly once.
7. Provider partition: 100 general and 28 reserved permits never borrow; root progresses under full general saturation and never consumes an admission envelope.
8. Restart convergence: mixed `Accepted`/`Started`/`Queued`/`Waiting` state converges without lease leak, duplicate task, replay, or duplicate refund.
9. Exact-SHA R10: sustained 100/156, item-257 zero-allocation overload, multi-root fairness, parent wait, cancellation promotion, root reserve, restart, all 0.34.11 sidecar/resident/transient regressions, and frozen resource/SLO thresholds.

### 18.6.3 Resolved restart-binding correction

The former RED 3/8 owner hold is resolved by Consult28. Cross-process recovery must not compare `ConfigGeneration`: it is a same-process ordinal and can be equal for changed semantics or differ for identical semantics. Before launch RED 3, TDD introduces private `RuntimeSemanticFingerprintV1`, computed from the complete canonical RuntimeSnapshot semantics after normal refresh/reconciliation/workdir discovery converges. Any behavior source lacking a deterministic identity makes the fingerprint unavailable.

Every restart-reconstructable row—including a pre-`Started` `Accepted` row because startup requeues it—atomically persists fingerprint version/value, diagnostic generation, and lossless accepted post-hook spawn intent under one integrity hash. Fingerprint determinism/section-isolation/unavailable-source tests and the private intent round trip precede restart launch. Recovery captures one current snapshot, requires fingerprint equality, exact-resolves the stable target and overlay, and pins all schema/dispatch/skills/sidecar/descendant work to it. Mismatch or unavailability aborts once before allocation; no generation/latest-catalog fallback or history store is allowed. A previously started `Waiting` parent remains stale-running and is fenced/aborted rather than replayed.

## 18.7 Consult29 prerequisite sequence before launch RED 3

Consult29 narrows the remaining pre-launch work. Keep the already-green `RuntimeSemanticFingerprintV1` sections unchanged. `hya-app` next owns an immutable `AdmissionResolutionContext` and private `AdmissionBindingFingerprintV1`; `hya-core` retains the runtime-view fingerprint and `hya-store` persists only opaque versioned fingerprints plus raw intent bytes.

Non-skippable atomic RED/GREEN order before the former RED 3:

1. composite determinism across restart, insertion/publication order, diagnostic generation, and transient provider-health changes;
2. base isolation: model/prompt/reasoning affect identity and recovered resolution, overwritten name/workdir do not;
3. category isolation: ordered candidates and consulted lookup inputs affect identity/selection, current unused `prompt_append`/`token_budget` do not;
4. configured-provider identity: order, implementation/version, configured model/alias/inclusion, deterministic capability and resolver-relevant non-secret configuration affect identity; live probing/state does not; unidentifiable providers fail closed;
5. canonical `SpawnIntentV1` round trip and integrity for all ruled raw post-hook fields, both fingerprints, resolver version, diagnostic generation, with explicit proof that no effective model/prompt/reasoning/candidate/`AgentSpec` is encoded;
6. exact per-row 1,048,576-byte acceptance, 1,048,577-byte typed rejection, arithmetic overflow closure, and complete batch rollback for one oversized member.

Only then open launch RED 3 with both identities. Encode and size-check every row before the store transaction. Initial unavailability rejects the batch with no durable/runtime allocation; recovery mismatch/unavailability terminalizes once before target resolution and cannot later rebind. Do not widen `RuntimeSnapshot`, persist app/provider objects or derived launch state, consult transient health, or create a second storage/runtime authority.

### 18.7.1 Consult29 composite-identity slice 1 evidence

The first atomic RED added `admission_binding_fingerprint_is_stable_for_fresh_equivalent_contexts`. Its exact focused command, `cargo test -p hya-app admission_binding_fingerprint_is_stable_for_fresh_equivalent_contexts`, exited 101 with the sole diagnostic E0433 because `AdmissionResolutionContext` was undeclared at `crates/hya-app/src/runtime.rs`; `cargo fmt --all --check` exited zero. No import, collection, or tool failure substituted for the behavior RED.

The minimum GREEN adds one private immutable app-owned context that retains the exact supplied base `AgentSpec`, `Arc<CategoryRegistry>`, and `Arc<ProviderRouter>`. This empty-only slice rejects nonempty category/provider inputs, validates fixed-width canonical length conversions during capture, and derives one domain-separated SHA-256 identity from the runtime fingerprint, inheritable base model/exact prompt/effective reasoning, explicit empty category/provider identities, and `ResolverSemanticsV1`; overwritten base name/workdir remain excluded. `ProviderRouter::is_empty` is the only provider surface added. Nonempty category/provider semantics, raw intent, persistence, recovery, allocation, public identity, and `ConfigGeneration` binding remain unopened.

Independent Sol Standards/Spec review passed. The focused test passed 1/1; `cargo clippy -p hya-app -p hya-provider --all-targets -- -D warnings`, `cargo fmt --all --check`, and `git diff --check` all exited zero.

The slice was committed and pushed atomically as `88265c1552d4e502aeb7ddfdd1be329e11bc48ec` (`feat(admission): add app binding fingerprint identity`). Exact-head CI run `30817432535` attempt 2, job `91701127842`, passed the TypeScript TUI, workspace fmt/Clippy/build/tests, and zero-inet-socket gate. Attempt 1 had failed only in the unchanged TypeScript PTY 80/140-column workspace tests; a read-only Bun 1.3.14 isolated loop independently reproduced four approximately 15-second passes, one existing event-propagation sentinel failure, and one exact marker timeout in five 80-column attempts. No TUI file differs between the prior green SHA and this commit, so the same-SHA successful rerun is the controlling remote evidence; no unrelated fixture/product change entered this slice.

### 18.7.2 Consult29 base-isolation slice 2 evidence

The next atomic RED added only `admission_binding_base_fields_match_reconstructed_agent`. It captures fresh contexts that vary each inheritable base field and a name/workdir-only control, compares the context path with the existing canonical `SessionEngine::agent_spec_for_binding`, and covers all five current `AgentSpec` fields without inventing another field. The exact focused command exited 101 with one E0599 at the intentionally absent `AdmissionResolutionContext::resolve_agent_for_binding`; after mechanical test-only rustfmt, the same sole diagnostic remained and `cargo fmt --all --check` exited zero.

The minimum GREEN adds only that private method and delegates directly to `engine.agent_spec_for_binding(binding, &self.base, stable_id)`. It therefore consumes the same retained base object as the fingerprint while preserving the existing stable-name and binding-workdir overwrite semantics. It adds no category/provider logic, cache, clone, fallback, error remap, production integration, persistence, or recovery path. Independent Sol Standards/Spec review passed; the new focused test and the prior fingerprint determinism test each passed 1/1, `cargo clippy -p hya-app --all-targets -- -D warnings`, `cargo fmt --all --check`, and `git diff --check` all exited zero.

The slice was committed and pushed atomically as `2168a7bbf8c92c5e5f010fea0db3fb39b6d57afd` (`feat(admission): bind base resolution semantics`); the commit contains only `crates/hya-app/src/runtime.rs` with 115 insertions. Exact-head pull-request CI run `30820175131`, job `91707820553`, completed GREEN on its first attempt: TypeScript TUI build/tests, workspace rustfmt, all-target Clippy, build, full workspace tests, and `verify-no-http` all passed. The full test step ran from `13:57:36Z` through its successful completion inside the 15m36s job; no rerun or exemption was used.

### 18.7.3 Consult29 category-isolation slice 3 evidence

The next atomic RED added only `admission_binding_category_fields_match_resolution_semantics`. It requires fresh equivalent registries with reversed insertion order to retain identity and selection, ordered candidate swaps to change both, category lookup-key changes to change identity and the available lookup, and current unused `prompt_append`/`token_budget` changes to affect neither. The exact focused command exited 101 with the sole E0599 at the intentionally absent private `AdmissionResolutionContext::resolve_category_for_admission`; after mechanical test-only rustfmt, the same sole diagnostic remained. `cargo fmt --all --check` and `git diff --check` exited zero.

The minimum GREEN adds one read-only `CategoryRegistry::resolution_candidates` view: category keys are lexicographically canonical while each primary-plus-fallback candidate chain retains exact order, and prompt/token shaping is excluded. Context capture now accepts a nonempty immutable category Arc, validates every count and byte length through fixed-width `u64`, retains one domain-separated canonical category section, and preserves the prior exact empty-registry marker. Fingerprinting consumes that section. The private selection seam delegates exactly to the existing `resolve_servable` with the same captured router; with the still-required empty router, existing best-effort behavior selects the first candidate. No provider identity, live state, second resolver/catalog, intent, persistence, recovery, allocation, or production integration entered this slice.

Independent Sol Standards/Spec review passed. The category test and both prior AdmissionBindingFingerprint tests each passed 1/1; the actual `hya-core --test category_routing` integration target passed 4/4. `cargo clippy -p hya-app -p hya-core --all-targets -- -D warnings`, `cargo fmt --all --check`, and `git diff --check` all exited zero.

The slice was committed and pushed atomically as `5b4b339d9bf97a6f841b2e6caec461ec0585d8ee` (`feat(admission): bind category resolution semantics`); only `crates/hya-app/src/runtime.rs` and `crates/hya-core/src/category.rs` entered the commit. Exact-head CI run `30822355165` attempt 2, job `91715922819`, completed GREEN in 15m37s: TypeScript TUI, workspace rustfmt/Clippy/build/full tests, and zero-inet-socket verification all passed. Attempt 1 failed only in the unchanged 140-column PTY smoke test after its 59.3-second `fresh root workspace` deadline at `pty-smoke.test.ts:253`, while 80-column passed. This SHA changed only Rust app/category files from the prior green SHA, and the earlier isolated Bun 1.3.14 loop had already reproduced the same PTY nondeterminism; one same-SHA failed-job rerun supplied the controlling successful evidence with no source change or exemption.

### 18.7.4 Consult29 configured-provider identity slice 4a evidence

The next atomic RED added only `configured_provider_identity_covers_routes_without_secrets_or_live_state`. A deliberately absent helper called `ProviderRouter::configured_identities_v1`; the exact focused command, `cargo test -p hya-provider configured_provider_identity_covers_routes_without_secrets_or_live_state`, exited 101 with the sole diagnostic E0599 at that helper. Mechanical test-only rustfmt preserved the same sole diagnostic, while `cargo fmt --all --check` and `git diff --check` exited zero.

The minimum provider-side GREEN adds one object-safe configured-identity method whose default fails closed, ordered fail-closed router aggregation, and deterministic identities for the two built-in executable provider profiles. `HttpProvider` binds its domain/version, implementation kind, normalized endpoint/base, supported model set, alias rules, ordered per-model reasoning variants, deterministic capabilities, auth mode and non-secret auth configuration, credential presence, and configured bearer-resolver slot. It sorts unordered model input, retains configured router precedence, never calls the live resolver, and excludes all credential bytes and resolver output. `DevProvider` binds its fixed any-model behavior and capabilities; `FakeProvider` and any provider without a complete deterministic identity remain unavailable. All variable fields/counts use checked fixed-width canonical encoding. No app context, admission row, persistence, recovery, allocation, live probe, or second routing authority entered this provider sub-slice.

Independent Sol Standards/Spec review passed. The focused contract passed 1/1 and the full provider package passed 16 unit, 18 conformance, 12 HTTP-header, 5 multiprovider, and 3 structured-error tests plus doctests. The first touched Clippy gate exposed only `clippy::expect_used` in the new test helper; the smallest test-only explicit `match` removed it without adding an allow. The complete focused/package test sequence then remained green, `cargo clippy -p hya-provider --all-targets -- -D warnings`, `cargo fmt --all --check`, and `git diff --check` all exited zero, and the product diff remained restricted to `lib.rs`, `router.rs`, `http.rs`, and `dev.rs` in `hya-provider`.

The provider sub-slice was committed and pushed atomically as `9029258cfaad749cdad85e0d3870ea28e3fa1d9c` (`feat(provider): add configured routing identity`), with parent `5b4b339d9bf97a6f841b2e6caec461ec0585d8ee` and exactly those four provider files. Exact-head pull-request CI run `30825453617`, attempt 1, job `91725796799`, completed GREEN in 14m9s: TypeScript TUI preparation/build/tests, workspace fmt/Clippy/build/full tests, and zero-inet-socket verification all passed. No rerun, exemption, or source change was used.

### 18.7.5 Consult29 configured-provider app slice 4b evidence

The app-side RED added exactly one test, `admission_binding_provider_fields_match_resolution_semantics`, plus its two provider test imports. It requires composite equality across normalized equivalent routes with different nonempty credential bytes and different live resolver outputs, non-invocation of both live resolvers, same-context configured candidate selection, inequality for endpoint/model-inclusion changes and route-order reversal, fallback selection when the primary model is not configured, and the exact typed `ProviderIdentityUnavailable` failure for an unidentifiable provider. The exact focused command compiled and ran one test, then exited 101 only at the first configured capture with `configured provider router must capture: NonEmptyProviderRouter` (0 passed, 1 failed, 124 filtered). `cargo fmt --all --check` and `git diff --check` exited zero; no compile, import, collection, or tool failure substituted for the behavioral RED.

The minimum GREEN removes the stale nonempty-router rejection and captures one canonical provider-resolution section from the same retained `Arc<ProviderRouter>`. Empty routers preserve the prior exact `ProviderRouterEmptyV1` bytes; nonempty routers encode a domain marker, checked big-endian `u64` count, and exact length-prefixed provider identities in route order. Unavailable identity maps only to `ProviderIdentityUnavailable`. The composite appends the captured section, while category selection continues to use the same retained router. No test edit, provider change, live probe, fallback/rebind, cache, persistence, allocation, public API, second router, or duplicate encoder entered the GREEN.

Independent Sol Standards/Spec review passed, including exact empty-marker stability, self-delimiting checked framing, ordered precedence, typed fail-closed behavior, and absence of transient-state input. Sol independently ran all four `admission_binding_` tests (4/4). The touched gate passed the complete hya-app package with 154 tests (125 unit, 1 built-in bundle build, 1 installed refresh, 3 nested spawn tree, 24 spawn admission), `cargo clippy -p hya-app --all-targets -- -D warnings`, `cargo fmt --all --check`, and `git diff --check`, all with zero failures or diagnostics. The slice modifies only selected hunks in `crates/hya-app/src/runtime.rs`; its older catalog/recovery/refresh WIP remains protected and outside the slice.

The app sub-slice was selectively committed and pushed as `0fb47067b1611a7ec2f2d20382cce7170a6a8499` (`feat(admission): bind provider resolution semantics`), with parent `9029258cfaad749cdad85e0d3870ea28e3fa1d9c` and only the ruled `runtime.rs` hunks; the five older runtime WIP hunks remained unstaged. Exact-head pull-request CI run `30827773954`, attempt 1, job `91733723021`, completed GREEN in 13m57s: TUI, workspace fmt/Clippy/build/full tests, and zero-inet-socket verification all passed. No rerun, exemption, or source change was used.

### 18.7.6 Consult29 canonical SpawnIntentV1 slice 5 evidence

The next atomic RED registered one private `hya-app` module and added exactly one test, `spawn_intent_v1_round_trips_raw_fields_and_rejects_tampering`. It freezes lossless raw member/inline/task fields, typed parent/stable target/operation and batch provenance, both versioned fingerprints, resolver version, diagnostic generation, deterministic binary re-encoding, full-payload integrity, hash-valid unknown-version/noncanonical rejection, and explicit absence of derived launch state, credentials, guidance, cancellation/reply state, and runtime handles. The exact focused command exited 101 with the sole E0432 unresolved import for the intentionally absent codec contract; formatter and diff checks were clean. Sol review tightened only the still-test-only contract to use typed `AgentName`, mutate an actual task byte rather than its length, and add cancellation/reply/runtime-handle sentinels; the same sole E0432 remained.

The minimum GREEN implements one private dependency-free canonical binary codec. Its fixed header carries domain plus format/runtime-fingerprint/admission-fingerprint/resolver versions, both 32-byte fingerprints, and diagnostic generation; checked `u32` length framing preserves every raw `SpawnMember`/`InlineAgent` field, followed by canonical parent, stable target, background, source/operation identity, ordinal/cardinality, and prior-start provenance. SHA-256 covers a separate integrity domain plus the complete pre-integrity payload and is verified before parsing. Strict tags, bounds/UTF-8/identity checks, operation derivation, metadata invariants, no trailing bytes, and byte-exact re-encoding reject corrupt or alternate forms. The constructor intentionally discards process-local actor ownership and accepts no derived model/prompt/reasoning/candidate/spec, provider/catalog/resource object, secret, guidance, cancellation/reply state, or runtime handle. The 1 MiB cap, persistence, batch transaction, recovery, and launch integration remain unopened.

Independent Sol source review caught one initial over-validation: current runtime accepts arbitrary raw `task_id` and only best-effort parses it later. Changing the same test fixture to a whitespace-bearing non-session string produced the exact behavioral RED (`NonCanonical` at construction); removing only raw-member validation restored 1/1 GREEN and preserves all raw strings losslessly. Sol then independently reran the focused test. The complete hya-app gate passed 155 tests. The first Clippy pass found only the new 11-argument constructor; grouping exactly those ruled inputs into private `SpawnIntentInputV1` removed the diagnostic without an allow or semantic change. Final `cargo clippy -p hya-app --all-targets -- -D warnings`, `cargo fmt --all --check`, and `git diff --check` all exited zero. Only `hya-app/src/lib.rs` and the new private `spawn_intent.rs` belong to this slice.

The slice was committed and pushed atomically as `522c4cf054322b7034b44832510aa71a71c17841` (`feat(admission): add canonical spawn intent`), with parent `0fb47067b1611a7ec2f2d20382cce7170a6a8499` and exactly those two files. Exact-head pull-request CI run `30830807825`, attempt 1, job `91743959662`, completed GREEN in 14m01s: TUI, workspace fmt/Clippy/build/full tests, and zero-inet-socket verification all passed. No rerun, exemption, or source change was used.

### 18.7.7 Consult29 exact intent-boundary slice 6 evidence

The next atomic RED added only `spawn_intent_v1_enforces_exact_size_and_batch_preparation`. It derives codec overhead from a small canonical fixture, requires exact 1,048,576-byte encode acceptance, exact typed 1,048,577-byte encode/decode rejection, checked `usize` addition overflow, an all-or-none two-member preparation failure with the oversized member second, and a successful exact-boundary one-member batch. The focused command exited 101 solely with E0432 for the intentionally absent private cap/checked-add/batch helpers and E0599 for the intentionally absent `EncodedSizeExceeded` variant. `cargo fmt --all --check` and `git diff --check` exited zero.

The minimum GREEN adds one fixed private 1,048,576-byte constant, one typed size error, and one bounded byte sink used by the existing canonical encoder. Every append performs checked addition and rejects before copying; the sink preserves the prior domain, field/tag order, framing, and integrity bytes without a second encoder or length calculator. Decode checks the same cap before integrity work. A private iterator/`collect` helper returns prepared bytes only when every member encodes, so no partial vector can reach the future writer transaction. No schema, store, runtime launch, compression/chunking, public API, configuration, or protected WIP entered the slice.

Independent Sol source review passed for exact byte-layout preservation, integrity coverage, cap ordering, arithmetic closure, and all-or-none result semantics. Sol reran both codec tests (2/2). The complete `hya-app` gate then passed 156 tests (127 unit, 1 built-in bundle build, 1 installed refresh, 3 nested spawn tree, 24 spawn admission), with zero failures or ignored tests. `cargo clippy -p hya-app --all-targets -- -D warnings`, `cargo fmt --all --check`, and `git diff --check` all exited zero.

The slice was committed and pushed atomically as `a232b8102fed13bb70f3d5c0a43d258467a2ff0a` (`feat(admission): bound canonical spawn intents`), with parent `522c4cf054322b7034b44832510aa71a71c17841` and only `crates/hya-app/src/spawn_intent.rs`. Exact-head pull-request CI run `30832799710` attempt 3, job `91756383884`, completed GREEN in 15m02s: TypeScript TUI, workspace fmt/Clippy/build/full tests, and zero-inet-socket verification all passed. Attempt 1 failed only at the unchanged 140-column PTY `fresh root workspace` deadline; attempt 2 cleared that gate and failed only in one of three unchanged unsynchronized `HYA_DB` process-environment tests in `hya-sdk`. The slice changes no TUI or `hya-sdk` source, and the `hya-sdk` blob is identical to the certified parent. The same-SHA successful rerun is therefore the controlling remote evidence; no product, fixture, workflow, or protected-state change entered the certification.

## 18.8 Preserved durable admission RED 1/2 integration

The continuation audit recovered the completed RED 1 boundary from the old coordinator's source-linked evidence and the preserved diff: `Queued`/`Waiting` first failed only because those states were absent; the row-count contract first failed only because `admission_counts` was absent; and startup reconciliation first failed only because `recover_nonterminal_admissions` was absent. The minimum preserved GREEN extends the sole admission journal through migration `0006`, derives active/non-active/total counts from row state, requeues stale non-actor `Accepted`, aborts stale `Started`, retains actor-bound takeover handling, and switches the sole app startup callsite. No Event, projection, second queue, timer, watcher, or replay of stale started work entered the slice.

The recovered RED 2 boundary then added one durable row per batch member and proved the fixed 100-active/156-non-active/256-total transaction. The first capacity test failed only on the absent batch claim and typed capacity error; the preserved minimum GREEN admits whole batches under `BEGIN IMMEDIATE`, returns 100 `Accepted` plus 156 `Queued`, rejects item 257 without rows, keeps full-capacity retry idempotent, rolls back a boundary-crossing batch, and rejects source-tool-call rebinding. Because the already-completed state and capacity REDs share one still-unreleased table-rebuild migration, integration keeps that source-verified migration intact rather than retroactively redesigning it into new migration history.

Sol independently reviewed the staged transaction arithmetic, schema constraints, idempotency/conflict checks, recovery transitions, and exact app hunk. Deterministic verification passed all 43 `hya-store` integration tests (13 admission, 12 bundle registry, and 6 each persistence/resident-claim/store), the focused app startup-recovery test 1/1, touched `hya-store`/`hya-app` all-target Clippy with `-D warnings`, workspace rustfmt, cached diff check, and worktree diff check.

The foundation was committed and pushed atomically as `4d314751259790b3ac5180ff77617737455ca537` (`feat(admission): add durable capacity foundation`), with parent `a232b8102fed13bb70f3d5c0a43d258467a2ff0a` and exactly eight paths: seven store/migration paths plus only the app startup-recovery hunk. Exact-head pull-request CI run `30836134471` attempt 1, job `91761647490`, completed GREEN in 14m35s with TUI, fmt, Clippy, build, full workspace tests, and zero-inet-socket verification all passing. No rerun, exemption, or protected-state change was used.

## 18.9 Verified BundleCatalog identity integration

The preserved catalog prerequisite retains canonical verified `PreparedCatalog` provenance in the existing sole `BundleCatalog`, exposes a deterministic order-independent semantic identity only for verified construction, and keeps raw `from_prepared` identity unavailable. Installed-bundle refresh now combines the exact registry snapshot with the same built-in catalog Arc, while production embedded built-ins are constructed through the verified path. This introduces no catalog history, fallback resolver, second runtime authority, or change to TurnBinding pinning.

Sol independently reviewed the five-file patch for full prepared-digest coverage, deterministic framing, publication-order independence, exact registry-snapshot merging, and one-catalog ownership. Deterministic gates passed all 95 `hya-bundle` tests, the focused embedded identity test, the installed-refresh integration, all 156 `hya-app` tests, touched all-target Clippy with `-D warnings`, workspace rustfmt, and cached/worktree diff checks.

The slice was committed and pushed atomically as `e9b76b8ef22b71f029f99e97dd4435146a90fd76` (`feat(runtime): retain verified catalog identity`), with parent `4d314751259790b3ac5180ff77617737455ca537` and exactly five approved bundle/app paths. Exact-head pull-request CI run `30837696867` attempt 1, job `91766813635`, completed GREEN with TUI, fmt, Clippy, build, full workspace tests, and zero-inet-socket verification all passing. No rerun, exemption, or protected-state change was used.

## 18.10 Tool dispatch and immutable permission identity integration

The preserved tool prerequisite adds deterministic dispatch identity to the existing `ToolRegistrySnapshot`: built-ins bind the hya-tool implementation version and canonical name, explicitly identified runtime registrations retain their supplied identity, aliases remain resolution metadata rather than competing identities, removal clears only the canonical identity, and unidentified tools remain unavailable. `PermissionPlane` hashes only ordered immutable rules, invocation model/rules, and an identified interceptor; persistent/native/call grants, session/message/call IDs, live decisions, and channel state remain excluded. An unknown interceptor makes the identity unavailable instead of silently disappearing.

Sol independently reviewed registry insertion/removal/snapshot behavior, canonical/alias ownership, ordered permission evaluation, fail-closed interceptor propagation, and transient-state exclusions. Deterministic verification passed all 155 `hya-tool` tests, touched all-target Clippy with `-D warnings`, workspace rustfmt, and cached/worktree diff checks. Selective lockfile staging included only hya-tool's `sha2` dependency.

The slice was committed and pushed atomically as `fa38c9150dafddf2fbb212ef432d42ae7bc9a94e` (`feat(runtime): identify tool and permission semantics`), with parent `e9b76b8ef22b71f029f99e97dd4435146a90fd76` and exactly Cargo.lock plus five hya-tool paths. Exact-head pull-request CI run `30839299033` attempt 1, job `91772147503`, completed GREEN with every TUI, fmt, Clippy, build, full workspace-test, and zero-inet-socket step passing. No rerun, exemption, or protected-state change was used.

## 18.11 Plugin permission-bridge identity integration

The preserved plugin prerequisite makes the existing `PermissionBridge` an identified permission interceptor. Its deterministic identity follows the same configured-order `permission.ask` chain as dispatch and binds each effective participant's stable plugin ID, verified canonical initialize declaration, and effective safer posture. Connect completion is already normalized back to configured order. Nonparticipants, process handles, PIDs, restart/health state, transient connection results, event counters, and secret environment values remain excluded; no second host or permission chain is introduced.

Sol independently reviewed participant selection/order against actual dispatch, canonical declaration provenance, posture override handling, and transient-state exclusions. The complete hya-plugin package gate passed all unit/integration/transport/protocol tests, followed by touched all-target Clippy with `-D warnings`, workspace rustfmt, and cached/worktree diff checks. Selective lockfile staging included only hya-plugin's `sha2` dependency.

The slice was committed and pushed atomically as `6a8a64d91a66081457f8ae2a92f160adb77577e3` (`feat(plugin): identify permission bridge semantics`), with parent `fa38c9150dafddf2fbb212ef432d42ae7bc9a94e` and exactly Cargo.lock plus three hya-plugin paths. Exact-head pull-request CI run `30840643177` attempt 1, job `91776543437`, completed GREEN in 16m24s with every TUI, fmt, Clippy, build, full workspace-test, and zero-inet-socket step passing. No rerun, exemption, or protected-state change was used.

## 18.12 Runtime semantic binding fingerprint integration

The preserved core prerequisite adds `TurnBinding::semantic_fingerprint_v1` over the one captured runtime snapshot and the final immutable `PermissionPlane`. Its versioned canonical identity binds the verified bundle-catalog identity; explicit none, basic, and full tool views; only the selected workdir's ordered skill view; canonical plugin/MCP source declarations, resources, exports, permissions, aliases, and dispatch closure; and immutable permission semantics. Canonical JSON object ordering and sorted registry views make fresh equivalent owners stable, while `ConfigGeneration`, publication/insertion order, unrelated workdir caches, Arc/process identity, live grants and decisions, process health, secrets, and transient connection state remain excluded. Unverified catalog provenance, unidentified tools or permission interceptors, inconsistent source dispatch, and non-UTF-8 skill paths fail closed with no fingerprint. Runtime-source exports receive their deterministic dispatch identity through the existing registry; no core-to-plugin dependency, second authority, wire change, or latest-semantics fallback enters the slice.

Sol independently reviewed the three-file patch for complete ruled-section coverage, canonical framing, view ordering and precedence, dispatch-source consistency, fail-closed propagation, and transient-state exclusions. The four focused fingerprint tests each passed 1/1. The complete `hya-core` gate passed 62 unit tests and all integration targets; the worktree-lifecycle integration passed its executable case with its pre-existing explicit ignored case unchanged. `cargo clippy -p hya-core --all-targets -- -D warnings`, `cargo fmt --all --check`, and cached/worktree diff checks all exited zero.

The slice was committed and pushed atomically as `919fee659d6148bd83876b92cc424f353fde6f4c` (`feat(runtime): add semantic binding fingerprint`), with parent `6a8a64d91a66081457f8ae2a92f160adb77577e3` and exactly Cargo.lock, `crates/hya-core/Cargo.toml`, and `crates/hya-core/src/runtime_registry.rs`. Exact-head pull-request CI run `30842194144` attempt 1, job `91781709312`, completed GREEN in 14m52s with every TUI, fmt, Clippy, build, full workspace-test, and zero-inet-socket step passing. No rerun, exemption, or protected-state change was used.

## 18.13 Durable per-member launch binding integration

The source-reconciliation checkpoint first recorded the final Consult28/29 authority, source-corrected intent and recovery boundaries, and strict launch RED sequence without changing product behavior. It was committed and pushed as `7ff7cc4c534796c5c90eff30108e54c359ac1e78` (`docs(trellis): record admission identity authority`); exact-head CI run `30843702617` attempt 1, job `91786680195`, completed GREEN in 10m54s.

The next atomic RED added one durable binding test and failed only at the intentionally absent `AdmissionIntent`/`AdmissionBatchClaimOutcome` imports and the one-argument batch-claim signature. The minimum GREEN adds migration `0007`, one nullable all-or-none runtime/admission fingerprint plus opaque spawn-intent tuple per row, and an intent-bearing batch claim. New admissions return launch instructions only for committed `Accepted` members, so the exact 101-member boundary returns 100 launch instructions while one durable row remains `Queued`. An exact retry returns `Existing` without loading payload blobs; any changed byte conflicts. The store reuses the app codec's exact 1,048,576-byte cap and persists no resolved `AgentSpec`, provider, catalog, resource, task, actor, sidecar, or permit.

Sol reviewed schema nullability, fixed-column/blob equality, ordinal alignment, all-or-none insertion, idempotency/conflict behavior, and the absence of allocation or a second queue. The focused RED became 1/1 GREEN; admission tests passed 14/14, all 44 store integrations and all 156 app tests passed, followed by touched Clippy, workspace rustfmt, and diff checks. The slice was committed and pushed as `790ad74af5e480f20e73b14858ab76281c7514f6` (`feat(admission): persist durable launch bindings`), with parent `7ff7cc4c534796c5c90eff30108e54c359ac1e78` and exactly the app codec cap, migration, store API/export, and admission tests. Exact-head CI run `30845531585` attempt 1, job `91792780513`, completed GREEN in 14m20s with every gate passing.

## 18.14 Per-member start-barrier integration

The next atomic RED made concurrent callers start a specific accepted batch member and exited 101 only because `SessionStore::start_admission_member` was absent. The minimum GREEN shares one private SQL compare-and-set implementation: a member transitions `Accepted -> Started` exactly once; another ordinal remains independent; and a `Queued` member returns its existing state without crossing the barrier. Sol review caught one compatibility drift before release: the legacy `start_admission` wrapper must remain restricted to the sole ordinal-zero, `batch_size = 1` path rather than starting member zero of a batch. The corrected generalized API has no such batch restriction and preserves actor fencing.

The focused concurrency, independent-ordinal, queued, and compatibility tests passed; admission tests passed 15/15, all 45 store integrations and the complete core gate passed, followed by touched Clippy, workspace rustfmt, and diff checks. The slice was committed and pushed as `1f696108bdd80d016c9bd25e08ca10de188cca46` (`feat(admission): start batch members exactly once`), with parent `790ad74af5e480f20e73b14858ab76281c7514f6` and only the admission store source/tests. Exact-head CI run `30847353620` attempt 1, job `91798803908`, completed GREEN in 15m54s with every gate passing.

## 18.15 Bound-only pre-start recovery integration

The pre-start recovery RED inserted one legacy/unbound `Accepted` row and observed `Queued` instead of the required `Aborted`. The minimum GREEN keeps recovery in one bounded `UPDATE ... RETURNING`: stale `Started` rows still abort and release once; an `Accepted` row requeues only when all five runtime-fingerprint, admission-fingerprint, and intent columns are present; any missing tuple member aborts once without claiming a release. `Queued`, `Waiting`, and actor-bound rows remain outside this command. Recovery performs only structural presence checks here—no semantic parse, target resolution, task, sidecar, actor, provider, or latest-runtime rebind.

The new unbound and existing bound-positive recovery tests each passed 1/1; admission tests passed 16/16, all 46 store integrations and all 156 app tests passed, followed by touched Clippy, workspace rustfmt, and diff checks. The slice was committed and pushed as `c4f8be9f8dd66247a6cd0e6fa20dd3468c7410a8` (`fix(admission): abort unbound recovered work`), with parent `1f696108bdd80d016c9bd25e08ca10de188cca46`, tree `d44d011cd8b661a0cb7cd0c794eb0aa4c393d029`, and only admission store source/tests.

Exact-head CI run `30849031466` attempt 2, job `91805736175`, completed GREEN in 15m27s with TUI/PTy, fmt, Clippy, build, full workspace tests, strace, and zero-inet-socket verification passing. Attempt 1 failed only when both unchanged TypeScript PTY workspace widths timed out on a Main marker before any Rust gate; the immediately preceding SHA had passed them, this commit changes only `hya-store`, and exact local 80/140 focused reproductions both passed. The same-SHA successful rerun is controlling; no source edit, fixture change, exemption, or protected-state change was used.

## 18.16 Reconstructed launch and nested-admission identity continuation

Recovered accepted launches next reconstructed the exact persisted parent binding and current matched semantics before allocation. Commit `5d166ce9d84fc21f83c59fe65fe7a9d901731bad` (`fix(admission): reconstruct promoted launch bindings`) passed exact-head CI run `30872086840` attempt 1/job `91875987488`. Commit `ab6d569e3b12f4ea6668da869ab2f3e33951372e` (`feat(admission): execute accepted transient launches`) then installed one real accepted transient member behind its durable `Started` barrier, finalized its exact row, and returned committed promotions; exact-head run `30873348313` attempt 1/job `91879639657` passed.

Commit `546fce5dac8846c2d5e5ac64be124db852a1c4d8` (`feat(admission): propagate parent admission identity`) added process-local `AdmissionMemberIdentity` task context and retained it on nested `BoundSpawnRequest` without changing Event/session/wire/store bytes; exact-head run `30874580366` attempt 1/job `91883351320` passed. Commit `dd2cade149df57573938f89e19c322d2f76c189e` (`feat(admission): scope transient nested spawns`) made only the recovered non-actor transient installer call the singular pre-admitted-member path with its exact operation/member identity; actor and live supervisor paths remained unopened. Local focused, full hya-app, Clippy, fmt, and diff gates passed, followed by exact-head CI run `30875698710` attempt 1/job `91886593022`, green from `2026-08-04T03:46:25Z` to `03:57:49Z` with all TUI, fmt, Clippy, build, workspace-test, strace, and zero-INET steps passing.

That last CI is compatibility evidence only for the committed source. Post-commit review found that its test shadows the engine's uniquely configured fake router with a new DevProvider router before `AdmissionResolutionContext::capture`; therefore the intended same-`Arc` resolution/execution invariant is not yet certified. Before any live queued-reply RED, one test-only behavioral RED must observe that the engine provider did not participate in capture, then GREEN must delete the shadow and reuse the exact engine router. No production/wire/store change is authorized for that repair.

## 18.17 Consult30 live queued caller-reply authority

Pro chose Option A for 0.34.12 live queued caller replies. `Queued` is internal admission only and completes no caller reply. Foreground retains one process-local sender until its complete admitted batch is durable-terminal. One-member background/resident returns the existing real running outcome only after exact promotion, member `Started` CAS, and real registration. Queued cancellation and promotion race solely through the existing journal writer transaction on the same row; cancellation-first terminalizes allocation-free and returns unit `SpawnError::Cancelled`, promotion-first follows established later cancellation.

The first Mac source check used a stale sync area. Exact fuji1 HEAD already has a Result-bearing reply oneshot and already maps channel loss to `SpawnError::Unavailable`; no channel-type or flattening change is needed. Sender loss never cancels/retries durable work, and no sender survives restart. Required live REDs remain the 101-member foreground whole-batch barrier, background/resident registration gate, cancellation-first, promotion-first, dropped-receiver variants, and mixed foreground cancellation, one certified atomic boundary at a time. Public queued outcomes, placeholder identities, reply registries, queued allocation, replay/reconnect, and Event/DTO/wire changes remain excluded.

## 18.18 Consult31 corrected foreground owner/lifecycle authority

Commit `994ea6b2a6afb0b80183d448c36398c5d23446aa`
(`feat(admission): queue foreground transient batches`) is retained without
history rewrite. Exact-head CI run `30885299291` attempt 3/job `91917608968`
completed GREEN at `2026-08-04T07:15:33Z`; attempts 1-2 stopped before Rust on
the unchanged, source-proven 140-column PTY timing signature. The successful
run is behavior-compatibility evidence only. The commit remains source-rejected
because it creates a detached post-claim owner, does not provide a bounded
supervisor lifecycle, and cannot safely route foreign-operation promotions.

RED0 source closure proves no new store read API is required. The pre-admission
handler retains the exact prepared `AdmissionIntent` vector; existing ordered
`SessionStore::admissions(operation_id)` supplies current journal state. Exact
count, contiguous unique ordinals, and complete immutable claim identity are
checked before constructing existing `AdmissionLaunch` values and reusing
`SpawnIntentV1::decode_admission_launch`, `resolve_admission_launches`, and the
member-specific `start_admission_member` CAS. Exact reply evidence stays in the
owning handler.

The controlling implementation order is strictly one RED/GREEN boundary at a
time: (1) uniform pre-admission handler across accepted, mixed, and all-Queued;
(2) 256-handler intake cap; (3) foreign promotion is wake-only; (4) owner
rehydrates and starts each ordinal once; (5) identical members preserve exact
reply order and identity; (6) postcommit wake race matrix; (7) all three current
batch sites wake only after commit; (8) explicit shutdown drains 256 handlers;
(9) Drop fallback is nonblocking; and (10) every production exit awaits
shutdown. Queued-cancel batch wake and root/session batch-cleanup migration
remain later mandatory boundaries.

The already accepted simplicity pruning is integrated into these minimum
GREENs rather than layered afterward: one checked ordinal-slot vector,
capacity-one completion/wake channels, direct ownership of real handles,
consolidated resolution and fail-after-claim flow, no evidence clone, and
bounded test instrumentation. The first code boundary is only
`foreground_handler_uniform_pre_admission`; no later owner, cancellation,
background, resident, or root-cleanup RED overlaps it.

## 18.19 Consult31 owner rehydrate-on-wake slice 4

The next atomic RED added only `owner_rehydrates_accepted_ordinals_exactly_once_on_wake`.
It reuses the foreign-promotion topology (A0 holds the last active lease; A1 is
intentionally unresolvable so finalization wakes B without A executing B) and
requires B's owner to cross `Started` after the wake with exactly one `worker-b`
resolution. The focused command exited 101 with the sole diagnostic that the
2s Started wait timed out while B remained `Accepted`.

The minimum GREEN retains claim-time `AdmissionIntent`s on the foreground owner,
drives the wake receiver in the owner `run` loop, and on register plus every wake
rereads `SessionStore::admissions` to start currently Accepted unscheduled
ordinals via the existing promote/resolve/`start_admission_member` path. Foreign
`completion.promoted` launches wake the owning operation instead of failing the
committer. No store launch-read API, public types, Event/DTO/wire changes, or
shutdown/cancel scope entered.

Focused `owner_rehydrates_accepted_ordinals_exactly_once_on_wake` 1/1,
`foreign_promotion_is_wake_only` 1/1, `foreground_handler_*` 2/2, wake-router unit
tests 2/2, full `hya-app` lib 144/144, `cargo clippy -p hya-app --all-targets -- -D warnings`,
`cargo fmt --all --check`, and `git diff --check` all green.

## 18.20 Consult31 ordered-reply lock and BuiltSessionEngine lifecycle

Consult31 RED 5 is already satisfied by ordinal-indexed owner evidence. Commit
`69c39e0c` locked reverse-completion identical-member ordered replies with
distinct MemberId/session identity without product change.

The next product slice introduced public non-Clone `#[must_use] BuiltSessionEngine`
from `build_session_engine`, owning engine products plus private
`SpawnSupervisorLifecycle`. Explicit `shutdown` cancels intake, aborts handlers,
and drains the supervisor JoinSet; Drop is nonblocking stop+abort only. Headless
backend commands and serve paths shut down after body completion. Cleanup
finalization wakes foreign owners only after durable commit (third finalize site).

Focused lifecycle tests, full hya-app lib 147/147, clippy `-D warnings` on
hya-app/hya-backend, and rustfmt are green.

## 18.21 Consult30 whole-batch foreground reply lock

Consult30 RED 1 is locked with a capacity-faithful mixed batch: 99 active
fillers force a 2-member foreground claim into Started+Queued. The owner keeps
the reply pending until the queued ordinal is promoted, started, and terminal,
then returns ordinal-ordered MemberOutcome identity. Full hya-app lib green.

## 18.22 Consult30 background delayed running reply

Single-member background transient batches now use the durable admission owner
with `BackgroundRunningOnRegister` reply mode: claim may leave the row `Queued`
with no session and no caller reply; after promotion, durable `Started` CAS,
and real session registration the owner sends one `running` outcome and detaches
work. Added `SpawnError::Cancelled` for later cancel-first barriers. Focused
`background_running_reply_only_after_registration` plus full hya-app lib and
clippy are green.

## 18.23 Consult30 cancel-first before activation

While the durable owner still holds the caller oneshot, cancellation finalizes
all nonterminal journal rows as `Cancelled`, aborts any handles, releases debit,
and returns `SpawnError::Cancelled` with no child session allocation. Later
promotion skips the cancelled operation. Focused cancel-first and background
registration tests plus full hya-app lib/clippy are green.
