# Implementation plan: Workflow stage model routing

## Execution rules

This is an ordered TDD plan. For Slices 1–10, add the smallest observable
failing contract first, run only the named focused test to prove RED, implement
only the owned change, and rerun the same focused command to prove GREEN. These
focused RED/GREEN slices must not run formatters, linters, workspace builds,
project-wide tests, process-E2E, release rehearsal, or live-provider gates.

Slices 11 through release preparation, plus the final project gate, are explicit
authorized exceptions with their commands listed in this plan. Slice 11 may
build and run deterministic process E2E with local FakeLlm. Release preparation
may run current-version project/release checks and the non-publishing rehearsal.
Live provider traffic is authorized only in the mandatory live slice, after the
0.36.7 release gates produce the exact final binary and provider identity.
Preserve unrelated user changes.

The implementation starts only after explicit approval of this complete plan and
these public seams, followed by the normal Trellis task activation. No source
code, task activation, commit, push, or live request is part of this planning
turn.

## Shared contracts to land first

The following wire and runtime names are used by later slices:

```text
hya-workflow::WorkflowModelCandidate
hya-workflow::WorkflowModelAssignment
hya-core::WorkflowModelRouteCandidate
hya-core::WorkflowModelRoute
hya-proto::WorkflowModelCandidate
hya-proto::WorkflowModelAssignment
hya-proto::WorkflowModelResolvedCandidate
hya-proto::WorkflowStageRouteOutcome
hya-proto::WorkflowRouteFailureClass
```
The source assignment is `{id, reasoning?, fallback[]}` and every assignment or
fallback `id` is a base model reference without `#variant`; the compiler rejects
every embedded suffix. It trims provided reasoning and rejects only an empty
provided reasoning string at compile time; unknown non-empty labels are parsed
and capability-checked at runtime. Authored effort remains optional. The
admitted candidate and route-outcome effort are required canonical strings, with
`ReasoningEffort::Off` serialized as `none`. The durable StagePlan contains the
requested worker/verifier assignments and distinct resolved selected candidates.
The route-outcome Event contains one explicit-route stream-group result:
`run/stage/member/role/iteration/step`, `candidate_index`, base model id,
required effort, and stable failure class only. Re-export compiler values from
`hya-workflow/src/lib.rs`, and re-export core route values from both
`hya-core/src/workflow/mod.rs` and `hya-core/src/lib.rs`.

## Slice 1 — Compiler model, syntax, duplicates, and revision compatibility

### RED

Modify `crates/hya-workflow/tests/compile.rs` and
`crates/hya-workflow/tests/semantics.rs` first:

1. Add a Workflow fixture with a worker `model` block and a loop `verify.model`
   block. Assert preferred id, fallback order, and effort accessors.
2. Add a same-id/different-effort fixture and assert both entries survive in
   order.
3. Add exact duplicate preferred/fallback and duplicate fallback entries;
   assert `WorkflowCompileErrorKind::Validation`, Stage location, and a
   duplicate-pair diagnostic.
4. Add empty id, explicitly empty reasoning, unknown nested key, and `#variant`
   ids both with and without `reasoning`; assert every embedded suffix and empty
   value is rejected. Add a non-empty unknown effort label and assert compilation
   preserves it for the runtime seam rather than rejecting it.
5. Keep the existing known no-assignment revision assertion and add a new
   assignment fixture whose revision differs from the no-assignment revision.

Run:

```sh
cargo test -p hya-workflow --test compile --test semantics
```

Expected RED: the assignment fields, accessors, and validation do not yet
exist.

### GREEN

Update:

- `crates/hya-workflow/src/model.rs`: add the compiler-owned assignment and
  candidate structs, accessors, `WorkflowStage.model`, and `VerifySpec.model`.
- `crates/hya-workflow/src/lib.rs`: re-export the assignment/candidate values and
  add a public import test.
- `crates/hya-workflow/src/compiler.rs`: deserialize the nested blocks under
  existing `deny_unknown_fields`, trim ids and provided effort strings, reject
  empty ids/effort and every embedded suffix unconditionally, preserve unknown
  non-empty labels for runtime parsing, reject exact ordered duplicate pairs, and
  copy values into the immutable plan.
- `crates/hya-workflow/src/compiler.rs`: preserve the current hash input exactly
  for plans with no assignments; append one domain-separated, length-delimited
  model-routing extension only when any worker/verifier assignment exists.

Do not add provider dependencies to `hya-workflow`; provider-specific effort
parsing stays in runtime/provider code. Do not change the prepared bundle format.

Focused GREEN gate:

```sh
cargo test -p hya-workflow --test compile --test semantics --test render
```

## Slice 2 — Provider default metadata and shared Agent model-policy resolution

### RED

Add focused tests beside the existing provider/config tests:

1. A deterministic Provider implementation returns a configured default effort
   through the new provider metadata seam; an implementation without metadata
   returns `None`.
2. `ProviderRouter` returns the default for a routed model and no default for an
   unknown model.
3. Existing config parsing preserves explicit model defaults, configured variant
   order, and the current default selection.
4. The shared Agent model/category helper reproduces normal task-spawn
   precedence for explicit model, category, base model, and reasoning.
5. Provider configured identity includes per-model defaults, including `none`
   for Off, in stable code-unit model-id order. A default-only change changes
   provider identity; the same rows inserted in another order do not.

Run:

```sh
cargo test -p hya-provider
cargo test -p hya-app --lib config::
cargo test -p hya-app --lib runtime::
```

Expected RED: Provider has no default metadata method and Workflow/runtime code
cannot resolve a candidate's own default.

### GREEN

Update:

- `crates/hya-provider/src/lib.rs`: add the default `Provider` method
  `reasoning_default(&ModelRef) -> Option<ReasoningEffort>` with a no-metadata
  default; keep `ProviderModel` wire/catalog shape unchanged.
- `crates/hya-provider/src/http.rs`: store per-model defaults and add a builder
  for them; return the value only for a model claimed by the route. Extend
  `configured_identity_v1` with length-delimited per-model default labels,
  including canonical `none`, sorted by stable code-unit model id.
- `crates/hya-provider/src/router.rs`: expose the default through the resolved
  route and retain existing first-match semantics; aggregate provider identity
  bytes without relying on insertion order.
- `crates/hya-app/src/config.rs`: pass `ParsedModel.reasoning_default` into
  `HttpProvider` while retaining current configured-variant validation/order.
- Add provider identity tests proving default-only sensitivity and insertion-order
  stability. Confirm the identity flows into runtime/admission fingerprints and
  Workflow request hashes in the later app/runtime slices.
- Extract the model/category portion of the normal task-spawn resolution from
  `crates/hya-app/src/runtime.rs` into one reusable helper (or a core helper
  accepting category/servability callbacks). Make both normal spawn and
  Workflow resolution call it; do not create a second precedence list.

Focused GREEN gate:

```sh
cargo test -p hya-provider
cargo test -p hya-app --lib config::
cargo test -p hya-app --lib runtime::
```

## Slice 3 — Proto wire values, route-outcome Event, and SDK mirrors

### RED

Add tests before implementation:

1. `crates/hya-proto/tests/workflow_projection.rs`: authored assignment and
   selected `WorkflowModelResolvedCandidate` fields round-trip; authored
   reasoning remains optional while selected/outcome reasoning is required
   canonical (`none` for Off). A route outcome carries role, iteration, step,
   candidate index, base model, required effort, and stable failure class.
2. Add old Event/projection JSON fixtures with no routing fields and assert exact
   decode/re-encode value and byte compatibility, including omitted empty route
   collections.
3. Add duplicate outcome-key replay cases and assert only one row remains.
4. `crates/hya-sdk/tests/workflow_mirror_conformance.rs`: the same JSON decodes
   into SDK mirrors and preserves optional authored plus required resolved fields.

Run:

```sh
cargo test -p hya-proto --test workflow_projection
cargo test -p hya-sdk --test workflow_mirror_conformance
```

Expected RED: the wire structs, Event variant, reducer fields, and SDK mirrors
are absent.

### GREEN

Update:

- `crates/hya-proto/src/workflow.rs`: add string-based authored assignment and
  candidate mirrors plus distinct `WorkflowModelResolvedCandidate` with required
  canonical effort and index. Add optional authored/selected worker and verifier
  route fields to `WorkflowStageInfo` and `WorkflowStagePlan`; add
  `route_outcomes` to `WorkflowStageProjection` with
  `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- `crates/hya-proto/src/event.rs`: add
  `WorkflowStageRouteOutcome` and the closed stable failure-class enum. Include
  it in `Event::session()` and all Workflow-event validation paths. Make outcome
  effort a required canonical string (`none` for Off); keep all fields bounded
  and secret-free.
- `crates/hya-proto/src/projection.rs`: fold outcomes only for the active run and
  matching Stage, deduplicating `(stage, member, role, iteration, step)` and
  ignoring late old/terminal events. Do not resolve models in the reducer.
- Apply `#[serde(default, skip_serializing_if = "Option::is_none")]` to every
  new optional field and default/skip every new empty collection. Ensure a
  no-assignment projection and old Event JSON re-encode with identical values
  and bytes.
- `crates/hya-proto/src/lib.rs`: re-export all authored, resolved, and outcome
  public types.
- `crates/hya-sdk/src/workflow.rs` and `crates/hya-sdk/src/lib.rs`: add exact
  transport-independent authored/resolved/outcome mirrors and re-exports with
  additive defaults.

Focused GREEN gate:

```sh
cargo test -p hya-proto --test workflow_projection
cargo test -p hya-sdk --test workflow_mirror_conformance
```

## Slice 4 — Request-local fallback and bounded route observation

### RED

Extend `crates/hya-core/tests/model_fallback.rs` and add a focused Workflow
runtime fixture:

1. Two concurrent/independent Stage activations use the same preferred model
   but different fallback order and efforts. Assert each provider sees only its
   own chain; no global map collision occurs.
2. Assert each candidate receives its base model and separate
   `CompletionRequest.reasoning`, including omitted effort resolving to its own
   configured default or typed `ReasoningEffort::Off`; no model suffix is used.
3. A route admitted at `selected_index = 1` never requests unroutable preferred
   index 0, and a later pre-stream fault advances only to later candidates.
4. Preserve existing retryable pre-stream advancement, auth/incompatible/decode
   no-advance, and post-stream no-replay tests.
5. A provider stream group with tools/multiple assistant steps records one
   finalized route observation per step, not one per candidate attempt.
6. Assert selection returns stream, selected candidate/index, and pending
   pre-stream class; `none` is finalized only after successful stream collection.
7. Assert post-stream errors finalize their stable class without failover/replay,
   and exhausted pre-stream failures finalize before returning the error.
8. A final provider failure still records one bounded observation with a stable
   failure class and no raw provider text.
9. Cancellation before the first candidate attempt starts returns cancellation
   with no route observation.
10. Cancellation after a candidate attempt starts, including after selection
    returns a stream, finalizes exactly one `cancelled` observation for the last
    attempted/selected candidate and does not fail over.

Run:

```sh
cargo test -p hya-core --test model_fallback
```

Expected RED: engine fallback is global-only and has no Workflow route context
or observation.

### GREEN

Update:

- `crates/hya-core/src/engine/turn.rs`: thread an optional request-local route
  and route recorder through the internal turn activation. Keep existing
  wrappers passing `None`. Make the local chain win over the global
  `HashMap<ModelRef, Vec<ModelRef>>`; preserve the current
  `is_retryable_before_stream`/`UnknownModel` boundary and never switch after a
  stream exists.
- `crates/hya-core/src/subagent.rs`: add a Workflow-only team execution helper
  that passes a route and stream-group context to each member without changing
  ordinary `MemberSpec` callers or installing global state.
- `crates/hya-core/src/engine/turn.rs` (or a small core-owned route module):
  classify provider failures into stable classes and return a selection record
  containing stream, selected candidate/index, and pending pre-stream class.
  Start at immutable `selected_index`, advance only forward, and finalize one
  bounded observation after `collect_stream_round` or at the terminal error
  boundary. Store only candidate index/model/effort/class and context ids.
- Update the loop worker/verifier call path to pass independent route contexts,
  the assistant step index, and the finalized-outcome handoff.

Focused GREEN gate:

```sh
cargo test -p hya-core --test model_fallback
```

## Slice 5 — Workflow route resolution and preflight

### RED

Extend `crates/hya-core/tests/workflow.rs` with deterministic provider fixtures:

1. A worker assignment overrides the Agent model/category/reasoning policy.
2. A verifier assignment independently overrides its Agent policy.
3. An absent assignment uses the exact existing Agent resolution path and global
   category behavior, creates no local route/selected fields or recorder, and
   emits no route-outcome Event.
4. Compiler-trimmed unknown non-empty effort labels reach runtime; runtime
   rejects them through typed `ReasoningEffort` and provider capability checks.
5. An omitted preferred effort uses the preferred model default; an omitted
   fallback effort uses the fallback model default; neither inherits the other
   or the Agent effort.
6. Same model with different efforts remains ordered; exact effective duplicate
   fails before any child/provider effect.
7. An unroutable preferred candidate plus routable fallback admits index 1; all
   later stream attempts start at index 1 and never request index 0.
8. Unknown tail candidates are retained when another candidate routes; an
   all-unknown chain fails before child/resident/mail/provider side effects.
9. Two Stages sharing an actor key with different effective routes fail before
   actor creation; identical routes reuse the resident.
10. The critical loop case uses one base model for worker and verifier with
    different efforts. Assert exact request reasoning, distinct worker/verifier
    roles, selected candidates, and replayed outcome rows.
11. A Stage route is request-local even when its preferred model equals a
    category preferred model.

Run:

```sh
cargo test -p hya-core --test workflow
```

Expected RED: `WorkflowRunContext` has no model-routing context and
`resolve_agent` has no Stage route.

### GREEN

Update:

- `crates/hya-core/src/workflow/run.rs`: add a shared routing context containing
  the configured `CategoryRegistry` and `ProviderRouter`, and resolve each
  explicit worker/verifier assignment before reservation. Apply Agent
  model/category policy through the shared helper, then the explicit assignment.
  Absent assignments call the exact old path and create no route/selected data,
  recorder, or outcome Event.
- Add `WorkflowModelRouteCandidate` and `WorkflowModelRoute` in the core
  workflow module; each candidate keeps a base `ModelRef` plus typed
  `ReasoningEffort`, never a suffix. Validate known effort, advertised provider
  capability, effective duplicates, and at least one routable candidate. Keep
  unknown tails for `UnknownModel` fallback.
- For explicit routes, store the first routable candidate's immutable
  `selected_index` on `ResolvedAgent`/`PreparedWorkflowRun` and expose a
  read-only route-plan view for app admission. Every stream group starts there,
  advances only to later declarations, and never wraps. Do not resolve again
  after preflight.
- Compare full effective worker routes for actor keys. Reject route mismatch;
  keep verifier routes transient and independently effort-bound.
- `crates/hya-core/src/workflow/mod.rs` and `crates/hya-core/src/lib.rs`:
  re-export the public route types and add import/compile tests for the promised
  seams.
- Update every `WorkflowRunContext` fixture/caller to provide the optional
  routing context; `None` remains valid for old no-assignment direct tests.

Focused GREEN gate:

```sh
cargo test -p hya-core --test workflow
cargo test -p hya-core --test model_fallback
```

## Slice 6 — Transient/resident route propagation and durable outcome append

### RED

Add runtime tests that observe the owning root Session log:

1. A transient explicit-route Stage emits one finalized
   `WorkflowStageRouteOutcome` after each stream group drains and before
   Stage/run terminalization; an absent assignment emits none.
2. The critical loop case uses one base model for worker and verifier with
   different efforts. Assert distinct request reasoning, worker/verifier roles,
   selected candidates, and replayed outcomes.
3. A resident explicit-route Stage emits its finalized outcome to the root
   Workflow log despite the child actor claim; ordinary resident wakes and
   absent assignments emit no Workflow outcome.
4. Pre-attempt cancellation emits no route outcome; post-attempt and
   post-selection cancellation persist exactly one `cancelled` outcome for the
   last attempted/selected candidate, with no failover, while preserving the
   existing Stage/run status.
5. Outcome model ids and required effort strings are bounded and contain no
   provider response text, credentials, directives, or inputs.

Run:

```sh
cargo test -p hya-core --test workflow
cargo test -p hya-core --test resident
```

Expected RED: resident slots and transient member runs do not carry route
recorders or Workflow outcome context.

### GREEN

Update:

- `crates/hya-core/src/resident.rs`: add a fixed request-local route to
  `SlotState`/`RunPlan` only for explicit Workflow actors, plus an
  activation-scoped bounded recorder installed only while the actor is idle.
  Pass it through `run_one_turn`; clear it after the outer Workflow activation
  drains exactly one finalized outcome per explicit-route stream group. Recovery,
  ordinary resident registration, and absent assignments remain route-free.
- `crates/hya-core/src/workflow/run.rs`: pass route context to explicit transient
  members, loop worker iterations, and verifier judgments. Persist finalized
  outcomes to the owning root with the existing actor fence before
  StageFinished. For resident activations, drain the supervisor recorder and
  append with the outer Workflow actor claim; never append a root event with a
  child claim.
- Add the new Workflow event to `record_workflow_event_for_actor` validation.
- Keep existing Stage/member lifecycle order. Use an activation-owned
  finalization guard/recorder so cancellation after attempt start finalizes once
  and the outer Workflow owner drains it; pre-attempt cancellation emits none.
  Post-stream failures do not trigger failover or replay.

Focused GREEN gate:

```sh
cargo test -p hya-core --test workflow
cargo test -p hya-core --test resident
```

## Slice 7 — App control, catalog DTOs, and admission plan

### RED

Extend `crates/hya-app/tests/workflow_control.rs`:

1. `Info` exposes authored worker/verifier assignments in exact order, with
   optional authored reasoning preserved.
2. `Run`/`State` expose the same requested route and the admission-selected
   `WorkflowModelResolvedCandidate` with required canonical effort from the
   durable plan.
3. Configured provider defaults and Stage-over-Agent precedence survive app
   control preflight. A default-only change changes provider identity, admission
   binding fingerprint, Workflow request hash, and effective effort; changing
   insertion order alone does not.
4. No-routable assignment maps to one bounded stable Workflow control error and
   creates no child/event side effect beyond the existing error path.
5. A route outcome is present in the returned run projection and idempotent run
   retry returns the same replayed outcome rows.
6. The critical loop worker/verifier pair uses one base model with distinct
   efforts and exposes distinct roles, selected candidates, request reasoning,
   and replayed outcomes through the public app result.

Run:

```sh
cargo test -p hya-app --test workflow_control
```

Expected RED: WorkflowControl does not own configured categories/router and
mapping omits route fields/outcomes.

### GREEN

Update:

- `crates/hya-app/src/runtime.rs`: retain/share the configured category and
  provider contexts when constructing `WorkflowControl`; include stable
  per-model reasoning defaults in provider identity/fingerprint data without
  exposing secrets. Do not add a second router or alter category fallback wiring.
- `crates/hya-app/src/workflow_control.rs`: pass the shared routing context into
  `WorkflowRunContext`; use the prepared route-plan getter when constructing
  `WorkflowRunStarted`; map authored assignments and resolved selected DTOs to
  proto DTOs. Preserve revision fencing, request hashing, actor claims,
  Started/Finished delivery, and idempotency.
- Add app tests that compare provider identity, admission binding fingerprint,
  Workflow request hash, and effective effort for default-only changes and prove
  insertion-order stability. Verify no-assignment requests preserve the exact
  old result/event path with no route fields.
- Map route validation failures to existing bounded Workflow invalid-source or
  execution categories without leaking provider response data.

Focused GREEN gate:

```sh
cargo test -p hya-app --test workflow_control
```

## Slice 8 — Store/replay and event compatibility

### RED

Extend `crates/hya-store/tests/workflow_recovery.rs` and proto tests:

1. Close/reopen equality includes explicit-assignment Stage plans and finalized
   route outcomes.
2. Duplicate outcome keys do not grow projection state.
3. Old no-route Event/projection JSON and old prepared v2 sources decode and
   re-encode with identical values and bytes; `route_outcomes` and every new
   empty collection remain omitted.
4. An absent assignment has exact old Event-log equality and emits no route
   fields, recorder, selected plan, or outcome Event.
5. Late outcome events for old/terminal runs are ignored.
6. Replay causes no provider request, child spawn, resident wake, or route
   resolution.

Run:

```sh
cargo test -p hya-store --test workflow_recovery
cargo test -p hya-proto --test workflow_projection
```

Expected RED: store Event validation/query fixtures and projection expectations
do not include the new event/fields.

### GREEN

Update:

- `crates/hya-store/src/workflow.rs` and any event validation helpers to accept
  the new Workflow event while keeping the same owning-session transaction and
  actor-fence rules. No SQL schema or separate table is needed.
- Update store fixtures for additive event serialization and replay. Apply
  `serde(default, skip_serializing_if = "Option::is_none")` to new optional
  fields and `serde(default, skip_serializing_if = "Vec::is_empty")` to every
  new routing collection so old JSON re-encodes byte-identically.
- Keep startup interruption/recovery unchanged: explicit-route outcomes already
  in the log survive; recovery appends one Interrupted run finish and never
  replays a Stage or route. Absent assignments remain on the old path.

Focused GREEN gate:

```sh
cargo test -p hya-store --test workflow_recovery
cargo test -p hya-proto --test workflow_projection
```

## Slice 9 — CLI, server, SDK, Agent tool, and TUI compatibility

### RED

Add focused parity tests:

1. `crates/hya-server/tests/workflow_session_api.rs`: native/legacy/v2 typed
   responses contain identical route/outcome JSON and structured errors.
2. `crates/hya-backend/tests/workflow_cli.rs`: text `info`, `state`, and `run`
   show route ids/efforts/outcome classes without secrets; `--json` equals the
   shared DTO.
3. `crates/hya-tool/tests/workflow.rs`: tool results retain route fields and
   existing operation/actor identity.
4. `crates/hya-sdk/tests/workflow_mirror_conformance.rs`: typed state/activity
   mirrors retain route fields.
5. `packages/hya-tui-ts/test/workflow-presentation.test.ts`: a projection with
   optional route fields produces the same sidebar presentation as the old
   projection and malformed core fields still report `invalid`.

Run:

```sh
cargo test -p hya-server --test workflow_session_api
cargo test -p hya-backend --test workflow_cli
cargo test -p hya-tool --test workflow
cargo test -p hya-sdk --test workflow_mirror_conformance
bun --cwd packages/hya-tui-ts test test/workflow-presentation.test.ts
```

Expected RED: CLI formatting and mirror fixtures omit the new values; all
existing TUI fields must remain unchanged.

### GREEN

Update:

- `crates/hya-backend/src/workflow_cmd.rs`: print compact requested/selected
  routes and outcome summaries; never print raw provider errors or secrets.
- `crates/hya-server` route adapters and `crates/hya-tool/src/workflow_plane.rs`:
  keep commands and delivery unchanged while passing the extended shared DTOs.
- `crates/hya-sdk/src/workflow.rs`: finish mirror/conformance updates if Slice 3
  did not cover all projection fields.
- `packages/hya-tui-ts/src/hya/workflow-presentation.ts`: retain compact sidebar
  behavior; parse/ignore optional route fields safely if the typed shape is
  updated. Do not render a route block, add polling, or add a second client.

Focused GREEN gate:

```sh
cargo test -p hya-server --test workflow_session_api
cargo test -p hya-backend --test workflow_cli
cargo test -p hya-tool --test workflow
cargo test -p hya-sdk --test workflow_mirror_conformance
bun --cwd packages/hya-tui-ts test test/workflow-presentation.test.ts
```

## Slice 10 — Bundle and docs

### RED

Add bundle and documentation contract tests:

1. `crates/hya-bundle/tests/prepare.rs` compiles a WorkflowBundle with worker and
   verifier routes, but still writes prepared format v2.
2. Add a no-assignment prepared-v2 fixture asserting the existing compiler
   revision remains unchanged.
3. `crates/hya-bundle/tests/catalog.rs` confirms route-bearing sources use the
   common Workflow compiler and local model availability is deferred to runtime.
4. `crates/hya-bundle/tests/docs_example.rs` requires the new syntax, precedence,
   route-outcome, and no-TUI claims in both Workflow docs.

Run:

```sh
cargo test -p hya-bundle --test prepare --test catalog --test docs_example
```

Expected RED: docs and fixtures do not describe model assignment.

### GREEN

Update:

- `docs/workflows.md`: add worker/verifier assignment syntax, defaults,
  precedence, local fallback, route outcome semantics, and no-TUI scope. State
  that every new assignment/fallback id is a suffix-free base model reference;
  `reasoning` is the only effort field and embedded `#variant` is rejected.
- `.autors/hya/wiki/pages/architecture/workflow-composition.md`: update the
  architecture/code-map and durable route audit description.
- `docs/architecture/providers.md` and any model-selection subsection:
  distinguish the suffix-free Workflow assignment block from existing
  `#variant` request behavior in non-Workflow model APIs.
- Bundle fixtures/examples only where needed; do not add secrets or bump
  `PreparedWorkflowBundle` format.

Focused GREEN gate:

```sh
cargo test -p hya-bundle --test prepare --test catalog --test docs_example
```

## Slice 11 — Mandatory deterministic process E2E

### RED

Add `crates/hya-e2e/tests/p19_workflow_model_routing.rs` and the new Track P
matrix entry `T2.14` in `crates/hya-e2e/matrix.toml` (the existing P17 `T2.13`
row and `p17_workflow_composition.rs` test must remain unchanged and continue
to run).
The new production-binary scenario uses local scripted FakeLlm with several
fake base model ids and a user-authored Workflow containing worker and verifier
routes. It must include the critical loop case: worker and verifier use the
same base model with different efforts. Capture exact model and separate
reasoning values from requests, invoke the public Workflow CLI/HTTP control
path, read public Workflow state route outcomes, and assert roles, selected
candidates, and close/reopen replay. The fixture must not use `#variant` in any
new assignment id.

Run the exact binary before focused process tests, serially:

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p17_workflow_composition -- --test-threads=1
cargo test -p hya-e2e --test p19_workflow_model_routing -- --test-threads=1
cargo run -p xtask -- matrix-check
```

Expected RED: the new route-bearing fixture, request capture, public state
outcomes, and matrix row do not exist.

### GREEN

Implement the scenario through `E2eEnvBuilder` and the production
`hya-backend` binary. Use local FakeLlm queues/routes so request attribution is
deterministic. Assert exact base model plus `reasoning` on every worker and
verifier request, the same-base/different-effort role pair, fallback outcome
rows, public state JSON, and replay equality. Keep FakeLlm evidence bounded and
secret-free. Re-run the two focused tests, then the full serial Track P suite:

```sh
cargo test -p hya-e2e --test p17_workflow_composition -- --test-threads=1
cargo test -p hya-e2e --test p19_workflow_model_routing -- --test-threads=1
cargo test -p hya-e2e -- --test-threads=1
```

## Slice 12 — Release preparation and current-version gates

After the deterministic focused slices and Slice 11 process E2E pass, apply the
release edits before any live-provider proof. The current release is `0.36.6`;
update the next version to `0.36.7` in `Cargo.toml`, all hya package entries in
`Cargo.lock`, `README.md`, `packages/hya-tui-ts/package.json`, and
`crates/hya/tests/version_metadata.rs`. Move the current root `CHANGELOG.md`
contents byte-for-byte to `docs/changes/CHANGELOG_0.36.6.md`, then leave root
`CHANGELOG.md` newest-only with the `0.36.7` feature notes.

The inspected `.github/workflows/release.yml` keeps tag/version validation,
packaging, checksum, and binary smoke logic in inline shell; no reusable local
release validator exists. Add `crates/xtask/src/release_rehearsal.rs`, register
the `release-rehearsal` command in `crates/xtask/src/main.rs`, and add focused
`crates/xtask/tests/release_rehearsal.rs` tests rather than duplicating the logic
in an ad-hoc script. The non-publishing command is:

```sh
cargo run -p xtask -- release-rehearsal --workflow .github/workflows/release.yml --version 0.36.7 --target x86_64-unknown-linux-gnu --no-publish
```

The rehearsal must parse the workflow YAML; run `actionlint
.github/workflows/release.yml`; syntax-check every extracted embedded shell
`run` block with `bash -n`; validate representative `v0.36.7` tag/version/
changelog values without creating a Git tag; run the exact locked target build;
package `hya`, `hya-backend`, and `hya-ts` plus the prepared `hya-tui-ts`
runtime and compatibility adapter; generate `SHA256SUMS` and verify it with
`sha256sum -c`; extract the archive; smoke all three binaries; and assert the
legal files, client-present files, and server-absent runtime paths required by
the workflow. It must verify every third-party `uses:` pin is an immutable
commit SHA and that the publishing job uses the `release` environment. It must
never create a tag, publish a release, or print credentials or live traffic.

### Final current-version project/release gate

Run the version metadata test, release rehearsal test, and current-version
project/release gates after the edits and before live proof:

```sh
cargo test -p hya --test version_metadata
cargo test -p xtask --test release_rehearsal
cargo build -p hya-backend --bin hya-backend
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
cargo test -p hya-e2e -- --test-threads=1
cargo run -p xtask -- matrix-check
bun --cwd packages/hya-tui-ts run typecheck
bun --cwd packages/hya-tui-ts test
cargo build --release --locked -p hya -p hya-backend -p hya-ts --bins --target x86_64-unknown-linux-gnu
actionlint .github/workflows/release.yml
cargo run -p xtask -- release-rehearsal --workflow .github/workflows/release.yml --version 0.36.7 --target x86_64-unknown-linux-gnu --no-publish
```

These are the explicit release/final-gate exceptions to the focused-slice
restriction. They remain no-live: no provider request is allowed before Slice
13, and the release artifact produced here is the only binary eligible for the
live proof.

## Slice 13 — Mandatory live-provider proof

This slice is the only authorized live-provider exception. It runs only after
Slices 1–10, the deterministic Slice 11 process E2E, the 0.36.7 edits, and all
Slice 12 project/release gates pass. Use the exact final release binary
produced after those edits:
`target/x86_64-unknown-linux-gnu/release/hya-backend`. Record its SHA256,
`--version` output, and the secret-free configured provider identity used by the
run. Do not reuse pre-bump binary or provider-binding evidence.

First use the read-only `hya-backend models` catalog command (or equivalent
public catalog API) to list configured API model names only; never print or
persist credentials, keys, headers, prompts, request bodies, or response bodies.
Select healthy configured names dynamically; classify an externally unavailable
selection and choose another configured name instead of claiming a pass.

Run one bounded real Workflow through the public CLI or HTTP Workflow control
path. Its source must assign different models/efforts to different Stages and
must include the critical loop worker/verifier pair using the same base model
with distinct efforts. Assert sanitized exact model/reasoning observations,
roles, selected candidates, public route outcomes, replay equality, Event/
Session ids, status, and timestamps. Force fallback only with a deterministic
local pre-stream transport/429 fault before forwarding a real API fallback;
never induce an external provider outage. Stop at a bounded timeout and retain
only model names, bounded route outcomes, Event/Session ids, status, timestamps,
binary checksum/version, and the secret-free provider identity fingerprint.

After live evidence, rerun the affected security/release checks before review:

```sh
actionlint .github/workflows/release.yml
cargo test -p hya --test version_metadata
cargo build --release --locked -p hya -p hya-backend -p hya-ts --bins --target x86_64-unknown-linux-gnu
cargo run -p xtask -- release-rehearsal --workflow .github/workflows/release.yml --version 0.36.7 --target x86_64-unknown-linux-gnu --no-publish
```

If live evidence changes any packaged artifact or release input, the locked
build and complete rehearsal must be rerun rather than relying on pre-live
evidence. No pre-bump release evidence satisfies this gate.

## Slice 14 — Trellis review, commit, finish, archive, and journal

After the live proof and affected-check rerun pass, invoke `trellis-check`,
create one atomic semantic feature commit, and push it without staging unrelated
work. Then invoke the authoritative `trellis-finish-work` workflow. It archives
the active task with normal auto-commit:

```sh
python3 ./.trellis/scripts/task.py archive workflow-stage-model-routing
```

Record the developer journal with the verified feature hash and normal
auto-commit:

```sh
python3 ./.trellis/scripts/add_session.py --title "Workflow stage model routing" --commit <feature-hash> --summary "Implemented and verified workflow model routing"
```

Finally push only the archive and journal maintenance commits. Do not run
`task.py finish` separately unless the finish-work skill explicitly directs it.
Do not create a tag or publish a release unless separately requested.

## Final verification

Final evidence must prove:

- the critical same-base-model loop worker/verifier pair has distinct efforts,
  roles, selected candidates, requests, and replayed outcomes;
- cancellation before a candidate attempt emits no route outcome, while
  cancellation after attempt start and after selection emits exactly one
  `cancelled` outcome for the last attempted/selected candidate with no
  failover;
- every explicit-route stream group has one finalized outcome, including
  post-stream and terminal pre-stream failures, while absent assignments have
  exact old event-log/value/byte behavior and no route fields/Event;
- provider default-only identity changes affect provider/admission/request
  fingerprints and effective effort, while insertion-order changes do not;
- admitted `selected_index` never wraps or requests an earlier candidate;
- CLI/API/SDK agree, route payloads are bounded/secret-free, and the TUI sidebar
  remains unchanged.

## Rollback / failure handling

If a slice fails, keep its RED test and record the exact failure in the Trellis
progress log; do not weaken the contract or skip the test. Revert only the
slice's uncommitted changes, never unrelated user files. A failed provider
request in implementation tests is not a reason to change retry semantics:
replace it with the deterministic provider fixture. If an API route cannot
append a root outcome under a resident child claim, use the activation recorder
specified in Slice 6 rather than weakening actor fencing or writing a second
read model.
