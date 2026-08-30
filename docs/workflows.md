# Workflows

A Workflow is a Markdown document that composes authorized Agents into a
directed acyclic graph. The compiler validates the complete document before the
runtime creates a child Session or sends mail.

## Discovery

| Precedence | Source |
| --- | --- |
| Project | `<workdir>/.hya/workflows/*.hya.md` |
| User | `$HOME/.config/hya/workflows/*.hya.md` |
| Bundle catalog | Installed and read-only first-party `WorkflowBundle` payloads |

Project sources take precedence over user and bundle sources with the same declared name. User sources take precedence over bundle sources. Two bundle sources with the same bare name are ambiguous; use the exact `bundle:<bundle-id>/workflow/<workflow-id>` source id. YAML-only Workflow documents and the removed `stages:`/`needs:` format are not accepted.

## Packaging a WorkflowBundle

A `WorkflowBundle` packages one compiled Workflow and its exact Agent closure. It is a distinct closed payload kind beside singular `AgentBundle`.

The source must use `bundle.yaml`; `bundle.hya.md` is rejected because a multi-Agent payload has no unambiguous body prompt.

```yaml
kind: WorkflowBundle
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
    spawn_lifecycle: transient
```

The package closure also contains `workflows/feature-delivery.hya.md` in the document format below and `prompts/workflow-worker.md`.

Preparation fails unless all of these conditions are true:

- `workflow.path` matches `workflows/*.hya.md`, and `workflow.id` equals the compiled document `name`.
- Every packaged Agent prompt path is under `prompts/`.
- `agents:` contains every Stage Agent, verifier Agent, and recursive `can_spawn` target.
- `agents:` contains no Agent outside that reachable closure. Built-in Agent ids are reserved and cannot substitute for packaged closure members.
- Agent ids are globally unique across the prepared catalog.
- Shared `resources` and `extensions` satisfy the same digest, path, resource-view, and executable-sidecar rules as an AgentBundle.

Preparation stores the compiled Workflow source, source digest, compiler revision, sorted Agent closure, and shared resources in one canonical prepared payload. Install refresh publishes its Workflow, Agents, and prepared Skill contributions in one runtime generation. Existing turn bindings keep the old generation.

The normal package commands accept both closed kinds:

```sh
hya-backend bundle install feature-delivery.hyabundle
hya-backend bundle list
hya-backend bundle info acme/feature-delivery
```

`bundle list` and `bundle info` report `kind=WorkflowBundle`, the Workflow id, and the packaged Agent ids.

### Full Argus example

The repository ships a complete ordinary WorkflowBundle source at
[`bundles/examples/argus-example`](../bundles/examples/argus-example/). It uses
the public graph, package, registry, catalog, and executor paths; no engine
branch recognizes its topology. Package and install it from a source checkout:

```sh
scripts/package-argus-example.sh bundles/examples/argus-example /tmp/hya-argus-example.hyabundle
hya-backend bundle install /tmp/hya-argus-example.hyabundle
hya-backend bundle info hya/argus-example
hya-backend workflow run argus --input request="Investigate and deliver the requested change"
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

- `fail_fast` (default): finish the admitted level, mark later Stages skipped,
  and finish the run failed.
- `collect_all`: continue eligible Stages with explicit failed evidence, then
  finish the run failed if any Stage failed.

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
claiming success.

## Stage model routing

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

Prepared WorkflowBundles remain prepared format v2 because they already retain
the complete source and compiler revision. A route-bearing v2 source still
requires a runtime with the model-routing compiler; unchanged no-assignment v2
sources retain their existing revision. The Session sidebar does not render
model routes; typed CLI, API, SDK, and state results expose the durable route
data while the compact TUI presentation remains unchanged.

## Resident actors

```yaml
actor: planner
```

An actor key routes sequential Stages to one resident Session. Repeating an
Agent id without `actor` still creates distinct transient Sessions. The target
Agent must declare resident spawn lifecycle. An actor key on a transient Agent,
a resident Agent without an actor key, an actor key bound to different Agent
ids, or same-level reuse of one actor key fails before execution.

The first and later actor directives are durable mail. A Stage completes only
after the team Projection shows that its captured inbox boundary is consumed,
resident work is absent, and the actor is idle or failed.

## Governance

The executor resolves every worker and verifier through the caller's immutable
runtime binding. Each target keeps its own roster, resource policy, sidecar, and
spawn lifecycle. The complete worst-case run budget is reserved before the
first child or mail effect. Same-level transient Stages run as one governed Team
batch. Loop and resident work continue to use the existing iteration and
resident supervisors.

Cancellation stops new admissions, cancels transient work, stops active
run-owned resident work, waits for admitted boundaries, and returns a truthful
cancelled report.

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

## TUI and native Session commands

The Session sidebar reads the typed Workflow Projection from normal bootstrap
and `session.updated` synchronization. It shows selection/revision availability,
run status, graph level, declaration-ordered active Stages, active/total Agent
instances, Stage progress, and bounded current work. It does not poll, create a
second SDK client, fold raw Workflow Events, or replace the existing run-tree
roster used to navigate child Agents.

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
hya-backend workflow list
hya-backend workflow info feature-delivery
hya-backend --db sessions.db workflow use feature-delivery --session hysec_...
hya-backend --db sessions.db workflow run feature-delivery \
  --session hysec_... --input request="Fix parser retries"
hya-backend --db sessions.db workflow state --session hysec_...
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
