# Findings

## Initial Evidence

- Reported symptom: agents cannot correctly invoke the tool used to start subagents.
- Relevant prior tasks: `07-14-e2e-gpt56-sol-subagents` and `07-14-close-gpt56-subagent-e2e-gaps`.
- The worktree already contains an unrelated untracked task, `07-21-grok-build-provider`; it must remain untouched.
- Current source version `0.33.2` previously passed nested spawn on the exact GPT 5.6 Sol route: root `hysec_4lN7lPZqFffzIaTcJrqZ`, child `hysec_6gy7W7zgkTGRNRbz59GP`, and grandchild `hysec_YLdVM6BFVXl9Sc6uJS6s` produced two correlated `task` calls/results and correct depth-2 ancestry.
- Earlier model omissions and unexpected child `task` requests were classified as model adherence unless a current-source behavior boundary failed deterministically.
- The prior disposable SQLite databases were removed after redacted evidence was recorded, so the current failure needs a fresh persisted-session anchor.

## Current Failure Anchor

- The running `hya-backend 0.33.16` instance exposed root session `hysec_860vXRv1C6o1K2DKocPb` through `http://127.0.0.1:32771`.
- The root session contained 22 `tool_call_requested`, 4 `tool_error`, 17 `tool_result`, and 6045 `tool_input_delta` events when inspected.
- Its first two `task` calls supplied `task_id` as `""` and `"new"`; both failed with `invalid task_id` before reaching the spawner.
- A later call supplied an all-zero UUID and passed validation, after which five child sessions were created.
- The same model output populated other optional fields with placeholders, including empty strings and an empty `inline_agent` object.
- The full saved event payload was recovered from the current OpenCode conversation store after the live server exited.
- Requests at event sequences 3498 and 3507 were five-member batch calls. Their only relevant difference was top-level `task_id: ""` versus `task_id: "new"`; both failed before any member reached the spawner.
- The third otherwise equivalent batch at sequence 6202 used an all-zero UUID, passed the eager parser, and proceeded to member creation.

## Code Boundary

- `TaskInput.task_id` is `Option<String>` in `crates/hya-tool/src/task.rs`, so an omitted field is `None` but an empty string is `Some("")`.
- `TaskTool::execute` parses every present `task_id` as `SessionId` before constructing members or calling `SpawnerPlane`; invalid placeholders therefore fail at the tool-input boundary.
- `inline_agent`, `model`, and `category` are also optional model-facing fields. An empty `inline_agent` object currently becomes a real inline-agent override, while empty model/category strings remain present values.
- `crates/hya-provider/src/openai.rs` and `openai/responses.rs` pass `ToolSchema.input_schema` directly as OpenAI `parameters`; neither adds `strict` nor rewrites `required` or `additionalProperties`.
- The canonical `task` schema requires only `description`, `prompt`, and `subagent_type`. `task_id`, `inline_agent`, `model`, and `category` remain optional in the model-facing JSON schema.
- Placeholder emission is therefore model behavior exposed by the permissive optional-field contract, not provider-side strict-schema conversion.
- Existing tests cover omitted `task_id`, a valid resume ID, and a malformed ID, but not an empty placeholder or an irrelevant top-level ID on a multi-member call.
- `TaskTool::execute` assigns `task_id: None` to every member supplied through `members`; the top-level `task_id` is semantically unused whenever `members` is non-empty.
- The reproduced root cause is branch ordering: validation of the top-level resume ID occurs before deciding between batch members and the single-member path that actually forwards it.
- The minimum semantic fix is to validate `task_id` only in the single-member path. This leaves `""`, `"new"`, and every malformed value invalid when used as an actual resume ID, while ignored batch fields cannot block dispatch.
- Other placeholders did not reproduce a failure: top-level batch overrides are discarded, while `resolve_subagent` trims empty spawn/inline model, category, and prompt values before applying overrides. The empty inline name falls back to the existing `subagent_type`.
- No generic optional-string normalizer or provider-specific workaround is justified by the evidence.

## Open Evidence Gaps

- Deterministic command that reproduces the observed failure.
- The original live server has exited and its endpoint now refuses connections, so uncaptured event payloads require conversation-memory recovery or a fresh persisted run.

## Project Constraints

- Workspace version is `0.33.16`; project rules require every source fix to increment it explicitly.
- Root `CHANGELOG.md` contains only `0.33.16`, so the fix must archive it as `docs/changes/CHANGELOG_0.33.16.md` and write a new single-version `0.33.17` root changelog.
- Existing `task_forwards_task_id_to_spawner_for_resume` and `task_rejects_invalid_task_id` tests already protect valid and malformed single-member resume behavior.
- Required Rust verification is `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, plus a local executable build.

## Planner Consensus

- All four planners support the branch-local parse as the smallest architecture-valid fix.
- The mode boundary is `members.is_empty()`, not member count: even a one-element `members` array ignores the top-level resume ID today.
- Moving the existing parse block unchanged to the start of the single-member branch preserves validation and error precedence there.
- A malformed batch may reveal a later, more relevant validation error after the fix, such as the existing background cardinality error; this is intentional.
- Splitting the wire input into single/batch variants or adding `oneOf`, a custom deserializer, provider normalization, or placeholder-specific handling is unjustified.
- `SpawnMember.task_id` remaining `Option<String>` and downstream `.parse().ok()` are a separate internal contract weakness. Defer typed-ID hardening until another producer appears or a deliberate compatibility window.

## Workspace Baseline

- Initial `git status --short` contains this task plus unrelated untracked work at `.trellis/tasks/07-21-fix-subagent-navigation-roster/`, `.trellis/tasks/07-21-grok-build-provider/`, `.trellis/tasks/07-22-enable-websearch-config/`, and `docs/architecture/agent-tool-surface.md`.
- Only `.trellis/tasks/07-21-debug-subagent-spawn-tool-calls/` belongs to this planning task; all other baseline paths must remain untouched and unstaged.
- A later status review also showed concurrent tracked edits in `crates/hya-core/src/engine/turn.rs`, `crates/hya-core/src/engine/turn/messages.rs`, `crates/hya-core/tests/tool_filtering.rs`, `crates/hya-server/src/compat/experimental_tool.rs`, and `crates/hya-server/tests/compat_experimental_tool_api.rs`; they do not overlap the planned files and must remain untouched.
- Version `0.33.16` is mirrored in `README.md` and `packages/hya-tui-ts/package.json` in addition to Cargo metadata and changelog files.
- `docs/changes/CHANGELOG_0.33.16.md` does not currently exist.
- The most recent version-bump commit, `dac61e8d`, updated both mirrors together with `Cargo.toml`, `Cargo.lock`, and the changelog files; `0.33.17` should follow that established release file set.

## Implementation Baseline Revalidation

- Before RED, the worktree already reported `0.33.17` in `Cargo.toml`, all workspace entries in `Cargo.lock`, `README.md`, and `packages/hya-tui-ts/package.json`.
- `docs/changes/CHANGELOG_0.33.16.md` already exists, and root `CHANGELOG.md` now contains `0.33.17` notes for concurrent websearch and Grok Build work.
- These are incompatible changes to the approved release-file target set. Implementation stopped before editing tests or production code because preserving them would require an unapproved version/changelog redesign.
- User decision: wait for the concurrent `0.33.17` owner to commit, then resume this task as an isolated `0.33.18` fix. Do not edit shared release files before that condition is met.

## Resume Baseline

- The shared release files are now clean and aligned at `0.33.18`; `docs/changes/CHANGELOG_0.33.17.md` exists and `docs/changes/CHANGELOG_0.33.18.md` does not.
- The remaining dirty files belong to concurrent Grok Build/spec work and do not overlap `crates/hya-tool/src/task.rs`, `crates/hya-tool/tests/task.rs`, or the release metadata.
- Per the project release rule, this fix now targets `0.33.19` and archives the current `0.33.18` root changelog.
