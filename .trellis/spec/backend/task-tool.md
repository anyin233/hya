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

## Scenario: Bounded Background Spawn Admission

### 1. Scope / Trigger

- Trigger: changes to `SpawnerPlane`, background `task` dispatch, subagent
  governor reservation, or resident/transient spawn supervision.
- This is the `0.34.3` in-memory contract. Durable queueing, cancellation
  refund, and recovery are separate future work.

### 2. Signatures

- `SpawnerPlane::with_capacity(capacity: usize)` returns a bounded Tokio
  channel-backed plane and receiver.
- `SpawnerPlane::spawn_background(...)` uses non-blocking `try_send`.
- Full transport returns `SpawnError::Overloaded`; closed transport returns
  `SpawnError::Unavailable`.
- `TaskTool` preserves overload as `ToolError::Overloaded`, and the engine
  serializes that typed tool failure as `"overloaded"`.
- `pre_admit_team(engine, parent, member_count)` makes one all-or-none
  depth/per-run-budget decision for a background request.

### 3. Contracts

- Runtime queue capacity derives from the existing resolved
  `SubagentLimits::per_run_budget`, clamped to
  `1..=tokio::sync::Semaphore::MAX_PERMITS` before constructing the channel.
  Do not introduce `100`, `128`, or `256` as queue defaults.
- Queue full fails immediately. The rejected request never enters the
  supervisor, so it cannot create a request-owned task, child Session, or child
  event.
- Background transient and resident requests use the same pre-admission
  boundary before the request-owned Tokio task,
  `SessionEngine::create`, `ResidentSupervisor::ensure_main`, or
  `spawn_resident`.
- A pre-admitted transient continuation must not call the governor reserve a
  second time.
- Foreground batch partial-grant/depth evidence remains unchanged.
- A rejected child may still produce the normal parent Turn's typed tool-error
  event. “No event” applies to rejected child/session/member/roster state.

### 4. Validation & Error Matrix

- Queue has capacity -> request enters the supervisor.
- Queue is full -> typed overload, immediate return, no enqueued request.
- Queue receiver is closed -> typed unavailable.
- Background request exceeds depth or exact remaining run budget -> typed
  overload before child allocation.
- Exact reservation cannot grant the whole request -> grant none; a later
  fitting request still sees the unconsumed budget.
- Admitted background transient -> child creation and execution proceed once;
  budget is charged once.
- Background resident denial -> no main/resident registration or child state.

### 5. Good / Base / Bad Cases

- Good: one queued request occupies a capacity-one transport; a second
  background task immediately returns typed overload and never reaches the
  receiver.
- Base: one background transient fits the run budget, creates one child, and a
  later request after budget exhaustion returns overload.
- Bad: enqueue on an unbounded channel and create a Tokio task/child Session
  before checking the governor.
- Bad: pre-admit a background transient and then call the normal reserving
  `run_team` path, consuming the run budget twice.

### 6. Tests Required

- `hya-tool` unit: fill a capacity-one `SpawnerPlane`; assert the next request
  fails fast with `SpawnError::Overloaded` and receiver length stays one.
- `hya-tool` unit: assert an explicitly bound plane whose receiver is closed
  returns `SpawnError::Unavailable`, and an extreme requested capacity is
  clamped without panicking.
- `hya-tool/tests/task.rs`: assert `TaskTool` returns
  `ToolError::Overloaded` without rewriting it as unavailable or input error.
- `hya-core` unit: assert exact reservation is all-or-none.
- `hya-app/tests/spawn_admission.rs`: zero-budget background transient and
  resident requests return overload with no child Session, parent
  event/projection, resident-supervisor, or provider-call delta.
- The same integration suite must prove one admitted background transient is
  counted exactly once and one admitted resident reaches registration and its
  provider turn through the shared boundary.
- Keep `nested_spawn_tree` and foreground subagent suites green to protect
  existing foreground semantics.

### 7. Wrong vs Correct

#### Wrong

```rust
let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
tokio::spawn(async move {
    let child = engine.create(create).await?;
    let _ = governor.reserve(root, 1);
    run_child(child).await
});
```

#### Correct

```rust
tx.try_send(request).map_err(|error| match error {
    tokio::sync::mpsc::error::TrySendError::Full(_) => SpawnError::Overloaded,
    tokio::sync::mpsc::error::TrySendError::Closed(_) => SpawnError::Unavailable,
})?;

pre_admit_team(engine.as_ref(), parent, member_count).await?;
tokio::spawn(run_pre_admitted_background(...));
```
