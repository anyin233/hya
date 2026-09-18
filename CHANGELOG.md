# 0.36.35

## HTTP `/v1` binding goes live: sessions, event-driven turns, streams (server)

- The `/v1` HTTP surface from the `hya.v1` contract is now served by
  `hya-server`, mounted alongside the legacy routes: process
  health/location/config (deep-merge PATCH), the aggregated bootstrap
  snapshot, catalog reads (agents, models, providers, commands, skills,
  tools), provider auth key storage/removal, frontend log ingest, and the
  saved permission-rule list.
- Session lifecycle: create (with validation), get, list (parent filter,
  cursor pagination), patch (title/agent/model), delete, fork (provenance
  + metadata + message copy), compact, and summarize. Sequence-targeted
  revert answers `unavailable` until the legacy diff machinery is ported.
- Turns are event-driven as designed: `CreateTurn` admits a prompt,
  slash-command (with workflow interception), or shell turn and returns a
  `RUNNING` handle immediately; the model round runs on a spawned task and
  terminal state arrives via `GetTurn`/`WaitTurn`/`CancelTurn` and the
  streams. Messages and the todo list read from the shared projection —
  no second read model.
- Events: replay with `since_seq` watermark plus the two SSE streams
  (session-scoped and global) emitting the curated `StreamFrame` protojson
  with the typed `resync` signal and keepalives. The unified interaction
  plane lists and answers pending permission/question requests.
- Cross-cutting: stable error model rendered as
  `{"error":{"code","message"}}`, opaque cursor pagination, and the
  `x-hya-directory` scope header. Action RPCs use subpath routes
  (`POST /v1/sessions/{id}/turns/{turn}/wait`); the IDL and generated
  docs/OpenAPI were regenerated to match.
- Integration coverage: `tests/v1_api.rs` exercises process/config,
  catalog, auth storage, session lifecycle, an end-to-end event-driven
  turn (finish + transcript + replay + SSE headers), and the error model.
  Legacy routes are untouched; the gRPC binding lands next.
