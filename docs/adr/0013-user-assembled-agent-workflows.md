# ADR 0013: Compiled user-authored Agent Workflows

Date: 2026-08-26
Status: Accepted (revised 2026-08-28)

## Context

hya has bounded transient Teams, persistent resident Agents, durable mailbox
delivery, a shared loop driver, and one event-sourced Projection. Users need to
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
4. An optional actor key binds sequential Stages to one resident Session.
   Transient Stages continue through the governed Team path. Loops continue
   through `IterationDriver` with an independent verifier.
5. The runtime resolves all workers and verifiers through the caller's frozen
   binding, validates resident semantics, and reserves the complete worst-case
   run budget before the first child or mail side effect.
6. Completion and cancellation are Projection-driven. Resident completion
   requires consumed inbox boundaries, no active resident work, and terminal
   idle or failed state. Run cancellation stops new admissions and waits for
   admitted transient and run-owned resident boundaries.

## Consequences

- Authors see one graph language and one validation result before execution.
- Graph declaration order, normalized by the compiler, owns deterministic Stage
  and join ordering; YAML map order does not.
- Source-format changes do not add a second scheduler or Projection.
- Full child transcripts remain in child Sessions; joins receive capped direct
  predecessor evidence only.
- Built-in Workflow bundles and user sources can share the same compiler and
  runtime without hard-coded pipeline logic.
