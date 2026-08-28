# ADR 0014: WorkflowBundle distribution and event-sourced Workflow control

Date: 2026-08-28
Status: Accepted

## Context

Workflows must cross authoring, package installation, Session recovery, CLI,
HTTP, SDK, and TUI boundaries without creating a second package schema, runtime
scheduler, or durable read model. A selected Workflow must also retain the exact
source revision that the user chose, while bundle Agent prompts and resources
can change independently of the Workflow graph.

## Decision

1. `WorkflowBundle` is a distinct closed payload beside singular `AgentBundle`
   in prepared format v2. It contains exactly one compiled Workflow and the
   complete reachable Agent closure. Both kinds use the same package inspection,
   digest, registry row, atomic install, catalog, resource ownership, and removal
   paths.
2. An installed Workflow identity includes its bundle-qualified source and a
   revision that folds the compiler revision with the owning prepared-bundle
   digest. Agent, prompt, Skill, tool, hook, or Extension changes therefore make
   an existing selection stale.
3. The owning Session event log and `hya_proto::Projection` are the only durable
   Workflow read model. Lifecycle Events store identity, bounded plan metadata,
   member references, status, and a request hash, but never directives, input
   values, Stage output, or child transcripts.
4. `hya_app::WorkflowControl` is the single list, info, select, state, and run
   seam for the Agent tool, CLI, native and Compat routes, SDK, and TUI-backed
   command path. Only `hya_core` executes the compiled plan.
5. Startup recovery requires exclusive runtime ownership of the Session store.
   It marks prior nonterminal runs interrupted exactly once and never replays a
   Stage. Catalog availability and active Member work are derived at hydration;
   they are not persisted as another projection.

## Consequences

- First-party and third-party WorkflowBundles use the same immutable catalog and
  governed execution paths; first-party content is visible but cannot be
  replaced, removed, or selected automatically.
- Prepared-v1 rows require reinstall, and binaries predating Workflow Events
  cannot safely read a store after 0.36.0 writes those Events.
- Clients receive typed replay state through existing transports and
  event-driven invalidation. The TUI does not own a poller, HTTP client, event
  reducer, or Agent-activity authority.
- Package closure and contribution equality fail closed before publication or
  Agent execution.
