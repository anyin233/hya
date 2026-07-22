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
