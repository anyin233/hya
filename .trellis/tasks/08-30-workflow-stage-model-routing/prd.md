# Workflow stage model routing

## Goal

Allow each Workflow Stage, including a loop verifier, to select a concrete model
with an ordered, effort-aware fallback chain. Preserve the existing governed
Workflow executor, category/provider routing, pre-stream failover boundary,
event-sourced replay, and all command/API surfaces.

## User outcomes

- A Workflow author can give different Stages different model routes.
- Every route entry carries its own reasoning effort. The same model can appear
  in multiple entries with different efforts and remains distinct and ordered.
- A Stage route can fail over only to the next declared candidate before a
  provider stream exists; established streams are never replayed onto another
  model.
- A run records the immutable route declaration, the candidate selected during
  admission, and one bounded route outcome Event per explicit-route provider
  stream group only.
- Existing Workflow documents, installed prepared v2 bundles, category routes,
  and old Workflow events keep their current behavior when no assignment is
  present.

## Requirements

### R1. Workflow source syntax

Add an optional `model` block to each `nodes.<stage>` entry. Every `id` shown
in this block is a base model reference without any `#variant` suffix; the
compiler rejects every embedded suffix, whether or not `reasoning` is present.

```yaml
model:
  id: 12th-oai/gpt-5.6-sol
  reasoning: high
  fallback:
    - id: 12th-anth/claude-sonnet-4-6
      reasoning: medium
```

Add the same optional block under `nodes.<stage>.verify`. Every `id` in the
block is a base model reference without a `#variant` suffix. The block's `id`
is the preferred entry; `fallback` order is preserved. An omitted `reasoning`
is resolved independently for that model from its configured default; it never
inherits the effort of the preferred entry, another fallback entry, or the
Agent policy. Existing `provider/model#variant` inputs remain valid in existing
model/category APIs, but the Workflow block uses the separate `reasoning` field
and rejects every embedded `#variant` suffix rather than allowing dual encodings
or silently choosing one value.

The compiler trims model ids and any provided `reasoning` strings. For an
assignment it rejects an empty model id, an explicitly empty reasoning string,
malformed assignment shape, unknown assignment keys, and exact duplicate
`(id, reasoning)` entries after syntax-level normalization. It does not classify
unknown non-empty effort labels; runtime parses known `ReasoningEffort` values
and validates provider capability. The same model id with different efforts is
valid.

### R2. Resolution and precedence

The assignment is optional. With no assignment, the Stage uses the existing
Agent model/category resolution result and reasoning behavior. With an
assignment, the Stage assignment overrides the Agent's model, category, and
reasoning policy. Each worker and verifier route is resolved against the
immutable runtime binding used by the run.

Runtime resolution validates typed `ReasoningEffort` values, consults the
configured model's default when an entry omits effort, and uses typed
`ReasoningEffort::Off` behavior when the model has no configured default. It
never synthesizes a model suffix for a Workflow assignment. Provider-specific
wire mapping remains inside the existing provider encoders.

### R3. Preflight and execution

Before any child Session, resident registration, mail, or provider request:

- validate every explicit route entry's model/effort shape and effective duplicate key;
- resolve the first routable candidate in declaration order and store its
  immutable `selected_index` for the run;
- start every stream group at that index, request only that candidate and later
  declared candidates, and never wrap to earlier entries;
- reject an explicit assignment whose chain has no routable candidate;
- retain valid unknown/unroutable tail candidates for the existing `UnknownModel`
  pre-stream failover behavior;
- reserve the existing worst-case Workflow budget;
- require identical effective route chains for all Stages sharing one resident
  actor key; reject dynamic resident model switching.

Stage-local chains are request-local data. They must not be inserted into the
engine's global `HashMap<ModelRef, Vec<ModelRef>>`, because two Stages can share
the same preferred model while requiring different effort or fallback order.

The existing failover boundary remains exact: transport and retryable 429/5xx
pre-stream failures, plus `UnknownModel`, may advance; auth, incompatible
capability, decode, ordinary HTTP, and every post-stream failure do not consume
the chain.

### R4. Durable plan, route outcomes, and replay

Extend the existing `WorkflowStagePlan` carried by `WorkflowRunStarted` with the
ordered requested worker/verifier entries and the first routable candidate
selected during admission. Authored assignment effort remains optional, while
the admitted candidate uses a distinct resolved-candidate DTO with required
canonical string effort; `ReasoningEffort::Off` serializes as `none`.

For each provider stream group belonging to an explicit `stage.model` or
`verify.model`, emit one additive, bounded `WorkflowStageRouteOutcome` Event.
It records only the run/stage/member role, member iteration, assistant step,
immutable start/selected candidate index, base model id, required canonical
effort string, and a stable failure class. A successful first candidate records
`none`; a fallback or terminal failure records the stable class that caused/
ended selection. It never stores raw provider text, credentials, prompts,
inputs, or response bodies. An absent assignment creates no selected-route
fields, recorder, or route-outcome Event and preserves the old
Agent/category/global path exactly.

Route selection returns a stream, selected candidate/index, and any pending
pre-stream failure class without finalizing an Event. Finalize `none` only after
`collect_stream_round` drains successfully; finalize a stable provider class at
the post-stream error boundary without failover or replay; finalize terminal
pre-stream failure before returning the error. Workflow activation drains exactly
one finalized outcome for each explicit-route stream group.

Cancellation before a stream group and its first candidate attempt starts emits
no route outcome. Once the first candidate attempt starts, cancellation does not
fail over: an activation-owned finalization guard records exactly one
`cancelled` outcome for the last attempted/selected candidate, and the outer
Workflow activation drains it.

Projection replay folds route outcomes into the active Stage in Event order. It
does not resolve models, retry providers, or execute Stages. Route outcomes are
deduplicated by `(stage, member, role, iteration, step)`, while the existing
Workflow and Team budgets bound the number of stream groups in a run. Each Event
payload uses bounded model/effort/class fields; raw provider data is never
stored.

New fields and the new Event are additive. No second Workflow table, reducer,
scheduler, or event log is allowed. Local route, selected-plan fields, recorder,
and route-outcome Events exist only when an explicit worker or verifier
assignment exists. With no explicit assignment, the canonical compiler revision,
Agent/category/global behavior, event log, and serialized plan remain exactly
the old form. Only a Workflow with at least one assignment receives the
conditional model-routing hash extension. Prepared WorkflowBundle format remains
v2.

### R5. Shared surfaces

Keep `WorkflowControl::execute` and all five `WorkflowCommand` variants
unchanged. CLI text/JSON, Agent tool, native/legacy/v2 HTTP routes, Session
hydration, SDK mirrors, and state/replay all use the extended shared DTOs.
`workflow info` and state/run output show the route declaration, admitted
candidate, and bounded outcomes without exposing secrets. The existing TUI
sidebar remains compact and unchanged; it must safely decode and present state
when optional route fields are present, while typed API/SDK clients retain them.

### R6. Verification seams

The implementation must expose behavior through these public or existing
contract seams only:

1. `hya_workflow::compile(WorkflowSource)` for syntax, normalization, duplicate
   rejection, and revision compatibility.
2. The governed Workflow runtime/provider request seam for per-entry model and
   reasoning, local-chain isolation, preflight, actor-route equality, route
   outcome recording, and pre-stream failover.
3. Existing proto/store replay for additive Event persistence, outcome
   deduplication, bounded fields, and no side-effect replay.
4. Existing app/server/CLI/SDK command seams for one shared DTO and error path.
5. Existing TUI presentation tests only for compatibility with optional fields;
   no second client or polling path.

### R7. Mandatory deterministic, release, and live proof

Planning and all RED/GREEN focused gates use only deterministic providers and
must not send live provider traffic. After those gates pass, run the mandatory
Track P process proof with the production `hya-backend` binary and local
scripted FakeLlm: retain the existing P17 scenario, add the matrix-registered
routing scenario, and invoke the public Workflow CLI/HTTP control path. Capture
exact model/reasoning requests, public state route outcomes, selected
candidates, roles, and replay without secrets.

Only after the 0.36.7 release preparation and current-version project/release
gates in R8 pass may the bounded real-provider proof run. It must use the exact
final `target/x86_64-unknown-linux-gnu/release/hya-backend` binary produced
after the version/changelog edits and its secret-free configured provider
identity; pre-bump binary or provider evidence does not satisfy this criterion.
First list configured API model names through the safe catalog/config surface,
select healthy names dynamically, and classify/replace externally unavailable
selections without exposing credentials, keys, headers, prompts, requests, or
responses.

The live proof must include multiple Stages with different assigned base models
and efforts, plus the critical loop worker/verifier pair using the same base
model with different efforts. Assert exact sanitized model/reasoning values,
roles, selected candidates, public state route outcomes, replay equality,
Event/Session ids, status, timestamps, binary checksum/version, and the
secret-free provider identity. Force fallback with a deterministic local
pre-stream fault before a real API fallback; never induce a real provider outage.
After live evidence, rerun any affected security/release checks before review.

### R8. Release preparation and delivery

After deterministic focused tests and the mandatory process proof, update the
current `0.36.6` release to `0.36.7` in `Cargo.toml`, every hya package entry in
`Cargo.lock`, `README.md`, `packages/hya-tui-ts/package.json`, and
`crates/hya/tests/version_metadata.rs`. Move the current root notes byte-for-byte
to `docs/changes/CHANGELOG_0.36.6.md`; root `CHANGELOG.md` must be newest-only
for `0.36.7`. Run current-version project/release gates before live proof.

The non-publishing release rehearsal must parse `.github/workflows/release.yml`,
run `actionlint`, syntax-check every embedded shell block, validate
representative `v0.36.7` tag/version/changelog data without creating a tag, run
the locked target release build, package all three binaries plus prepared TUI
runtime, generate and verify `SHA256SUMS`, extract and smoke all binaries, and
assert legal/client-present/server-absent runtime files. It must verify pinned
action SHAs and the `release` environment. Because the workflow currently keeps
this logic inline and no reusable local validator exists, add a boring
non-publishing `xtask release-rehearsal` command with focused tests:

```sh
cargo run -p xtask -- release-rehearsal --workflow .github/workflows/release.yml --version 0.36.7 --target x86_64-unknown-linux-gnu --no-publish
```

The final binary/release gates run before live proof; after live evidence rerun
the affected security/release checks. Then run `trellis-check`, create and push
one atomic semantic feature commit, and invoke the authoritative
`trellis-finish-work` flow: archive with normal auto-commit using
`python3 ./.trellis/scripts/task.py archive workflow-stage-model-routing`, run
`add_session.py --title ... --commit <feature-hash> --summary ...` with normal
auto-commit, and push only archive/journal maintenance commits. Do not run
`task.py finish` separately unless the skill directs it; do not create a tag or
publish a release unless requested.

## Non-goals

- No live provider requests during planning or deterministic RED/GREEN focused
  gates; implementation delivery proof must run the bounded real-API scenario
  in R7 after those gates.
- No generic provider-option passthrough, category syntax change, or new model
  router.
- No mid-stream retry, transcript replay, automatic Workflow restart, or
  dynamic resident model switch.
- No new Workflow command, TUI model picker, sidebar block, or parallel read
  model.
- No prepared bundle format bump and no revision churn for no-assignment sources.

## Acceptance criteria

- [ ] R1 worker and verifier assignments compile with ordered fallback entries;
      ids are base-only, every embedded `#variant` rejects, empty ids and
      explicitly empty reasoning reject, exact duplicate pairs reject, and
      same-id/different-effort entries remain valid. Unknown non-empty effort
      labels are preserved by compilation and rejected only at runtime.
- [ ] R2 explicit Stage routes override Agent model/category/reasoning; absent
      assignments preserve the exact old Agent/category/global path. Each
      omitted entry uses only its own configured model default or typed Off.
- [ ] R3 all candidate validation, provider capability, effective duplicate,
      budget, and resident route-equality checks happen before side effects.
      Admission stores an immutable `selected_index`; each stream group starts
      there, requests only later declared candidates, and never wraps around.
- [ ] R3 deterministic providers receive base models and distinct efforts in
      declaration order; only safe pre-stream failures advance, including the
      unroutable-preferred/routable-fallback case starting from its admitted
      index.
- [ ] R4 authored effort remains optional; admitted candidate and outcome effort
      are required canonical strings, with Off serialized as `none`. A durable
      run plan contains requested routes and the admitted selected candidates.
- [ ] R4 selection returns stream/selected candidate/index plus pending
      pre-stream class; `none` finalizes only after successful
      `collect_stream_round`, post-stream failures finalize in place without
      failover/replay, terminal pre-stream failures record before return, and
      activation drains exactly one finalized outcome per explicit-route group.
- [ ] R4 cancellation before a candidate attempt emits no outcome; cancellation
      after attempt start, including after selection, finalizes exactly one
      `cancelled` outcome for the last attempted/selected candidate with no
      failover or replay.
- [ ] R4 exactly one bounded route-outcome Event per explicit-route stream group
      survives close/reopen, deduplicates by key, and contains no raw provider
      data. Without `stage.model` or `verify.model`, no route fields, recorder,
      or new Event are serialized, and the old event log re-encodes identically.
- [ ] R5 CLI/API/SDK/Agent-tool surfaces expose one shared route/outcome DTO and
      bounded errors; the existing TUI presentation remains stable with
      optional fields.
- [ ] R6 focused red/green tests cover compiler, provider/runtime, proto/store,
      app/server/CLI/SDK parity, TUI compatibility, and explicit public
      `hya-workflow`, `hya-core/workflow`, and `hya-core` re-exports.
- [ ] R7 the mandatory deterministic process E2E uses several fake model ids,
      captures exact model/reasoning requests, checks public state route
      outcomes, leaves the existing P17 scenario passing, and includes one loop
      worker/verifier on the same base model with different efforts, roles,
      selected candidates, and replayed outcomes.
- [ ] R7 only after deterministic/process gates and R8's 0.36.7 current-version
      project/release gates, a bounded real Workflow runs through public CLI or
      HTTP using the exact final release binary and provider identity. Configured
      API model names are listed without secrets, healthy models are selected
      dynamically, unavailable models are classified/replaced, and a local
      pre-stream fault feeds a real API fallback without an external outage.
      Only bounded model/outcome/id/status/timestamp/checksum evidence remains.
- [ ] R8 after deterministic/process proof, update versions and changelog to
      `0.36.7` and run the complete non-publishing release rehearsal: workflow
      YAML parse, `actionlint`, embedded-shell syntax checks, representative
      tag/version/changelog validation without a tag, locked target build,
      three-binary plus prepared-TUI packaging, `SHA256SUMS`, extraction,
      binary smoke, legal/client-present/server-absent runtime checks, pinned
      action SHAs, and `release` environment validation.
- [ ] R8 run affected security/release checks after live evidence, then run
      `trellis-check`, push one atomic feature commit, archive with normal
      auto-commit, journal with `--commit <feature-hash>` and normal auto-commit,
      and push only maintenance commits. No tag/release is created unless asked.

## Resolved planning decisions

- Persist the complete requested chains in `WorkflowStagePlan`, the admission
  selected candidate in that plan, and one route-outcome Event per explicit-route
  provider stream group only. The outcome Event is the durable audit unit; it is
  not emitted per retry attempt and contains only the bounded stable fields in
  R4.
- New Workflow assignment and fallback ids are base-only. Every embedded
  `#variant` suffix is rejected, and resolved route candidates carry a base
  `ModelRef` plus separate typed effort rather than suffix-encoded effort.
