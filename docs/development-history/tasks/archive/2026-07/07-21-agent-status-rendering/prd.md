# Fix agent status and activity rendering

## Goal

Make agent output, reasoning effort, activity, and terminal state remain visible and accurate in the current TypeScript TUI.

## Background

The reported symptoms have three confirmed causes: the legacy prompt endpoint drops the selected reasoning variant, terminal lifecycle events automatically discard observation panes, and lifecycle labels prefer stale roster state over the active transient member state. Existing event conversion, projection, transcript synchronization, and reasoning rendering already support the required content when those inputs are preserved.

## Requirements

- Preserve a prompt's explicit non-empty top-level reasoning variant through the legacy message endpoint so the selected effort and provider reasoning route survive the first message.
- Keep existing nested model variants when no explicit top-level variant is supplied; an explicit top-level variant takes precedence when both are present.
- Retain an observed subagent transcript after terminal completion until the user explicitly closes it or session reconciliation proves it no longer exists.
- Resolve lifecycle presentation from transient member status when present, otherwise roster status.
- Show a visible `Working` label and activity indicator for spawning, running, or busy agents.
- Show successful completion as `Finished`; preserve distinct `Failed`, `Cancelled`, and true `Idle` labels.
- Continue rendering only reasoning content present in the shared event projection; do not synthesize missing reasoning or introduce a second message/state store.

## Acceptance Criteria

- [x] A legacy object-form prompt with nested variant `low` and explicit top-level variant `high` records and returns `high` for the user message and session model.
- [x] Missing or empty top-level variant preserves an existing nested variant, and string-form model requests retain current behavior.
- [x] A terminal observation remains open with its synchronized output and thinking content after completion and after focus changes.
- [x] Explicit close and stale-session reconciliation still remove observation panes.
- [x] Both the observation header and subagent dialog use the same member-first lifecycle mapping.
- [x] Working rows show `Working` plus the existing spinner; successful terminal rows show `Finished` without a spinner.
- [x] Focused regression tests fail on the current behavior and pass after the fixes.
- [x] Required TypeScript and Rust formatting, linting, tests, builds, and version consistency checks pass.

## Out Of Scope

- New event types, provider protocols, transcript stores, reasoning renderers, or lifecycle models.
- Automatic cleanup of terminal observations beyond explicit close and stale-session reconciliation.
- Release tag or GitHub Release publication.
