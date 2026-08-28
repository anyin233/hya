# Design

The parent design sections **Authoring Module** and **Governed Runtime Module** are authoritative.

## Owned Module

Create `crates/hya-workflow` as the deep compiler module. Its only public construction path is `compile(WorkflowSource) -> CompiledWorkflow`. Definition/plan fields remain private and read-only. Move author model, graph planning, join rendering, and canonical revision logic out of `hya-core`.

`hya-core` owns runtime effects only. `run_workflow` accepts a compiled program and resolved run context; it never parses source or creates a second scheduler.

## Invariants

- Markdown frontmatter plus simplified `flowchart TD` is the only public source format.
- First graph occurrence determines stable Stage order; incoming-edge declaration determines join order.
- Edges inject bounded direct-predecessor evidence automatically.
- Every input/Agent/verifier/actor/budget error is detected before the first spawn or mail append.
- Repeated Agent id has no identity meaning. Only an explicit actor key selects one resident Session.
- The DAG schedules activations once. `IterationDriver` owns loop repetition; `ResidentSupervisor` owns mail repetition.
- Run terminal state waits for every admitted activation boundary, not global Team quiescence.

## Internal Seams

Private parser and planner adapters may be tested independently, but public behavior tests must compile through the one interface. Resident waiting uses EventBus notification plus Projection re-read; no sleep/poll loop is added.

## Compatibility

Delete old parser/constructor exports and migrate all callers. No deprecated alias, dual parser, or stage-output placeholder remains.
