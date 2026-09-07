# Implementation Plan

1. Add hya-proto red tests for Workflow ids, selection A->B transcript preservation, run/stage/member fold, duplicate links/sequence, stale old-run events, and terminal stickiness.
2. Add shared types/Event variants and Projection reducer arms. Update every exhaustive Event match as a compile-guided clean cutover.
3. Add store close/reopen and dead-owner interrupted-run tests. Implement durable append/replay/reconciliation without a table or Stage replay.
4. Add app control red tests for list/info/select/state, stale revision, missing/extra inputs, unauthorized preflight, stable run id/hash replay/conflict, busy Session, and ToolOperation retention.
5. Implement `WorkflowControl` over immutable catalogs, engine/store/resident dependencies, and the one core executor. Remove duplicate app/backend discovery/run helpers and dead caller-roster authority.
6. Migrate Agent tool and backend CLI to the control interface; preserve finished delivery for their existing reports/exit codes.
7. Add server red tests for typed native/legacy/v2 routes and command interception. Assert zero parent-provider calls for list/info/use/state and only Stage calls for run. Implement one shared parser/handler before model admission.
8. Add Session hydration/event mapping and structured stable errors. Ensure raw envelopes remain available and no compatibility reducer diverges.
9. Add hya-sdk red tests for DTO conformance, projected activity, unrelated transcript stability, existing transport use, and structured HTTP/native 409/403 bodies. Implement mirrored types and client method.
10. Run real backend restart, switch, idempotency, abort, and transcript-preservation tests.
11. Run the three state/control mutations, revert them, then run the focused child gate.
12. Run Trellis check review and finish only after every route uses the same compiled revision/control adapter.

Rollback: new Events make old binaries unable to replay stores after use. All child verification uses isolated stores. Do not write new Events to a real user store before final release acceptance.
