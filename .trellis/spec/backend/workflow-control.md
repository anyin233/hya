# Durable Workflow Control

## Scenario: Event-Sourced Workflow Control Across Surfaces

### 1. Scope / Trigger

Use this contract when a change affects Workflow selection, run admission, Stage lifecycle Events, restart recovery, the Agent `workflow` tool, CLI commands, HTTP command routes, Session hydration, or SDK Workflow state.

The Session event log and `hya_proto::Projection` are the only durable Workflow read model. Catalog availability and live Agent activity are derived data. Do not add a Workflow table, a second reducer, or a surface-specific executor.

### 2. Signatures

The shared app seam is:

```rust
pub async fn WorkflowControl::execute(
    &self,
    session: SessionId,
    invocation: WorkflowInvocation,
    command: WorkflowCommand,
    cancel: CancellationToken,
) -> Result<WorkflowCommandResult, WorkflowControlError>;
```

`WorkflowCommand` has exactly five operations:

```rust
List
Info { name }
Select { name, expected_revision }
State
Run { name, expected_revision, inputs, run }
```

Durable store mutations are atomic writer transactions:

```rust
SessionStore::select_workflow(actor_claim, session, Event::WorkflowSelected { .. })
SessionStore::admit_workflow_run(actor_claim, session, Event::WorkflowRunStarted { .. })
SessionStore::claim_runtime_owner(owner)
SessionStore::recover_nonterminal_workflows(owner, reason)
```

The server uses one dependency-inverted port:

```rust
WorkflowControl::execute(session, command, delivery)
WorkflowControl::decorate(session, persisted_state)
WorkflowControl::active_run(session)
WorkflowControl::cancel(session)
```

### 3. Contracts

- `WorkflowSelected`, `WorkflowRunStarted`, `WorkflowStageStarted`, `WorkflowStageMemberLinked`, `WorkflowStageRouteOutcome`, `WorkflowStageFinished`, and `WorkflowRunFinished` are appended to the owning root Session.
- Events store typed identity, revision, plan metadata, status, Member references, and a request hash. Route outcomes store only bounded model/effort/class fields. They never store directives, input values, Stage output, provider text, credentials, or child transcripts.
- Projection applies Stage/member/route/terminal events only to the active run ID. Member links deduplicate `(member, role, iteration)`; route outcomes deduplicate `(stage, member, role, iteration, step)`. Terminal run and Stage states are sticky.
- Selection changes only `SessionProjection.workflow.selection`. It must preserve the complete message vector and canonical Member state.
- A model-tool invocation carries the original `ToolOperation`, actor claim, `TurnBinding`, and caller. `WorkflowRunId` derives from the operation. A direct caller may supply a stable run ID or let control mint one.
- Run admission fences the actor claim, compares a prior run ID/hash, rejects another active run, and appends `WorkflowRunStarted` in one `BEGIN IMMEDIATE` transaction. Event publication occurs only after commit.
- An explicit worker or verifier route is immutable request-local data. Preflight resolves its first routable candidate, validates effort capability and effective duplicates, and rejects different effective worker routes for one resident actor key before budget reservation or side effects. Stream groups start at the admitted index, advance only for safe pre-stream failures, and never replay an established stream.
- CLI and Agent tool use `WorkflowDelivery::Finished`. HTTP and slash commands use `Started`; terminal progress arrives through Events and state reads.
- Server Select/Run and parent-model admission reserve the same process-local `RunRegistry`. The reservation is held until app control installs its active Workflow claim. List/Info/State remain readable while a run is active.
- Startup must call `claim_runtime_owner` before Workflow recovery. A file-backed store holds an exclusive `0600` `<canonical-db>.runtime-owner.lock` until the final clone drops. The lock path rejects symbolic links. No heartbeat or TTL is used.
- Recovery requires the matching held owner claim. It appends one `Interrupted` terminal Event for each prior nonterminal run and never replays a Stage.
- `WorkflowProjection.availability` is runtime-only. Replay leaves it absent. Exact source ID + name + revision is `available`; an existing changed or invalid exact source is `stale`; a missing exact source is `unavailable`.
- Compat Session hydration calls the app decoration port once. SDK activity joins Workflow Member references to canonical Member projections and excludes unrelated run-tree members.
- Every Workflow lifecycle Event emits the normal `session.updated` invalidation and the raw envelope, in that order.

### 4. Validation & Error Matrix

| Condition | Stable code | HTTP status |
|---|---|---:|
| Invalid slash syntax | `WORKFLOW_SYNTAX` | 400 |
| Invalid Workflow source or inputs | `WORKFLOW_INVALID_SOURCE` / `WORKFLOW_INVALID_INPUT` | 422 |
| Missing Session/source/selection | `SESSION_NOT_FOUND` / `WORKFLOW_NOT_FOUND` / `WORKFLOW_NOT_SELECTED` | 404 |
| Unauthorized Stage or verifier Agent | `WORKFLOW_UNAUTHORIZED` | 403 |
| Busy Session, stale revision, or changed immutable operation request | `WORKFLOW_BUSY` / `WORKFLOW_STALE_REVISION` / `WORKFLOW_OPERATION_CONFLICT` | 409 |
| Runtime fingerprint unavailable | `WORKFLOW_RUNTIME_UNAVAILABLE` | 503 |
| Store or internal failure | `WORKFLOW_INTERNAL` | 500 |
| Another live process owns the database | `RUNTIME_OWNER_BUSY` | startup failure |
| Recovery without the matching claim | `RUNTIME_OWNER_CLAIM_REQUIRED` | startup failure |

A governed Stage failure is a successful transport response with a terminal failed run. Public and persisted error text is bounded to 2,048 Unicode scalar values. Syntax errors must not echo raw `key=value` input assignments.

### 5. Good / Base / Bad Cases

- Good: two independent SQLite handles submit the same run ID/hash. One appends the start; the other returns `Existing`; no second child starts.
- Good: a selected file changes. Session state reports `stale`, and Run fails until explicit reselection.
- Good: a backend crashes. The OS releases the owner lock; the next backend claims it, appends one `Interrupted` event, and starts no Stage.
- Base: selection with no run reports `available`; switching selection preserves every message ID and text byte.
- Bad: compare busy state outside the writer transaction, then append later.
- Bad: treat a different `OwnerRunId` as proof of death without holding the database owner lock.
- Bad: resolve a missing selected source by another same-name file.
- Bad: append a slash command transcript row before acquiring the shared Select/Run reservation.

### 6. Tests Required

- `hya-proto`: exact transcript preservation, sequence idempotency, stale-run filtering, member-link and route-outcome deduplication, old-wire omission, and terminal stickiness.
- `hya-store`: close/reopen equality including route plans/outcomes, cross-handle atomic admission, hash conflict, busy selection, actor fencing, owner-lock exclusion, symlink rejection, and exactly-once interruption.
- `hya-core`: member links appear before worker/verifier execution; durable actor-fenced Events use the owning root Session; Stage routes preserve effort/order/start index, pre-stream-only fallback, cancellation cardinality, and per-activation resident identity.
- `hya-app`: all five commands share one catalog; exact source/revision fencing; normal Agent model/category precedence before an explicit Stage override; operation-derived idempotency including historical route outcomes; Started/Finished behavior; availability decoration.
- `hya-tool` and backend CLI: all commands map to shared DTOs, retain `ToolOperation`, and use Finished delivery.
- `hya-server`: native/legacy/v2 typed and slash parity; zero parent-provider calls; shared admission race tests; hydration availability; structured error mapping; dual Event delivery.
- `hya-sdk` and `hya-native`: mirror conformance, canonical activity join, and structured non-2xx status/code/message/body preservation.

Run the focused gate listed in
`.trellis/tasks/archive/2026-08/08-28-durable-workflow-control/implement.md`
after a contract change.

### 7. Wrong vs Correct

#### Wrong

```rust
if projection.session.workflow_is_idle() {
    store.append_event(session, start).await?;
}

if persisted.owner != this_process {
    interrupt(persisted.run).await?;
}
```

This code has a writer race and treats owner inequality as liveness proof.

#### Correct

```rust
store.claim_runtime_owner(owner)?;
store.recover_nonterminal_workflows(owner, "backend startup recovery").await?;

match store.admit_workflow_run(actor_claim, session, start).await? {
    WorkflowAdmissionOutcome::Admitted(envelope) => publish_after_commit(envelope),
    WorkflowAdmissionOutcome::Existing => return projected_existing_run(),
    WorkflowAdmissionOutcome::Conflict => return operation_conflict(),
    WorkflowAdmissionOutcome::Busy { run } => return busy(run),
}
```

The lock proves exclusive runtime ownership for recovery. The writer transaction owns run idempotency and exclusion. The event log still owns projected Workflow state.
