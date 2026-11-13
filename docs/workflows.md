# Workflows

A Workflow is a Markdown document that composes authorized Agents into a
directed acyclic graph. The compiler validates the complete document before the
runtime creates a child Session or sends mail.

## Discovery

| Precedence | Source |
| --- | --- |
| Project | `<root>/.hya/workflows/*.hya.md` for every root of the bound catalog scope |
| User | `$HOME/.config/hya/workflows/*.hya.md` |
| Bundle catalog | Installed and read-only first-party `Bundle` payloads |

A Session's catalog scope decides which roots the Project tier reads: a
Session inside a registered Project reads `<workdir>/.hya/workflows` first,
then every one of the Project's registered roots in declaration order
(deduped); a Session in a plain directory (no Project) reads just that
directory; the project-less global scope (`hya serve` with no directory
named) reads no project root at all.

Across multiple Project roots, a Workflow's source id stays
`project:<path relative to its own root>`. A later root's file is skipped
(with a warning) when either its source id or its declared Workflow name
was already published by an earlier root — the first root to publish a
given source id or name wins.

Project sources take precedence over user and bundle sources with the same declared name. User sources take precedence over bundle sources. Two bundle sources with the same bare name are ambiguous; use the exact `bundle:<bundle-id>/workflow/<workflow-id>` source id. YAML-only Workflow documents and the removed `stages:`/`needs:` format are not accepted.

## Packaging a Bundle

A `Bundle` packages one compiled Workflow and its exact Agent closure. It uses the same Bundle format as agentless and multi-Agent packages.

The source must use `bundle.yaml`; `bundle.hya.md` is rejected because a multi-Agent payload has no unambiguous body prompt.

```yaml
kind: Bundle
identity:
  id: acme/feature-delivery
  version: 1.0.0
  publisher: acme
workflow:
  id: feature-delivery
  path: workflows/feature-delivery.hya.md
agents:
  - id: workflow-worker
    description: Executes the packaged Workflow.
    role: subagent
    prompt: prompts/workflow-worker.md
```

The package closure also contains `workflows/feature-delivery.hya.md` in the document format below and `prompts/workflow-worker.md`.

Preparation fails unless all of these conditions are true:

- `workflow.path` matches `workflows/*.hya.md`, and `workflow.id` equals the compiled document `name`.
- Every packaged Agent prompt path is under `prompts/`.
- `agents:` contains every Stage Agent, verifier Agent, and recursive `can_spawn` target.
- `agents:` contains no Agent outside that reachable closure. Built-in Agent ids are reserved and cannot substitute for packaged closure members.
- Agent ids are globally unique across the prepared catalog.
- Shared `resources` and `extensions` satisfy the same digest, path, resource-view, and executable-sidecar rules as a Bundle.

Preparation stores the compiled Workflow source, source digest, compiler revision, sorted Agent closure, and shared resources in one canonical prepared payload. Install refresh publishes its Workflow, Agents, and prepared Skill contributions in one runtime generation. Existing turn bindings keep the old generation.

The normal package commands accept both closed kinds:

```sh
hya bundle install feature-delivery.hyabundle
hya bundle list
hya bundle info acme/feature-delivery
```

`bundle list` and `bundle info` report `kind=Bundle`, the Workflow id, and the packaged Agent ids.

### Full Argus example

The repository ships a complete ordinary Bundle source at
[`bundles/examples/argus-example`](../bundles/examples/argus-example/). It uses
the public graph, package, registry, catalog, and executor paths; no engine
branch recognizes its topology. Package and install it from a source checkout:

```sh
scripts/package-argus-example.sh bundles/examples/argus-example /tmp/hya-argus-example.hyabundle
hya bundle install /tmp/hya-argus-example.hyabundle
hya bundle info hya/argus-example
hya workflow run argus --input request="Investigate and deliver the requested change"
```

Release archives include the same package as
`examples/hya-argus-example.hyabundle`; install that file directly instead of
repackaging it. The example contains investigation, planning, parallel
implementation and test work, independent architecture and quality reviews,
synthesis, and parallel/final verification. It is not installed or selected
automatically.

## Document format

```markdown
---
kind: Workflow
name: feature-delivery
description: Plan, implement, and review one request.
inputs:
  request: Work request to complete.
on_failure: collect_all
nodes:
  plan:
    title: Produce plan
    agent: workflow-planner
    directive: Plan {{input.request}}.
  impl_a:
    title: Implement core
    agent: workflow-implementer
    directive: Implement the core path.
  impl_b:
    title: Implement tests
    agent: workflow-implementer
    directive: Implement contract tests.
  review:
    title: Review result
    agent: workflow-reviewer
    directive: Review all direct predecessor evidence.
---
flowchart TD
  plan --> impl_a & impl_b
  impl_a & impl_b --> review
```

The graph grammar is intentionally small:

- The first non-comment line is `flowchart TD`.
- A standalone identifier declares an isolated node.
- `a --> b`, `a --> b & c`, and `a & b --> c` declare edges.
- A line that starts with `%%` is a comment.
- Labels, shapes, subgraphs, edge labels, style directives, self-edges, and
  cycles are invalid.
- Every frontmatter node must occur in the graph. Every graph node must have a
  frontmatter definition.

First graph occurrence defines stable Stage order. Incoming edge declaration
defines direct-predecessor order at a join.

## Inputs and evidence

All declared inputs are required. `{{input.<name>}}` is the only public
placeholder. Missing, unknown, malformed, and undeclared input references fail
before authorization and scheduling.

Edges carry data automatically. A downstream directive receives one
`<workflow-upstream>` block. It contains only direct predecessors, in compiled
predecessor order. Each entry contains the Stage id, Agent id, terminal status,
and at most 4,000 bytes of UTF-8-safe output. Full child transcripts stay in the
child Sessions.

`on_failure` has two values:

- `fail_fast` (default): finish already activated Stages, stop new activations
  after observing failure, mark remaining Stages skipped, and finish failed.
- `collect_all`: continue independent eligible branches, skip successors of
  failed or skipped Stages, and finish failed if any Stage failed.

Stages become ready when their unfinished direct-predecessor count reaches zero.
Every predecessor must have succeeded before activation. For `a --> c` with an
independent `b`, `c` can start and finish while `b` is still running. Each loop
Stage starts its verifier as soon as its worker completes. Join evidence remains
in compiled predecessor order even when the predecessors finish in reverse
order. No scheduling option or concurrency limit is added; team capacity still
bounds the run. Topological levels are retained in plan/state responses and
Stage guidance as graph metadata, not scheduling barriers.

## Loop Stages

```yaml
mode: loop
verify:
  agent: workflow-reviewer
  until: The implementation satisfies the request.
  max_iterations: 3
```

Loop repetition uses the shared `IterationDriver`. A fresh verifier Session is
the stop authority for each judgment. The worker cannot finish its own loop by
claiming success. The run's cancellation token reaches every iteration and
judgment. A cancelled loop retains its latest completed worker output and is
recorded Cancelled. A wrap-up capacity refusal remains `WORKFLOW_OVERLOADED`;
wrap-up errors or cancellation cannot complete the Stage successfully.

## Stage model routing

Each Workflow invocation captures the engine’s current provider router for
preflight and Stage routing. Providers added after startup are available to the
next invocation without restarting the runtime. For example, add a provider,
then set a Stage’s `model.id` to its model using the schema below.

Each Stage can override its Agent model, category, and reasoning policy with an
ordered model route. A loop verifier has an independent route:

```yaml
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
  max_iterations: 3
  model:
    id: 12th-oai/gpt-5.6-sol
    reasoning: low
    fallback:
      - id: 12th-anth/claude-sonnet-4-6
```

Every `id` is a suffix-free base model reference. The separate `reasoning:`
field is the only effort encoding in a Workflow assignment; an embedded
`#variant` suffix is invalid even when `reasoning:` is omitted. Existing
non-Workflow model APIs can still use their documented `provider/model#variant`
form.

The preferred entry and every fallback entry resolve effort independently. An
omitted `reasoning:` uses only that model's configured default, or canonical
`none` when the model has no default. It never inherits another route entry's
effort or the Agent policy. The same base model can therefore appear more than
once with different efforts.

Admission validates the complete route before creating a child Session,
registering a resident actor, sending mail, or starting a provider request. It
records the first routable candidate index and every provider stream group
starts there without wrapping to an earlier entry. Only `UnknownModel`,
transport failure, HTTP 429, and HTTP 5xx can advance before a stream exists.
Once a stream exists, hya never replays it on another candidate. Stage routes
remain request-local and are not inserted into the engine's global category
fallback map. Stages that share one resident actor key must have the same
effective worker route.

The durable run plan retains the requested routes and the candidates selected
during admission. The root Session records one route outcome per explicit-route
provider stream group as `WorkflowStageRouteOutcome`. Each outcome contains the
Stage/member role, iteration, assistant step, candidate index, base model,
canonical effort, and stable failure class. It contains no prompt, input,
provider response, credential, header, or raw error text. A Workflow without an
explicit assignment keeps the existing Agent/category/global route path and
does not add route fields or route-outcome Events.

Prepared Bundles use prepared format v4 and retain
the complete source and compiler revision. A route-bearing v2 source still
requires a runtime with the model-routing compiler; unchanged no-assignment v2
sources retain their existing revision. Client session sidebars do not render
model routes; typed CLI, API, SDK, and state results expose the durable route
data while the compact presentation remains unchanged.

## Resident actors

```yaml
actor: planner
```

An actor key routes sequential Stages to one resident Session. Named actors
retain their resident lifetime after successful Stages and runs; anonymous
workers end when their Stage finishes, after loop verification and wrap-up.
Their claims and descendants are released before successors activate. Repeating an
Agent id without `actor` still creates distinct one-shot Stage Sessions. Any
Agent can back either shape: since 0.41.0 Agent definitions carry no spawn
lifecycle (`spawn_lifecycle` is a removed manifest key), so the Stage's `actor`
key alone selects a persistent actor Session. An actor key bound to different
Agent ids or an actor key combined with `mode: loop` fails before execution.
Ready Stages sharing an actor serialize in compiled Stage order; an actor is
never activated while its earlier Stage is busy, even across graph levels.

The first and later actor directives are durable parent mail. The episode
completes an activation after its inbox boundary is consumed and its work
finishes durably, returning the final assistant text. A terminal report also
completes the Stage with its declared outcome and text; a forced ending fails
the Stage (or cancels it when the run requested cancellation). A settled actor
remains live and can accept a later Stage. An archived actor is revived under
the same capacity and parent-claim checks as ordinary parent mail.

For example, `plan --> refine` with `actor: planner` on both nodes runs two
mail activations on one Session. The second activation installs its own
Workflow guidance and model route before work can start. Stage completion
uses an episode-owned waiter; it adds no event-bus subscription or per-envelope
Projection polling.

## Governance

The executor resolves every worker and verifier through the caller's immutable
runtime binding. Each target keeps its own roster, resource policy, and
sidecar. Before any child or mail effect, the executor checks the complete
worst-case activation count against the configured budget and the root's
remaining live capacity. Preflight reserves nothing. An oversized graph is
invalid source; current occupancy or a later registration/revival capacity
refusal returns `WORKFLOW_OVERLOADED`. Ready one-shot Stages (no `actor`)
register parked residents and activate independently. Loop continuations
reactivate the same worker: settlement retains its claim and runtime, while a
report releases capacity and a later activation revives it. Every verifier
judgment uses a fresh resident and releases its claim after judgment. For
example, a loop worker that settles and then receives a verifier's denial keeps
one live slot across both iterations; each judgment temporarily uses a second
slot. The shared iteration driver still owns stop decisions. Workers and
verifiers can both call `report`; plain assistant text also completes an
activation. There is no transient executor. Deadline notices ask an actor to
call `report` only if its current request offers that tool; otherwise they ask
it to finish its turn. This also covers core embedders without a governor,
where `report` is hidden by the depth-zero tool view.

Cancellation stops new admissions and activations, waits for active Stage
controllers, and ends every run-owned actor and its descendants, including
registered actors that have not activated. Cleanup completes while the run owns
root-driver activity, before returning a cancelled report.

## Durable Session state

Workflow selection and execution are Session state. The owning Session event
log records selection, run start, Stage transitions, member links, and run
finish. Replay reconstructs the latest Workflow Projection; it does not depend
on child-process memory or a second read model.

Restart reconciliation never replays Stage effects. A nonterminal run whose
owner is provably dead becomes `interrupted`. A selected source that disappeared
remains `unavailable`; a selected source whose revision changed remains `stale`.
Both conditions require explicit reselection. hya never substitutes another
same-name source for the selected identity.

`hya_app::WorkflowControl` is the shared list, info, select, run, and state seam
for CLI, Agent tool, native command, HTTP, SDK, and in-process transports. Only
the core executor runs the compiled plan.

## Native Session commands

Clients render Workflow state from the typed Workflow Projection delivered
through normal bootstrap and `session.updated` synchronization: selection and
revision availability, run status, topological Stage levels (which may be
active concurrently), declaration-ordered active Stages, active/total Agent instances, Stage progress, and bounded current work.
A client should not poll, create a second SDK client, fold raw Workflow
Events, or replace the existing run-tree roster used to navigate child Agents.

Use the native command in the current Session:

```text
/workflow list
/workflow info feature-delivery
/workflow use feature-delivery
/workflow run request=fix-parser-retries
/workflow state
```

Native Workflow commands bypass parent-model admission. Selection and results
use the normal command transcript path; selection does not remove existing
messages. A run can also name the Workflow explicitly:
`/workflow run feature-delivery request=fix-parser-retries`.

## CLI and Agent tool

```sh
hya workflow list
hya workflow info feature-delivery
hya --db sessions.db workflow use feature-delivery --session hysec_...
hya --db sessions.db workflow run feature-delivery \
  --session hysec_... --input request="Fix parser retries"
hya --db sessions.db workflow state --session hysec_...
```

`workflow use` and `state` require an existing owning Session through
`--session`. `workflow run` accepts `--session`; with it, the Workflow name may
be omitted to use that Session's selection. Without it, the command creates a
new Session and requires a name.
`workflow run` prints one terminal row for every compiled Stage. `--json` emits
the same shared result as JSON. Input values split on the first `=`. `--revision`
adds an optimistic compiler-revision fence.

Agents can use the governed `workflow` tool with
`action=list|info|select|run|state`. `run` accepts `name`, `inputs`, an optional
`expected_revision`, and an optional stable run id for idempotent retries. A
run without `name` uses the Session's selected Workflow.

## Library surface

`hya-workflow::compile(WorkflowSource)` is the only authoring construction
path. It returns a read-only `CompiledWorkflow` with metadata, normalized
Stages and levels, automatic join rendering, and a canonical revision.

`hya-core::run_workflow` accepts only a `CompiledWorkflow` plus a resolved
`WorkflowRunContext`. It does not parse source or rebuild the plan.

Execution owns the team's activity until the run settles. Stage handoffs cannot
start `TEAM QUIESCED` synthesis on the root while a Workflow runs. A Workflow
started on a cold root (for example `hya workflow run my-workflow`) runs only
its stages; it does not start a root model turn. A later user root turn or
explicit model-team activation restores autonomous main wakes. This rule lives
in the core runner and also applies to `WorkflowControl`, HTTP and tool runs;
callers do not acquire a root turn lease to enforce it.

### Reads during execution

The `workflow` tool handles each request independently. While
`workflow {"action":"run","name":"my-workflow"}` waits for a long run,
`workflow {"action":"list"}` and `workflow {"action":"state"}` return without
waiting for completion, including in the same Session. The command and result
schemas are unchanged; per-session admission still rejects a second active run.
Runtime shutdown cancels and waits for admitted Workflow execution. Finished
delivery also uses a tracked task: disconnecting or dropping its waiting caller
cancels the run, and cleanup and the terminal Event finish before ownership is
released. Started delivery continues after its admission response.

An admitted run owns its terminal `WorkflowRunFinished` append independently of
its calling actor's lifetime. Admission and execution effects remain actor-fenced;
the tracked run owner writes the terminal Event without the caller's claim, which
may already be released during cancellation cleanup. A cancelled run remains
`cancelled` if execution or ending fails; its bounded error is persisted and the
caller still receives the typed error. No tool parameters or wire fields change.

Never-started Stages are `skipped` in both the core run report and durable
projection, including cancellation; activated Stages cancelled by the run are
`cancelled`. The scheduler emits `WorkflowStageFinished { status: Skipped }`
when it skips a Stage, so `/workflow state` and live updates show that decision
while independent branches continue. Join evidence contains successful direct
predecessors only (`status="done"`). Rust `StageEvidence::new(output: &str)`
therefore carries only output; the unreachable evidence-status enum is removed.
Existing wire status values and generated APIs are unchanged.
