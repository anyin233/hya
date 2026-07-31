# Error Handling

> How errors are handled in this project.

---

## Overview

- Libraries use typed `thiserror` enums and propagate errors with `?`; library
  code must not panic on runtime failures.
- Preserve typed errors across existing layer boundaries. Add the smallest
  variant needed at store/core/tool/spawn seams rather than copying an
  independent error stack.

---

## Error Types

- `StoreError::OperationIdConflict` is the durable immutable-request conflict
  and displays the stable code `OPERATION_ID_CONFLICT`.
- `StoreError::AdmissionTransitionConflict` rejects terminal rewrites.
- `StoreError::ActorAlreadyClaimed` distinguishes an ordinary competing claim;
  `StoreError::StaleActorClaim` rejects any old epoch/owner capability at the
  canonical mutation boundary.
- `SpawnError` distinguishes queue/admission overload, unavailable transport,
  operation conflict, and already-handled idempotent replay.
- `TaskTool` maps these into matching `ToolError` variants; engine tool-error
  payloads retain distinct machine-readable types.

---

## Error Handling Patterns

- Fail closed before child/session/effect creation when identity, fingerprint,
  persistence, or admission is uncertain.
- A duplicate identical operation is not redispatched. A conflicting duplicate
  is not mutated.
- Startup must return an error rather than expose spawn surfaces if admission
  recovery fails.
- Resident startup must remain closed if epoch takeover, running-work
  terminalization, or resident re-registration fails. A stale completion is a
  typed rejection and must not be converted to a successful no-op that wakes
  work or advances projection state.
- Logging is supplemental; do not log-and-continue past a failed pre-create
  safety gate.

---

## API Error Responses

- Operation identity remains internal in 0.34.4; do not add it to HTTP/proto/CLI
  payloads.
- Existing tool-result event JSON carries `{ "error": { "type", "message" } }`.
  Use `operation_id_conflict` and `operation_already_handled` for the new tool
  variants.

---

## Common Mistakes

- Do not collapse overload into unavailable or input errors.
- Do not treat an identical terminal replay as a second release.
- Do not convert an operation conflict into a fresh operation ID.
- Do not add `unwrap`/`expect` to library paths to satisfy tests.
