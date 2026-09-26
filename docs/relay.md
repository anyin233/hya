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

The proxy server library (`hya_relay::server`, see [Bindings](#bindings)),
the client library (`hya_relay::client`, see
[Client transport](#client-transport)), the `hya proxy` command, and
`hya relay doctor` exist, and so do the client side, `hya bridge` and
`hya --connect <link>` ([Connecting from a client](#connecting-from-a-client)),
and the backend side, `hya serve --relay` and `hya serve relay …`
([Hosting a backend on a relay](#hosting-a-backend-on-a-relay)), and the
TUI's `/connect-remote` / `/disconnect-remote`
([From a running TUI](#from-a-running-tui-connect-remote)).

### `hya proxy`

```sh
hya proxy --port 8766
```

Runs the relay proxy standalone: a blind Noise rendezvous a backend and a
client reach each other through (see [Proxy behavior](#proxy-behavior)). It
is dispatched before any runtime composition — no config, providers,
plugins, MCP, or session store — so it starts as fast as `hya update`.
Readiness line on stdout, exactly:

```text
hya proxy listening on <scheme>://<addr><prefix>
```

`<scheme>` is `https` when `--tls-cert`/`--tls-key` are given, else `http`;
`<addr>` is the bound socket address (so `--port 0` picks a free port and
still prints it); `<prefix>` is the normalized `--path-prefix`, or empty.
Operational notices go to stderr. `hya proxy` stops on SIGINT or SIGTERM: it
stops accepting, drains every stream (`RelayServer`'s graceful shutdown, see
[Bindings](#bindings)), and exits 0.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--host <HOST>` | `0.0.0.0` | Bind host. The proxy has no local trust boundary (Noise encrypts the payload end to end), so the default is every interface. |
| `--port <PORT>` | `8766` | Bind port. `0` picks a free port. |
| `--tls-cert <PEM>` | none | Certificate chain; requires `--tls-key`. |
| `--tls-key <PEM>` | none | Private key; requires `--tls-cert`. |
| `--path-prefix <PREFIX>` | none | Serve both bindings under a path prefix. |
| `--trust-forwarded <HEADER>` | off | Identify clients by the address in exactly one forwarding header instead of the socket address: `cf-connecting-ip`, `x-real-ip`, or `x-forwarded-for` (its **rightmost** entry, the one your hop appended). Other forwarding headers are ignored. Only safe when every client reaches the proxy through a hop that sets exactly that header; see [Client identity](#bindings) and the [recipes](#deployment-recipes). |
| `--max-rooms <N>` | `1024` | [`ProxyLimits::max_rooms`](#proxy-behavior). |
| `--max-streams-per-room <N>` | `64` | `ProxyLimits::max_streams_per_room`. |
| `--max-streams-per-peer <N>` | `256` | `ProxyLimits::max_streams_per_peer`. |
| `--max-streams <N>` | `8192` | `ProxyLimits::max_streams` (the whole proxy). |
| `--max-rooms-per-peer <N>` | `16` | `ProxyLimits::max_rooms_per_peer`. |
| `--max-pending-registrations-per-peer <N>` | `8` | `ProxyLimits::max_pending_registrations_per_peer`. |
| `--max-pending-registrations <N>` | `256` | `ProxyLimits::max_pending_registrations` (the whole proxy). |
| `--idle-timeout-secs <N>` | `120` | `ProxyLimits::idle_timeout`. |
| `--stream-rate-bytes-per-sec <N>` | `8388608` | `ProxyLimits::stream_rate_bytes_per_sec` (`0` = unlimited). |
| `--stream-rate-burst-bytes <N>` | `1048576` | `ProxyLimits::stream_rate_burst_bytes`. |
| `--max-chunk-data <N>` | `262144` | `ProxyLimits::max_chunk_data`. |
| `--early-data-limit <N>` | `65536` | `ProxyLimits::early_data_limit`. |
| `--max-early-data-bytes <N>` | `67108864` | `ProxyLimits::max_early_data_bytes` (the whole proxy). |
| `--accept-timeout-secs <N>` | `10` | `ProxyLimits::accept_timeout`. |
| `--handshake-timeout-secs <N>` | `10` | `ProxyLimits::handshake_timeout`. |
| `--drain-timeout-secs <N>` | `10` | `RelayServerConfig::drain_timeout`. |

Every limit flag mirrors a [`ProxyLimits`](#proxy-behavior) field one for
one; durations are given in whole seconds (nothing else in `hya` parses
`10s`-style durations yet). See [Deployment recipes](#deployment-recipes)
for complete configs in front of `hya proxy`.

### `hya relay doctor`

```sh
hya relay doctor https://relay.example.com/hya
hya relay doctor hya://relay.example.com/hya/<room_id>      # a redacted link
hya serve relay link | hya relay doctor - --measure-idle     # a full link, on stdin
```

Probes a relay path end to end and recommends a `t=` value (ADR-0025 D9).
The probes only need the relay address, so the target is a proxy URL
(`https://…`/`http://…`, the same origin you'd pass to `--relay`) or a
**redacted** `hya://`/`hya+insecure://` link (no `#…`, as `hya serve relay
status` prints it). A full link works too, but it is a credential: pass it
as `-` and write it to stdin; given as an argument (visible in process
listings) it is accepted with a warning on stderr. **The link's secret is
never printed**, only its redacted form
(`hya[+insecure]://host[:port][/prefix]/<room_id>`), and error messages cut
any echoed input at `#`.

| Flag | Meaning |
| --- | --- |
| `--relay-ca <PEM>` | Extra trusted CA certificates, for a private CA. |
| `--timeout <SECS>` | Deadline for each probe (default 5s). |
| `--measure-idle` | Also measure how long an idle stream survives on this path (bounded at 130s). It opens a stream to the room, so it needs the **full** link (its open token, see [Open tokens](#the-hyarelayv1-protocol)) of a room that currently has a host registered — a proxy URL or a redacted link cannot be measured. |
| `--json` | Emit the report as JSON instead of text. |

The report covers reachability/TLS, the gRPC probe result and reason, the
WebSocket probe result and reason, whether the path prefix routes correctly,
the optional idle measurement, and the recommended `t=` value with one-line
advice:

| `ProbeFailureKind` | Advice |
| --- | --- |
| `NoHttp2`, `TrailersStripped`, `HopRejected`, `Timeout` | This hop does not carry gRPC end to end; pin or let `auto` pick `t=ws`. |
| `WrongPath` | The path prefix does not match the proxy's `--path-prefix`. |
| `Tls` | Check `--relay-ca` (private CA) or the host name. |
| `Connect` | Cannot reach the host/port; check the address and firewall. |

Exit status: **0** when at least one binding works, **1** when neither does.
See each [deployment recipe](#deployment-recipes) for the matching `hya
relay doctor` command and expected recommendation, and the
[troubleshooting table](#troubleshooting) keyed by doctor output.

### Hosting a backend on a relay

The **host connector** in `hya serve` puts a backend on the relay: it keeps
a control stream to `hya proxy` open and registers the backend's **room**,
and for every client stream it answers the Noise handshake and serves the
ordinary `/v1` router over the decrypted bytes — REST, SSE, and the PTY
WebSocket work unchanged. Nothing listens on a new port: the connection to
the relay is outbound.

```sh
# Foreground: join at start. stdout keeps the readiness line; the link goes
# to stderr once, clearly marked.
hya serve --relay https://relay.example.com/hya
#   hya server listening on http://127.0.0.1:8080
#   hya relay link: hya://relay.example.com/hya/eh7ddx5bksrgcytl7bkai36se4#AAEC….____…
#   hya: the relay link is a secret: anyone holding it controls this backend (…)

# The backend daemon of a database (ADR-0023): `start` prints the link.
hya serve start --relay https://relay.example.com/hya
hya serve restart          # the new daemon rejoins the same relay, same link

# A running backend: join, inspect, leave, re-key.
hya serve relay connect http://100.64.0.7:8766 --transport ws
hya serve relay status     # state, proxy, room, binding, streams, last error
hya serve relay link       # the full link, on stdout
hya serve relay rotate     # new key: every earlier link stops working
hya serve relay disconnect # the room is released, relay clients are cut off
```

`--relay` takes the relay's **public** URL — the one clients reach, never
the proxy's own listen address: `https://host[:port][/prefix]` (TLS to the
first hop) or `http://…` for plaintext on a LAN or tailnet; the link forms
`hya://…` and `hya+insecure://…` are accepted too. The link is built from it
(see [Relay link grammar](#relay-link-grammar)). Flags of `hya serve`,
`hya serve start`, and `hya serve restart`:

| Flag | Meaning |
| --- | --- |
| `--relay <URL>` | Join this relay at start and print the link. |
| `--relay-transport auto\|grpc\|ws` | The binding (default `auto`: gRPC, falling back to WebSocket); also the link's `t=`. |
| `--relay-ca <PEM>` | Extra trusted CA certificates for the relay's TLS. |
| `--relay-ephemeral` | A throwaway identity instead of the identity file: the link dies with the process. |
| `--relay-heartbeat <SECONDS>` | Heartbeat interval of the relay streams (default 15; three silent intervals mean a dead peer). |

**The link is a secret.** Whoever holds it controls the backend — it can run
tools, edit files, and open a shell on the backend's machine. The backend
prints it only on the start output (`hya serve --relay` on stderr,
`hya serve start|restart --relay` on stderr, `hya serve relay connect`,
`hya serve relay rotate`) and through `hya serve relay link`; it never writes
it to the discovery file or the daemon log (a daemon started with `--relay`
does not print it; `hya serve start` reads it over the loopback rpc and
prints it). Share it the way you would share an SSH private key. A leaked
link is revoked with `hya serve relay rotate`, which also closes every open
relay connection — including legitimate ones, which need the new link.

**Identity.** A backend on a file database keeps its relay identity in
`<db>.relay-identity.json`, next to `<db>.lock` (mode 0600; created on first
use; refused when readable by other users):

```json
{"version": 1, "ed25519": "<b64url>", "x25519": "<b64url>", "psk": "<b64url>"}
```

`ed25519` is the room key (`room_id = base32(sha256(pub))[..26]`), `x25519`
the Noise static key, and `psk` the link's pre-shared key; each is a 32-byte
secret, unpadded base64url. The same database therefore keeps the same room
and link across restarts; `rotate` replaces only `psk`. An in-memory database
(`--db ""`) uses a throwaway identity for the process lifetime, and
`--relay-ephemeral` (or `connect --ephemeral`) a throwaway identity for that
connection.

**Restart.** The relay connection is not persisted across independent
starts: a plain `hya serve start` joins no relay. While joined, the discovery
file `<db>.server.json` records the relay's public settings (never the link
or a key), and `hya serve restart` re-passes them to the new daemon unless
it is given its own `--relay`:

```json
{"url": "http://127.0.0.1:53124", "pid": 4242, "version": "…", "startedAt": 1790433521484,
 "relay": {"proxyUrl": "https://relay.example.com/hya", "transport": "auto", "ephemeral": false}}
```

(`ca` and `heartbeatSecs` appear when set.) `hya serve relay disconnect`
removes the record, so a later restart stays off the relay.

**Behavior.**

- The control stream reconnects with jittered backoff (1 s doubling to
  60 s; after `ALREADY_EXISTS` — another process holds the room identity —
  30 s doubling to 10 min) and re-registers the room; `status` shows
  `backoff` and the last error meanwhile. Clients' open connections drop
  with a cut stream and are retried by the client.
- The connector registers the hash of the room's [open token](#the-hyarelayv1-protocol),
  so the proxy refuses opens without the link (they look like an offline
  room) before this backend hears of them. `rotate` sends the new hash on
  the same control stream (`update_open_token`) and waits for the proxy's
  confirmation, so old links are refused at the proxy from then on.
- Each relay stream gets a 3 s Noise handshake deadline (the client sends
  its hello right after `opened`); failed handshakes are logged without key
  material. At most 16 streams are in the handshake and, separately, at
  most 64 are being served at once (`RelayHostConfig::max_handshakes` /
  `max_streams`): a stream takes a serving slot only once it is
  authenticated, so stalled handshakes never crowd out working streams.
- Relay streams are served with HTTP/1.1 (with upgrades, for the PTY
  WebSocket) or HTTP/2, whichever the client speaks.
- At shutdown the live event streams of relay clients get the same last
  `serverStopping {reason}` frame as local ones; after the drain the
  connector closes its control stream (the proxy releases the room) and gives
  the remaining relay streams 5 s before closing them.

**Relay origin (ADR-0025 D5).** Requests that arrive through the relay carry
the server-side request extension `hya_server::Origin::Relay`. They may use
the whole `/v1` API — holding the link means owner trust — except:

> **The link is full control of the backend.** The refusals below are a UX
> guard against accidents, **not a security boundary**: a link holder can
> run shell commands and PTYs as the backend's user, and from there call the
> loopback `RelayControl` rpcs, read or rotate the link, or stop the
> process. Share a link only with someone you would give a shell to, and
> rotate it (`hya serve relay rotate`) when that changes.

| Refused with `permission_denied` (HTTP 403) | Why |
| --- | --- |
| Every `RelayControl` rpc (`/v1/relay/*`) | A link holder must not change the relay, read the link, or rotate it away from the local owner. |
| `Process.DisposeProcess`, `Process.UpgradeProcess` | A link holder must not stop or replace the backend. |

`RelayControl` is also refused for a TCP peer that is not a loopback address
(a server bound to `0.0.0.0`), for a request whose client address is unknown
(fail-closed: no TCP peer and no gRPC peer), and for browser requests (an
`Origin` or `Sec-Fetch-Site` header), because the server's CORS policy
mirrors any origin; the gRPC binding refuses non-loopback and unknown peers
the same way.

**No browsers over the relay.** A relay-origin request that carries an
`Origin`, `Sec-Fetch-Site`, `Sec-Fetch-Mode`, or `Sec-Fetch-Dest` header is
refused before any route runs — `403 {"error":{"code":"permission_denied",
"message":"browser requests are not accepted over the relay …"}}` — so a web
page can neither fetch through a bridge (CORS request or preflight) nor open
a WebSocket through it (browsers always send `Origin` on a WebSocket
handshake). hya's own clients (the TUI, `hya-client`, the SDK, curl) send
none of these headers.

**Allowed Host names.** Every request, local or relay-origin, must name an
allowed Host: `localhost`, `127.0.0.1`, or `[::1]` on any port, plus the
server's `--allow-host` names and a non-wildcard `--bind` host
([protocol](protocol/README.md#allowed-host-names)). A request through a
bridge carries the bridge's loopback address (`127.0.0.1:<port>`) and passes;
a DNS-rebinding page, whose requests name the attacker's host, gets `403
permission_denied` and cannot read `GET /v1/relay/link` or anything else.

**`hya serve relay` commands** find the running backend of `--db` through
its discovery file (like `hya serve status`) and call its `RelayControl`
rpcs on loopback:

| Command | Prints | Exit |
| --- | --- | --- |
| `connect <URL> [--transport auto\|grpc\|ws] [--relay-ca <PEM>] [--ephemeral] [--json]` | the status lines and `hya relay link: <link>` (`--json`: `{status, link}`) | 0; 1 on an invalid URL or CA file |
| `disconnect [--json]` | `relay disconnected` (`--json`: the status) | 0 (also when not joined) |
| `status [--json]` | `relay <state>` and `proxy`, `room`, `link` (redacted), `transport`, `since`, `streams`, `identity`, `error` lines (`--json`: the `RelayStatus` message) | 0 in every state |
| `link` | the full link alone on stdout | 0; 1 when not joined |
| `rotate [--json]` | `hya relay link: <new link>` (`--json`: `{link, status}`) | 0; 1 without an identity |

Every command exits **1** with `no hya server is running on <db>` when no
backend runs, and with `hya serve relay: <message>` when the rpc fails.

### Connecting from a client

A relay link (`hya://…#<key>.<psk>`, see [Relay link grammar](#relay-link-grammar))
is all a client needs. The client side of the tunnel runs in `hya`: a
**bridge** listens on a loopback port and carries every TCP connection made
to it, end-to-end encrypted, to the backend behind the link. The TUI and the
WebUI keep their plain HTTP/SSE/WebSocket client and use the bridge's URL as
their `--server`; the loopback URL is never shown to the user in place of the
remote (the header says `remote: <relay>/<room>`).

```sh
hya --connect -                  # paste the link (not echoed); TUI + WebUI on the remote backend
printf '%s\n' "$LINK" | hya bridge -     # a standalone bridge; prints its URL
HYA_RELAY_LINK="$LINK" hya bridge --json # the same, for a parent process
```

**Keep the link out of process listings.** The link is the credential
(ADR-0025): whoever holds it controls the backend. Pass it on stdin (`-`;
on a terminal `hya` prompts for it with echo turned off) or in
`HYA_RELAY_LINK`. A link given as an argument works but is visible to every
local user in `ps`, so `hya` warns (`the relay link was given as an argument,
so it is visible in process listings; …`). Only the redacted form
(`hya[+insecure]://host[:port][/prefix]/<room_id>`) ever appears in output
or logs.

**`hya bridge [<LINK>|-]`.** Dispatched before any runtime composition (no
config, providers, or database), like `hya proxy`.

| Flag | Default | Meaning |
| --- | --- | --- |
| `<LINK>` | `$HYA_RELAY_LINK` | The link; `-` reads one line from stdin (recommended). |
| `--listen <ADDR>` | `127.0.0.1:0` | Loopback address (`127.0.0.1:PORT`, `[::1]:PORT`, `localhost:PORT`, or a port). Any other address is refused: the bridge adds no authentication of its own, so reaching it means controlling the backend. |
| `--relay-ca <PEM>` | none | Extra trusted CA certificates for a relay behind a private CA. |
| `--transport auto\|grpc\|ws` | the link's `t=` | Relay binding, overriding the link. |
| `--json` | off | Print one JSON line instead of the plain readiness line. |
| `--exit-with-stdin` | off | Exit when stdin reaches end of file, so a parent that holds the pipe takes the bridge down with it. |

Readiness (stdout, exactly one line, once listening and after the start-up
check below):

```text
hya bridge listening on http://127.0.0.1:<port>
{"url":"http://127.0.0.1:<port>","room":"<room_id>","proxy":"hya+insecure://relay.lan:8766","label":"remote: relay.lan:8766/<room_id>"}
```

`proxy` is the redacted link without the room; `label` is what a TUI shows
for the server (`--server-label`). Status lines go to stderr, prefixed
`hya bridge:`: the binding and why it was chosen (`relay hya://…: grpc
binding (gRPC works on this path)`), and every change of the backend's
state (online, offline, relay unreachable, reachable again). SIGINT, SIGTERM,
or SIGHUP stops accepting, gives open connections 1 s, and exits **0**.

**Start-up check.** Before it prints the readiness line, the bridge picks the
relay binding (`t=auto` probes, see [Client transport](#client-transport))
and opens one tunnel to check the link. It exits **1** when no binding
reaches the relay (`cannot reach the relay …`) or when the backend rejects the
handshake (`the remote backend rejected the relay link <redacted> (rotated or
wrong link); ask for a new one`). An **offline** backend (its room has no
host) is not an error: the bridge starts and says so.

**Per connection.** Each accepted TCP connection opens its own relay stream
and Noise tunnel (`RelayClient::open`, then `NoiseStream::initiate_link`
within 15 s) and is then spliced byte for byte, so REST, SSE, and the PTY
WebSocket upgrade work unchanged. There is no persistent session to lose: a
TUI that reconnects simply opens new connections to the same URL.

| Situation | What the client connection sees |
| --- | --- |
| Tunnel open | The backend's bytes, unchanged; a clean close when both sides finish. |
| A record fails authentication (tampered) or the stream ends without the close record (truncated) | A **TCP reset** (`SO_LINGER 0`), never a clean close, so a cut or forged response cannot pass for a complete one. The bridge logs `… failed its integrity check …; the connection was reset`. |
| The room is offline, the relay is unreachable (`Unavailable`, `NoBinding`, `Timeout`, …), or the backend rejects the link | If the first bytes are an HTTP request line: `503 Service Unavailable` with the hya server's error envelope, `{"error":{"code":"unavailable","message":"remote backend is offline"}}` (or `the relay is unreachable: …`, `the remote backend rejected the relay link …`), then a clean close. Anything else: a TCP reset. |

A failed connection after a binding error clears the remembered binding, so
the next connection probes again.

**`hya --connect [<LINK>|-]`.** Bare `hya` without the local daemon: it
starts the bridge in its own process (with the checks above, before the
terminal is touched), then the WebUI host and the terminal TUI exactly like
`hya --backend <url>`, with `--server <bridge-url> --remote --server-label
"remote: <relay>/<room>"` in both TUI commands and no `--db`/`--hya`
([cli.md](cli.md#bare-hya)). `--connect` conflicts with `--backend`; without
a value it reads `HYA_RELAY_LINK`. The bridge lives as long as that `hya`;
its status lines go to `hya.log`.

#### From a running TUI (`/connect-remote`)

A TUI that is already running (bare `hya`, or `bun packages/hya-tui/src/main.ts`)
moves to a remote backend with `/connect-remote <link>`, or `/connect-remote`
alone, which asks for the link in a concealed entry (bullets and a count;
Enter connects, Esc cancels). `--transport auto|grpc|ws` and `--relay-ca
<pem>` pass through to the bridge.

```text
/connect-remote --transport ws          # then paste the link; it is not shown
/disconnect-remote                      # back to the local daemon
```

The TUI runs its own bridge child, `<hya> bridge - --json --exit-with-stdin
[--transport T] [--relay-ca PEM]` (the `hya` of `--hya`, `HYA_BIN`, or PATH),
writes the link and a newline to its stdin and holds the pipe open, so the
bridge exits 0 when the TUI closes it or dies. It waits up to 20 s for the
JSON readiness line, then uses `url` as its server and `label` in the header,
sidebar, `/status`, and status lines; stderr lines (`hya bridge: …`) are shown
on the status line; a start-up failure (exit 1) is shown as `Remote
connection failed: <the bridge's reason>`. On the remote the TUI behaves like
a `--remote` start (the Project view opens, no session is created) and never
starts or looks for a local daemon; while the remote is offline its requests
get the bridge's `503 unavailable: remote backend is offline` and its streams
keep retrying. `/disconnect-remote` closes the bridge's stdin (SIGTERM after
2 s) and goes back to the local backend (the database's daemon, found or
started); a TUI started by bare `hya --connect` has none to go back to.

The link stays a secret throughout: never in argv, never on screen after it is
submitted, not kept in the input history (`/connect-remote` is kept without
it), and any other input holding a link is refused rather than sent. See
[docs/tui.md](tui.md#remote-backends-connect-remote) for the status lines and
keys.

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
| `Open` / `ws/open` | `Chunk` | `Chunk` | Client side of one data stream; first frame `open{room_id, open_token}`. An offline room, and a missing or wrong open token, fail with the same `NOT_FOUND`. The proxy sends `opened{}` once the host accepted the stream. |

Messages:

| Message | Fields | Meaning |
| --- | --- | --- |
| `HostFrame` | oneof `register` \| `heartbeat` \| `update_open_token` | Host → proxy on the control stream. |
| `ProxyToHost` | oneof `challenge` \| `registered` \| `heartbeat` \| `incoming` \| `error` \| `open_token_updated` | Proxy → host on the control stream. |
| `Challenge` | `nonce: bytes` (32) | First proxy frame; the host must sign it. |
| `Register` | `ed25519_pubkey: bytes` (32), `signature: bytes` (64), `open_token_hash: bytes` (32) | Ed25519 signature over `"hya.relay.v1/register/v2\0" ‖ nonce ‖ open_token_hash` (`hya_relay::proto::register_signing_message`). The first revision's `"hya.relay.v1/register\0" ‖ nonce` is no longer accepted; the contexts differ at byte 21, so neither signature verifies as the other. A missing hash is `INVALID_ARGUMENT`. |
| `UpdateOpenToken` | `open_token_hash: bytes` (32), `signature: bytes` (64) | Replaces the room's hash after a PSK rotation. Signed by the room key over `"hya.relay.v1/update-open-token/v1\0" ‖ nonce ‖ open_token_hash`, with this control stream's challenge nonce. |
| `OpenTokenUpdated` | — | The proxy applied the update: from now on only the new token opens streams. |
| `Registered` | `room_id: string` | Registration accepted. |
| `Heartbeat` | `seq: uint64`, `pong: bool` | Liveness probe (`pong=false`) or reply echoing `seq` (`pong=true`); valid in both directions on every stream. |
| `Incoming` | `stream_id: string` | A client opened a stream; the host calls `Accept` with this id. Proxy-generated and unguessable. |
| `Chunk` | oneof `open{room_id, open_token}` \| `accept{stream_id}` \| `data: bytes` \| `close{}` \| `heartbeat` \| `error` \| `opened{}` | One data-stream frame. `data` is opaque end-to-end ciphertext; `close` ends the sender's direction; `opened` (field 7) is the proxy's open acknowledgement. |
| `RelayError` | `code: RelayErrorCode`, `message: string` | Terminal failure; the WebSocket stand-in for a gRPC status. |

**Open tokens.** Only link holders may make a host do any work. The room's
open token is `HMAC-SHA256(key = psk, "hya.relay.v1/open\0" ‖ room_id)`
(`hya_relay::keys::OpenToken`, `RelayLink::open_token()`). The host
registers only `sha256(open_token)`; an opener presents the token itself in
`open.open_token`, and the proxy admits the open only when its sha256
equals the registered hash (compared in constant time). A missing or wrong
token gets exactly the answer of an offline room — `NOT_FOUND` "room is
offline" — before any stream slot is taken or the host is told, so a room
id alone neither reveals whether the room is online nor spends the room's
or the host's resources. The proxy never holds the PSK; it learns a token
only when a link holder presents it, and a token opens nothing but the
proxy's gate (the Noise handshake still needs the PSK and the server key).
Rotating the PSK changes the token; the host sends the new hash with
`update_open_token`, and old tokens are refused from the proxy's
`open_token_updated` on. Probes (`hya relay doctor`, `auto`) open a random,
never-registered room without a token and still get `NOT_FOUND`.

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
client IP, IPv6 bucketed by /64) used only for per-client limits.
`ProxyCore::stats()` returns `ProxyStats{rooms, streams, pending_streams,
pending_registrations, early_data_bytes}`; `ProxyCore::shutdown()` ends
every stream with `UNAVAILABLE`, refuses new ones, and waits until all served
streams have finished. The proxy keeps no state on disk.

**Registration.** On a `Host` stream the proxy sends `challenge{nonce}` (32
fresh random bytes, single use). The first host frame must be
`register{ed25519_pubkey, signature, open_token_hash}` within the handshake
timeout. The signature must verify (Ed25519 `verify_strict`) over
`"hya.relay.v1/register/v2\0" ‖ nonce ‖ open_token_hash`; the room id is
derived from the key and returned in `registered{room_id}`, and the hash
gates every open of the room. `update_open_token{open_token_hash,
signature}` (signed over the same nonce) replaces the hash; the proxy
answers `open_token_updated{}`, or ends the control stream with
`UNAUTHENTICATED` when the signature does not verify. The room is online while the control
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
`open{room_id, open_token}` within the handshake timeout. A malformed room
id is `INVALID_ARGUMENT`; an offline room and a missing or wrong open token
are both `NOT_FOUND` "room is offline", checked before any limit. Otherwise
the proxy
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
  Heartbeats count as activity for the idle timeout. Once the proxy has
  ended a leg's receiving side (it forwarded the other side's `close{}`), a
  probe arriving on that leg is dropped silently: it can no longer be
  answered, and it does not mean the other side went away.
- An `error` frame from one side is forwarded to the other and ends the
  stream; a transport failure on one side ends the other with
  `UNAVAILABLE`. A handshake frame (`open`, `accept`, `opened`) after the
  splice is `INVALID_ARGUMENT` to both sides.

**Errors.** Every failure the proxy reports is a final `RelayError` frame
followed by the end of its sending direction; the gRPC binding reports that
final frame as the stream status instead.

| Code | When |
| --- | --- |
| `INVALID_ARGUMENT` | Wrong or empty first frame; malformed room id; a `register` or `update_open_token` without a 32-byte `open_token_hash`; handshake frame after the splice. |
| `DEADLINE_EXCEEDED` | No first frame (registration, `open`, `accept`) within the handshake timeout; a leg or control stream idle past the idle timeout; a peer that stops reading for that long. |
| `NOT_FOUND` | `open` to an offline room or with a missing or wrong open token (indistinguishable); `accept` with an unknown, expired, or already accepted stream id. |
| `ALREADY_EXISTS` | Sent to a host whose room was taken over by a newer registration. |
| `RESOURCE_EXHAUSTED` | A room, stream, per-client, or proxy-wide limit is reached; a `data` payload over the chunk limit (sent to both sides). |
| `FAILED_PRECONDITION` | A second `register` on a registered control stream. |
| `UNAVAILABLE` | The host did not accept in time; the room went offline or was replaced; the other side went away; the proxy is shutting down. |
| `UNAUTHENTICATED` | Registration or open token update key or signature is malformed or does not verify. |

**Limits** (`hya_relay::proxy::ProxyLimits`, ADR-0025 D8). The per-client
limits bound what one address can take; the proxy-wide `max_*` caps bound
the total when many addresses (a botnet, or an IPv6 range) act together:

| Field | Default | Effect |
| --- | --- | --- |
| `max_rooms` | 1024 | Registered rooms; a replacement needs no new slot. Over: `RESOURCE_EXHAUSTED`. |
| `max_streams_per_room` | 64 | Concurrent streams (waiting or spliced) per room. Over: `RESOURCE_EXHAUSTED`. |
| `max_streams_per_peer` | 256 | Concurrent streams opened by one `PeerInfo`. Over: `RESOURCE_EXHAUSTED`. |
| `max_streams` | 8192 | Concurrent streams on the whole proxy, whoever opened them. Over: `RESOURCE_EXHAUSTED`. |
| `max_rooms_per_peer` | 16 | Rooms registered by one `PeerInfo`. Replacing a room the same client holds needs no new slot; a room taken over by another client frees the old owner's slot. Over: `RESOURCE_EXHAUSTED`. |
| `max_pending_registrations_per_peer` | 8 | Host control streams of one `PeerInfo` that have not finished registration (challenge sent, no valid `register` yet). Over: the new control stream gets `RESOURCE_EXHAUSTED` instead of a challenge. |
| `max_pending_registrations` | 256 | The same, over all clients together. |
| `idle_timeout` | 120 s | A leg or control stream with no frame at all (heartbeats count), or a peer not taking a frame, for this long: `DEADLINE_EXCEEDED`. |
| `stream_rate_bytes_per_sec` | 8 MiB/s | Token-bucket cap on `data` bytes per stream and direction; excess is delayed, not dropped. `0` = unlimited. |
| `stream_rate_burst_bytes` | 1 MiB | Burst of that bucket. |
| `max_chunk_data` | 256 KiB | Largest `data` payload. Over: `RESOURCE_EXHAUSTED` to both sides. |
| `early_data_limit` | 64 KiB | Opener `data` buffered before the accept; beyond it the proxy stops reading. |
| `max_early_data_bytes` | 64 MiB | Opener `data` buffered before accepts, over all streams; beyond it the proxy stops reading every waiting opener until buffered data is delivered. Checked before each read, so it can be exceeded by at most one chunk per waiting stream. |
| `accept_timeout` | 10 s | Wait for the host's `Accept`; then `UNAVAILABLE` to the opener. |
| `handshake_timeout` | 10 s | Deadline for the first frame of every stream (the registration timeout for hosts). |

### Bindings

`hya_relay::server::RelayServer` serves both bindings of `hya.relay.v1` on
**one port** in front of one `ProxyCore`. `hya proxy` (coming in a later
step) is a thin CLI around it.

```rust
use hya_relay::server::{ForwardedHeader, RelayServer, RelayServerConfig, TlsFiles};

let config = RelayServerConfig::new("0.0.0.0:8766".parse()?)
    .path_prefix("/relay")?                      // optional
    .tls(TlsFiles { cert: "cert.pem".into(), key: "key.pem".into() }) // optional
    .trust_forwarded(Some(ForwardedHeader::XRealIp)) // only behind a hop that sets it
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
| `trust_forwarded(Option<ForwardedHeader>)` | `None` | Identify clients by one named forwarding header (below). |
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
default it is the socket's remote IP: an IPv4 address (IPv4-mapped IPv6
shown as IPv4) as is, an IPv6 address as its **/64** (`2001:db8:1:2::/64`),
since one host commonly holds a whole /64 and could otherwise take a new
identity per address. With `trust_forwarded(Some(header))` it is the address
in exactly that `ForwardedHeader` — `CfConnectingIp` (`CF-Connecting-IP`),
`XRealIp` (`X-Real-IP`), or `XForwardedFor` (the **rightmost**
`X-Forwarded-For` entry of the last header line, the one the trusted hop
appended; entries to its left come from the client) — bucketed the same
way, and the socket IP when it is missing or does not parse. Every other
forwarding header is ignored, so a client cannot pick a more favorable one.
Name a header only when every request reaches the proxy through a hop that
sets (overwrites, or for `X-Forwarded-For` appends to) exactly that header;
otherwise clients choose their own identity.

**Graceful shutdown.** When the shutdown future completes the server stops
accepting (the port closes), ends every relay stream with `UNAVAILABLE`
(gRPC status, or error frame plus close `4014`), asks every connection to
finish (HTTP/2 `GOAWAY`, HTTP/1.1 close after the response), and waits up to
the drain timeout before cutting what is left.

### Client transport

`hya_relay::client::RelayClient` is the client side of both bindings,
shared by the host connector (in `hya serve`) and the client bridge. It
turns a relay address into relay streams, each returned as the
binding-independent transport type:

```rust
use hya_relay::client::{ClientConfig, RelayClient, register_host};

// Client side (bridge): one tunnel per TCP connection.
let client = RelayClient::from_link(&link, ClientConfig::default())?;
let leg = client.open(link.room_id()).await?;          // waits for `opened`
let tunnel = NoiseStream::initiate_link(leg, &link, TunnelConfig::default()).await?;

// Host side (connector): control stream, then one Accept per `incoming`.
let client = RelayClient::new(RelayAddress::parse_proxy_url("https://relay.example.com/hya")?, ClientConfig::default())?;
let mut control = client.host().await?;
let open_token_hash = OpenToken::derive(&psk, &room_id).hash();
let registration = register_host(&mut control, &signing_key, &open_token_hash, Duration::from_secs(10)).await?;
// after a PSK rotation: control.send(registration.update_open_token_frame(&signing_key, &new_hash))
// on ProxyToHost::incoming{stream_id}:
let leg = client.accept(&stream_id).await?;
```

| Method | Returns | Notes |
| --- | --- | --- |
| `RelayClient::new(RelayAddress, ClientConfig)` | `Result<RelayClient, ClientError>` | Nothing connects yet; fails only on an unreadable CA file (`Config`). Clones share the gRPC connection. |
| `RelayClient::from_link(&RelayLink, ClientConfig)` | same | A pinned `t=` in the link wins over `transport: Auto`; a pinned `config.transport` wins over the link. The client keeps the link's open token. |
| `open(&RoomId)` | `ChunkTransport` | Sends `open{room, open_token}` — the link's token for the link's room (a `from_link` client), none otherwise — and returns once `opened` arrived (within `open_timeout`). |
| `open_with_token(&RoomId, &OpenToken)` | `ChunkTransport` | The same with an explicit token. |
| `accept(&str)` | `ChunkTransport` | Sends `accept{stream_id}`; proxy errors (for example `NOT_FOUND` for an unknown id) arrive on the stream. |
| `host()` | `HostControlTransport` | The proxy's first frame is the challenge; `register_host(&mut t, &SigningKey, &open_token_hash, deadline) -> Result<HostRegistration, ClientError>` answers it. `HostRegistration::room()` is the room; `update_open_token_frame(&SigningKey, &hash)` builds the signed update for this control stream. |
| `binding()` | `BindingChoice{binding, reason}` | The binding streams use: pinned, remembered, or negotiated now. |
| `probe(Binding)` | `Result<(), ProbeFailure>` | One end-to-end check of a binding (for `hya relay doctor`). |
| `remembered_binding()` / `forget_binding()` | | Read or clear the per-address memo. |

`ClientConfig` (all public fields, `Default`):

| Field | Default | Meaning |
| --- | --- | --- |
| `transport: Transport` | `Auto` | `Auto` negotiates; `Grpc` / `Ws` pin a binding and never probe. |
| `extra_ca_pem: Option<PathBuf>` | none | PEM file of extra trusted CA certificates (`--relay-ca`). |
| `heartbeat: HeartbeatConfig` | every 15 s, dead after 45 s | See heartbeats below. |
| `connect_timeout: Duration` | 10 s | TCP, TLS, HTTP/2 or WebSocket handshake of one connection, and the proxy's first answer on a new gRPC stream. |
| `probe_timeout: Duration` | 5 s | Deadline of each binding probe. |
| `open_timeout: Duration` | 30 s | Deadline for `open` to receive `opened`. |

**Bindings on the wire.** gRPC uses one HTTP/2 connection per client
(h2c with prior knowledge for `hya+insecure://`; ALPN `h2` under TLS, and
the server must select it) with every rpc path below the link's prefix
(`<prefix>/hya.relay.v1.Relay/<Method>`) and HTTP/2 keepalive pings at the
heartbeat interval. WebSocket uses one HTTP/1.1 connection per stream
(`ws[s]://host[:port]<prefix>/hya.relay.v1/ws/<route>`, ALPN `http/1.1`
only). A final `RelayError` frame or a `4000 + code` close becomes
`TransportError::Status`, the same as a gRPC status. Closing a transport's
sink half-closes the stream (gRPC end of request stream, WebSocket close
frame); **dropping** a stream that was not closed aborts it (gRPC
`RST_STREAM`, WebSocket connection dropped without a closing handshake), so
the proxy reports `UNAVAILABLE` to the other side. Frames already accepted
by the sink are still delivered after a drop.

**TLS.** `hya://` uses rustls (ring provider, TLS 1.2 and 1.3) trusting
the operating-system roots (`rustls-native-certs`), Mozilla's webpki roots,
and every certificate in `extra_ca_pem`. The server name (SNI and
certificate check) is the link host; an IP host is checked as an IP.
`hya+insecure://` is plaintext end to end of the hop (Noise still encrypts
the payload).

**Auto negotiation (`t=auto`).** The first stream to a relay address
probes gRPC: it opens a stream to a random, well-formed, offline room and
expects the relay's gRPC status `NOT_FOUND` within `probe_timeout`. Any
other outcome means gRPC does not work on this path, and the client probes
the WebSocket binding the same way (`NOT_FOUND` as a `RelayError` frame):

| gRPC probe outcome | `ProbeFailureKind` | Typical hop |
| --- | --- | --- |
| TCP connect failed | `Connect` | wrong host or port |
| TLS failed (certificate, name) | `Tls` | wrong CA, wrong host name |
| TLS did not select `h2`; HTTP/2 failed after connecting | `NoHttp2` | HTTP/1.1-only hop (Cloudflare Tunnel default origin, ingresses) |
| an HTTP status instead of gRPC (`grpc-status` missing, or a non-gRPC body) | `HopRejected` | a hop's 404/502/… |
| the stream ended without a status | `TrailersStripped` | an HTTP/2 hop that drops trailers |
| no answer in time | `Timeout` | a hop that buffers streaming responses |
| the relay said `UNIMPLEMENTED "not a relay path"` (gRPC) or answered `404 hya relay` (WebSocket) | `WrongPath` | the link prefix does not match the relay's `--path-prefix` |
| anything else | `Unexpected` | |

If WebSocket works the client uses it and records
`BindingChoice { binding: Ws, reason: GrpcFailed(ProbeFailure) }`; if gRPC
works, `{ binding: Grpc, reason: GrpcWorks }`; pinned bindings report
`Pinned`. The choice is remembered per (scheme, host, port, prefix) for the
process lifetime, shared by every client in the process, so a path is
probed once. When both probes fail, `binding()` and every stream call fail
with `ClientError::NoBinding { grpc, ws }` carrying both reasons, and
nothing is remembered. The host connector and the bridge negotiate
independently.

**Heartbeats and dead-peer detection.** Every stream a `RelayClient`
returns is wrapped by `hya_relay::client::with_heartbeat(transport,
HeartbeatConfig)` (usable on any transport):

- It sends a probe (`heartbeat{seq, pong: false}`) when nothing was sent,
  or nothing was received, for one `interval`, at most once per interval.
  The proxy answers every probe, so both directions of every hop see
  traffic.
- It answers the peer's probes with pongs and swallows pongs: users never
  see heartbeat frames.
- It fails the stream (`TransportError::Transport`, "the connection is
  presumed dead") when no frame at all arrived for `dead_peer_after`, and
  drops the underlying stream.
- Probing and dead-peer detection stop for good once this side sent
  `close{}` (or closed the sink) or received `close{}`: the proxy may then
  legitimately stay silent on that leg. A pong that cannot be sent is
  dropped; it never ends the other direction.

`HeartbeatConfig { interval, dead_peer_after }`: `HeartbeatConfig::every(d)`
sets `dead_peer_after = 3 × d`; `.dead_peer_after(d)` overrides it;
`Duration::ZERO` disables probing or dead-peer detection;
`HeartbeatConfig::disabled()` disables both. The default is 15 s / 45 s,
below common idle cuts (nginx 60 s, Cloudflare about 100 s) and the
proxy's own 120 s idle timeout.

**Reconnect policy.** `hya_relay::client::Backoff` paces the host
connector's control-stream reconnects (`ReconnectPolicy`):

| Field | Default | Meaning |
| --- | --- | --- |
| `initial` | 1 s | First delay after an ordinary failure. |
| `max` | 60 s | Cap of ordinary delays. |
| `conflict_initial` | 30 s | First delay after `ALREADY_EXISTS` (another process holds the room identity). |
| `conflict_max` | 10 min | Cap of those delays. |
| `stable_after` | 60 s | A connection up this long resets the schedule. |

`next_delay(RetryKind)` doubles from `initial` up to the cap with equal
jitter (a delay is uniformly random in `[ceiling/2, ceiling]`);
`RetryKind::of_client_error` / `of_transport_error` classify
`ALREADY_EXISTS` as `Conflict` and everything else as `Normal`, which use
separate schedules. Call `connected()` once the room is registered;
`reset()` starts over. `Backoff::with_seed` makes the jitter reproducible.

**Message size.** The client accepts relay messages of at most
`MAX_CLIENT_MESSAGE_SIZE` (one 65,535-byte Noise record plus 1 KiB of
framing): the WebSocket binding's message and frame size and the gRPC
binding's decode size are both bounded, so a hostile relay or peer cannot
make a client buffer more. A larger message fails the stream.

`ClientError` variants: `RoomOffline` (`NOT_FOUND` on open: offline, or the
wrong open token), `Unavailable`
(`UNAVAILABLE`), `Relay{code, message}` (any other proxy status),
`Connect{binding, failure}` (a pinned or remembered binding cannot connect),
`NoBinding{grpc, ws}`, `Transport`, `Timeout`, `Protocol`, `Config`;
`ClientError::code()` returns the relay code when there is one.

### Conformance

`crates/hya-relay/tests/conformance.rs` runs a real `RelayServer` behind
in-process intermediaries (`tests/support/hops.rs`) and, in every case,
drives a full splice through `RelayClient`: a backend with a
reconnecting host control stream accepts the stream as the Noise
responder and echoes, and a client opens it as the Noise initiator. It
runs in about 2.5 s and is the CI gate for "works behind arbitrary
proxies".

| Case | Hop | Checks |
| --- | --- | --- |
| (a) | h2c-capable HTTP reverse proxy (HTTP/1.1 and h2c in, h2c or HTTP/1.1 out) | both bindings round-trip; `auto` picks gRPC |
| (b) | TLS on the relay (rcgen certificate, `extra_ca_pem`) | both bindings over TLS (ALPN `h2` / `http/1.1`); `auto` picks gRPC |
| (c) | HTTP/1.1-only reverse proxy that forwards WebSocket upgrades | `auto` lands on WebSocket (`NoHttp2`); pinned gRPC fails |
| (d) | HTTP/2 proxy that strips trailers | `auto` lands on WebSocket (`TrailersStripped`) |
| (e) | TCP hop that cuts connections idle for 300 ms | with a 50 ms heartbeat an idle tunnel and the control stream survive 4× the cut, on both bindings; without heartbeats the cut happens |
| (f) | TCP hop that cuts every connection after 400 ms | the host re-registers through the reconnect policy after each cut and new opens succeed, on both bindings |
| (g) | proxy that routes only `/relay/…` to a relay with `--path-prefix /relay` | both bindings under the prefix; without the prefix both probes report `HopRejected` |
| (h) | proxy that rewrites Host / `:authority` | both bindings round-trip |
| (i) | none: a parsed `hya+insecure://` link straight to the relay | `t=auto`, `t=grpc`, `t=ws` |

Which cases stand for each first-class deployment:

| Deployment | Cases |
| --- | --- |
| Cloudflare Tunnel | (c) + (e) + (h) |
| nginx | (a) or (c), + (e) |
| Caddy | (a) + (h) |
| Tailscale, plain tailnet | (i) |
| `tailscale serve` / `funnel` | (b) + (h) |

The process-level golden path lives in the Track P suite:
`crates/hya-e2e/tests/p38_relay.rs` runs a real `hya proxy`,
`hya serve --relay` (with the FakeLlm provider), and `hya bridge` over each
pinned binding and once over TLS, drives the API (Projects, tool reads with a permission ask,
SSE, PTY, rotation, shutdown) through the bridge only, and asserts that a
capture hop in front of the proxy never sees a prompt, file contents, shell
output, or the link's PSK ([process-e2e.md](testing/process-e2e.md#relay-scenarios-p38_relay)).

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

### The `RelayControl` service

`hya.v1.RelayControl` (`proto/hya/v1/relay_control.proto`; not to be
confused with the proxy's `hya.relay.v1.Relay`) controls the host connector
of the backend it is called on. Loopback only, see
[Relay origin](#hosting-a-backend-on-a-relay).

| Rpc | HTTP | Request | Response | Errors |
| --- | --- | --- | --- | --- |
| `ConnectRelay` | `POST /v1/relay/connect` | `{proxyUrl, transport?, extraCaPath?, ephemeral?}` | `{status: RelayStatus, link}` | `invalid_argument` (URL, transport, relative or missing CA path) |
| `DisconnectRelay` | `POST /v1/relay/disconnect` | `{}` | `RelayStatus` | |
| `GetRelayStatus` | `GET /v1/relay/status` | | `RelayStatus` | |
| `GetRelayLink` | `GET /v1/relay/link` | | `{link, status}` | `failed_precondition` when not joined |
| `RotateRelayKey` | `POST /v1/relay/rotate` | `{}` | `{link, status}` (`link` empty when not joined) | `failed_precondition` without an identity |

All of them answer `permission_denied` from relay origin, a non-loopback or
unknown peer, or a browser. `RelayStatus` (protojson):

| Field | Type | Meaning |
| --- | --- | --- |
| `state` | `RELAY_STATE_DISCONNECTED` \| `_CONNECTING` \| `_CONNECTED` \| `_BACKOFF` | Connector state. |
| `proxy` | string | The relay's public base URL. |
| `roomId` | string | This backend's room. |
| `redactedLink` | string | The link without its secret fragment. |
| `transport` | string | Configured binding: `auto`, `grpc`, `ws`. |
| `binding`, `bindingReason` | string | Binding in use and why (pinned, gRPC works, or why gRPC failed). |
| `lastError` | string | Last connection or registration failure (cleared on success). |
| `connectedSince` | Timestamp | When the room was registered. |
| `activeStreams` | uint32 | Relay connections being served (including upgraded PTY WebSockets). |
| `ephemeral` | bool | Whether the identity is throwaway. |

In Rust, `hya_server::RelayHost` is the connector (`connect`,
`disconnect`, `status`, `link`, `rotate`, `shutdown`, `set_service`,
`set_settings_hook`), configured by `RelayHostConfig` and
`hya_server::AppState::with_relay_host`; `hya_server::Origin` is the request
extension.

## Deployment recipes

`hya proxy` needs only one inbound TCP port and speaks both bindings on it
(see [Bindings](#bindings)), so any of these fronts work; the intermediary
conformance suite (`crates/hya-relay/tests/conformance.rs`) is the CI proof
that each shape works, and each recipe below ends with the matching `hya
relay doctor` command. These four are first-class (ADR-0025 D9); anything
that behaves like "an HTTPS hop that may be HTTP/1.1-only, may cut idle
streams, and may rewrite Host/paths" is expected to work the same way.

### 1. Cloudflare Tunnel

`cloudflared` proxies to `hya proxy` over plain HTTP; no inbound port is
needed on the proxy host at all.

```yaml
# cloudflared config.yml
tunnel: <tunnel-id>
credentials-file: /etc/cloudflared/<tunnel-id>.json
ingress:
  - hostname: relay.example.com
    service: http://localhost:8766
  - service: http_status:404
```

By default `cloudflared`'s origin connection is **HTTP/1.1**, so the
WebSocket binding carries the traffic (case (c) in the conformance suite);
`auto` picks it up automatically (`ProbeFailureKind::NoHttp2`). To let gRPC
through instead, add `http2Origin: true` under the ingress rule's
`originRequest`:

```yaml
  - hostname: relay.example.com
    service: http://localhost:8766
    originRequest:
      http2Origin: true
```

The Cloudflare edge cuts idle connections at roughly 100s; the relay's
default heartbeat (15s, dead after 45s) stays well under that, so open
streams survive (case (e)). Targets this Cloudflare Tunnel targeting
`cloudflared` 2024+.

Every client reaches the proxy from `cloudflared` on localhost, so the
per-client limits need the client address from Cloudflare: it sets
`CF-Connecting-IP` (overwriting any client value) on every request. Trust
that header and nothing else — and only when the proxy port is reachable
through the tunnel alone (bind `--host 127.0.0.1`), since a direct client
could send its own `CF-Connecting-IP`.

```sh
hya proxy --host 127.0.0.1 --port 8766 --trust-forwarded cf-connecting-ip
hya relay doctor https://relay.example.com
# expect: WebSocket ok, recommended t=auto (or t=ws if pinning); with
# http2Origin: true, gRPC ok too.
```

### 2. nginx

nginx needs an `http2`-enabled `server` for gRPC and a separate `location`
for the WebSocket upgrade; both proxy to the same `hya proxy` port. Targets
nginx 1.25+ (`grpc_pass` and `http2 on;` inside `server` are both stable
since 1.25.1; older nginx needs `listen … http2;` instead).

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name relay.example.com;

    ssl_certificate     /etc/nginx/relay.crt;
    ssl_certificate_key /etc/nginx/relay.key;

    # gRPC binding: content-type: application/grpc.
    location / {
        if ($content_type !~ "^application/grpc") {
            break;
        }
        grpc_pass grpc://127.0.0.1:8766;
        grpc_set_header X-Real-IP $remote_addr;
        grpc_read_timeout 1h;
        grpc_send_timeout 1h;
    }

    # WebSocket binding.
    location /hya.relay.v1/ws/ {
        proxy_pass http://127.0.0.1:8766;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 1h;
        proxy_send_timeout 1h;
        proxy_buffering off;
    }

    client_max_body_size 0;
}
```

`grpc_read_timeout`/`grpc_send_timeout` and `proxy_read_timeout`/
`proxy_send_timeout` at 1h (or any value comfortably above the relay's own
120s idle timeout) keep nginx from cutting long-lived streams itself;
`proxy_buffering off` and `client_max_body_size 0` keep it from buffering
the streaming bodies. `X-Real-IP $remote_addr` overwrites whatever the
client sent with the address nginx saw, so `hya proxy --trust-forwarded
x-real-ip` gives the per-client limits the real client (bind the proxy to
`127.0.0.1` so nothing bypasses nginx). Path-prefix variant (`hya proxy --path-prefix
/relay`): change both `location` blocks to match under `/relay/` and keep
the prefix in the link/proxy URL:

```nginx
    location /relay/ {
        if ($content_type !~ "^application/grpc") { break; }
        grpc_pass grpc://127.0.0.1:8766;
        grpc_set_header X-Real-IP $remote_addr;
    }
    location /relay/hya.relay.v1/ws/ {
        proxy_pass http://127.0.0.1:8766;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header X-Real-IP $remote_addr;
    }
```

```sh
hya proxy --host 127.0.0.1 --port 8766 --trust-forwarded x-real-ip   # or: --path-prefix /relay
hya relay doctor https://relay.example.com
# expect: gRPC ok, WebSocket ok, recommended t=auto.
```

### 3. Caddy

Caddy's `reverse_proxy` with an `h2c://` upstream carries both gRPC and
WebSocket over one directive, and Caddy manages TLS automatically. Targets
Caddy 2.7+.

```caddyfile
relay.example.com {
    reverse_proxy h2c://127.0.0.1:8766 {
        flush_interval -1
    }
}
```

`flush_interval -1` disables Caddy's response buffering so streaming frames
are forwarded immediately (needed for both bindings). This is the suite's
"canary" shape: case (a) (h2c carries both bindings) plus, if you rewrite the
Host header, case (h).

Caddy appends the address it saw to `X-Forwarded-For` (and, unless you
configure `trusted_proxies`, drops any value the client sent), so the
**rightmost** entry is the client — which is exactly the entry `hya proxy
--trust-forwarded x-forwarded-for` reads; entries to its left are never
used.

```sh
hya proxy --host 127.0.0.1 --port 8766 --trust-forwarded x-forwarded-for
hya relay doctor https://relay.example.com
# expect: gRPC ok, WebSocket ok, recommended t=auto.
```

### 4. Tailscale

Two shapes, both avoiding a public listener entirely.

**(a) Plain tailnet, no `tailscale serve`.** Run `hya proxy` directly on a
tailnet node (it may be the same machine as the backend) and use
`hya+insecure://` — plaintext toward the first hop is fine here because
WireGuard already encrypts the tailnet hop and Noise still encrypts the
relay payload end to end; nothing between the two hosts ever sees plaintext
application data.

```sh
hya proxy --host 0.0.0.0 --port 8766
```

`hya+insecure://`'s default port is **80**, so the link must name `:8766`
explicitly:

```text
hya+insecure://100.64.1.2:8766/<room_id>#<key>.<psk>
# or the tailnet MagicDNS name:
hya+insecure://myhost.tailnet-name.ts.net:8766/<room_id>#<key>.<psk>
```

```sh
hya relay doctor hya+insecure://100.64.1.2:8766
# expect: gRPC ok, WebSocket ok, recommended t=auto. (Case (i) in the
# conformance suite: a direct plaintext link, no intermediary at all.)
```

**(b) `tailscale serve` / `tailscale funnel`.** `tailscale serve` puts
Tailscale's own HTTPS in front of `hya proxy` on the tailnet (or, with
`funnel`, on the public internet), so the link becomes a real `hya://`:

```sh
hya proxy --host 127.0.0.1 --port 8766
tailscale serve --bg --https=443 http://127.0.0.1:8766
# public instead of tailnet-only:
tailscale funnel --bg 443
```

```text
hya://myhost.tailnet-name.ts.net/<room_id>#<key>.<psk>
```

`tailscale serve`/`funnel` terminate TLS and forward over HTTP/1.1, so this
is cases (b) (TLS) plus (h) (the proxy sees `tailscale serve`'s own
`Host`/`:authority`, not the public name) in the conformance suite.

```sh
hya relay doctor https://myhost.tailnet-name.ts.net
# expect: WebSocket ok; gRPC ok too if tailscale serve's origin carries h2.
```

### 5. Direct TLS

`hya proxy` can terminate TLS itself with no intermediary at all:

```sh
hya proxy --port 8766 --tls-cert relay.crt --tls-key relay.key
```

```text
hya://relay.example.com:8766/<room_id>#<key>.<psk>
```

```sh
hya relay doctor https://relay.example.com:8766
# expect: gRPC ok, WebSocket ok, recommended t=auto.
```

## Troubleshooting

Keyed by `hya relay doctor`'s report (see [`hya relay
doctor`](#hya-relay-doctor)); `docs/troubleshooting.md` links here for the
short version.

| Doctor says | Likely cause | Fix |
| --- | --- | --- |
| gRPC failed `NoHttp2`; WebSocket ok | The hop in front of the proxy is HTTP/1.1-only (default Cloudflare Tunnel origin, many ingresses/PaaS). | Nothing to fix — `t=auto` already picked WebSocket. Enable `http2Origin`/an h2-capable upstream if you want gRPC too. |
| gRPC failed `TrailersStripped`; WebSocket ok | An HTTP/2-aware hop drops gRPC trailers (`grpc-status`). | Use WebSocket (`t=ws`), or reconfigure the hop to pass trailers through. |
| Both failed `HopRejected` | A hop answered with its own HTTP error (404/502/…) instead of reaching the relay. | Check the hop's routing/upstream address and that `hya proxy` is actually running on the port it points at. |
| Both failed `WrongPath` | The link/proxy URL's path prefix does not match the proxy's `--path-prefix`. | Match the prefix on both sides, or drop it from both. |
| Both failed `Tls` | Wrong certificate, wrong CA, or a host name mismatch. | Pass `--relay-ca <pem>` for a private CA, or check the link/proxy URL host against the certificate's name. |
| Both failed `Connect` | Wrong host/port, firewall, or the proxy is not running. | Check the address and that `hya proxy` is listening (its own readiness line). |
| Both failed `Timeout` | A hop buffers streaming responses instead of forwarding them as they arrive. | Disable response buffering on that hop (for nginx: `proxy_buffering off`; see the [nginx recipe](#2-nginx)). |
| `--measure-idle` reports a cut | An intermediary's idle timeout is shorter than the relay's own heartbeat interval reaching it (rare with the 15s default). | Lower `--relay-heartbeat` (Phase 6) below the hop's idle cut, or configure the hop's idle timeout upward (see the [nginx](#2-nginx)/[Cloudflare Tunnel](#1-cloudflare-tunnel) recipes). |
| Neither binding works (exit 1) | The path is not reaching a relay at all. | Confirm the proxy is running, the hop's upstream address/port, and DNS for the host in the URL/link. |
