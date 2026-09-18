# 0.36.32

## Removed the compat integration and credential stub routes (server)

- The nine `/api/integration/*` and `/api/credential/*` compatibility routes
  are gone. Every one was a stub with no storage, engine wiring, or
  consumer: integration discovery always returned `[]`/`null`, the connect
  and attempt-complete endpoints returned a hardcoded 400
  `integration_authorization` error, attempt status returned 500, and the
  attempt/credential mutations returned 204 without doing anything.
- `GET /api/reference` — the one real capability that shared the module —
  moved to the metadata route group and behaves exactly as before. The
  generated OpenAPI document (`/doc`, `/openapi.json`) no longer lists the
  removed paths.
- This is an accepted break for the existing TUI's vendored upstream data
  layer, whose lazy `integration.list` call now finds no endpoint. It is
  the first cleanup step of the API v1 consolidation toward the dual
  HTTP/gRPC contract; third-party service connectors, if ever needed, will
  be designed as real v1 RPCs rather than carried over as stubs.
