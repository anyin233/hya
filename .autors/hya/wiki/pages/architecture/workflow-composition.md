---
title: Workflow Composition and Durable Control
description: Compiled Workflow graphs, governed execution, packages, replay, and TUI presentation.
---

# Workflow Composition and Durable Control

A Workflow is one compiled, user-authored Agent graph. It uses the same Team,
loop, resident, permission, event, and Session machinery as direct Agent work.
There is no Workflow-specific scheduler or second read model.

## Authoring contract

A source is one `*.hya.md` document. Strict YAML frontmatter declares metadata,
required inputs, failure policy, and nodes. A restricted `flowchart TD` body
declares topology:

```markdown
---
kind: Workflow
name: feature-delivery
description: Plan, implement, and review a request.
inputs:
  request: Work request to complete.
on_failure: collect_all
nodes:
  plan:
    agent: planner
    directive: Plan {{input.request}}.
  impl_a:
    agent: implementer
    directive: Implement the core path.
  impl_b:
    agent: implementer
    directive: Implement the tests.
  review:
    agent: reviewer
    directive: Review the direct predecessor evidence.
---
flowchart TD
  plan --> impl_a & impl_b
  impl_a & impl_b --> review
```

The compiler accepts standalone node ids, `a --> b`, `a --> b & c`, and
`a & b --> c`. It rejects cycles, self-edges, labels, shapes, subgraphs,
undeclared nodes or inputs, and the removed YAML `stages:` / `needs:` format.
First graph occurrence defines stable Stage order. Incoming edge declaration
order defines join order.

Discovery precedence is:

1. `<workdir>/.hya/workflows/*.hya.md`;
2. `$HOME/.config/hya/workflows/*.hya.md`;
3. installed `WorkflowBundle` entries;
4. the read-only first-party catalog.

The immutable identity contains source id, declared name, and canonical
revision. Project and user sources can shadow a bare bundle name. A qualified
`bundle:<bundle-id>/workflow/<workflow-id>` id always resolves exactly.

## Evidence and failure semantics

Every edge carries bounded evidence automatically. A downstream Stage receives
one `<workflow-upstream>` block for its direct predecessors only. Entries use
compiled order and include Stage id, Agent id, terminal status, and at most
4,000 UTF-8-safe bytes of output. Full transcripts remain in child Sessions.

`fail_fast` finishes the admitted level and skips later Stages.
`collect_all` continues eligible Stages with explicit failed evidence. Either
policy returns a failed run when any Stage fails.

## Governed execution

`hya_core::run_workflow` receives only a validated `CompiledWorkflow` and a
resolved run context. Before any Stage starts, the control plane validates
inputs, resolves every worker and verifier through the caller's frozen binding,
and reserves the complete worst-case activation budget.

Transient Stages at one graph level run as one pre-admitted Team batch. Each
Stage keeps its own Agent roster, resource policy, bundle sidecar, model policy,
and spawn lifecycle. A Stage cannot inherit broader delegation authority from
the parent Agent.

A loop Stage uses the shared `IterationDriver`; a fresh verifier Session owns
the stop decision. An `actor` key routes sequential Stage activations to one
run-scoped resident Agent through durable mailbox boundaries. Resident work
continues through `ResidentSupervisor`, not a Workflow-owned actor loop.

Cancellation stops new admissions, cancels transient work, waits for admitted
resident boundaries, and records a truthful cancelled result. It does not
report terminal state while run-owned work remains active.

## WorkflowBundle closure

A `WorkflowBundle` is a closed installable payload beside singular
`AgentBundle`. It contains one Workflow plus exactly every reachable Stage
Agent, verifier Agent, and transitive `can_spawn` target. Missing, extra,
duplicate, unreachable, or reserved built-in Agent ids fail preparation. A
package cannot depend on an unpinned host Agent definition.

Preparation stores the compiled source, source digest, compiler revision,
sorted Agent closure, and shared resources in prepared format v2. Install
validation checks the immutable first-party bundle/Agent/resource namespace
before registry mutation. Refresh publishes Workflow, Agents, and validated
plugin contributions in one runtime generation. Existing turn bindings keep
their pinned generation.

The repository ships two ordinary catalog examples:

- `plan-impl-review`: selectable first-party WorkflowBundle; never selected by
  default;
- `bundles/examples/argus-example`: installable investigation, planning,
  parallel implementation/review, synthesis, and verification WorkflowBundle.

Neither graph has an executor special case.

`bundle list` and `bundle info hya/plan-impl-review` expose the first-party
payload as immutable content. It cannot be replaced or uninstalled. Production
installations resolve executable bundle sidecars through the Compat adapter in
this order: explicit `HYA_COMPAT_ADAPTER_DIR`, adjacent `lib/hya/compat-adapter`,
then workspace source for development. Installer and release assets stage the
adapter with locked production dependencies.

## Durable control and replay

`hya_app::WorkflowControl` is the single command seam for list, info, select,
run, and state operations. CLI, Agent tool, native command, HTTP, Rust SDK, and
in-process transports call this seam. Only `hya_core::run_workflow` executes a
compiled plan.

The owning Session event log records selection, run start, Stage start, member
links, Stage finish, and run finish. `hya_proto::Projection` reconstructs the
current selection, declaration-ordered Stages, linked members, progress, and
terminal state. It ignores late events for old or terminal runs.

A restart never replays Stage effects. If the recorded run owner is provably
dead, reconciliation records one `interrupted` terminal event. A missing source
remains `unavailable`; a changed revision remains `stale`. Both require explicit
reselection and never silently substitute a same-name source.

## Operator surfaces

Backend commands use the shared control path:

```sh
hya-backend workflow list
hya-backend workflow info plan-impl-review
hya-backend --db sessions.db workflow use plan-impl-review --session hysec_...
hya-backend --db sessions.db workflow run --session hysec_... \
  --input request="Fix parser retries"
hya-backend --db sessions.db workflow state --session hysec_...
```

`use` and `state` require an existing owning Session. `run [name]` can bind an
existing Session and use its selection when the name is absent; without
`--session`, it creates a new Session and requires a name.

Inside a Session, native `/workflow list`, `/workflow info <name>`,
`/workflow use <name>`, `/workflow run [name] [key=value ...]`, and
`/workflow state` bypass parent-model admission. Selection does not remove or
rewrite transcript messages.

Session GET/list/bootstrap responses carry the typed Workflow Projection.
Existing `session.updated` SSE invalidation replaces the same Session value.
For display activity, the server joins Workflow Member references to canonical
Session members and emits only linked `spawning`/`running` rows as bounded
`workflowActivity`; unrelated, terminal, and full directive rows stay out of
the Session DTO.

The TypeScript TUI's hya-owned `sidebar_content` plugin reads those synchronized
values directly; it adds no polling timer, second SDK client, graph reducer, or
roster renderer. The sidebar shows selected identity and revision availability,
run status, graph level, active Stage ids, active/total Agent instances, Stage
progress, and bounded current work. Parallel Stage names preserve declaration
order and compact as `first +N`. The existing run-tree roster remains the
navigation surface for linked child Agents.

## Code map

- `crates/hya-workflow`: source parser, compiler, plan, and canonical revision.
- `crates/hya-core/src/workflow`: governed graph execution.
- `crates/hya-app/src/workflow_control.rs`: catalog resolution, commands,
  idempotency, reconciliation, and run orchestration.
- `crates/hya-proto/src/workflow.rs`: shared identities, Events, Projection DTOs,
  commands, and results.
- `crates/hya-store/src/workflow.rs`: durable Workflow event queries.
- `crates/hya-server/src/workflow*.rs`: typed and native command routes.
- `packages/hya-tui-ts/src/hya/workflow-presentation.ts`: strict Projection
  decoding and deterministic compact presentation.
- `packages/hya-tui-ts/src/upstream/feature-plugins/sidebar/workflow.tsx`:
  synchronized sidebar adapter.
