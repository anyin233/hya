# Debug subagent spawn tool invocation failures

## Goal

Restore reliable model invocation of the `task` tool by identifying and fixing the shared contract defect that turns optional spawn inputs into invalid values before dispatch.

## Background

- Nested subagent spawning worked on the same GPT 5.6 Sol route in `0.33.2`.
- In current root session `hysec_860vXRv1C6o1K2DKocPb`, the model sent `task_id: ""` and `task_id: "new"`; both were rejected as invalid session IDs before spawning.
- A later all-zero UUID passed parsing and allowed child creation, showing that tool discovery and spawner dispatch remain functional.
- All three calls were five-member batches. Batch construction always sets each member's resume ID to `None`, so the rejected top-level values had no runtime meaning.

## Requirements

- Prove the earliest shared boundary responsible for the invalid optional-field values.
- Add one focused regression test that reproduces the persisted failure before implementation.
- Validate top-level `task_id` only when the single-member path will forward it, without weakening validation of genuine resume session IDs.
- Preserve valid new-task, resume-task, single-member, multi-member, foreground, and background behavior.
- Keep unrelated provider, task, and Trellis work untouched.
- Increment the workspace version from `0.33.18` to `0.33.19`, archive the prior root changelog, and make root `CHANGELOG.md` describe only this fix.

## Acceptance Criteria

- [ ] A batch call with a malformed, semantically unused top-level `task_id` is represented by a deterministic failing test before the fix.
- [ ] The same batch reaches the spawner after the fix, and every supplied member still has `task_id: None`.
- [ ] A valid `task_id` still reaches the resume path, while a genuinely malformed non-placeholder identifier still returns an input error.
- [ ] Focused tests and the repository-required Rust verification gate pass.
- [ ] `[workspace.package].version`, lockfile package versions, and the root `0.33.19` changelog agree.

## Out Of Scope

- Changing subagent lifecycle, governor limits, permission policy, or session ancestry.
- Adding provider-specific fallback retries or model prompt workarounds when the schema/input contract can express the behavior directly.
- Normalizing empty model, category, or inline-agent fields; current resolver behavior already ignores their empty values in the reproduced path.
- Broad refactoring of `TaskTool`, `SpawnerPlane`, or provider request construction.

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
