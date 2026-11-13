# ADR 0013: Compiled user-authored Agent Workflows

Date: 2026-08-26
Status: Accepted (revised 2026-11-13)

Execution substrate superseded by [ADR-0017](0017-workflow-on-unified-substrate.md):
all workers and verifiers now use resident episode activations. Compilation
and control-plane contracts below still apply; ready scheduling replaces level barriers.

## Context

hya has persistent resident Agents, durable mailbox delivery, a shared loop
driver, and one event-sourced Projection. Users need to
compose these primitives into reusable graphs. Direct execution of a loose
Stage-list format duplicated validation, left join data flow in prompt
placeholders, and could admit work before the complete run cost was known.

## Decision

1. A Workflow source is one `*.hya.md` document. Typed YAML frontmatter defines
   metadata and nodes. A restricted `flowchart TD` body defines graph topology.
2. `hya-workflow::compile` is the only authoring construction path. It validates
   the complete source and returns immutable metadata, normalized Stage order,
   direct predecessors, topological levels, worst-case activation counts, and a
   canonical revision.
3. Edges carry bounded predecessor evidence automatically. Public directives
   can reference only declared inputs through `{{input.<name>}}`; they cannot
   address arbitrary Stage outputs.
4. Every Stage uses a resident activation. An optional actor key shares one
   resident Session across Stages; ready uses of that actor serialize in
   compiled Stage order. Loops use `IterationDriver` and a fresh resident
   verifier per judgment. Verification starts when its worker completes,
   without waiting for unrelated Stages.
5. The runtime resolves all workers and verifiers through the caller's frozen
   binding and preflight-checks worst-case activations against live capacity
   before the first child or mail effect. Registration/revival rechecks capacity
   transactionally; preflight reserves no slots.
6. The scheduler counts unfinished direct predecessors. Reaching zero makes a
   Stage ready; it activates when every predecessor succeeded and its actor is
   free. Levels remain graph/display metadata, never execution barriers.
   Evidence is rendered from indexed results in compiled predecessor order,
   regardless of completion order.
7. `fail_fast` finishes already activated Stages and admits no new Stage after
   observing failure. `collect_all` continues independent eligible branches;
   successors of failed or skipped Stages are skipped under either policy.
   Cancellation stops admission, awaits active Stage controllers, and ends every
   run-owned actor and its descendants, including parked actors, before releasing
   root-driver activity. Activation waiters own completion; Events persist it.

## Consequences

- Authors see one graph language and one validation result before execution.
- Graph declaration order, normalized by the compiler, owns deterministic Stage
  and join ordering; YAML map order does not.
- Source-format changes do not add a second scheduler or Projection.
- Full child transcripts remain in child Sessions; joins receive capped direct
  predecessor evidence only.
- Built-in Workflow bundles and user sources can share the same compiler and
  runtime without hard-coded pipeline logic.
