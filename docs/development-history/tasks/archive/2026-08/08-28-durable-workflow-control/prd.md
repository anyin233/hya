# Implement Durable Workflow Control

## Outcome

Persist Workflow selection/run lifecycle in the shared Session event log and expose one app-owned control path to CLI, Agent tool, native/Compat server routes, and hya-sdk.

## Requirements

- Add typed Workflow source/revision/run identities, lifecycle Events, Projection state, command DTOs, statuses, and bounded errors to `hya-proto`.
- Keep Member/child Session projection authoritative for activity and transcripts; Workflow state stores only Stage plan/status and Member references.
- Reconcile a provably dead nonterminal run to Interrupted without replaying a Stage.
- Implement `hya-app::WorkflowControl` for list/info/select/state/run, revision checks, idempotency, busy rules, binding, and execution dispatch.
- Preserve model-tool `ToolOperation`/actor identity and use the same compiler/catalog/executor as CLI and direct requests.
- Intercept `/workflow` before parent-model admission in native, legacy Compat, and Compat v2 command paths.
- Expose typed command/state endpoints and hya-sdk mirror types over existing transports with structured non-2xx errors.

## Acceptance Criteria

- [x] Projection replay reconstructs selection and latest run, preserves the exact message vector, deduplicates links, ignores stale runs, and keeps terminal state sticky.
- [x] Closing/reopening the store produces the same Workflow Projection; stale active work becomes Interrupted with no duplicate child effects.
- [x] Repeated stable run id/request hash does not execute twice; a mismatched hash and Session conflict return typed 409 errors.
- [x] CLI, Agent tool, and direct routes resolve the same revision and validation semantics.
- [x] `/workflow list|info|use|run|state` never calls the parent model; Stage Agents still execute normally.
- [x] Native/legacy/v2 endpoints, Session hydration/events, and SDK state/errors pass conformance tests.
- [x] Switching selected Workflow preserves every prior message id and content.

## Exclusions

- No TypeScript sidebar rendering or release/version work in this child.
