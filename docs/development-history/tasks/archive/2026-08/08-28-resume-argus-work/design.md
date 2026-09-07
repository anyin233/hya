# Workflow Platform Completion Design

## Context

Argus left a tested transient Workflow DAG executor, but the implementation does not satisfy the original authoring, packaging, resident scheduling, durable state, control, or TUI requirements. This design keeps the useful governed execution work and replaces the narrowed public contract before release.

The design follows four fixed constraints:

1. Event replay is the only durable Session read model.
2. `SubagentGovernor`, governed Team execution, `IterationDriver`, and `ResidentSupervisor` remain the only scheduling mechanisms.
3. `AgentBundle` remains a one-Agent package. A Workflow is a different payload kind inside the same `.hyabundle` distribution path.
4. Every surface resolves one compiled Workflow revision and calls one app-owned control adapter.

## Module Map

```text
Workflow Markdown
    -> hya-workflow::compile
       -> CompiledWorkflow { definition, plan, revision }

project/user files ---------+
installed WorkflowBundles --+-> WorkflowCatalog snapshot
first-party WorkflowBundle -+
                                  |
                                  v
hya-app::WorkflowControl::execute(command, invocation)
    -> selection/run Events -> hya-proto::Projection
    -> hya-core::run_workflow(CompiledWorkflow)
         -> governed transient Team batches
         -> IterationDriver loop stages
         -> ResidentSupervisor mail activations

Projection
    -> hya-server Session DTO + session.updated
    -> hya-sdk typed mirror
    -> existing TypeScript SDK/Sync state
    -> built-in sidebar_content Workflow view
```

## Domain Decisions

- **Workflow**: one compiled, user-composed DAG plus metadata and input declarations.
- **Stage**: one graph-node activation. A Stage becomes eligible once per run.
- **Actor Key**: a Workflow-local name that routes sequential Stage activations to one resident Agent Session. It is not an Agent id.
- **WorkflowBundle**: one installable `.hyabundle` payload containing exactly one Workflow and its complete reachable Agent closure.
- **Workflow Identity**: name plus typed source identity and immutable revision.
- **Workflow Run**: one durable execution of one Workflow revision in one owning Session.

Actor keys are scoped to one Workflow run. A successful resident remains in the owning Team and can receive ordinary mail after the run, but a later Workflow run creates a new actor for the same key. This avoids accidental cross-run transcript inheritance.

## Authoring Module

### Deep Compiler Interface

Create a dependency-light `hya-workflow` crate. Both filesystem discovery and WorkflowBundle preparation need the same compiler, while `hya-core` already depends on `hya-bundle`; keeping the compiler in either crate would create a dependency cycle or duplicate validation.

```rust
pub fn compile(source: WorkflowSource<'_>)
    -> Result<CompiledWorkflow, WorkflowCompileError>;

pub struct CompiledWorkflow { /* private author model + normalized plan */ }

impl CompiledWorkflow {
    pub fn definition(&self) -> &WorkflowDefinition;
    pub fn plan(&self) -> &WorkflowPlan;
    pub fn revision(&self) -> WorkflowRevision;
}
```

The compiler owns frontmatter parsing, simplified Mermaid parsing, normalization, validation, topological planning, deterministic join order, and canonical revision hashing. Callers cannot construct an unvalidated `WorkflowPlan`.

`hya-workflow` depends only on `hya-proto` plus parsing/hash libraries. It has no engine, store, package, plugin, HTTP, or TUI dependency.

### Public Document

A Workflow is a Markdown document. YAML frontmatter contains metadata and node definitions. The body contains exactly one simplified directed flowchart.

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
    directive: |
      Plan {{input.request}}.
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

Supported graph syntax is intentionally small:

- The first non-comment line is `flowchart TD`.
- A standalone identifier declares an isolated node.
- `a --> b`, `a --> b & c`, and `a & b --> c` declare edges.
- `%%` starts a graph comment.
- Identifiers use the existing stable-id grammar.
- Labels, shapes, subgraphs, edge labels, style directives, and cycles are rejected with source line/column.

Every frontmatter node must occur in the graph and every graph node must have frontmatter. First graph occurrence defines deterministic node order. For a join, predecessor order is left-to-right within a line, then graph-line order across additional incoming edges.

All declared inputs are required. Only `{{input.<name>}}` interpolation is public. Unknown or missing inputs fail before authorization or scheduling. The compiler rejects old `stages:`/`needs:` documents; there is no compatibility parser.

### Normalized Join Contract

Edges carry evidence automatically. Before a downstream Stage directive is sent, the executor appends one `<workflow-upstream>` block containing only direct predecessors in compiled predecessor order. Each entry contains Stage id, Agent id, terminal status, and a UTF-8-safe output capped at 4,000 bytes. Failed evidence carries its typed failure status and bounded error text. Full prompts and child transcripts stay in their child Sessions.

This contract removes stage-output placeholders. A graph edge now has data-flow meaning without duplicating ancestors at every downstream node.

### Loop and Resident Declarations

A loop node uses the existing iteration contract:

```yaml
mode: loop
verify:
  agent: workflow-reviewer
  until: The implementation satisfies the request.
  max_iterations: 3
```

A resident activation declares an actor key:

```yaml
actor: planner
```

The resolved target Agent must have resident spawn lifecycle. A resident target without an actor key, an actor key on a transient target, one actor key bound to different Agent ids, or two same-level activations of one actor key fails preflight. Repeating an Agent id without `actor` remains multiple transient Sessions.

## Governed Runtime Module

`hya_core::run_workflow` remains the only DAG executor. It accepts only `CompiledWorkflow`; it does not parse or rebuild a plan.

### Level Execution

For each compiled level:

1. Resolve all worker and verifier targets through the caller's immutable `TurnBinding`.
2. Derive each target Agent's own roster, resource policy, sidecar factory, and spawn lifecycle.
3. Reserve the level's worst-case governed spawn/turn budget before the first effect.
4. Run transient members through one `run_pre_admitted_team` batch.
5. Spawn or wake resident actors through the existing `ResidentSupervisor`.
6. Await all same-level transient and resident activation futures concurrently.
7. Record bounded evidence in compiled Stage order.

The Workflow module never owns a provider semaphore, mail loop, actor task, or retry scheduler.

### Resident Activation

The DAG controls activations, not actor lifetime.

- First activation creates one parked resident through `ResidentSupervisor::spawn_resident` using the already resolved target context.
- First and later directives are appended through `SessionEngine::mail_send`.
- Subscribe to EventBus before append, then capture the recipient inbox length from Projection.
- Completion is authoritative only when Projection shows `resident_cursor` at or beyond that captured boundary, `resident_work == None`, and activity `Idle` or `Failed`.
- `Idle` produces Done evidence from the bounded last assistant output in that resident Session. `Failed` produces failed evidence.
- Mail coalescing, epochs, recovery, Team turn limits, and message budgets remain owned by `ResidentSupervisor`.

A run may finish while successful resident actors remain idle and addressable. It does not wait for unrelated Team quiescence or main-agent synthesis.

### Failure and Cancellation

- `fail_fast`: finish the admitted current level, mark every not-started Stage Skipped, and finish the run Failed.
- `collect_all`: continue every topologically eligible Stage, inject explicit failed evidence, then finish the run Failed if any Stage failed.
- Completed means every Stage succeeded.
- Workflow cancellation stops new admissions, cancels transient work through the existing Team token, waits for already-admitted resident activation boundaries to settle, marks remaining nodes Cancelled/Skipped, and finishes Cancelled. It never reports terminal while run-owned resident work remains active.
- Loop repetition stays inside `IterationDriver`; graph cycles remain invalid.

## Bundle and Plugin Modules

### Source and Prepared Types

Keep `AgentBundle` unchanged at the source level and add a second source kind:

```yaml
kind: WorkflowBundle
identity:
  id: hya/argus-example
  version: 1.0.0
  publisher: hya
workflow:
  id: argus
  path: workflows/argus.md
agents:
  - id: argus-planner
    role: subagent
    prompt: prompts/planner.md
  - id: argus-engineer
    role: subagent
    prompt: prompts/engineer.md
  - id: argus-reviewer
    role: subagent
    prompt: prompts/reviewer.md
```

Use separate `deny_unknown_fields` source structs selected by `kind`. Public AgentBundle archives retain `bundle.hya.md`. Public WorkflowBundle archives use the already supported explicit `bundle.yaml` manifest because a Markdown body cannot unambiguously be both an Agent prompt and package metadata.

Prepared format v2 uses a closed enum, not optional fields:

```rust
pub enum PreparedInstallableBundle {
    Agent(PreparedAgentBundle),
    Workflow(PreparedWorkflowBundle),
}
```

`PreparedWorkflowBundle` contains one authoritative Workflow source, all packaged Agents, resources, extensions, and canonical digests. The registry still stores one bundle-id row and needs no SQL schema migration.

Preparation compiles the Workflow through `hya-workflow`, verifies that its id matches the manifest, and verifies the Agent closure. Every Stage/verifier Agent must be packaged. Every non-built-in `can_spawn` reference reachable from those Agents must also be packaged. Unreachable extra Agents are rejected. Built-in host Agent references, if allowed by the existing authorization contract, are explicit and version-bound; first-party examples package their own Agents.

Prepared format v1 registry rows become `unreadable (reinstall)` under v2. Source AgentBundles remain valid and re-prepare without source changes. No v1 decode shim is added.

### Catalog

`BundleCatalog` indexes all Agents from both payload kinds plus Workflow entries. Stable Workflow ids are `bundle:<bundle-id>/workflow/<workflow-id>`. Agent origins remain `AgentOrigin::Bundle`, so every packaged Stage gets the existing owner-scoped resources and sidecar.

The immutable `WorkflowCatalog` merges sources in this precedence:

1. project Workflow files;
2. user Workflow files;
3. installed WorkflowBundles;
4. the read-only first-party WorkflowBundle.

Qualified source ids always resolve exactly. Project/user sources may shadow a bare name. Installed and first-party bundle Workflow names must remain unambiguous.

A filesystem revision is a domain-separated hash of the canonical compiled definition. A bundle revision folds both that compiler revision and the owning prepared-bundle digest, so changed prompts, Agents, Skills, tools, or hooks make a selected revision stale.

### One Plugin Contribution Interface

Deepen the existing plugin initialization seam with a typed `PluginContributionSet` containing tools, Skills, hooks, and workspace adapters.

- External and Compat plugin hosts produce this set from protocol initialization.
- An in-process prepared-bundle adapter produces the same set for static Skills.
- Executable bundle tools/hooks still use the existing sidecar and `tool/call` protocol.
- A Skill contribution carries bounded content plus digest. For a prepared bundle, hya-app requires exact selected id/digest/content equality with signed prepared bytes before publication.
- Missing, extra, duplicate, or wrong-digest declarations fail before model execution.
- Old plugins default to zero Skills. A bundle selecting Skills cannot silently bypass a host that did not declare them.

`hya-core` stops parsing bundled Skills directly. `RuntimeReconciler` accepts only the shared contribution set, giving plugin and bundle paths one real seam.

### Compat Adapter Delivery

Build a deterministic production Compat adapter artifact and install it under the adjacent `lib/hya` tree. Resolver precedence is explicit environment override, installed adjacent artifact, then workspace source for development. Release archives and `install.sh` include it in atomic staging/rollback. A smoke test starts a real bundle sidecar from outside the repository with no environment override.

### First-Party and Example Bundles

- `hya/plan-impl-review`: a prepared WorkflowBundle merged into the ordinary first-party catalog. It is selectable out of the box but never selected automatically. No executor branch knows its graph.
- `hya/argus-example`: a full installable WorkflowBundle shipped under release examples. It contains investigate, plan, parallel execute, multi-perspective review, synthesis, and verification Agents using ordinary graph/actor/loop contracts.

Editable sources remain in the repository. A deterministic hya-bundle writer creates public `.hyabundle` bytes for tests and release packaging. Drift tests regenerate and compare artifacts. The full example is not installed automatically.

## Durable Workflow State

### Shared Types and Events

Add `WorkflowRunId`, `WorkflowSourceId`, `WorkflowRevision`, Workflow identity/status types, and Projection DTOs to `hya-proto`.

Durable Events in the owning root Session log:

- `WorkflowSelected { session, workflow }`
- `WorkflowRunStarted { session, run, workflow, request_hash, owner, stages }`
- `WorkflowStageStarted { session, run, stage }`
- `WorkflowStageMemberLinked { session, run, stage, member, role, iteration }`
- `WorkflowStageFinished { session, run, stage, status }`
- `WorkflowRunFinished { session, run, status, error }`

`WorkflowRunStarted.stages` stores stable display/provenance data only: Stage id/title, Agent id, mode, and level. Events do not store directives, input values, Stage outputs, or child transcripts. `request_hash` covers source revision, caller, sorted input pairs, and bound runtime semantics without exposing values.

### Projection Rules

`SessionProjection` gains `workflow: Option<WorkflowProjection>`.

- Selection is last-write-wins and never mutates messages or members.
- Run start selects the same identity and creates declaration-ordered Pending stages.
- Stage/member events apply only to the active run id.
- Member links deduplicate `(member, role, iteration)`.
- Terminal run state is sticky; late events cannot reopen it.
- Failed/cancelled/interrupted runs mark remaining Pending stages Skipped.
- Canonical Member/child Session projection remains the authority for Agent activity, summaries, and transcript.
- Running Agent counts and current work are derived by joining Workflow member references to canonical Members; data is not duplicated in Workflow events.

Envelope sequence idempotency remains the existing complete reducer guard.

### Recovery

`WorkflowRunStarted.owner` records the existing runtime owner/fence identity. Before serving Workflow state or admitting a new mutating command, `WorkflowControl` reconciles a nonterminal run whose owner is provably dead to Interrupted and appends one terminal Event. It does not replay any Stage. Multi-process stores use the existing owner claim instead of assuming another process is dead.

A selected source that is missing or has a different current revision remains in Projection. The server decorates it as `unavailable` or `stale`; run fails until explicit reselection. It never substitutes a same-name source.

## App Control Module

`hya-app::WorkflowControl` is the composition seam because hya-app owns config, catalogs, binding, installed bundles, engine, store, and resident supervisor.

```rust
pub async fn execute(
    &self,
    session: SessionId,
    invocation: WorkflowInvocation,
    command: WorkflowCommand,
    cancel: CancellationToken,
) -> Result<WorkflowCommandResult, WorkflowControlError>;
```

Commands are `List`, `Info`, `Select`, `State`, and `Run`. Run carries name/revision, input map, stable run id, and delivery `Started | Finished`.

- CLI and Agent tool request `Finished`.
- HTTP and slash commands request `Started`; progress and terminal state arrive through Events.
- A tool invocation preserves its existing `ToolOperation` and actor claim. `WorkflowRunId` is derived deterministically from that operation.
- A direct request accepts an idempotency id or mints one.
- Same run id and request hash returns projected state without rerun; same id with another hash is conflict; an active match is busy.
- List/info are read-only. Select/run use existing per-Session run exclusion.
- Only this module resolves catalogs, revisions, stale state, input errors, and interrupted recovery. Only `hya_core::run_workflow` executes the plan.

Stable errors map invalid syntax/input to 400/422, missing source to 404, unauthorized target to 403, busy/id conflict to 409, unavailable runtime to 503, and store/internal failure to 500. A governed Stage failure is a successful transport response with a terminal failed run.

## CLI, Tool, Server, and SDK

- Backend `workflow list|info|run` calls `WorkflowControl`; it no longer loads or validates files itself.
- The Agent `workflow` tool supports list/info/select/run/state and preserves the bound turn operation. It awaits terminal run results.
- Native `/workflow list`, `/workflow info <name>`, `/workflow use <name>`, `/workflow run [name] [key=value ...]`, and `/workflow state` are parsed before model admission.
- Native, legacy Compat, and Compat v2 command routes share one parser/decision. Unrelated slash commands keep their existing path.
- Typed Workflow command/state endpoints are mounted on the native and existing Compat route families and return the same shared DTOs.
- Slash commands append their normal command/result transcript rows but never invoke the parent model. Typed select calls need not append transcript rows.
- Session GET/list/bootstrap include projected Workflow state. Workflow Events emit the existing `session.updated` invalidation plus the raw envelope.
- `hya-sdk` mirrors typed Workflow DTOs, exposes the command/state method over its existing `Transport`, joins member references for activity, and preserves structured non-2xx error bodies. It adds no HTTP client and no timeline reducer.

## TypeScript TUI

The operator explicitly requires sidebar presentation. Implement one hya-owned built-in `sidebar_content` plugin, not a roster sidebar, status line, second DAG, or poller.

The existing SDKProvider and SyncProvider remain the only transport/state owners. Initial bootstrap/Session hydration supplies Workflow state; existing `session.updated` events replace it. Child Agents continue to appear in the existing run-tree and observation panes.

The sidebar view shows:

- selected Workflow and current/stale/unavailable revision state;
- `Ready`, `Running`, `Completed`, `Failed`, `Cancelled`, or `Interrupted`;
- active/total unique Agent instances;
- completed/total Stages and graph level;
- all currently running Stage ids at normal width, with deterministic compact `first +N` fallback;
- current Stage title/work, truncated last;
- failed Stage when terminal failure needs context.

No selection renders a muted `Workflow: none`. The view uses existing semantic theme fields, sidebar width, feature-plugin ordering, and cleanup. Narrow terminals keep existing sidebar visibility behavior; no duplicate footer is added. `/workflow` continues through the existing server command catalog/prompt path, so the TUI does not add a second command parser or model client.

## Compatibility, Versioning, and Rollback

1. First verify and commit the recovered predecessor tree as release `0.35.2`. Correct stale provider and Workflow claims before that commit.
2. Complete this clean cutover as `0.36.0` because author syntax, prepared bundle format, Events, and public DTOs are breaking.
3. Archive root `CHANGELOG.md` to `docs/changes/CHANGELOG_0.35.2.md`, then write only `0.36.0` notes at root. Align Cargo workspace, lockfile, and TUI package versions.
4. Existing AgentBundle source remains valid. Installed prepared-v1 rows require reinstall. The unpublished old Workflow author format has no fallback.
5. No release tag or GitHub Release is created in this task.

Rollback of binaries is safe only before a `0.36.0` Event is written to a production Session store. Old binaries do not understand new Event variants or prepared-v2 rows. Verification uses isolated stores; deployment notes require a store backup before upgrade. No SQL migration is required.

## Performance and Security

- Compile once per source revision and cache in immutable runtime snapshots; do not parse per Stage.
- Keep plans and Projection state declaration-ordered to avoid repeated sorts.
- Bound all persisted/display errors and joined evidence. Never persist input values or complete outputs in Workflow Events.
- Resolve and authorize every worker/verifier before any spawn or mail.
- Reuse package path containment, digest verification, exact closure checks, and plugin declaration equality.
- Reuse governor concurrency/spawn/turn/message limits. User graphs cannot create a new semaphore or bypass admission.
- Preserve no-replay provider semantics after a stream exists.

## Verification Design

Tests cross public module interfaces and fail under plausible mutations:

1. Compiler tests pin grammar, source locations, clean old-format rejection, graph order, automatic joins, inputs, cycles, and actor constraints.
2. Runtime tests prove actual fan-out overlap, bounded ordered evidence, collect-all failure, transient same-Agent separation, one resident Session with two mail boundaries, and cancellation truthfulness.
3. Bundle tests prove deterministic prepared-v2/package bytes, one-Agent AgentBundle preservation, Workflow Agent closure, catalog resolution, registry atomicity, plugin Skill/tool equality, and external installed Compat adapter startup.
4. Projection/store tests prove transcript-preserving selection, replay equality, member-link deduplication, sticky terminal state, stale old-run no-op, restart interruption, and no side-effect replay.
5. App/server/SDK tests prove one control path, stable operation idempotency, no parent-model call for `/workflow`, typed errors, Session busy rules, and native/Compat parity.
6. TUI tests prove event-driven sidebar state, counts based on Workflow member references rather than unrelated run-tree members, terminal/stale rendering, plugin cleanup, and no polling/second client.
7. Process tests install and run the first-party and Argus bundles with real backend/FakeLlm paths.
8. Final proof includes focused red/green logs, mutation checks, Rust workspace gates, Track P where applicable, Bun TUI gates, a built executable smoke, and browser/terminal visual verification of the actual TUI.

## Rejected Alternatives

- **Make AgentBundle a union:** repeals ADR-0012 and makes every singular Agent caller conditional. WorkflowBundle is a distinct payload behind the same distribution interface.
- **Keep YAML `stages`/`needs`:** contradicts the recovered Mermaid-inspired requirement and leaves edges without automatic data-flow meaning.
- **Infer resident reuse from Agent id:** breaks valid fan-out where several transient workers use the same Agent definition.
- **Add a Workflow scheduler:** duplicates resident mail, Team batching, governor, and recovery behavior.
- **Persist full Workflow prompts/outputs:** duplicates child Session authority and leaks inputs/content into lifecycle Events.
- **Build TUI state from run-tree polling:** counts unrelated Agents, loses selection on restart, and creates a parallel read model.
- **Render only in the prompt footer:** conflicts with the operator's explicit sidebar requirement.
