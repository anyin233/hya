# 0.36.0

## Workflow language and governed execution

- Workflow authoring now uses one Markdown document with strict YAML frontmatter
  and a simplified `flowchart TD` graph. Declaration order defines deterministic
  Stage and join order; `&` supports fan-out and fan-in. The unpublished YAML
  `stages:` / `needs:` format is rejected without a compatibility path.
- Required inputs use `{{input.<name>}}`. Edges carry bounded, typed output from
  direct predecessors. `fail_fast` and `collect_all` have distinct terminal
  semantics, and malformed graphs, inputs, loops, verifier combinations, or
  resident actor reuse fail before the first Agent starts.
- Each transient graph level runs through the existing pre-admitted Team batch.
  Loop Stages use the shared iteration driver with an independent verifier.
  Explicit actor keys reuse one resident Agent Session through durable mailbox
  work boundaries. Workflow execution does not add another scheduler,
  permission plane, or Agent runtime.

## Workflow bundles and contributions

- Public packages now use a closed `AgentBundle | WorkflowBundle` payload and
  prepared format v2. `AgentBundle` remains singular. `WorkflowBundle` carries
  one compiled Workflow and its exact reachable Agent closure in
  one install/registry row. Missing, extra, duplicate, unreachable, or
  built-in-shadowing Agents fail preparation.
- Install refresh publishes the Workflow, Agent catalog, and validated static
  Skill contributions in one runtime generation. Existing turn bindings retain
  their pinned catalog. Bundle Skills use the shared typed plugin contribution
  contract; selected sidecar Skill declarations must match prepared ids, bytes,
  and SHA-256 digests exactly.
- hya includes a selectable, non-default first-party
  `plan-impl-review` WorkflowBundle and an installable Argus-style example with
  investigation, planning, parallel implementation/review, and verification.
  Both use the same package, catalog, and execution paths as third-party
  bundles.
- `bundle list` and `bundle info` merge the immutable first-party bundle with
  installed rows and report payload kind, Workflow identity, and packaged Agent
  ids. Colliding installs fail before registry mutation. Release installation
  includes the production Compat adapter required by executable bundles.

## Durable control plane

- Event replay now reconstructs Workflow selection, revision, Stage
  transitions, child or resident Session bindings, progress, and terminal run
  state. Restart reconciliation marks abandoned nonterminal work interrupted
  without replaying Stage effects. Changed or missing selected sources remain
  visible as stale or unavailable until explicitly reselected.
- One app-owned controller serves CLI commands, the governed `workflow` Agent
  tool, native `/workflow` commands, HTTP routes, the Rust SDK, and in-process
  transport. Supported native forms are `list`, `info`, `use`, `run`, and
  `state`; headless CLI use/run/state can bind the same durable Session. Command
  handling does not enter model admission, and Workflow selection does not
  remove transcript messages.
- Project and user Workflow documents take precedence over installed bundle
  sources. Qualified bundle Workflow source ids remain exact and revisions also
  include the owning prepared bundle identity.

## Upgrade safety

- Back up each production Session database before starting 0.36.0. Workflow
  lifecycle Events and prepared bundle format v2 are a breaking cutover: older
  binaries cannot replay new Workflow Events, and bundles installed by an older
  binary must be reinstalled before they can publish again.

## TUI presentation

- The TypeScript TUI registers one hya-owned `sidebar_content` plugin for typed,
  synchronized Workflow state. It shows selection/revision availability,
  running or terminal status, graph level, declaration-ordered active Stages,
  bounded current work, Agent counts, and Stage progress.
- Bootstrap and existing `session.updated` invalidation drive the same state.
  The sidebar adds no polling loop, second SDK client, roster renderer, or local
  Workflow reducer, and it preserves compact and narrow terminal layouts.
