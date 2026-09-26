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
| `Open` / `ws/open` | `Chunk` | `Chunk` | Client side of one data stream; first frame `open{room_id}`. An offline room fails with `NOT_FOUND`. The proxy sends `opened{}` once the host accepted the stream. |

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
| `Chunk` | oneof `open{room_id}` \| `accept{stream_id}` \| `data: bytes` \| `close{}` \| `heartbeat` \| `error` \| `opened{}` | One data-stream frame. `data` is opaque end-to-end ciphertext; `close` ends the sender's direction; `opened` (field 7) is the proxy's open acknowledgement. |
| `RelayError` | `code: RelayErrorCode`, `message: string` | Terminal failure; the WebSocket stand-in for a gRPC status. |

**Open acknowledgement.** On an `Open` stream the proxy sends exactly one
`opened{}` frame once the host's `Accept` for that stream has been spliced;
it precedes every frame relayed from the host. Openers should wait for
`opened` before sending `data`. Data sent earlier is not lost, but the
proxy buffers at most a small bounded amount of it (64 KiB by default) and
otherwise stops reading the opener's stream (backpressure) until the splice
is up. If the host does not accept within the accept timeout (10 s by
default), the opener's stream fails with `UNAVAILABLE`. `opened` is a new
field of the `Chunk` oneof, so older peers that ignore it still decode every
other frame.

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

### Tunnel encryption

Every data stream carries one end-to-end encrypted tunnel,
`hya_relay::tunnel::NoiseStream`. It runs the Noise handshake over the
stream's `Chunk` frames and then behaves as a plain tokio byte stream
(`AsyncRead + AsyncWrite`), so the backend can serve HTTP over it with
`hyper` and a client bridge can splice a TCP connection into it.

| Property | Value |
| --- | --- |
| Pattern | `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s` (`snow`, pure-Rust primitives) |
| Initiator | The client: knows the backend's static X25519 public key and the 32-byte PSK (both from the link fragment) |
| Responder | The backend: holds the static X25519 keypair and the PSK |
| Prologue | `"hya.relay.v1/noise\0"` followed by the room id (ASCII), binding the session to its room (`tunnel::prologue`) |
| Handshake | Two messages, each one `Chunk.data`: `-> psk, e, es` (sent as soon as the initiator is constructed), `<- e, ee`. Empty payloads. |
| Records | Every Noise transport message is exactly one `Chunk.data`; the protobuf field is its length. At most 65535 bytes of ciphertext: plaintext is split into records of at most 16 KiB by default (`TunnelConfig::with_max_record_plaintext`, 1..=65519) plus a 16-byte tag. |
| End of direction | An encrypted empty record (the close record), then `Chunk.close{}` |

Usage (the host connector and bridge wire this up; shown with an in-memory
transport):

```rust
use hya_relay::keys::{Psk, StaticKeypair};
use hya_relay::tunnel::{NoiseStream, TunnelConfig};

// Backend (responder), per accepted stream:
let tunnel = NoiseStream::respond(accept_transport, &room_id, &keypair, &psk, TunnelConfig::default()).await?;
// Client (initiator), per opened stream:
let tunnel = NoiseStream::initiate_link(open_transport, &link, TunnelConfig::default()).await?;
```

`NoiseStream::initiate(transport, room_id, server_public, psk, config)` is
the same without a link. Handshakes have no built-in timeout; wrap them in
`tokio::time::timeout`.

Behavior:

- **Authentication.** A client with the wrong PSK, the wrong server key, or a
  different room fails at the first handshake message on the backend
  (`TunnelError::Handshake`), before any application byte exists. The
  backend then sends `Chunk.close{}` (best effort) and drops the stream; the
  client's handshake fails with `TunnelError::Handshake` too.
- **Half-close.** `shutdown()` sends the close record and `Chunk.close{}`.
  The peer's reads then return EOF, and the peer can keep writing until it
  shuts down its own direction. Writing after `shutdown()` fails with
  `BrokenPipe`.
- **Truncation.** A `Chunk.close{}` or end of stream that is not preceded by
  the close record fails the read with `UnexpectedEof`
  (`TunnelError::Truncated`), so a hop cannot silently cut a response short.
- **Tampering.** A record that fails authentication (modified, replayed,
  dropped, or reordered — Noise nonces are implicit counters) fails the read
  with `InvalidData` (`TunnelError::Decrypt`). No plaintext from that record
  is returned, and every later read and write on the stream fails.
- **Relay errors.** A `Chunk.error` frame or a transport status fails the
  stream with `TunnelError::Relay { code, message }` (I/O kind derived from
  the code: `NOT_FOUND` → `NotFound`, `UNAVAILABLE`/`CANCELLED` →
  `ConnectionReset`, `DEADLINE_EXCEEDED` → `TimedOut`, …).
- **Heartbeats.** `Chunk.heartbeat` frames are invisible to the byte stream,
  during and after the handshake. A probe (`pong=false`) is answered with a
  pong echoing its `seq`, also after the local direction is shut down. The
  tunnel does not send probes itself. Other frame kinds (for example a proxy
  acknowledgement) are ignored.
- **Backpressure.** A record is encrypted only when the transport can accept
  it, so a slow peer blocks `write` instead of growing a buffer.
- **Errors.** Data-path failures are `std::io::Error`s whose inner error
  (`get_ref()`) is a `TunnelError`: `InvalidConfig`, `Handshake`,
  `Transport`, `Relay{code, message}`, `Decrypt`, `Truncated`, `Protocol`.
  Messages never contain key material or payload bytes.

Keys (`hya_relay::keys`): `StaticKeypair::generate()` /
`StaticKeypair::from_secret([u8; 32])` (derives the public key; `secret()`,
`public()`), and `Psk::generate()` / `Psk::from_bytes([u8; 32])`
(`as_bytes()`). Secrets are zeroized on drop and redacted in `Debug`;
persisting them is the host connector's job.

### Relay link grammar

A relay link is the one string a client needs, and it is the credential:
whoever holds it controls the backend. `hya_relay::link::RelayLink` parses
and formats it.

```text
hya://<host>[:port][/<prefix>]/<room_id>[?t=auto|grpc|ws]#<b64url(x25519_pub)>.<b64url(psk)>
hya+insecure://<host>[:port][/<prefix>]/<room_id>[?t=…]#<…>.<…>
```

| Part | Meaning |
| --- | --- |
| `hya://` | TLS toward the first hop; default port **443**. |
| `hya+insecure://` | Plaintext toward the first hop (LAN, tailnet, dev); default port **80**. Name the port explicitly for a bare `hya proxy` (`:8766`). |
| `<host>` | The **public** relay host the backend was given, never the proxy's listen address. DNS name, IPv4, or bracketed IPv6; lowercased. No userinfo. |
| `<prefix>` | Optional path prefix (one or more segments of `A-Za-z0-9-._~`, no `.`/`..`) under which the relay is published. |
| `<room_id>` | 26 lowercase base32 characters (`a-z2-7`): the first 26 characters of unpadded base32 of `sha256(ed25519_pub)`. `hya_relay::link::room_id_from_ed25519`. |
| `t` | Transport binding hint: `auto` (default; gRPC, falling back to WebSocket), `grpc`, or `ws`. Given twice is an error; other query parameters are ignored. |
| fragment | The backend's Noise static public key (X25519) and the PSK, each exactly 32 bytes, unpadded base64url, joined by `.`. |

Example:
`hya://relay.example.com/hya/eh7ddx5bksrgcytl7bkai36se4?t=ws#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8.__________________________________________8`.

Formatting is canonical: the host is lowercased, the default port and
`t=auto` are omitted. `RelayLink::to_secret_string()` returns the full link;
`Display`, `Debug`, and `RelayLink::redacted()` show only
`hya[+insecure]://host[:port][/prefix]/<room_id>`, and `LinkError` messages
never contain key material. `LinkError` variants: `UnknownScheme`,
`InvalidHost`, `InvalidPort`, `InvalidPath`, `MissingRoom`, `InvalidRoomId`,
`UnknownTransport`, `InvalidQuery`, `MissingFragment`, `InvalidFragment`,
`InvalidBase64{field}`, `InvalidKeyLength{field, len}`.

`RelayAddress` (from a link, or `RelayAddress::parse_proxy_url("https://relay.example.com/hya")`)
yields the binding endpoints:

| Method | Example (`hya://relay.example.com/hya/…`) |
| --- | --- |
| `origin()` | `https://relay.example.com` |
| `base_url()` / `grpc_url()` | `https://relay.example.com/hya` (gRPC paths `/hya.relay.v1.Relay/<Method>` go below it) |
| `ws_url(WsRoute::Host)` | `wss://relay.example.com/hya/hya.relay.v1/ws/host` |

`hya+insecure://` links map to `http://` and `ws://` the same way.
