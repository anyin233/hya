# Remove Agent Tool-Calling Limit

## Goal

Allow every hya agent turn to continue beyond the current fixed 25 tool-bearing rounds until the provider finishes, the user cancels, or a real execution error occurs.

## Background

`SessionEngine::run_turn_rounds` is shared by root agents and subagents. It currently ends a turn with `FinishReason::Error` after 25 provider rounds containing tool calls, even though the provider has not finished.

## Requirements

- R1: Agent execution must not stop solely because a tool-call or tool-round count reaches a fixed value.
- R2: The change must apply through the shared engine path to root agents and subagents.
- R3: Cancellation, provider-directed completion, provider/tool error handling, token accounting, and step lifecycle events must retain their current behavior.
- R4: Do not add a replacement limit, configuration field, abstraction, or alternate enforcement path.
- R5: Record the change as workspace patch version `0.33.28` with project-compliant changelog metadata.

## Acceptance Criteria

- [ ] AC1: A regression test scripts 26 tool-bearing rounds followed by a unique final response and proves the turn returns `FinishReason::Stop` with that final text present.
- [ ] AC2: The regression test fails against the current 25-round guard for the expected behavioral reason before implementation and passes after implementation.
- [ ] AC3: No production path in `run_turn_rounds` terminates because the removed fixed count was reached; normal step numbering remains in place.
- [ ] AC4: Existing cancellation and provider-completion tests pass together with the full Rust workspace verification gate and a local `hya` executable build.
- [ ] AC5: `Cargo.toml`, `Cargo.lock`, root `CHANGELOG.md`, and `docs/changes/CHANGELOG_0.33.27.md` consistently represent the patch release.

## Out Of Scope

- Goal-mode and loop-mode iteration budgets.
- Subagent spawn, concurrency, depth, resident-turn, and message budgets.
- Compat `AgentEntry.steps` metadata and tool result/search output limits.
- Release tagging or publication.
