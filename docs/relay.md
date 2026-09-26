# Secure relay

The secure relay lets a client reach a hya backend through a third-party
server, `hya proxy`, for remote control. The proxy is a blind rendezvous: the
backend registers a *room* it owns, a client opens a stream to that room, and
the proxy splices the two byte streams together. Everything the proxy
forwards is end-to-end encrypted by the backend and the client (Noise
`NKpsk0`), and the key material travels only inside the relay link, so the
proxy — and any HTTPS hop in front of it (nginx, Cloudflare Tunnel, Caddy,
Tailscale, …) — sees room ids, stream ids, and ciphertext only.

The relay is being built in steps (`crates/hya-relay`). This page documents
what exists today; the parts marked *coming in later steps* are not
implemented yet.

## Usage

*Coming in later steps:* `hya proxy`, `hya serve --relay`, `hya serve relay
…`, `hya bridge`, `hya --connect <link>`, `/connect-remote`, `hya relay
doctor`, and deployment recipes (Cloudflare Tunnel, nginx, Caddy, Tailscale,
direct TLS).

## Interfaces

### The `hya.relay.v1` protocol

Source: [`proto/hya/relay/v1/relay.proto`](../proto/hya/relay/v1/relay.proto).
It is a separate protobuf package from `hya.v1` and carries no hya
semantics. Generated Rust (prost messages and the tonic `Relay` client and
server) is committed in `crates/hya-relay/src/gen/` and exposed as
`hya_relay::proto`; regenerate it after editing the proto with:

```sh
cargo run -p xtask -- gen-relay
```

The same messages serve two bindings:

| Binding | Endpoints | Frames | Errors |
| --- | --- | --- | --- |
| gRPC (HTTP/2) | `<prefix>/hya.relay.v1.Relay/{Host,Accept,Open}` | one message per stream item | gRPC status |
| WebSocket (HTTP/1.1+) | `GET <prefix>/hya.relay.v1/ws/{host,accept,open}` | one encoded message per binary frame | a final frame with `error` set (`RelayError`), then close |

Streams:

| rpc / WS route | Client sends | Proxy sends | Purpose |
| --- | --- | --- | --- |
| `Host` / `ws/host` | `HostFrame` | `ProxyToHost` | Host control stream: registration, heartbeats, `Incoming` notices. The room is online while it is open. |
| `Accept` / `ws/accept` | `Chunk` | `Chunk` | Host side of one data stream; first frame `accept{stream_id}`. |
| `Open` / `ws/open` | `Chunk` | `Chunk` | Client side of one data stream; first frame `open{room_id}`. An offline room fails with `NOT_FOUND`. |

Messages:

| Message | Fields | Meaning |
| --- | --- | --- |
| `HostFrame` | oneof `register` \| `heartbeat` | Host → proxy on the control stream. |
| `ProxyToHost` | oneof `challenge` \| `registered` \| `heartbeat` \| `incoming` \| `error` | Proxy → host on the control stream. |
| `Challenge` | `nonce: bytes` (32) | First proxy frame; the host must sign it. |
| `Register` | `ed25519_pubkey: bytes` (32), `signature: bytes` (64) | Ed25519 signature over `"hya.relay.v1/register\0" ‖ nonce` (`hya_relay::proto::register_signing_message`). |
| `Registered` | `room_id: string` | Registration accepted. |
| `Heartbeat` | `seq: uint64`, `pong: bool` | Liveness probe (`pong=false`) or reply echoing `seq` (`pong=true`); valid in both directions on every stream. |
| `Incoming` | `stream_id: string` | A client opened a stream; the host calls `Accept` with this id. Proxy-generated and unguessable. |
| `Chunk` | oneof `open{room_id}` \| `accept{stream_id}` \| `data: bytes` \| `close{}` \| `heartbeat` \| `error` | One data-stream frame. `data` is opaque end-to-end ciphertext; `close` ends the sender's direction. |
| `RelayError` | `code: RelayErrorCode`, `message: string` | Terminal failure; the WebSocket stand-in for a gRPC status. |

`RelayErrorCode` values equal the gRPC status codes they mirror
(`CANCELLED`=1, `UNKNOWN`=2, `INVALID_ARGUMENT`=3, `DEADLINE_EXCEEDED`=4,
`NOT_FOUND`=5, `ALREADY_EXISTS`=6, `PERMISSION_DENIED`=7,
`RESOURCE_EXHAUSTED`=8, `FAILED_PRECONDITION`=9, `INTERNAL`=13,
`UNAVAILABLE`=14, `UNAUTHENTICATED`=16; `UNSPECIFIED`=0 reads as `UNKNOWN`).
`hya_relay::proto::relay_error_code_to_grpc` / `relay_error_code_from_grpc`
convert between them.

### The transport abstraction

`hya_relay::transport::RelayTransport<Tx, Rx>` is any
`Sink<Tx, Error = TransportError> + Stream<Item = Result<Rx, TransportError>>
+ Send`. Each binding adapts its stream to it, and the relay state machines
are written once against it. Aliases: `HostControlTransport` (host end of
`Host`), `ProxyControlTransport` (proxy end of `Host`), and `ChunkTransport`
(either end of `Accept`/`Open`), all boxed as `BoxedTransport<Tx, Rx>`.

Semantics: the stream yields `Ok(message)` per frame, `Err(_)` for a failure,
and `None` after the peer closed its direction; closing the sink ends only
the local direction (half-close); sending after the peer is gone fails with
`TransportError::Closed`. `TransportError` variants: `Closed`,
`Status{code, message}` (a gRPC status or a WebSocket `RelayError`),
`Decode`, `Transport`.

`hya_relay::transport::memory::pair(capacity)` returns a connected in-memory
pair for tests; `MemoryTransport::inject_error` delivers a failure to the
peer.
