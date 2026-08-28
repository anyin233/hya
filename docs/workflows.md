# Workflows

A Workflow is a Markdown document that composes authorized Agents into a
directed acyclic graph. The compiler validates the complete document before the
runtime creates a child Session or sends mail.

## Discovery

| Precedence | Path |
| --- | --- |
| Project | `<workdir>/.hya/workflows/*.hya.md` |
| User | `$HOME/.config/hya/workflows/*.hya.md` |

Project sources take precedence over user sources with the same declared name.
YAML-only files and the removed `stages:`/`needs:` format are not accepted.

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

## CLI

```sh
hya-backend workflow list
hya-backend workflow info feature-delivery
hya-backend workflow run feature-delivery \
  --input request="Fix parser retries"
```

`workflow run` prints one terminal row for every compiled Stage. `--json`
emits the same report as JSON. Input values split on the first `=`.

Agents can use the governed `workflow` tool:

- `{"action": "list"}` lists discovered Workflows.
- `{"action": "run", "name": "feature-delivery", "inputs": { ... }}` runs
  the selected graph in the current Session tree.

## Library surface

`hya-workflow::compile(WorkflowSource)` is the only authoring construction
path. It returns a read-only `CompiledWorkflow` with metadata, normalized
Stages and levels, automatic join rendering, and a canonical revision.

`hya-core::run_workflow` accepts only a `CompiledWorkflow` plus a resolved
`WorkflowRunContext`. It does not parse source or rebuild the plan.
