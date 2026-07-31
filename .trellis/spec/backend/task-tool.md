# Task Tool Contract

## Scenario: Single And Batch Input Validation

### 1. Scope / Trigger

- Trigger: changes to the `task` tool schema, resume IDs, member normalization,
  or `SpawnerPlane` dispatch.

### 2. Signatures

- Single mode uses top-level `description`, `prompt`, `subagent_type`, and an
  optional `task_id` parsed by the shared `SessionId` parser.
- A non-empty `members` array selects batch mode. Each listed member supplies
  its own description, prompt, and subagent type.

### 3. Contracts

- `members.is_empty()` selects single mode; validate and forward top-level
  `task_id` only in this branch.
- Non-empty `members` selects batch mode; top-level `task_id` is unused and must
  not block dispatch. Every normalized batch member has `task_id: None`.
- Validate compatibility-shaped fields only when the selected mode consumes
  them. Do not globally normalize malformed resume IDs into new tasks.

### 4. Validation & Error Matrix

- Single mode with malformed `task_id` -> `ToolError::Input("invalid task_id: ...")`.
- Single mode with missing required top-level fields -> input error before spawn.
- Batch mode with any top-level `task_id` -> ignore it and validate the members.
- Background mode with more than one normalized member -> input error.

### 5. Good / Base / Bad Cases

- Good: two members plus `task_id: ""` reach `SpawnerPlane`; both member IDs are
  `None`.
- Base: omitted `task_id` creates a new single task; a valid ID resumes one.
- Bad: parsing top-level `task_id` before checking whether `members` selected
  batch mode.

### 6. Tests Required

- A `TaskTool` integration test must capture the batch request at `SpawnerPlane`
  and assert every member ID is `None`.
- Keep coverage for valid and malformed single-mode resume IDs.
- Run `cargo test -p hya-tool --test task` after changing this contract.

### 7. Wrong vs Correct

#### Wrong

```rust
if let Some(task_id) = task_id.as_deref() {
    task_id
        .parse::<SessionId>()
        .map_err(|e| ToolError::Input(format!("invalid task_id: {e}")))?;
}
// Batch members are built afterward and discard the top-level task ID.
```

#### Correct

```rust
if members.is_empty() {
    if let Some(task_id) = task_id.as_deref() {
        task_id
            .parse::<SessionId>()
            .map_err(|e| ToolError::Input(format!("invalid task_id: {e}")))?;
    }
}
```

## Scenario: Durable Spawn Admission

### 1. Scope / Trigger

- Trigger: changes to `ToolCtx`, `SpawnerPlane`, task dispatch, subagent
  governor reservation, the admission journal, startup, or resident/transient
  spawn supervision.
- This is the `0.34.4` control-plane contract. It is not a durable runnable
  queue and does not promise external-effect replay.

### 2. Signatures

- Every persisted `ToolCallId` deterministically derives one domain-separated
  `OperationId`. `ToolCtx` and `SpawnRequest` carry the immutable pair through
  `ToolOperation`; no independent operation UUID is minted.
- `SpawnerPlane::with_capacity(capacity: usize)` returns a bounded Tokio
  channel-backed plane and receiver.
- Foreground and background spawn use non-blocking `try_send`.
- Full transport returns `SpawnError::Overloaded`; closed transport returns
  `SpawnError::Unavailable`.
- A reused operation with a different immutable request returns
  `OPERATION_ID_CONFLICT`; an identical already-started or terminal request
  returns `OperationAlreadyHandled` and never dispatches again.
- The store journal owns `accepted -> started ->
  completed|cancelled|aborted`; the governor owns only the current process's
  in-memory debit and cancellation token.

### 3. Contracts

- Runtime queue capacity derives from the existing resolved
  `SubagentLimits::per_run_budget`, clamped to
  `1..=tokio::sync::Semaphore::MAX_PERMITS` before constructing the channel.
  Do not introduce `100`, `128`, or `256` as queue defaults.
- Queue full fails immediately. The rejected request never enters the
  supervisor, so it cannot create a request-owned task, child Session, or child
  event.
- The first durable write records operation ID, source tool-call ID, root
  session, SHA-256 request fingerprint, admission units, and `accepted` before
  any governor debit, child/session creation, resident registration, or
  dispatch.
- `accepted` means no capacity was charged. Only the request that atomically
  debits the governor may transition to `started`; all transient/resident and
  foreground/background continuations then use the pre-admitted execution
  path and must not reserve again.
- Same operation plus the same immutable request observes existing state
  without another debit. A changed parent/background/member request fails
  closed without mutation.
- Terminal states are irreversible. Replaying the same terminal is idempotent;
  attempting a different terminal returns a typed transition conflict.
- Finalization marks a logical release only when the row was `started`.
  Governor release is keyed by operation and removes/credits at most one
  recorded debit. An `accepted` overload/cancel never credits capacity.
- Explicit cancellation, normal completion, spawn failure, and root-turn
  cleanup call the same store-first finalizer. Root cleanup cancels current
  operation tokens before finalizing them.
- Before building any spawn/resident supervisor, startup atomically moves all
  `accepted`/`started` rows to `aborted`. A recovered `started` row records a
  logical release for audit only; it never credits the fresh governor.
- The journal emits no public `Event`; session replay/projection remains
  exclusively event-log based.
- A rejected child may still produce the normal parent Turn's typed tool-error
  event. “No event” applies to rejected child/session/member/roster state.

### 4. Validation & Error Matrix

- Queue has capacity -> request enters the supervisor.
- Queue is full -> typed overload, immediate return, no enqueued request.
- Queue receiver is closed -> typed unavailable.
- Request exceeds depth or exact remaining run budget -> durable `aborted`,
  typed
  overload before child allocation.
- Identical duplicate -> existing state, no debit, task, session, event, or
  dispatch.
- Different request fingerprint -> `OPERATION_ID_CONFLICT`, no mutation.
- Cancel before debit -> `cancelled`, `logical_released = false`.
- Complete/cancel/abort after `started` -> one logical release and at most one
  governor release.
- Restart with nonterminal rows -> all become `aborted` before spawn readiness;
  repeated recovery changes zero rows.

### 5. Good / Base / Bad Cases

- Good: one queued request occupies a capacity-one transport; a second
  background task immediately returns typed overload and never reaches the
  receiver.
- Base: one operation claims, debits, starts, creates once, completes, and
  releases its exact debit; a serial or concurrent retry observes existing
  state.
- Bad: enqueue on an unbounded channel and create a Tokio task/child Session
  before checking the governor.
- Bad: derive an operation ID from randomness/process state, insert the journal
  row after child creation, use a terminal replay to credit twice, or resume a
  nonterminal operation after restart.

### 6. Tests Required

- `hya-tool` unit: fill a capacity-one `SpawnerPlane`; assert the next request
  fails fast with `SpawnError::Overloaded` and receiver length stays one.
- `hya-tool` unit: assert an explicitly bound plane whose receiver is closed
  returns `SpawnError::Unavailable`, and an extreme requested capacity is
  clamped without panicking.
- `hya-tool/tests/task.rs`: assert `TaskTool` returns
  `ToolError::Overloaded`, preserves the persisted tool-call operation pair,
  and exposes typed operation conflict/already-handled failures.
- `hya-store/tests/admission.rs`: serial/concurrent claim/start idempotency,
  fingerprint conflict, terminal immutability, release marker semantics,
  repeatable startup recovery, and event-log independence.
- `hya-core` unit: exactly-once operation debit/release, cancellation before
  debit, and repeated root cleanup.
- `hya-app/tests/spawn_admission.rs`: transient/resident overload creates no
  child or lifecycle state; duplicate and concurrent retries dispatch once;
  changed request conflicts; foreground/background completion releases once.
- Keep queue saturation, nested spawn, event replay, and projection suites
  green.

### 7. Wrong vs Correct

#### Wrong

```rust
let child = engine.create(create).await?;
store.claim(operation, fingerprint).await?;
governor.reserve(root, units);
```

#### Correct

```rust
match store.claim_admission(&claim).await? {
    Claimed(_) => {
        governor.try_reserve_operation(root, operation, units, cancel)?;
        store.start_admission(operation).await?;
        run_pre_admitted_team(...).await;
        finalize(operation, Completed).await?;
    }
    Existing(_) => return Err(OperationAlreadyHandled),
}
```
