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
- An explicit worker or verifier route is immutable request-local data. Preflight resolves its first routable candidate, validates effort capability and effective duplicates, and rejects different effective worker routes for one resident actor key before live-capacity preflight or side effects. Stream groups start at the admitted index, advance only for safe pre-stream failures, and never replay an established stream.
- Agent tool requests execute concurrently under the built runtime task owner; per-session durable admission and the governor retain run exclusion and resource limits. List/Info/State requests for either the running Session or another Session do not wait for run completion. Started runs share the same owner and are cancelled and joined on runtime shutdown.
- CLI and Agent tool use `WorkflowDelivery::Finished`. HTTP and slash commands use `Started`; terminal progress arrives through Events and state reads.
- Server Select/Run and parent-model admission reserve the same process-local `RunRegistry`. The reservation is held until app control installs its active Workflow claim. List/Info/State remain readable while a run is active.
- Startup must call `claim_runtime_owner` before Workflow recovery. A file-backed store holds an exclusive `0600` `<canonical-db>.runtime-owner.lock` until the final clone drops. The lock path rejects symbolic links. No heartbeat or TTL is used.
- Recovery requires the matching held owner claim. It appends one `Interrupted` terminal Event for each prior nonterminal run and never replays a Stage.
- `WorkflowProjection.availability` is runtime-only. Replay leaves it absent. Exact source ID + name + revision is `available`; an existing changed or invalid exact source is `stale`; a missing exact source is `unavailable`.
- Replayed-state decoration calls the app decoration port once (the former Compat Session-hydration caller is deleted; v1 state reads go through the same control handle). SDK activity joins Workflow Member references to canonical Member projections and excludes unrelated run-tree members.
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
| Workflow live capacity exhausted | `WORKFLOW_OVERLOADED` | 503 |
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
- Client-side mirror conformance, canonical activity join, and structured
  non-2xx status/code/message/body preservation were covered by the removed
  legacy SDK/transport suites; that coverage returns with a `hya-sdk-v1`
  frontend.

After a contract change, run the tests listed above for each affected crate.

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

### Bounded tool admission and runtime shutdown

The `workflow` tool queue and concurrent request admission share the existing
`subagents.per_run_budget: u64` capacity (after channel capacity clamping).
There is no additional setting. For example, `subagents: { per_run_budget: 1 }`
allows one catalog/admission handler at a time. A saturated queue or handler
pool returns `ToolError::Overloaded("workflow host queue is full; retry shortly")`.
The host reply represents handler saturation as
`WorkflowHostError { code: "WORKFLOW_OVERLOADED", message: string }`; the tool
plane maps it to the same overload result as a full channel.

A Run releases its request slot after durable admission, before waiting for its
Finished result. Admitted runs remain tracked and governed by the existing
per-session claim and resident budgets; List and State can proceed while a run
executes. Catalog filesystem walks and compilation use tracked blocking tasks,
so they do not block Tokio runtime workers. Direct control calls retain their
existing admission rules; the tool queue capacity applies to tool requests.

All public `WorkflowControl` entry points (`execute`, `list`, `decorate`) take
a runtime tracker token before checking cancellation. Once shutdown returns,
they reject new work with `WORKFLOW_RUNTIME_UNAVAILABLE`. Work admitted before
shutdown remains tracked through any Started run handoff and terminal write.

### Stage capacity and unsuccessful run cleanup

Workflow preflight does not reserve slots. A graph whose worst-case activation
count exceeds `subagents.per_run_budget: u64` is invalid source. When that graph
fits but current live claims leave too few slots, preflight returns
`WORKFLOW_OVERLOADED`. Registration and revival recheck capacity transactionally;
mid-run refusals use the same code (HTTP 503, gRPC `ResourceExhausted`,
`ToolError::Overloaded`). For example, with budget 2, a two-Stage graph can pass
preflight and then be refused if a concurrent `task` occupies one slot.

On an error, cancellation, or failed run, the controller waits for the normal
ending of every actor it registered, including parked actors awaiting
activation. Claims belonging to concurrent tasks are untouched. Successful
runs retain their parked actors for subsequent activation.

Loop continuations reactivate the same worker through `activate` / its
`ActivationWaiter`. Settlement retains its identity, runtime and capacity claim;
a report releases the claim, so continuation revives it with the fenced,
capacity-checked `revive_episode` transaction. Each verifier judgment registers
a fresh parked resident, activates it, consumes successful settlement or a Done
report, then awaits `end_member` on every result path. Loop and verifier capacity
refusals keep `WORKFLOW_OVERLOADED`.

Parked registration returns the committed identity after episode installation,
before initializer hooks finish. Its episode still completes initialization
before any turn. This lets the controller cancel an activation while a hook is
blocked. Ordinary task registration retains its initialization wait. The
iteration driver does not race and drop a pending Workflow judgment: worker and
verifier activation controllers observe the run token and finish their owned
cleanup before returning. No extra executor task or wire API is introduced.

### Episode activation contract (core-internal)

`ResidentSupervisor::activate(parent: SessionId, member: SessionId,
directive: String, guidance: Arc<str>, route: Option<WorkflowTurnRoute>,
parent_claim: Option<&ActorClaim>) -> Result<ActivationWaiter, CoreError>`
configures one idle member and sends its directive on the existing parent-child
DM channel, or revives an archived member in the fenced capacity-checked
transaction. Blank directives are rejected before leasing or mail effects.
Live/archive selection is serialized with endings by the claim-write boundary.
The existing turn lease
prevents any mail wake from starting before guidance, route and the post-mail
inbox boundary and pending work have been installed, before bus delivery.
A busy, stopped, ending, or already armed
member is refused without arming mail. No SQL connection spans a non-store
await.

Run cleanup captures live episode identities before any await, then ends
descendants deepest-first and joins the parent ending. Root deletion can remove
the roster while these ending waiters are pending without invalidating them.

`ActivationWaiter::wait(&mut self) -> Result<ActivationResult, CoreError>` is
cancel-safe: dropping a pending wait does not end the episode. Its result is
one of `Settled { final_text: String }`,
`Reported { outcome: ReportOutcome, text: String }`, or
`Ended { reason: ArchiveReason, status: MemberRunStatus, note: String }`.
The episode sends `Settled` only after the durable work-finished/idle commit
covers the boundary, at the Idle decision with no pending work, unread incoming
mail or live child. A trailing empty wake still reaches this decision without
starting a model turn. The route remains installed across follow-up turns and
is cleared only at true rest. Accepted reports and requested endings own
completion before settlement.
Its own ending sends `Reported`/`Ended` after terminal publication and claim
release. A terminal-write failure is returned as `CoreError`.

For `plan --> refine` sharing one actor, the controller awaits each activation
and supplies the next directive and rendered guidance. The Stage controller
accepts both `Settled` and `Reported(Done)` as success; other reports/endings
fail it, while requested run cancellation returns Cancelled. The episode drops
request-local route recorders before resolving settlement and on ending, so a
parked actor cannot retain a completed run's route sender. These types remain
crate-visible; no wire or plugin schema is added by the activation API.

Deadline notices use the current `CompletionRequest.tools: Vec<ToolSchema>`
to select their completion instruction. A child receives “call report” only
when the `report` schema is offered; otherwise it receives the normal turn
completion notice. For example, a core Workflow without a governor hides
report at depth zero and completes through `Settled.final_text`. No new
configuration, tool or wire contract is required.

### Ready scheduling

Execution uses unfinished-predecessor counts, not topological levels. On a
successful Stage result, ready successors can activate immediately; that Stage's
verifier runs without waiting for unrelated workers. Results occupy compiled
Stage indices, and `render_stage` visits compiled predecessor indices, so join
evidence order never depends on completion time. Ready uses of the same actor
serialize in compiled Stage order until its activation reaches true rest.

`fail_fast` drains activated Stages and stops new admission after failure is
observed. `collect_all` continues independent branches. Both skip successors of
failed/skipped Stages. Cancellation stops admission and drains Stage futures
before ending all registered run-owned actors under `RootDriverActivity`.
Public plan/state `level: usize` fields remain topological display metadata;
no wire schema or configuration changes.

Finished delivery runs on the existing runtime task tracker too. Dropping the
waiting caller cancels its run token; it does not drop the execution future.
The task retains admission ownership through actor cleanup and the terminal
`WorkflowRunFinished` write. Root-driver activity also covers that write. Started
delivery transfers ownership to its background task on successful return.
