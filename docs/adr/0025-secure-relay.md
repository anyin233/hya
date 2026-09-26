# A secure relay for remote control of a backend

Status: Accepted, 2026-09-26.

`hya serve` listens on loopback and has no authentication: the bind address is
its only protection (ADR-0022, ADR-0023). Users want to drive a backend from
another machine — a laptop controlling a workstation, a phone-side terminal,
a backend behind NAT — without opening a port on it and without trusting
whoever runs the machine in the middle. The middle machine is usually
published as HTTPS, but through an unknown chain: nginx, Cloudflare Tunnel,
a CDN, Caddy, Tailscale, a PaaS ingress, a corporate proxy.

## Decision

A new `hya proxy` command runs a **Relay**: a dumb rendezvous that forwards
ciphertext between a backend and its clients. Encryption lives only in the
backend and the client. The **Relay link** carries everything a client needs,
including the key, and holding it is full control.

### Roles

```text
TUI ──HTTP──▶ Bridge (loopback, client machine)
                │ Noise NKpsk0, one session per TCP connection
                │ binding: gRPC (HTTP/2) | WebSocket (HTTP/1.1+)   [t=auto]
                ▼
     ┄┄ any HTTPS hop(s): nginx / cloudflared / CDN / Caddy / … ┄┄
                ▼
           hya proxy 0.0.0.0:8766  (gRPC + WS on one port;
                │                   sees room_id + ciphertext only)
                │ control stream Host(room) → Incoming{stream_id}
                ▼ backend opens Accept(stream_id)   (own binding choice)
           Host connector in hya serve ──Noise responder──▶ hyper → v1 router
```

- **`hya proxy`** (relay only) listens on `0.0.0.0:8766` by default
  (`--host`, `--port`). It keeps **Rooms** in memory, one per connected
  backend, and splices client streams to the backend's streams. It never
  holds a key that decrypts traffic, and it persists nothing.
- **Host connector** (in `hya serve`): `hya serve --relay <public-url>` joins
  a relay at start; a running backend joins or leaves with
  `hya serve relay connect <url>|disconnect|status|link|rotate`.
- **Bridge** (in `hya`, on the client machine): `hya --connect <link>`,
  `hya bridge <link>`, or the TUI's `/connect-remote <link>` starts a loopback
  HTTP listener. The TUI talks plain `hya.v1` HTTP/SSE/WebSocket to the
  bridge and needs no crypto of its own; it switches to it with its existing
  `switchServer`.

### D1 — Tunnel: raw HTTP over an encrypted pipe

Each TCP connection a client opens to the bridge becomes one relay stream and
one Noise session. The backend feeds the decrypted bytes into `hyper`'s
`serve_connection` over the existing `hya_server::router`. REST, SSE, and the
PTY WebSocket upgrade therefore work unchanged, with no per-route relay code.
(`V1Grpc` already dispatches by calling the router with an `http::Request`, so
the router is known to work as a plain service.)

### D2 — Noise `Noise_NKpsk0_25519_ChaChaPoly_BLAKE2s`

Implemented with the `snow` crate.

- **NK:** the client knows the backend's static X25519 key from the link, so
  it authenticates the backend; the client itself has no static key.
- **psk0:** the link's 32-byte pre-shared key is mixed in at the first
  message. A client without it fails the handshake before any application
  byte is decrypted or delivered to the router.
- **Forward secrecy** from fresh ephemeral keys per stream.
- **Framing:** Noise messages are at most 65535 bytes; application data is
  chunked (default 16 KiB, configurable) and each record is exactly one
  relay `Chunk.data` payload (the protobuf field is the length prefix). An
  encrypted empty record marks the end of a direction, so a bare close is
  detected as truncation. Any decryption failure closes the stream.

### D3 — Room identity and ownership

The backend's relay identity is an Ed25519 keypair; `room_id =
base32(sha256(ed25519_pub))[..26]`. A host registers a room by signing a
fresh proxy-issued nonce, so nobody without the private key can take over or
squat a room id. The Noise static key (X25519) is a separate key stored beside
it. Both, with the link's PSK, persist per database in
`<db>.relay-identity.json` next to `<db>.lock` (mode 0600, versioned JSON), so
a backend's link survives restarts and two databases never share a room; an
in-memory database, or `--relay-ephemeral`, uses a throwaway identity. (First
drafted as one `$XDG_STATE_HOME/hya/relay/identity.json` per user; changed when
the host connector landed, because the room belongs to the backend of one
database, whose lock holder alone writes the file.)

### D4 — Link grammar

```text
hya://<public-host>[:port][/<prefix>]/<room_id>?t=<auto|grpc|ws>#<b64url(x25519_pub)>.<b64url(psk)>
```

- `hya://` means TLS to the first hop (default port 443); `hya+insecure://`
  means plaintext (LAN, tailnet, development).
- `<public-host>`, port, and prefix are the **public** URL the backend was
  given with `--relay https://…`, never the proxy's own listen address.
- `t` is the transport binding hint, default `auto`.
- The fragment (server key and PSK) is never sent to any hop: clients strip it
  before connecting. End-to-end security never depends on TLS or on any
  intermediary; whatever terminates TLS sees only Noise ciphertext.
- `hya serve relay rotate` issues a new PSK (and with it a new link) and
  rejects handshakes with the old one. There is no per-device list: the link
  is the credential.

### D5 — Relay origin

Requests that arrive through the host connector carry an axum extension
`Origin::Relay`. Relay-control rpcs (connect, disconnect, rotate, link) and
`process` stop are refused from relay origin; they stay loopback-only.
Everything else is allowed, because holding the link means owner trust.

### D8 — Proxy limits

The proxy enforces, with flags and conservative defaults: maximum rooms,
maximum concurrent streams per room, an idle timeout per stream, and a byte
rate cap per stream. Nothing is persisted; a restarted proxy has no rooms
until hosts re-register.

### D9 — Work through any intermediary

We assume only that the path contains "some HTTPS hop that may speak
HTTP/1.1 only, may cut idle or long streams, may rewrite Host and paths, and
may buffer".

- **One protocol, two bindings.** `hya.relay.v1` (`proto/hya/relay/v1`, a
  separate package from `hya.v1`) defines `Host`, `Accept`, and `Open` as
  bidirectional message streams, once. **Relay bindings:**
  1. **gRPC** (HTTP/2 with trailers) — primary.
  2. **WebSocket** at `GET <prefix>/hya.relay.v1/ws/{host|accept|open}`,
     binary frames carrying the same protobuf messages; errors are a final
     `RelayError{code,message}` frame plus a close code, mirroring gRPC status.
     This is the fallback for hops that are HTTP/1.1-only or drop trailers
     or HTTP/2 upstreams.
  `hya proxy` serves both on one port (routing on `content-type:
  application/grpc` versus a WebSocket upgrade), so the operator configures
  nothing binding-specific.
- **Negotiation (`t=auto`).** Try gRPC with a short handshake timeout; on a
  failure that points at an intermediary (an HTTP/1.1 response, a 4xx/5xx from
  the hop, missing trailers, a protocol error), fall back to WebSocket and
  remember the working binding per relay host for the process lifetime.
  `t=grpc` and `t=ws` pin a binding. The host connector and the bridge
  negotiate independently; they may be behind different networks.
- **Heartbeat and reconnect.** Every stream sends an application heartbeat
  (default 15 s, `--relay-heartbeat`), below common idle cuts (nginx 60 s,
  Cloudflare about 100 s), plus HTTP/2 keepalive or WebSocket ping. The host
  control stream reconnects with jittered backoff and re-registers its room.
  Data streams are one per TCP connection, so a cut stream drops only that
  HTTP connection; the TUI already recovers (SSE `sinceSeq` resync, PTY
  `cursor` reattach, retry of idempotent reads).
- **No reliance on hop behavior.** Nothing depends on the client IP (used
  only for rate limits; `--trust-forwarded` reads `X-Forwarded-For` /
  `CF-Connecting-IP`), the Host header, a fixed port, or TLS at a particular
  hop. The proxy can terminate TLS itself (`--tls-cert`/`--tls-key`) or run
  plaintext behind a terminator or tunnel. Clients use rustls with native and
  webpki roots, `--relay-ca <pem>` for private CAs, and SNI = the link host.
- **Path prefix.** `--path-prefix` on the proxy and a prefix in the link, in
  both bindings, for hops that can only route by path and cannot rewrite gRPC
  paths.
- **Diagnostics.** `hya relay doctor <proxy-url|link>` probes TLS, each
  binding, prefix routing, and a bounded idle-cut measurement, and recommends a
  `t=` value; `hya serve relay status` shows the same.
- **First-class deployments.** Each has a recipe in `docs/relay.md`, a manual
  end-to-end check before release, and matching cases in the in-process
  conformance suite:
  1. **Cloudflare Tunnel** (`cloudflared` → `http://localhost:8766`). The
     default origin connection is HTTP/1.1, so the WebSocket binding is used;
     with `http2Origin: true` gRPC may work and `auto` picks it. The edge cuts
     idle streams, which the heartbeat covers. No inbound port on the proxy
     host.
  2. **nginx**: `grpc_pass` on an `http2` listener for gRPC and an
     `Upgrade`/`Connection` location for WebSocket, with long
     `grpc_read_timeout` / `proxy_read_timeout` and `proxy_buffering off`.
  3. **Caddy**: `reverse_proxy h2c://127.0.0.1:8766` carries both bindings,
     with automatic HTTPS.
  4. **Tailscale**: (a) plain tailnet, `hya+insecure://<100.x or
     MagicDNS name>:8766` — WireGuard encrypts the hop and Noise encrypts end
     to end, so no TLS is needed; the proxy may run on the backend machine.
     (b) `tailscale serve` (tailnet HTTPS, `*.ts.net` certificates) or
     `tailscale funnel` (public), linked as `hya://<name>.ts.net`.
  Direct TLS on the proxy (`--tls-cert`/`--tls-key`) is documented as well.
  Other intermediaries (Traefik, HAProxy, ngrok, frp, …) are covered by the
  principle and the conformance suite but get no dedicated recipe.

## Threat model

Assets: the backend's ability to run tools on its machine (shell, file
edits), the session contents, and the link secret.

- **Honest-but-curious proxy or intermediary.** Sees room ids, stream ids,
  connection times, stream counts, byte counts and timing, client IPs (or the
  last hop's), and the TLS-level metadata of its own hop. It sees no request,
  response, path, header, or session content: every application byte is
  inside Noise. This holds equally for anything that terminates TLS in front
  of it (nginx, Cloudflare's edge, `cloudflared`, Caddy, a corporate TLS
  inspector) — TLS protects only the hop; Noise protects end to end.
- **Malicious proxy.** It can drop, delay, reorder, or cut streams (denial of
  service), and it can open streams to a room. It cannot complete a handshake
  without the PSK, cannot impersonate the backend without the backend's
  static key (NK authenticates the responder), and cannot alter or inject
  records without failing AEAD, which closes the stream. It cannot register
  someone else's room without the Ed25519 key. Traffic analysis (sizes and
  timing of turns) remains possible and is accepted.
- **Replay.** Replaying a recorded handshake or records fails: each session
  depends on fresh ephemeral keys on both sides, and Noise nonces reject
  reordered or repeated records within a session. Replaying a registration
  fails because each proxy nonce is single-use.
- **Link leak.** A leaked link is full control of the backend, like a leaked
  SSH key without a passphrase. The response is `hya serve relay rotate`,
  which invalidates every earlier link at the next handshake; open streams
  from the old link are closed on rotate. The link's secret part is in the URL
  fragment so it does not end up in hop logs, and the backend never prints it
  except through `hya serve relay link`/`--relay` start output and never
  writes it to the discovery file.
- **Remote privilege.** Relay-origin requests cannot change the relay
  configuration or stop the process (D5), so a link holder cannot lock out the
  local owner or rotate the key away from them.
- **DoS and limits.** The proxy caps rooms, streams per room, idle time, and
  per-stream rate (D8). Handshakes are cheap for the backend to reject (one
  DH plus a failed AEAD). The proxy is not an authorization point; a flood of
  failed handshakes costs bandwidth, not access.
- **Out of scope.** A compromised backend or client machine; traffic
  analysis resistance; availability guarantees from a third-party proxy.

## Rejected alternatives

- **Client crypto in TypeScript.** Bun has WebCrypto X25519 and AES-GCM, but
  a second implementation of the tunnel in the TUI doubles the audit surface
  and adds dependencies to a package that must stay self-contained. The Rust
  bridge keeps one implementation and lets the TUI stay a plain HTTP client.
- **Device approval list.** Per-device keys with approve/revoke would allow
  revoking one device, but need an approval UI on the backend, a store, and a
  first-contact flow. Link = credential plus rotate covers the need for now.
- **gRPC-Web.** It has no client or bidirectional streaming, which the tunnel
  needs.
- **TLS-only security.** Relying on TLS to the proxy would make every
  TLS-terminating intermediary (and the proxy operator) able to read and drive
  the backend, and would tie security to how each deployment is configured.

## Consequences

- `hya serve` gains an outbound network path; it is off unless `--relay` or
  `hya serve relay connect` is used.
- A new crate `hya-relay` (tonic, prost, `snow`, `ed25519-dalek`,
  `tokio-tungstenite`) holds the protocol, link type, Noise stream, proxy core,
  and both bindings; it stays free of runtime crates.
- Remote clients see the backend's filesystem, not their own: they must pick a
  Project (ADR-0024), and local-path attachments are read on the client and
  sent inline.
- A relay adds latency per request and one Noise handshake per HTTP
  connection; clients keep connections alive.
- Rotating the link disconnects every remote client, including legitimate
  ones, which must be given the new link.
