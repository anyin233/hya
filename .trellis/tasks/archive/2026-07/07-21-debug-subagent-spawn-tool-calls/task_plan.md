# Debug subagent spawn tool invocation failures

## Goal

Identify a reproducible persisted-session failure in the subagent spawn tool path, prove its root cause, and define the smallest regression-tested fix.

## Phases

- [x] Planning: collect session evidence, establish a red-capable repro, and write PRD/design/implementation artifacts.
- [x] Execution: add one failing regression test, implement the root-cause fix, and verify the focused path.
- [x] Finish: run required workspace checks, review the diff, update durable specs if needed, then commit and push.

## Current Status

Implementation, independent verification, and durable contract capture are
complete at `0.33.19`. The atomic work commit is `2fa6a60f`; Trellis archival,
session journaling, and the final push remain.

## Planner Synthesis

- Consensus: move existing top-level `task_id` parsing unchanged into the `members.is_empty()` branch.
- Regression seam: a non-empty `members` request with `task_id: "new"` must reach the spawner and carry `None` for every member ID.
- Preserve existing valid/malformed single-member tests; do not alter schema, provider serializers, resolver normalization, or `SpawnMember` types.
- Revalidate shared release files immediately before implementation because they are common concurrent-edit targets.

## Artifact Checklist

- [x] Persist root-cause evidence and acceptance criteria in `prd.md` and `findings.md`.
- [x] Complete four independent planner reviews and synthesize their consensus.
- [x] Write `design.md` with the branch contract, test seam, risks, and impacted files.
- [x] Write `implement.md` with RED, GREEN, release, verification, and finish gates.
- [x] Replace seed context entries in `implement.jsonl` and `check.jsonl`.
- [x] Validate the planning artifacts.
- [x] Present the artifacts for user review and explicit approval before `task.py start`.

## Errors Encountered

| Error | Attempt | Resolution |
| --- | --- | --- |
| Live session endpoint `127.0.0.1:32771` refused the connection after the server exited. | Recover full arguments for root session `hysec_860vXRv1C6o1K2DKocPb`. | Use conversation/session memory for the prior captured output; run a fresh persisted reproduction if the arguments were not retained. |
| `sqlite3` CLI is not installed. | Inspect the local OpenCode conversation store for the earlier captured tool output. | Use Python's standard-library `sqlite3` module in read-only mode. |
| First Python SQLite query had an invalid escaped expression inside an f-string. | Search conversation rows for the failing hya session ID. | Build the selected column name outside the SQL string and rerun once. |
| Unindexed `LIKE` scan across the full OpenCode store exceeded 120 seconds. | Search all conversation tables for the hya session ID. | Restrict reads to the current OpenCode session ID, then filter its small row set in memory. |
| The first targeted payload parse selected an empty/non-JSON `state.output`. | Decode the saved filtered event array. | Inspect the part state shape and read the populated metadata output field explicitly. |
| Context validation found nonexistent generic guide paths in both manifests. | Validate the completed planning artifacts. | Replace them with the actual backend quality and error-handling spec paths, then rerun validation. |
| `get_context.py` rejected obsolete `implement` and `check` mode names. | Render both curated contexts after validation. | Use `--mode phase --step implementing` and `--mode phase --step verifying` from the current workflow. |
| `get_context.py` rejected descriptive step names. | Render phase detail after correcting the mode. | Resolve the Phase Index and use numeric step IDs `2.1` and `2.2`. |
