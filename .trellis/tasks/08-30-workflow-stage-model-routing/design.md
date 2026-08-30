# Design: Workflow stage model routing

## 1. Architectural boundary

Keep the existing ownership chain:

```text
Workflow Markdown
  -> hya_workflow::compile(WorkflowSource)
  -> immutable WorkflowStage / WorkflowPlan
  -> hya_app::WorkflowControl (catalog + frozen runtime binding)
  -> hya_core::prepare_workflow_run (preflight + budget reservation)
  -> governed Team / ResidentSupervisor / IterationDriver
  -> SessionEngine provider stream groups
  -> owning root Session Event log + hya_proto::Projection
  -> CLI / HTTP / SDK / TUI synchronization
```

`hya_workflow` remains the only authoring construction path. `hya-core` remains
the only Workflow executor. `WorkflowControl::execute` remains the only app
control seam. Do not add a Workflow table, a second reducer, a Workflow-specific
scheduler, or a second provider router.

The existing category/provider fallback plane remains the default for ordinary
Agent/task turns. A Workflow assignment is a request-local route supplied to the
specific worker/verifier activation. It is never installed into
`SessionEngine::model_fallbacks`.

## 2. Authoring model and syntax

Add two compiler-owned values in `crates/hya-workflow/src/model.rs`:

```rust
pub struct WorkflowModelCandidate {
    id: String,
    reasoning: Option<String>,
}

pub struct WorkflowModelAssignment {
    id: String,
    reasoning: Option<String>,
    fallback: Vec<WorkflowModelCandidate>,
}
```

Expose read-only accessors. The preferred entry is the assignment's `id` and
`reasoning`; `fallback` is an ordered tail. Add
`model: Option<WorkflowModelAssignment>` to `WorkflowStage` and to `VerifySpec`.
The author structs in `compiler.rs` use the same `deny_unknown_fields` discipline
as the existing frontmatter.
Re-export the compiler assignment values from `crates/hya-workflow/src/lib.rs`.
Re-export `WorkflowModelRouteCandidate` and `WorkflowModelRoute` from both
`crates/hya-core/src/workflow/mod.rs` and `crates/hya-core/src/lib.rs`; add
compile and import tests so the promised public seams are usable without
reaching into private modules.

The public source shape is shown below. Every `id` in this block is a base model
reference without a `#variant` suffix; the compiler rejects that suffix here.

```yaml
nodes:
  implement:
    agent: workflow-implementer
    directive: Implement the approved plan.
    model:
      id: 12th-oai/gpt-5.6-sol
      reasoning: high
      fallback:
        - id: 12th-anth/claude-sonnet-4-6
          reasoning: medium
    mode: loop
    verify:
      agent: workflow-reviewer
      until: The implementation satisfies the request.
      model:
        id: 12th-oai/gpt-5.5
        reasoning: low
        fallback:
          - id: 12th-oai/gpt-5.6-sol
            reasoning: medium
      max_iterations: 3
```

The compiler trims model ids and any provided `reasoning` strings. It rejects an
empty model id and an explicitly empty reasoning string, plus malformed
assignment shape, unknown assignment keys, and exact normalized `(id, reasoning)`
duplicates. It does not classify unknown non-empty effort labels; it preserves
them for runtime parsing and provider-capability validation. Every new Workflow
assignment or fallback `id` MUST be a base model reference without a `#variant`
suffix, whether or not `reasoning` is present. Existing category, Agent, API,
and direct model refs retain their established `provider/model#variant` behavior
outside this new Workflow block.

Duplicate validation uses the ordered preferred-plus-tail entries. It rejects an
exact normalized `(id, reasoning)` duplicate and keeps same-id/different-effort
entries. Runtime validation repeats the check after resolving omitted efforts,
because two omitted entries can resolve to the same configured default. Do not
deduplicate an otherwise valid chain silently.

`verify.model` is the verifier route; `verify.agent`, `until`, and
`max_iterations` retain their existing semantics. The loop actor restriction is
unchanged.

## 3. Revision and bundle compatibility

The existing `canonical_revision` algorithm in `compiler.rs` is preserved for
sources with no worker or verifier assignment. This is a hard compatibility
requirement: no-assignment Workflow Markdown and installed prepared v2 bundles
must retain their current digest bytes.
For a no-assignment source, no local route or selected-plan fields are created,
no recorder is installed, no route-outcome Event is appended, and the old
Agent/category/global path and Event-log serialization remain exactly unchanged.
The compatibility test must compare the old and new event-log values and bytes,
not only the compiler digest.

When at least one assignment exists, append a domain-separated conditional
extension after the existing normalized fields, for example:

```text
hya.workflow.model-routing.v1\0
for each Stage in normalized order:
  worker assignment presence + preferred id/reasoning + fallback count/entries
  verifier assignment presence + preferred id/reasoning + fallback count/entries
```

Length-prefix all strings and counts with the existing canonical hash helpers.
The extension includes every entry, its effort presence/value, and order. It is
not emitted at all for a no-assignment plan. Assignment edits therefore change
the Workflow revision and the existing request hash, while the prepared bundle
serialization stays format v2. `PreparedWorkflow` continues to carry the source
and compiler revision; no bundle format field or migration is added.

Bundle preparation may parse and hash the new syntax, but cannot resolve local
provider availability. It must not reject a package because its host has no
matching model route. Runtime preflight performs that local check under the
captured binding.

## 4. Runtime resolution value

Add a core-only resolved route value that keeps each base model identity,
effective typed effort, ordered candidates, and the selected admission index
together:

```rust
pub struct WorkflowModelRouteCandidate {
    pub model: ModelRef,              // base model reference; never #variant
    pub reasoning: ReasoningEffort,    // effective effort; Off means no reasoning
}

pub struct WorkflowModelRoute {
    /// Full declared order after resolving each entry's effective effort.
    pub candidates: Arc<[WorkflowModelRouteCandidate]>,
    /// Index of the first candidate whose provider route is available now.
    pub selected_index: usize,
}
```

Each resolved candidate keeps effort in its separate typed `reasoning` field;
Workflow routing never synthesizes, parses, or consumes a `#variant` suffix.
When the provider request is built, the base `ModelRef` and
`CompletionRequest.reasoning` are passed as separate values. Effective effort
is selected per candidate:

1. explicit `reasoning` parses through the existing typed
   `hya_provider::ReasoningEffort`;
2. otherwise use that candidate model's configured provider default;
3. otherwise use `ReasoningEffort::Off` as an explicit no-reasoning marker so
   the candidate cannot inherit another candidate's or the Agent's effort.

The route retains unknown/unroutable tail refs for the existing
`ProviderError::UnknownModel` pre-stream transition. `selected_index` must point
to a candidate whose provider resolves under the current router. If no candidate
resolves, preflight fails before budget reservation or any child/resident/mail
side effect.

### Provider default metadata

The provider layer currently exposes per-model reasoning variants in its catalog
but not the configured default to the runtime router. Extend the existing
provider seam without changing the wire request shape:

- add a default `Provider::reasoning_default(&ModelRef) -> Option<ReasoningEffort>`
  method returning `None` for providers without metadata;
- implement it in `HttpProvider` with a per-model map;
- add `ProviderRouter::reasoning_default` using the resolved route;
- pass `ParsedModel.reasoning_default` from `hya-app/src/config.rs` when building
  `HttpProvider` (the current config resolver already validates the default
  against the advertised variants);
- keep `ProviderModel` catalog JSON compatible unless a separate listing need
  appears.
The provider configured identity must include each model's reasoning default in
stable code-unit model-id order. Encode a canonical per-model row containing the
base model id and its default effort label, including `none` for Off; do not use
HashMap insertion order. The stable identity flows through the runtime semantic
fingerprint, admission binding fingerprint, and Workflow request hash. A
default-only change changes all three fingerprints and the effective route
effort; reordering the same model/default rows does not.

Use the same route resolution metadata to validate an explicit non-Off effort
against the model's advertised variants and `reasoning_request` capability.
`Off` is valid even when a route has no reasoning vocabulary because it emits no
reasoning parameter. An invalid effort is a bounded preflight error, not a
silently dropped request.

### Agent policy precedence

Extract the model/category portion of the existing normal spawn resolver into a
shared helper rather than writing a second precedence list. Workflow resolution
starts with the Agent's current prompt/reasoning/spec under the frozen binding,
then applies the normal Agent model/category policy, and finally applies the
explicit Stage assignment when present. Thus:

```text
base Agent -> Agent model/category policy -> explicit Stage assignment
```

An explicit Stage assignment clears the inherited Agent reasoning field and
uses the per-candidate `reasoning` values. An absent assignment leaves the
existing Agent resolution path intact. A verifier uses the same sequence
independently.

If the Agent policy came from a category, preserve the category's existing
ordered route as its lower-level fallback. If no Stage assignment exists, the
existing global category/model fallback behavior remains. For an explicit Stage
assignment, the route is local even if its first model equals a category or the
base model.

## 5. Workflow preflight and actor invariants

`prepare_workflow_run_for_actor` remains the preflight boundary. It resolves
routes only for Stages with an explicit worker `stage.model` or verifier
`verify.model` assignment. For each explicit route:

1. validate Workflow inputs;
2. authorize the Agent through `TurnBinding::resolve_spawn`;
3. resolve the Agent's own roster/resources/sidecar exactly as today;
4. apply the shared Agent model/category policy, then the explicit assignment;
5. parse known `ReasoningEffort` values and validate provider capability;
6. resolve each candidate's own default when reasoning is omitted, otherwise use
   typed `ReasoningEffort::Off`;
7. validate effective duplicate keys and at least one routable candidate;
8. compare effective worker routes for all Stages sharing an actor key;
9. validate resident/transient/loop semantics;
10. reserve the complete existing worst-case activation budget;
11. return the prepared run containing the fixed route and route-plan metadata.

When no explicit assignment exists, do not construct a local route, selected
candidate, selected index, recorder, or route-outcome Event. Use the exact old
Agent/category/global resolution, budget, and actor path.

For an explicit route, admission resolves the first routable candidate in
declaration order and stores its immutable `selected_index` in the prepared run
and StagePlan. Every stream group starts at that index, requests only that
candidate and later declared candidates, and never wraps to earlier entries.
An unknown/unroutable tail is valid when another candidate routes; a chain with
no routable candidate fails before budget reservation or any child/resident/mail
side effect. A later pre-stream fault can advance only forward from the
admitted index.

For every shared actor key, compare the full effective worker chain (base model,
typed effort, and order), not only the first model. A same-model/different-
effort route is different and fails before actor creation. Reusing a resident
actor therefore never switches its model policy dynamically. Verifier routes are
independent and are not part of actor-key equality.

Add explicit route data to `PreparedWorkflowRun` in a read-only form so
`WorkflowControl::run_inner` can build the exact `WorkflowRunStarted` plan from
the same preflight result that execution consumes. Do not resolve a route a
second time between preflight and admission.

## 6. Request-local execution and no global collision

The current engine API walks `model_fallbacks` for ordinary turns. Keep that map
for category/task behavior and add a request-local optional chain of
`WorkflowModelRouteCandidate` values to the shared turn activation path. Prefer
an internal option over a second scheduler:

- add a route/recorder option to the internal `SessionEngine` turn activation;
- preserve existing public/root wrappers by passing `None`;
- add Workflow-only bound/resolved turn wrappers that pass the local route;
- make `stream_with_model_fallback` use the local chain when present, otherwise
  the existing global chain;
- preserve `ProviderError::is_retryable_before_stream` and the `UnknownModel`
  branch exactly.

Thread the route through the existing governed paths:

- transient Workflow team batches pass each Stage route to the member turn;
- loop worker iterations pass the worker route and verifier judgments pass the
  verifier route;
- resident `SlotState` stores the fixed route at registration and each
  `RunPlan` carries it to `run_one_turn`;
- normal task and ordinary resident callers keep `None` and remain unchanged.

Do not add Stage chains to `SessionEngine::with_model_fallbacks`. This is the
specific protection against two Stages sharing a preferred model while requiring
different efforts or fallback order.

## 7. Durable route-outcome Event

Add one additive event variant in `crates/hya-proto/src/event.rs`:
```rust

WorkflowStageRouteOutcome {
    session: SessionId,           // owning root Workflow Session
    run: WorkflowRunId,
    stage: String,
    member: MemberId,
    role: WorkflowMemberRole,
    iteration: u32,
    step: u32,                    // assistant/provider stream-group index
    candidate_index: u32,
    model: ModelRef,              // base id; Workflow route ids cannot have #variant
    reasoning: String,            // required canonical effort; Off serializes as "none"
    failure_class: WorkflowRouteFailureClass,
}
```

Use a closed serde enum for `WorkflowRouteFailureClass` with stable values such
as `none`, `transport`, `rate_limited`, `server`, `unknown_model`, `auth`,
`incompatible`, `http`, `decode`, `exhausted`, and `cancelled`. Keep the enum
focused on classification; never include raw HTTP text/status bodies, provider
messages, credentials, prompts, inputs, or response content.


The provider fallback selection returns an internal result containing the
stream, selected candidate, selected index, and a pending pre-stream failure
class. It starts at the immutable route `selected_index`, advances only to later
declared candidates, and never wraps around. It does not finalize or persist an
outcome while selecting.

After selection, `collect_stream_round` must drain the stream successfully before
the outcome is finalized. A first-candidate success finalizes `none`; a fallback
success finalizes the pending class that caused the final advance. If collection
fails after a stream exists, finalize the stable provider class at that boundary,
return the original error, and do not fail over or replay. If pre-stream
candidates are exhausted, finalize the last attempted candidate and terminal
class before returning the error.

Cancellation is a separate lifecycle boundary. Before a stream group and its
first candidate attempt start, cancellation emits no route outcome. Once the
first candidate attempt starts, cancellation does not fail over: an
activation-owned finalization guard records exactly one `cancelled` outcome for
the last attempted/selected candidate, and the outer Workflow activation drains
that recorder under its root actor claim. This applies to cancellation after an
attempt starts and after selection returns a stream.

Workflow activation drains exactly one finalized outcome for each explicit-route
stream group. The event is emitted once per group, never once per candidate
attempt and never mid-stream. With no explicit worker/verifier assignment,
there is no route recorder and no new route-outcome Event.


Transient members can persist finalized outcomes after their turn returns. Loop
worker and verifier activations persist after each governed member result, with
distinct role/iteration/step even when they use the same base model at different
efforts. Resident turns cannot append a root event with a child actor claim, so
explicit-route activation installs a bounded activation-owned finalization guard
and recorder in resident `SlotState` while the actor is idle; `run_one_turn`
fills it, cancellation finalizes it at most once, and the outer
`activate_resident_stage` drains and appends it with the root claim before
`WorkflowStageFinished`. Ordinary resident wakes have no recorder.

The existing Workflow and Team budgets bound the number of stream groups in a
run. Bound each model/effort/class string using the existing public diagnostic
limits; never include raw provider data. The projection deduplicates by
`(stage, member, role, iteration, step)` and ignores late outcomes for old or
terminal runs according to the existing Workflow reducer fencing.

## 8. Durable plan, DTOs, and projection

Define wire/SDK mirror values with string effort labels, because `hya-proto` and
`hya-sdk` intentionally do not depend on the provider crate. Authored effort is
optional; an admitted candidate is a distinct resolved DTO with required
canonical effort:

```rust
pub struct WorkflowModelCandidate {
    pub id: String,
    pub reasoning: Option<String>,
}

pub struct WorkflowModelAssignment {
    pub id: String,
    pub reasoning: Option<String>,
    pub fallback: Vec<WorkflowModelCandidate>,
}

pub struct WorkflowModelResolvedCandidate {
    pub index: u32,
    pub id: String,
    pub reasoning: String, // canonical label; Off serializes as "none"
}
```

Add optional fields to `WorkflowStageInfo` for authored worker and verifier
assignments. Add optional fields to `WorkflowStagePlan` for each explicit route:

- requested worker assignment;
- admission-selected worker `WorkflowModelResolvedCandidate`;
- requested verifier assignment;
- admission-selected verifier `WorkflowModelResolvedCandidate`.

The selected candidate DTO's `index` is the immutable stream-group start index
for that run. Every selected/admitted and outcome effort is a required canonical
string; authored `reasoning` remains `Option<String>`.

Use `#[serde(default, skip_serializing_if = "Option::is_none")]` for every new
optional field. Use `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
for `route_outcomes` and every new routing collection. Keep assignment entry
order exactly as authored. A plan's selected candidate is the first currently
routable effective candidate; it is not a promise that later provider health
will remain unchanged.

Add `route_outcomes: Vec<WorkflowStageRouteOutcome>` to
`WorkflowStageProjection` with `#[serde(default, skip_serializing_if =
"Vec::is_empty")]`. The reducer only adds outcomes for the active run and the
matching Stage, deduplicating the route key. It does not perform provider
lookup. Close/reopen replay must produce the same plan and outcome rows.

When no explicit worker or verifier assignment exists, all routing options are
`None`, every routing collection is empty and omitted, and no route recorder or
outcome Event is emitted. Old Event and projection JSON must decode and
re-encode with identical values and bytes because new optional fields and empty
collections are omitted by serde.

`WorkflowRunStarted` remains the admission Event carrying the complete immutable
StagePlan. `WorkflowStageRouteOutcome` is the only new lifecycle/audit Event;
`WorkflowStageStarted` and member-link Events retain their existing fields.

## 9. App, CLI, HTTP, SDK, and TUI behavior

`WorkflowControl::new` receives the existing configured category/provider
contexts (or a single shared routing context) and passes them into the frozen
Workflow run context. `run_inner` uses the prepared run's route-plan getter to
construct `workflow_stage_plan`; there is no second route resolution. Existing
revision, idempotency, actor-fence, and Started/Finished delivery logic stays
unchanged.

`workflow_info` maps authored assignments. `workflow_stage_plan` maps authored
assignments plus effective admission-selected candidates. CLI `info`, `state`,
and `run` print compact route ids/efforts and outcome summaries; JSON uses the
same shared DTO. The Agent Workflow tool, native/legacy/v2 HTTP endpoints,
Session hydration, and SDK mirrors inherit the fields without a new command or
transport. All public diagnostics remain bounded.

The TUI sidebar does not render route details in this increment. Its strict
presentation function may retain the current type shape and safely ignore the
new optional fields; add a fixture proving route-bearing projections preserve
existing state/progress/current-work output. Do not add polling, a second SDK
client, a Workflow-specific model picker, or a local route reducer.

Update `docs/workflows.md`, the Workflow architecture page, and provider model
selection documentation with the syntax, precedence, default-effort, local
fallback, route-outcome, and TUI scope. Do not include credentials or live
provider output in documentation or task artifacts.

## 10. Error and compatibility matrix

| Condition | Behavior |
| --- | --- |
| Empty/unknown assignment key | Compiler frontmatter error before catalog admission |
| Empty model id or explicitly empty reasoning | Compiler validation error with Stage location |
| Any embedded `#variant` in an assignment/fallback id | Compiler validation error; use separate `reasoning` |
| Unknown non-empty effort label | Preserve through compile; runtime preflight rejects it after typed parse/capability validation |
| Duplicate effective `(id, effort)` | Runtime preflight error before reservation/side effect |
| No routable candidate | Runtime preflight error before child/resident/mail/provider effect |
| Unknown tail with another routable candidate | Preserve tail; provider `UnknownModel` may advance pre-stream |
| Unroutable preferred plus routable fallback | Admit fallback index; start every group there and never request earlier preferred entry |
| Explicit Stage route vs Agent model/category | Stage route wins; Agent prompt and resources remain bound to target Agent |
| Omitted Stage route | Exact old Agent/category/model behavior; no local route, selected fields, recorder, or new Event |
| Omitted entry effort | Candidate's own provider default, else typed Off serialized as required `none`; no inheritance |
| Retryable transport / 429 / 5xx pre-stream | Advance only to later local candidates; finalize one outcome after stream drain |
| `UnknownModel` pre-stream | Advance only to later local candidates; finalize one outcome after stream drain |
| Auth/incompatible/decode/ordinary HTTP | Do not advance; finalize one terminal-class outcome Event |
| Any post-stream error | Finalize stable provider class at the error boundary; do not fail over or replay |
| Cancellation before first candidate attempt | Emit no route outcome and preserve the existing cancellation status |
| Cancellation after first candidate attempt starts, including after selection | Finalize exactly one `cancelled` outcome for the last attempted/selected candidate; do not fail over or replay |
| Same actor key, different effective route | Preflight rejection; no dynamic resident switch |
| Provider default-only identity change | Changes provider identity, admission binding fingerprint, Workflow request hash, and effective effort |
| Provider model/default insertion order change | No identity/fingerprint/hash change when stable code-unit model order is unchanged |
| Old no-assignment Workflow | Exact old compiler revision, event-log value/bytes, projection value/bytes, and no new route fields/Event |
| Old prepared v2 bundle | Same prepared format; source compiles with unchanged no-assignment digest |
| Old Event/projection JSON | New optional fields and empty collections default/skip; decode/re-encode value and bytes remain unchanged |
| Route outcome fields | Bounded ids/efforts/classes only; no raw provider text or secrets |

## 11. Public test seams

### Compiler seam

`hya_workflow::compile(WorkflowSource)` tests should observe:

- worker and verifier model blocks, ordered entries, accessors, and source
  diagnostics;
- suffix-free assignment ids and unconditional rejection of every embedded
  `#variant` suffix; empty reasoning rejects, unknown non-empty labels do not
  fail compilation;
- exact duplicate rejection and same-id/different-effort acceptance;
- invalid nested/unknown fields and empty ids;
- conditional revision extension and the exact old no-assignment digest;
- unchanged compiled installed-v2 Workflow source behavior.

### Provider/runtime seam

At the existing deterministic provider seam, test:

- explicit Stage assignment overrides Agent model/category/reasoning;
- omitted entry effort uses each candidate's own default or explicit typed Off,
  and the wire selected/outcome effort is required canonical `none` or label;
- preferred/fallback candidates receive the correct base model and separate
  effort value, starting at immutable admission `selected_index` with no wrap;
- two Stages with the same preferred model retain independent local chains;
- unknown tail, unroutable-preferred/routable-fallback, and no-routable
  preflight behavior;
- provider identity/default and admission/request fingerprint sensitivity and
  insertion-order stability;
- retryable pre-stream advancement, nonretryable no-advance, and no mid-stream
  replay;
- stream selection/finalization ordering, including post-stream failure and
  terminal pre-stream failure;
- loop worker/verifier route separation using the same base model at different
  efforts, plus resident identical-route rejection;
- exactly one finalized outcome record per explicit-route stream group, including
  terminal failure, and no recorder/Event for an absent assignment.
- cancellation before a candidate attempt emits no observation;
- cancellation after attempt start and after selection finalizes exactly one
  `cancelled` observation for the last attempted/selected candidate, with no
  failover; the activation-owned guard and outer drain prevent duplicates.

### Proto/store seam

Test authored/selected/outcome DTO serde compatibility, required canonical
resolved effort strings, outcome-key deduplication, active-run/Stage fencing,
bounded payload fields, close/reopen equality, exact old projection/event
decode-re-encode value and bytes, and replay with no provider/child side effect.

## 12. Mandatory deterministic process proof

Add a matrix-registered Track P scenario (`T2.14`) at
`crates/hya-e2e/tests/p19_workflow_model_routing.rs`; keep the existing P17
(`T2.13`) fan-out/fan-in scenario and matrix row unchanged and green. The new
scenario
uses the production `hya-backend` binary with local scripted FakeLlm and several
fake base model ids. Its authored Workflow includes different worker routes,
the verifier route, and the critical loop case where worker and verifier use
the same base model with different efforts. Capture exact model and separate
reasoning values from FakeLlm requests, invoke the public Workflow CLI or HTTP
control path, read public Workflow state, and assert route outcomes, roles,
selected candidates, and close/reopen replay. Register the new file in
`crates/hya-e2e/matrix.toml` and run matrix validation.

Build the exact binary before process tests and run both focused scenarios and
the complete Track P suite serially:

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p17_workflow_composition -- --test-threads=1
cargo test -p hya-e2e --test p19_workflow_model_routing -- --test-threads=1
cargo test -p hya-e2e -- --test-threads=1
cargo run -p xtask -- matrix-check
```

## 13. Release preparation and current-version gates

After the deterministic focused slices and the mandatory process E2E pass, apply
the release edits before any live-provider proof. The current release is
`0.36.6`; update the next version to `0.36.7` in `Cargo.toml`, all hya package
versions in `Cargo.lock`, `README.md`, `packages/hya-tui-ts/package.json`, and
the expected release in `crates/hya/tests/version_metadata.rs`. Move the current
root notes byte-for-byte to `docs/changes/CHANGELOG_0.36.6.md` and leave root
`CHANGELOG.md` newest-only for `0.36.7`.

The inspected `.github/workflows/release.yml` keeps tag/version validation,
packaging, checksum, and binary smoke logic in inline shell; the repository has
no reusable local release validator. Add `crates/xtask/src/release_rehearsal.rs`,
register the `release-rehearsal` command in `crates/xtask/src/main.rs`, and add
focused `crates/xtask/tests/release_rehearsal.rs` tests rather than duplicating
that logic in an ad-hoc script. Its non-publishing command is:

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
the workflow. It must also verify every third-party `uses:` pin is an immutable
commit SHA and that the publishing job uses the `release` environment. It must
never create a tag, publish a release, or print credentials/live traffic.

Run the version metadata test and current-version project/release gates after
the edits and before live proof:

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

## 14. Mandatory live proof

Only after the 0.36.7 version/changelog edits and every deterministic,
process, project, and release gate passes, run a bounded real-provider Workflow
through the public CLI or HTTP Workflow control path. Use the exact final
release binary produced after the edits,
`target/x86_64-unknown-linux-gnu/release/hya-backend`, and record its SHA256,
`--version` output, and the secret-free configured provider identity used by the
run. Never substitute evidence from a pre-bump binary or provider binding.

First use the read-only `hya-backend models` catalog command (or its equivalent
public catalog API) to list configured API model names only; never print or
persist credentials, keys, headers, prompts, request bodies, or response
bodies. Select healthy configured models dynamically; classify an externally
unavailable selection and choose another configured name instead of claiming a
pass.

The live Workflow must prove different Stages receive their assigned base
models and efforts, and must include the critical loop worker/verifier pair
using the same base model with distinct efforts. Assert sanitized exact model
and reasoning observations, assigned roles, selected candidates, public state
route outcomes, Event/Session ids, status/timestamps, and replay equality.
Force one fallback with a deterministic local pre-stream transport or 429 fault
before forwarding a real API fallback; never induce an external provider outage.
Stop on the bounded timeout and retain only model names, bounded route outcomes,
Event/Session ids, status, timestamps, binary checksum/version, and the
secret-free provider identity fingerprint.

After live evidence, rerun every affected security/release check before review:
`actionlint .github/workflows/release.yml`, the non-publishing
`release-rehearsal` command, `cargo test -p hya --test version_metadata`, and
the locked target release build/package smoke when any release evidence or
artifact changed. Do not rely on a pre-bump release rehearsal.

## 15. Trellis delivery and maintenance

After all evidence and the affected-check rerun pass, invoke `trellis-check`,
create one atomic semantic feature commit, and push it without staging unrelated
work. Then invoke the authoritative `trellis-finish-work` workflow. It archives
the task with normal auto-commit:

```sh
python3 ./.trellis/scripts/task.py archive workflow-stage-model-routing
```

Then record the journal with the verified feature hash and normal auto-commit:

```sh
python3 ./.trellis/scripts/add_session.py --title "Workflow stage model routing" --commit <feature-hash> --summary "Implemented and verified workflow model routing"
```

Finally push only the archive and journal maintenance commits. Do not run
`task.py finish` separately unless the finish-work skill explicitly directs it.
Do not create a tag or publish a release unless separately requested.

## 16. Explicit non-goals

- No generic provider-option map or provider-specific fields in Workflow syntax.
- No category syntax or global fallback semantics change.
- No fallback after a stream exists, transcript replay, automatic run restart,
  or resident model switching.
- No bundle format bump, no second Workflow scheduler/read model, and no TUI
  route rendering.
