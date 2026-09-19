# 0.36.47

## Agent-model preferences reachable over v1; interaction frames are live on the streams (server, api)

- New `AgentModels` service (16th service, 79 rpcs total):
  `GET /v1/agent-models` lists every catalog agent's effective model and
  its source tier (session / configured / remembered / default), and
  `PUT /v1/agent-models/{agent_id}` sets or clears one agent's durable
  remembered preference — the surface the retired `/tui/agent-models`
  routes provided, now on the consolidated contract over the same
  app-owned `PersistentAgentModelControl` (owner-fenced store commits,
  catalog validation, live snapshot publication). The gRPC binding
  serves both rpcs through the same router and the parity suite
  asserts the unavailable-control error matches across transports.
- Interaction frames are wired end to end: the pending permission and
  question planes now merge into the shared live frame producer, so
  `permissionRequested`, `questionRequested`, and `interactionResolved`
  frames arrive on the session and global streams (SSE and gRPC alike,
  `seq == 0` live-only semantics). An agent's permission ask reaches
  the user through `/v1/interactions` listing and these frames; an
  "always" answer both resolves the frame in flight and persists a
  saved rule (`GET /v1/permissions/rules`).
- New integration suites: `v1_agent_models_api.rs` (list/set/clear,
  full stable-error table, unavailable control) and
  `v1_interaction_stream.rs` (asked + resolved frames over SSE, saved
  rule persistence, question reject resolution).
