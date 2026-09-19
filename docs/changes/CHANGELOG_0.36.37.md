# 0.36.37

## HTTP `/v1` binding completed: PTY sessions, WebSocket terminal stream, and the protocol guide (server)

- The PTY domain joins `/v1`: `GET /v1/pty/shells`, `POST /v1/pty`,
  `GET/PUT/DELETE /v1/pty/{id}`, and `POST /v1/pty/{id}/connect-token`
  over the shared PTY runtime (typed create/update payloads, one-time
  connect tickets with expiry).
- `GET /v1/pty/{id}/connect?ticket=...` upgrades to a WebSocket carrying
  protojson `PtyClientFrame` / `PtyServerFrame` — the same frame types as
  the future gRPC `StreamPty` rpc: base64 `input`/`output` bytes,
  `resize`, `ping`/`pong`, and terminal `exit`. The first server frame
  replays the current buffer; raw-binary frames from legacy clients are
  still accepted as terminal input. Runtime resize stays a documented
  no-op until the PTY state grows a resize API (shell-side SIGWINCH
  applies).
- `docs/protocol/README.md` is the third-party integration guide: base
  URL and `x-hya-directory` scoping, protojson rules (camelCase fields,
  full enum names, uint64-as-string, omitted defaults), the stable error
  table mapped across HTTP and gRPC, cursor pagination, the event-driven
  turn workflow with SSE frame examples, the interaction plane, PTY
  frames, and a minimal client walkthrough.
- Every `/v1` rpc in the `hya.v1` contract now has a live HTTP handler.
  A PTY lifecycle integration test extends `tests/v1_api.rs` (8 green).
  Next: the gRPC binding and the dual-transport parity suite.
