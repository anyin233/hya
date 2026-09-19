# 0.36.40

## `hya-client` speaks the v1 API; the e2e matrix runs on it end to end (client, server, e2e)

- `hya-client` is rewritten against `/v1`: protojson-typed
  `create_session`, event-driven `create_turn`/`wait_turn` (plus the
  synchronous `prompt` convenience), curated event replay, raw envelope
  replay for tooling, and pending-interaction list/respond. Failures
  surface the stable v1 error model (`code` + `message`) instead of bare
  HTTP statuses.
- The contract grew `ListEventsRequest.include_raw`: when set, replay
  also returns the canonical durable envelope JSON lines. Tooling and
  test harnesses keep exact-log access through the same transport while
  the curated stream stays the frontend surface; the internal envelope
  shape is documented as opaque.
- HTTP GET query parameters now coerce `true`/`false` to real protojson
  booleans (found by the e2e matrix: string-typed bools are invalid
  protojson).
- The process e2e harness (Track P, p01–p20) now drives the real
  backend through the v1 client — sessions, turns, event replay, and
  permission/question flows — and the full matrix passes against real
  backend processes. The remaining legacy-route probes inside the
  harness (tree/context/todo/status and p13/p18 route-parity cases)
  stay on the legacy surface until it is deleted, as intended.
