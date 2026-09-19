# 0.36.38

## gRPC binding lands: `hya.v1` now serves identical functionality over HTTP and gRPC (server, api)

- `hya-server::V1Grpc` implements all fifteen generated tonic services.
  Every unary rpc dispatches through the exact same axum `/v1` router the
  HTTP transport serves (protojson in, protojson out, stable error codes
  mapped from the JSON error body to gRPC statuses), so dual-transport
  parity holds by construction rather than by duplication.
- Streaming rpcs share producers with SSE: `StreamSessionEvents` and
  `StreamGlobalEvents` are backed by the same curated `StreamFrame`
  producer (typed `resync` included), and `StreamPty` is a bidirectional
  bridge over the PTY runtime whose first client frame is a new
  `PtyClientFrame.attach { id, token }` envelope added to the contract.
- The conformance gate is real network I/O, not a mock:
  `tests/v1_grpc_parity.rs` serves all fifteen services on an ephemeral
  port via tonic, drives health/catalog/session lifecycle, a full
  event-driven turn to terminal state, event replay, and error mapping
  through both a generated gRPC client and the HTTP router, and asserts
  the responses match (volatile ids/timestamps normalized).
- Embedders serve the binding with
  `tonic::transport::Server::builder().add_service(ProcessServer::new(V1Grpc::new(state))...)`;
  backend listener wiring follows in the cutover phase together with the
  SDK migration.
