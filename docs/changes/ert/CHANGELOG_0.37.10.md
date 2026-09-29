# 0.37.10

## Native gRPC connection for OpenTUI

The OpenTUI frontend can connect to the backend's separate `hya.v1` gRPC
listener with `--grpc host:port`. Sessions, turns, catalogs, provider keys,
workflows, interactions, and the API command view use the same frontend
workflows over the selected transport. Session event streams now replay durable
events after `sinceSeq` before continuing live, so reconnecting a frontend
does not miss a completed turn. A real-backend process test covers unary calls,
replay, and live event delivery.
