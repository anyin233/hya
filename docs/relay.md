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

The proxy server library exists (`hya_relay::server`, see
[Bindings](#bindings)). *Coming in later steps:* `hya proxy`, `hya serve --relay`, `hya serve relay
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

### Proxy behavior

`hya_relay::proxy::ProxyCore` is the proxy's state machine, written once
against the transport abstraction. A binding adapts each incoming stream and
calls `serve_host(ProxyControlTransport, PeerInfo)`,
`serve_open(ChunkTransport, PeerInfo)`, or
`serve_accept(ChunkTransport, PeerInfo)`; each returns a `'static` future the
binding spawns. `PeerInfo::new(id)` is an opaque client identity (normally the
client IP) used only for per-client limits. `ProxyCore::stats()` returns
`ProxyStats{rooms, streams, pending_streams}`; `ProxyCore::shutdown()` ends
every stream with `UNAVAILABLE`, refuses new ones, and waits until all served
streams have finished. The proxy keeps no state on disk.

**Registration.** On a `Host` stream the proxy sends `challenge{nonce}` (32
fresh random bytes, single use). The first host frame must be
`register{ed25519_pubkey, signature}` within the handshake timeout. The
signature must verify (Ed25519 `verify_strict`) over
`"hya.relay.v1/register\0" ‖ nonce`; the room id is derived from the key and
returned in `registered{room_id}`. The room is online while the control
stream stays open. Afterwards the proxy sends `incoming{stream_id}` per
opener and answers heartbeat probes with a pong; it does not probe itself.
A control stream that delivers no frame for the idle timeout is closed, so a
host must send heartbeats (default every 15 s).

**Room replacement.** A new valid registration for a room that is already
online **replaces** the current host rather than being refused: the old
control stream ends with `ALREADY_EXISTS` ("room was registered by a newer
control stream"), every stream of the old registration (waiting or spliced)
ends with `UNAVAILABLE`, and new opens reach the new host. This lets a host
whose control stream was cut re-register at once, before the proxy has
noticed the old stream is dead; only the key holder can do it. A host
connector that receives `ALREADY_EXISTS` should not reconnect in a tight
loop: another process holds the same identity.

**Eviction.** When a control stream ends (clean close, error, idle timeout),
its room is removed and every stream of that room ends with `UNAVAILABLE`.

**Open and accept.** The first frame of an `Open` stream must be
`open{room_id}` within the handshake timeout. A malformed room id is
`INVALID_ARGUMENT`; an offline room is `NOT_FOUND`. Otherwise the proxy
allocates a stream id (128 random bits, 32 lowercase hex characters), sends
`incoming{stream_id}` to the host, and waits for an `Accept` stream whose
first frame is `accept{stream_id}`. Each id can be accepted once; an
unknown, expired, or already accepted id is `NOT_FOUND`. If no accept arrives
within the accept timeout the opener gets `UNAVAILABLE`. On accept the proxy
sends `opened{}` to the opener — before any frame relayed from the host — and
splices the two streams. While waiting, the proxy buffers up to the
early-data limit of opener `data` and then stops reading the opener
(backpressure); the buffer is delivered to the host first, in order.

**Splice.** Once spliced:

- `data` is forwarded unchanged; the proxy never inspects or rewrites it.
- `close{}` is forwarded and ends that direction only (half-close); the
  other direction keeps flowing until it closes too. A stream that ends
  without `close` counts as a close.
- Heartbeats are **per leg**: the proxy answers a probe on the leg it
  arrived on and never forwards heartbeats, because each leg may cross
  different intermediaries with different idle cuts. Pongs are dropped.
  Heartbeats count as activity for the idle timeout.
- An `error` frame from one side is forwarded to the other and ends the
  stream; a transport failure on one side ends the other with
  `UNAVAILABLE`. A handshake frame (`open`, `accept`, `opened`) after the
  splice is `INVALID_ARGUMENT` to both sides.

**Errors.** Every failure the proxy reports is a final `RelayError` frame
followed by the end of its sending direction; the gRPC binding reports that
final frame as the stream status instead.

| Code | When |
| --- | --- |
| `INVALID_ARGUMENT` | Wrong or empty first frame; malformed room id; handshake frame after the splice. |
| `DEADLINE_EXCEEDED` | No first frame (registration, `open`, `accept`) within the handshake timeout; a leg or control stream idle past the idle timeout; a peer that stops reading for that long. |
| `NOT_FOUND` | `open` to an offline room; `accept` with an unknown, expired, or already accepted stream id. |
| `ALREADY_EXISTS` | Sent to a host whose room was taken over by a newer registration. |
| `RESOURCE_EXHAUSTED` | A room, stream, or per-client limit is reached; a `data` payload over the chunk limit (sent to both sides). |
| `FAILED_PRECONDITION` | A second `register` on a registered control stream. |
| `UNAVAILABLE` | The host did not accept in time; the room went offline or was replaced; the other side went away; the proxy is shutting down. |
| `UNAUTHENTICATED` | Registration key or signature is malformed or does not verify. |

**Limits** (`hya_relay::proxy::ProxyLimits`, ADR-0025 D8):

| Field | Default | Effect |
| --- | --- | --- |
| `max_rooms` | 1024 | Registered rooms; a replacement needs no new slot. Over: `RESOURCE_EXHAUSTED`. |
| `max_streams_per_room` | 64 | Concurrent streams (waiting or spliced) per room. Over: `RESOURCE_EXHAUSTED`. |
| `max_streams_per_peer` | 256 | Concurrent streams opened by one `PeerInfo`. Over: `RESOURCE_EXHAUSTED`. |
| `max_rooms_per_peer` | 16 | Rooms registered by one `PeerInfo`. Replacing a room the same client holds needs no new slot; a room taken over by another client frees the old owner's slot. Over: `RESOURCE_EXHAUSTED`. |
| `max_pending_registrations_per_peer` | 8 | Host control streams of one `PeerInfo` that have not finished registration (challenge sent, no valid `register` yet). Over: the new control stream gets `RESOURCE_EXHAUSTED` instead of a challenge. |
| `idle_timeout` | 120 s | A leg or control stream with no frame at all (heartbeats count), or a peer not taking a frame, for this long: `DEADLINE_EXCEEDED`. |
| `stream_rate_bytes_per_sec` | 8 MiB/s | Token-bucket cap on `data` bytes per stream and direction; excess is delayed, not dropped. `0` = unlimited. |
| `stream_rate_burst_bytes` | 1 MiB | Burst of that bucket. |
| `max_chunk_data` | 256 KiB | Largest `data` payload. Over: `RESOURCE_EXHAUSTED` to both sides. |
| `early_data_limit` | 64 KiB | Opener `data` buffered before the accept; beyond it the proxy stops reading. |
| `accept_timeout` | 10 s | Wait for the host's `Accept`; then `UNAVAILABLE` to the opener. |
| `handshake_timeout` | 10 s | Deadline for the first frame of every stream (the registration timeout for hosts). |

### Bindings

`hya_relay::server::RelayServer` serves both bindings of `hya.relay.v1` on
**one port** in front of one `ProxyCore`. `hya proxy` (coming in a later
step) is a thin CLI around it.

```rust
use hya_relay::server::{RelayServer, RelayServerConfig, TlsFiles};

let config = RelayServerConfig::new("0.0.0.0:8766".parse()?)
    .path_prefix("/relay")?                      // optional
    .tls(TlsFiles { cert: "cert.pem".into(), key: "key.pem".into() }) // optional
    .trust_forwarded(true)                       // only behind a hop that sets the headers
    .limits(hya_relay::proxy::ProxyLimits::default());
let (addr, serve) = RelayServer::bind(config, async { let _ = tokio::signal::ctrl_c().await; }).await?;
println!("listening on {addr}");
serve.await; // returns after the shutdown signal and the drain
```

`RelayServerConfig` (builder; all optional except the address):

| Method | Default | Meaning |
| --- | --- | --- |
| `new(bind: SocketAddr)` | — | Listen address (`127.0.0.1:0` picks a free port; `bind` returns the real one). |
| `path_prefix(&str) -> Result<_, RelayServerError>` | none | Serve both bindings under a prefix. Segments of `A-Za-z0-9-._~`, no `.`/`..`; leading and one trailing `/` are optional (`"relay"`, `"/relay/"`, `"/a/b"`). |
| `tls(TlsFiles { cert, key })` | plaintext | Terminate TLS with PEM files (certificate chain, then a PKCS#8, PKCS#1, or SEC1 key). rustls with the ring provider, TLS 1.2 and 1.3, ALPN `h2` and `http/1.1`. |
| `trust_forwarded(bool)` | `false` | Identify clients by forwarding headers (below). |
| `limits(ProxyLimits)` | defaults | The proxy core limits (see Limits). |
| `drain_timeout(Duration)` | 10 s | Upper bound of the shutdown drain. |

`RelayServer::bind(config, shutdown)` loads TLS, binds the listener, and
returns `(SocketAddr, RelayServe)`; `RelayServe` is the `Send` serve future.
Errors (`RelayServerError`): `InvalidPathPrefix`, `Bind{addr, source}`,
`Tls(message)`.

**Single-port routing.** Every connection is served as HTTP/1.1 or HTTP/2
(h2c prior knowledge in plaintext, ALPN `h2` under TLS), detected per
connection. A request whose `content-type` starts with `application/grpc`
goes to the gRPC binding; any other request goes to the WebSocket routes.
Anything that is not a relay route answers `404` with `content-type:
text/plain` and the body `hya relay`, so `hya relay doctor` can recognize the
proxy behind an intermediary. A gRPC request outside the prefix gets the
gRPC status `UNIMPLEMENTED`. HTTP/2 connections get keepalive pings every
30 s (20 s ack timeout).

**gRPC binding.** The tonic service `hya.relay.v1.Relay` at
`<prefix>/hya.relay.v1.Relay/{Host,Accept,Open}`; the prefix is stripped
before tonic routing. Each stream item is one message. The proxy's final
`error` frame is **not** sent as a message: it ends the call with the
matching gRPC status (`RelayErrorCode` = gRPC code, message = the error
message). A stream that ends without an error ends with status `OK`. The
response stays open until the proxy is done with both directions, so a
half-closed stream (`close{}` sent) keeps receiving. A cancelled call, reset
stream, or cut connection is a transport failure for the proxy (the other
side gets `UNAVAILABLE`); a client that just ends its request stream has
closed its direction.

**WebSocket binding.** `GET <prefix>/hya.relay.v1/ws/{host,accept,open}`
upgraded over HTTP/1.1 (WebSocket over HTTP/2 is not offered; a TLS client
should offer ALPN `http/1.1` for it). Every relay message is one **binary**
frame holding the protobuf encoding of the route's message:

| Route | Client → proxy | Proxy → client |
| --- | --- | --- |
| `ws/host` | `HostFrame` | `ProxyToHost` |
| `ws/accept` | `Chunk` | `Chunk` |
| `ws/open` | `Chunk` | `Chunk` |

A message is at most `max_chunk_data` + 1 KiB. WebSocket ping/pong is
answered but not required; the relay heartbeats are the liveness contract.
The proxy ends a stream with a close frame:

| Close code | When |
| --- | --- |
| `1000` | The stream ended without an error. |
| `4000 + code` | After a final binary frame carrying the `RelayError`; `code` is the `RelayErrorCode` (`4005` `NOT_FOUND`, `4008` `RESOURCE_EXHAUSTED`, `4014` `UNAVAILABLE`, …). The close reason repeats the error message (at most 123 bytes). |
| `1003` | The client sent a text frame (binary frames only). |

A client close frame ends the client's stream; a connection lost without a
closing handshake is a transport failure (the other side gets
`UNAVAILABLE`). A non-upgrade request to a WebSocket route is refused by the
upgrade check (`400`, `405`, or `426`).

**Client identity** (`PeerInfo`, used only for the per-client limits). By
default it is the socket's remote IP (IPv4-mapped IPv6 shown as IPv4). With
`trust_forwarded(true)` it is the first valid IP from, in order,
`CF-Connecting-IP` (first entry), `X-Real-IP` (first entry), and the leftmost
`X-Forwarded-For` entry; the socket IP when none parses. Enable it only when
every request reaches the proxy through a hop that overwrites these headers,
otherwise clients can pick their own identity.

**Graceful shutdown.** When the shutdown future completes the server stops
accepting (the port closes), ends every relay stream with `UNAVAILABLE`
(gRPC status, or error frame plus close `4014`), asks every connection to
finish (HTTP/2 `GOAWAY`, HTTP/1.1 close after the response), and waits up to
the drain timeout before cutting what is left.

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
