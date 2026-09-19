# 0.36.39

## Backend serves the hya.v1 gRPC surface next to HTTP (backend)

- `hya serve` now starts the full gRPC binding when `HYA_GRPC_BIND` is
  set (for example `HYA_GRPC_BIND=127.0.0.1:7423`): one tonic listener
  serves all fifteen `hya.v1` services from the same application state
  as the HTTP surface, announcing itself with a `hya grpc listening on`
  line. Without the variable the backend behaves exactly as before.
- Typed clients can now point at either transport of a live backend:
  HTTP `/v1` JSON+SSE+WebSocket, or gRPC `hya.v1` with the identical
  request/response semantics enforced by the parity suite.
