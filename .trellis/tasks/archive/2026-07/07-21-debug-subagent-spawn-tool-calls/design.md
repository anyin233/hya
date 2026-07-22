# Design: Ignore unused batch task IDs

## Background

`TaskTool::execute` accepts two compatibility-shaped input modes through one JSON object:

- When `members` is empty, top-level `description`, `prompt`, `subagent_type`, and optional `task_id` define one member.
- When `members` is non-empty, the listed members define the batch and top-level member fields are ignored.

The current implementation parses every present top-level `task_id` before selecting a mode. This rejects model placeholders such as `""` and `"new"` even in batch mode, although batch construction always sets every member's `task_id` to `None`. Persisted events prove that replacing only the placeholder with a syntactically valid UUID allows the otherwise equivalent batch to dispatch.

## Goals

- Ignore the unused top-level `task_id` whenever `members` is non-empty.
- Preserve strict validation and forwarding of top-level `task_id` in single-member mode.
- Add one deterministic regression test at the `TaskTool` to `SpawnerPlane` boundary.
- Release the fix as `0.33.19` with repository metadata aligned.

## Non-goals

- Change the model-facing task schema or provider serialization.
- Normalize empty strings, `"new"`, or malformed IDs globally.
- Add per-member resume IDs or change `SpawnMember.task_id`.
- Refactor the compatibility input into enums, `oneOf`, custom deserialization, or separate tools.
- Change sibling model, category, prompt, or inline-agent resolution.
- Add a nondeterministic live-model regression test.

## Decision

Move the existing `SessionId` parse block unchanged into the start of the `members.is_empty()` branch, before that branch validates its required top-level fields. No helper or type change is needed.

The resulting behavior is:

| Input mode | Top-level `task_id` | Result |
| --- | --- | --- |
| `members` empty | omitted | Create a new single task. |
| `members` empty | valid session ID | Forward the resume ID. |
| `members` empty | malformed | Return `invalid task_id`. |
| `members` non-empty | any value | Ignore it; every batch member receives `None`. |

The branch boundary is non-empty `members`, not member count. A one-element `members` list therefore has batch semantics and ignores the top-level ID, matching current member construction.

## Why This Boundary

Validation belongs where the value is consumed. Parsing before mode selection imposes a constraint on a field the selected mode discards. Placeholder-specific exceptions would preserve the same structural bug for the next malformed value, while global normalization could silently turn a failed resume request into a new session.

Splitting the public input type is not justified in this patch. Both modes immediately normalize to `Vec<SpawnMember>`, and a schema split would introduce provider-compatibility risk without strengthening an invariant used downstream.

## Regression Test

Add one integration test in `crates/hya-tool/tests/task.rs` using the existing `SpawnerPlane` fixture:

1. Execute a schema-valid two-member request with top-level `task_id: "new"`.
2. Require the request to reach the spawner.
3. Assert both members are present and every member has `task_id: None`.
4. Reply with two successful outcomes and require `TaskTool::execute` to complete successfully.

Before the source change, the test must fail because execution returns `invalid task_id` before spawner dispatch. Existing tests continue to cover valid single-member forwarding and malformed single-member rejection.

## Release Metadata

Follow the established version-bump file set:

- Change `[workspace.package].version` in `Cargo.toml` from `0.33.18` to `0.33.19`.
- Regenerate only workspace package versions in `Cargo.lock`; reject dependency or checksum churn.
- Add `docs/changes/CHANGELOG_0.33.18.md` with the current root changelog verbatim.
- Replace root `CHANGELOG.md` with notes only for `0.33.19`.
- Align the version mirrors in `README.md` and `packages/hya-tui-ts/package.json`.

Publishing or creating a release tag is outside this task.

## Risks And Controls

| Risk | Control |
| --- | --- |
| Single-member validation weakens | Relocate the parser unchanged and run both existing resume tests. |
| A batch ID leaks downstream | Assert every captured member ID is `None`. |
| Error precedence changes unexpectedly | Keep parsing first inside the single-member branch; accept only later batch-specific errors becoming visible. |
| Lockfile gains unrelated updates | Inspect the lockfile diff and accept only workspace version changes. |
| Concurrent work is overwritten or staged | Recheck status before edits and stage explicit task-owned paths only. |

## Deferred Hardening

`SpawnMember.task_id` is still `Option<String>` and downstream code reparses it with `.parse().ok()`. Converting it to `Option<SessionId>` is a separate cross-crate contract change. Reconsider it only when another producer appears or during a deliberate compatibility window.

## Impacted Files

| File | Change |
| --- | --- |
| `crates/hya-tool/tests/task.rs` | Add the single regression test. |
| `crates/hya-tool/src/task.rs` | Move parsing into the single-member branch. |
| `Cargo.toml` | Bump workspace version to `0.33.19`. |
| `Cargo.lock` | Align workspace package versions. |
| `CHANGELOG.md` | Describe only `0.33.19`. |
| `docs/changes/CHANGELOG_0.33.18.md` | Archive the prior root changelog. |
| `README.md` | Align the displayed workspace version. |
| `packages/hya-tui-ts/package.json` | Align the package version mirror. |
