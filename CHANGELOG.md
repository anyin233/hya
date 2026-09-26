# 0.42.0

## Secure relay: reach a backend from anywhere

- New `hya proxy` runs a relay: a blind rendezvous a backend and a client meet through. It listens on `0.0.0.0:8766` (`--host`, `--port`) and serves gRPC and WebSocket on the same port, so an HTTP/1.1-only hop in front of it still works. It needs no config, providers, or database. Limits (`--max-rooms`, `--max-streams-per-room`, `--idle-timeout-secs`, `--stream-rate-bytes-per-sec`, …), `--path-prefix`, direct TLS (`--tls-cert`/`--tls-key`), and `--trust-forwarded <header>` are flags.
- Everything through the relay is end-to-end encrypted between the backend and the client (Noise `NKpsk0`, a fresh session per connection). The proxy and every HTTPS hop in front of it see room ids and ciphertext only.
- The link is the credential: `hya://host[:port][/prefix]/<room>?t=<auto|grpc|ws>#<key>.<psk>` (`hya+insecure://` for plaintext on a LAN or tailnet). Whoever holds it controls the backend; `hya serve relay rotate` re-keys and every earlier link stops working. The key part never leaves the two endpoints, and hya only ever prints the link redacted, except where you ask for it.
- The client picks gRPC or WebSocket by itself (`t=auto`, falling back to WebSocket through hops that cannot carry gRPC) and keeps streams alive with heartbeats below common idle cuts (`--relay-heartbeat`, default 15 s). The backend reconnects to the relay with backoff.
- Recipes for Cloudflare Tunnel, nginx, Caddy, Tailscale (tailnet, `tailscale serve`/`funnel`), and direct TLS, with the timeouts and buffering each needs.
- New `hya relay doctor <proxy-url|link>` checks a path: TLS, the gRPC and WebSocket bindings (and why one fails), the path prefix, optionally the idle cut (`--measure-idle`), and the `t=` value to use. See [Secure relay](docs/relay.md) and [ADR-0025](docs/adr/0025-secure-relay.md).

## Hosting a backend on a relay

- `hya serve --relay <public-url>` joins a relay at start and prints the link once on stderr; `hya serve start --relay …` does the same for the database's daemon, and `restart` rejoins with the same link.
- A running backend: `hya serve relay connect <url>`, `disconnect`, `status` (state, relay, room, binding, streams, last error), `link`, and `rotate`. These work from the backend's own machine only, never through the relay.
- The backend's relay identity is kept in `<db>.relay-identity.json` (mode `0600`), so the link survives restarts; `--relay-ephemeral` uses a throwaway one. `--relay-transport` and `--relay-ca` choose the binding and trust a private CA. See [CLI](docs/cli.md).
- `hya serve` answers gRPC on its HTTP port (HTTP/2 without TLS; requests with `content-type: application/grpc*`), from the same server state as HTTP, so a session or PTY made through one protocol is visible through the other at once, and shutdown sends `serverStopping` to SSE and gRPC streams alike. gRPC works through the relay too, with the same refusals as REST. `HYA_GRPC_BIND` is no longer needed; it still adds an extra listener serving the same state. See [Protocol guide](docs/protocol/README.md).

## Connecting to a remote backend

- `hya --connect -` (paste the link, not echoed; or `$HYA_RELAY_LINK`) opens the TUI and the WebUI on a remote backend through an in-process bridge. No local daemon starts. `--transport` and `--relay-ca` apply to the bridge.
- `hya bridge -` runs the bridge on its own: it listens on a loopback port and prints its URL and a per-bridge token (`--json` for a parent process). Point a TUI at it with `--server <url> --remote`.
- In a running TUI, `/connect-remote <link>` (or `/connect-remote` alone, which asks for the link in a hidden entry) moves to a remote backend, and `/disconnect-remote` comes back to the local one. The header shows `remote: <relay>/<room>`, never the link or the bridge's local URL. A remote that goes offline shows as a status line, and the TUI picks up where it was when it comes back.
- In remote mode, `@path` images and pasted paths name files on the backend machine. A file that exists on your machine is read locally and sent inline. See [TUI](docs/tui.md).

## Projects

- A Project is a named list of directories (roots) on the backend machine; the first root is the primary one. Sessions belong to a Project. Starting in a directory reuses the Project that contains it, or creates one rooted there.
- New Project view (`/project`, `/projects`): open or switch Projects, create one with several roots (path completion on the backend), edit roots, rename, delete, and start a temporary session (`t`). Network errors show as one line in the view.
- A new left Projects sidebar lists every Project live, with busy markers and session counts. It shows on wide terminals by itself; Ctrl+P focuses it (or shows it when hidden), and `/projects-sidebar` toggles it.
- `/sessions` lists the active Project's sessions and temporary ones; F3 shows every Project.
- `/new --temp` starts a temporary session in its own scratch directory, `$XDG_CACHE_HOME/hya/scratch/<session id>` (else `~/.cache/hya/scratch/…`). hya never deletes scratch directories, not even with the session.
- A remote start (`--remote`, `hya --connect`, `/connect-remote`) creates no session: the Project view opens so you choose where to work. See [ADR-0024](docs/adr/0024-project-model-and-client-chosen-workspace.md).

## Permissions follow the Project's roots

- Read, write, edit, grep, glob, ls, and apply_patch may use any file inside any root of the session's Project without asking. Paths are canonicalized first, so a symlink cannot lead outside.
- Anything outside the roots asks as `external_directory`. `yolo` still auto-approves, and a permission bundle's approver still receives the ask.
- "Allow always" on such an ask now grants exactly that one canonical directory, for this Project only. Before, it saved a rule that allowed every directory.
- Bash is not path-checked: its working directory may be anywhere, and its own command rules still apply. See [ADR-0026](docs/adr/0026-multi-root-path-permissions.md).

## Breaking changes

- `hya.v1` Projects: `ProjectInfo.directory` is gone; `ProjectInfo` now has `roots`, `createdAt`, `updatedAt`, `sessionCount`, and `busy`. The Project service gains `ResolveProject`, `EnsureProjectForPath`, `CreateProject`, `GetProject`, and `DeleteProject`, and the global stream sends a live `projectsUpdated` frame.
- `CreateSession`: `workdir` is optional; a new root session names a `projectId`, a `workdir` (its Project is found or created), or `kind: SESSION_KIND_TEMPORARY`. Neither is `invalid_argument`. `SessionInfo` gains `projectId` and `kind`.
- `ListSessions`: the `directory` filter is removed; use `projectId`.
- `hya serve` has no working directory. Rpcs that work on a directory take it from the request (`x-hya-directory` or `directory`) or from the session, and answer `invalid_argument` without one instead of using the server's current directory. The backend daemon now runs in `$HOME`.
- The server accepts only known Host names: `localhost`, `127.0.0.1`, `[::1]`, a non-wildcard `--bind` host, and each `--allow-host <name>` (on `hya serve`, `serve start|restart`, and bare `hya`). Any other Host gets `403`, which blocks DNS rebinding. Reaching a backend by a LAN name or address now needs `--allow-host`.
- Requests that arrive through the relay and look like they come from a browser (`Origin`, `Sec-Fetch-*`) are refused.
- A bridge requires its token on every new connection (`x-hya-bridge-token`; the TUI and WebUI get it through `HYA_SERVER_TOKEN`). Without it the bridge answers `401` and opens nothing.
- The projection reducer version is 9: cached projections are rebuilt once from the event log.

## Security notes

- Treat a relay link like a password. Pass it on stdin (`-`) or in `HYA_RELAY_LINK`, not as an argument: an argument shows in process listings, and hya warns about it. hya removes `HYA_RELAY_LINK` from its own environment once read and from every child it starts.
- Rotate (`hya serve relay rotate`) when a link may have leaked. A rotated or wrong link looks like an offline backend to the client.
- Relay control and the rpcs that stop or upgrade the backend process are refused through the relay, but anyone holding the link can still run tools on the backend, so that refusal is a guard, not a security boundary.
- The proxy limits rooms, streams, pending registrations, and bytes per client (IPv6 clients per /64) and globally. It reads a client's address from a forwarded header only when you name that one header with `--trust-forwarded`.
- Text from the relay (errors, room names) has terminal control sequences removed before hya prints it.
