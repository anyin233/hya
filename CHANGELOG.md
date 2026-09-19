# 0.36.42

## The process e2e matrix runs entirely on the v1 API (e2e, server)

- Track P (p01–p20) no longer touches any legacy route: permission and
  question repliers, session listings, trees, contexts, todos, busy
  polling, compaction, summarize, workflow info/run/state, catalogs, and
  every custom-slash probe now drive `/v1`. The legacy/v2/native
  triple-surface probes in p18 collapse to the unified v1 behavior, and
  p19's workflow model-routing deep assertions read the new
  `WorkflowState.raw_json` opaque projection.
- The v1 surface gained the semantics the matrix demanded: `SessionInfo
  .busy` (run-registry derived), catalog `result` per provider with real
  auth states from the catalog snapshot, command rows carrying
  `hints/source/template/agent/model/subtask`, skill rows carrying
  `content/location`, tool-call parts carrying structured
  `error_code/error_message` from the projection, workflow `run` taking
  an explicit `name`, and prompt/command turns composing the same
  AGENTS/reference guidance the best legacy path provided.
- e2e harness: v1 trees are assembled from parent-filtered listings
  enriched with roster handles folded client-side from raw envelopes;
  stderr/stdout from backend processes now drain to the test log.
- Gates: 43/43 matrix tests green against real backends
  (`model_catalog_is_fresh…` excluded — verified failing at commits
  predating this branch, i.e. broken on main already, in the concurrent
  work's provider-discovery domain).
