# Troubleshooting

## Bare `hya` Prints a Banner Instead of the TUI

Bare `hya` starts the TUI and the WebUI only when both stdin and stdout are
terminals ([CLI reference](cli.md#bare-hya)). Piped, redirected, or run from
a script, it prints a version banner plus guidance and exits 0. Run it
directly in a terminal, or use a headless surface:

```sh
hya exec "summarize this repo"
hya serve --bind 127.0.0.1:8080   # then connect any v1 client
```

## Bare `hya` Says Bun or the TUI Is Missing

Bare `hya` exits with status 1 before touching the terminal when it cannot
run the frontends:

- `Bun is required for the TUI and the WebUI but was not found on PATH` —
  install Bun from <https://bun.sh> (the repository pins 1.4.2) or set
  `BUN=/path/to/bun`. `Bun not found at <path> (the BUN environment variable)`
  means `BUN` points at a missing file.
- `the TUI is not installed: no src/main.ts in …` (or `the WebUI host …`) —
  the message lists every place searched. A release archive or `install.sh`
  puts the packages under `<prefix>/lib/hya/tui` and `lib/hya/tui-web`; if you
  copied only `bin/hya`, reinstall, or point `HYA_TUI_DIR` / `HYA_TUI_WEB_DIR`
  at the packages.
- `HYA_TUI_DIR=<dir> has no src/main.ts` — the override is wrong; `hya` does
  not fall back to other places when it is set.
- `the TUI in <dir> has no dependencies` — run `bun install --frozen-lockfile`
  in that directory; in a source checkout, in `packages/hya-tui` and
  `packages/hya-tui-web`.

Other subcommands (`serve`, `exec`, `sessions`, …) do not need Bun or the
packages.

## WebUI Unavailable: Port in Use

`WebUI unavailable: port 3250 is in use · hya --port <N>` in the TUI's status
line (and `WebUI unavailable` in the status bar) means another program, often
another `hya`, already listens on the WebUI port. The TUI works normally
without it. Pick another port, or let `hya` choose a free one and read the
address from the status bar:

```sh
hya --port 3251
hya --port 0
lsof -nP -iTCP:3250 -sTCP:LISTEN   # who holds the port
```

Other reasons (`web host exited with code N: …`, `the web host printed no
readiness line within 20 s`) come from the WebUI host itself; its output is
in the log file below.

## Where Bare `hya` Logs

While the TUI owns the terminal, `hya` appends its own output (the server's
notices, the WebUI host's lines prefixed `[webui] `, and the shutdown steps)
to `$XDG_STATE_HOME/hya/hya.log`, else `~/.local/state/hya/hya.log`. At
start a log over 4 MiB is moved to `hya.log.1`. Read it after a problem:

```sh
tail -n 50 "${XDG_STATE_HOME:-$HOME/.local/state}/hya/hya.log"
```

## Run the TUI Without Bare `hya`

For development, a shared server, or custom `serve` flags, run the OpenTUI
frontend from a checkout; it starts (and stops) its own `hya serve`, found
through `--hya <path>`, `HYA_BIN`, or `PATH`, or it connects to one you run:

```sh
# From the repository root
bun packages/hya-tui/src/main.ts --dir "$PWD"

# Or: terminal 1
hya serve --bind 127.0.0.1:8080
# terminal 2
bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080
```

## TUI Cannot Start Its Backend

Without `--server` the TUI uses the database's backend daemon and starts it
with `hya serve start --json` when none runs. If that fails it prints the
reason and the last lines of the output, then exits with status 1:

- `hya binary not found: …` — no `--hya`, no `HYA_BIN`, and no `hya` on
  `PATH`, or the path given does not exist. Build one
  (`cargo build -p hya-backend --bin hya`) and pass
  `--hya target/debug/hya`, or connect to a running server with `--server`.
- `hya serve start exited with code 1` followed by `hya serve (daemon)
  exited with … before it was ready; see <db>.server.log` — the daemon
  failed at startup; the log lines below it say why (a config error, an
  unusable `--db` path, …). The whole log is `<db>.server.log` next to the
  database (`~/.local/state/hya/sessions.db.server.log` by default).
- `the hya server daemon did not answer within 60 s` — the daemon hung
  during startup; see [Diagnosing Slow Startup](#diagnosing-slow-startup).
- `database <db> is held by pid <pid>, which serves no reachable server` —
  see [Database Is Already in Use](#database-is-already-in-use).
- `hya serve start printed unexpected output (is --hya an older hya?)` — the
  binary predates `hya serve start`; point `--hya`/`HYA_BIN` at a current one.

`/status` in the TUI shows the daemon (`daemon · pid <pid> · db <db> ·
started <N>m ago`); `hya serve status` shows the same from a shell.

## The Backend Daemon

Bare `hya` and the TUI leave the backend daemon running when they quit
([ADR-0023](adr/0023-persistent-backend-daemon.md)); that is expected. To see,
stop, or replace it:

```sh
hya serve status            # url, pid, version, db, uptime (exit 1: none runs)
hya serve stop              # graceful; --force kills after --timeout (30 s)
hya serve restart           # after an upgrade, or to change --model/--yolo
tail -f ~/.local/state/hya/sessions.db.server.log
```

- **`backend 0.x ≠ tui 0.y · hya serve restart`** in the TUI, or `note: the
  running server is hya X, this is hya Y` from `hya serve` — a daemon of an
  older (or newer) hya is still running after an upgrade. Run
  `hya serve restart`; open TUIs reconnect by themselves.
- **`Backend stopped (hya serve stop) · /reconnect starts it again`** (and
  `backend stopped` in the status bar) — someone ran `hya serve stop`. Open
  TUIs start nothing and refuse prompts (`Not sent · the backend is stopped
  …`); type `/reconnect` to start the daemon again, or start `hya` / a TUI
  anywhere: stopped TUIs attach to that daemon by themselves. `Backend
  stopped (signal)` means the server got SIGTERM/SIGINT/SIGHUP from something
  else (a foreground `hya serve` interrupted, a supervisor); same handling.
- **`Backend restarting (hya serve restart) · waiting for the new one…`**,
  then **`Server moved · now pid N`** — `hya serve restart` ran; the TUI
  attached to the new daemon. If none answers within 60 s:
  `Backend did not come back after hya serve restart · /reconnect starts it
  again` (see `hya serve status` and the daemon log).
- **`Server stopped · reconnecting…`**, then **`Started a new server · pid N`**
  or **`Server moved · now pid N`** — the daemon went away without saying why
  (a crash, `kill -9`, a machine sleep) and the TUI found or started the next
  one; the open session was reloaded. A turn that was running ended with the
  old server.
- **`Server lost: <reason> · retrying`** — no daemon could be found or
  started; the TUI tries again on the next stream retry. Check
  `hya serve status` and the daemon log.
- **`--model`/`--yolo`/`--pure` of `hya` seem ignored** — they shape only a
  daemon that launch starts; a running daemon keeps its own. `hya serve
  restart --model …` (or `stop`, then start `hya` with the flags) applies
  them.
- **A project's `.hya/bundles` or `.hya/plugins` are not loaded** — the
  daemon starts in your home directory, not in the directory of the client
  that started it, so its project tier is `~/.hya/`. Run `hya serve --db
  <db>` in the project yourself (and point the TUI at it) to serve that
  project's bundles and plugins.
- **A TUI with `--server <url>` does not reconnect** — without `--db` the URL
  is fixed; add `--db <database>` to let it fall back to that database's
  daemon.

See the [TUI guide](tui.md), [CLI Reference](cli.md), and
[Protocol guide](protocol/README.md).

## Diagnosing Slow Startup

Set `HYA_STARTUP_TRACE=1` (the only truthy values are exactly `1` or `true`,
case-insensitive) to have `hya serve` emit structured startup phase marks on
**stderr**, ending with one after the listen line:

```sh
HYA_STARTUP_TRACE=1 hya serve --bind 127.0.0.1:0 2>trace.log
```

Each mark is one JSON line with a wall-clock timestamp:

| Mark | Source | Closes the phase |
| --- | --- | --- |
| `backend_start` | `hya` | `serve` command entered (process start overhead before it). |
| `store_open` | `hya` | SQLite opened and pending migrations applied. |
| `runtime_resolved` | `hya` | Config, auth, providers, MCP/plugin specs resolved. |
| `interrupted_turns_recovered` | `hya-app` | Runtime-owner claim and crash recovery of turns a dead process left open (indexed; touches only those sessions). |
| `store_recovery` | `hya-app` | Workflow, resident-claim, and admission recovery. |
| `engine_runtime` | `hya-app` | Tool registry, plugins, catalogs, and the session engine assembled. |
| `residents_recovered` | `hya-app` | Every resident actor whose claim survived the restart re-registered; `detail` is the count. Reads the team-root and actor projections. |
| `engine_built` | `hya` | Team/workflow supervisors started. |
| `backend_listen` | `hya` | Backend announced its listen URL (`detail`). |

For repeatable startup measurements use the benchmark task. `--db` copies an
existing database into each run's scratch directory (the original is never
opened) and prints each run's phase waterfall, so cold listen on a large event
log can be compared before and after a change:

```sh
cargo run -p xtask -- startup-bench
cargo run -p xtask -- startup-bench --db ~/.local/share/hya/hya.db --runs 3 --timeout-secs 300
```

## Provider Call Fails with `http: <status>: ...`

Upstream HTTP failures surface as `ProviderError::Http` with a message shaped
like `{status}: {body snippet}`
([`crates/hya-provider/src/http.rs`](../crates/hya-provider/src/http.rs)). The
snippet is taken with a **byte** slice of up to 500 bytes
(`text.get(..500)`). When byte offset 500 is not a UTF-8 character boundary,
the code falls back to the **entire** body, so non-ASCII error pages may appear
untruncated; when the slice succeeds, the cut is at a byte index (not a
character count).

**No automatic retry** is attempted at the provider layer for 429 or 5xx: the
turn fails immediately.

Once the SSE stream is open, any frame whose JSON carries an `error` object
aborts the stream with `Http(message)` **before** the frame reaches the protocol
decoder
([`crates/hya-provider/src/http/stream.rs`](../crates/hya-provider/src/http/stream.rs)).
Mid-stream provider errors therefore use the same error variant as non-2xx
responses.

## hya Uses the Offline Provider

If the selected model is `hya/offline` and the response says no live provider is
available, Hya did not resolve a live catalog row. Check:

- `$XDG_CONFIG_HOME/hya/config.yaml`
- `$HOME/.config/hya/config.yaml`
- each provider has a supported `kind` and its own `base_url`
- an absent or empty `models` list can reach that provider's model-list endpoint
- `Authentication required` or `Authentication rejected` means the endpoint
  needs a valid inline `api_key` or Hya-saved credential
- `kind` is a supported provider kind (see the full table in
  [Configuration](configuration.md) — including `openai`,
  `openai-compatible` / `openai-completion`, `openai-response`, `openai-codex`,
  `grok-build`, `anthropic`, and `google`)

See [Configuration](configuration.md).

## `unknown provider for model`

The selected model is not served by any configured provider. Check selection
order:

1. `--model`
2. `HYA_MODEL`
3. default model chosen from config

Then make sure that exact model id appears as an **item** in a supported
provider's `models` list — either as a bare string or as the `id` field of a
detailed mapping entry (`models` is a YAML sequence, not a map of keys).

## API Key Template Fails

For `{env:VAR}`, confirm the variable is exported in the shell that launches
hya:

```sh
echo "$VAR"
```

For `{file:/path/to/key}`, confirm the file exists and contains only the secret
or acceptable trailing whitespace.

## Mutating Tools Fail in Headless Mode

Headless **`exec`**, **`run`**, **goal mode** (`-p`), and **`rpc`** install
`spawn_reject_responder`: every residual permission **ask** is answered with
`Decision::Reject` and no feedback. Nothing is auto-approved by that responder.
Tools that still need an interactive allow/ask decision will fail closed.

**`serve`** does **not** install that reject responder. Unresolved asks are
forwarded on the server permission endpoint for a connected client (or the
interactive frontend) to answer. Listing `serve` as “headless auto-allow” is
wrong.

Use `--yolo` only when you intentionally want `PermissionModel::Danger`
(auto-approve **all** tool actions). That is an RCE risk on `serve` for any
client that can drive tools.

## Shell Output Is Truncated

Tool outputs are capped to protect model context. Large stdout/stderr strings
include a truncation marker. Narrow the command output or write results to a
file and read the specific section you need.

## `tail-session` Cannot Parse the Session Id

`tail-session` accepts any valid session id: a new `hysec_...` id, a legacy
`ses_...` display id, or a legacy raw UUID:

```sh
hya tail-session hysec_ABCDEFGHIJKLMNOPQRST --db hya.db
```

If parsing fails, confirm the id came from `hya sessions --db <PATH>`
for the same database path.

## Server SSE Emits `resync`

`GET /v1/sessions/{session}/events/stream` emits a `resync` SSE frame if the
broadcast receiver lagged. The client should call:

```text
GET /v1/sessions/{session}/events?sinceSeq=<last_seen_seq>
```

then resume reading the stream.

## Database Is Already in Use

One database has one server ([ADR-0022](adr/0022-one-writer-per-database.md)):
the server holds `<db>.lock` and publishes `<db>.server.json` next to the
database. TUIs and bare `hya` use it (it is normally the backend daemon,
[ADR-0023](adr/0023-persistent-backend-daemon.md)), but a second server is
refused:

- `hya serve: database <db> is already in use by hya server pid <pid> at <url>`
  (exit status 75): connect to that server
  (`bun packages/hya-tui/src/main.ts --server <url>`, or run bare `hya` or the
  TUI without `--server`, which attach), stop it, or pass another `--db`.
- `… is already in use by pid <pid> (lock <db>.lock); it is still starting or
  does not serve HTTP` (exit status 75): the holder has not published a URL.
  Wait for it to start, or find it with `ps -p <pid>`.
- Bare `hya`, the TUI, or `hya serve start`: `database <db> is held by pid
  <pid>, which serves no reachable server (waited 60 s)`: the holder is hung
  or is not a server. A headless `hya --db <db> exec` or `workflow run` holds
  the lock for its whole run; wait for it to finish. Otherwise stop it (`hya
  serve stop --force --db <db>`, or kill that pid).
- `hya exec: database <db> is in use by pid <pid> and it does not serve HTTP
  yet; try again or stop it` (also `hya run`, `hya workflow`, `hya sessions`;
  exit status 75): a daemon is still starting, or another headless command
  holds the database. Try again in a moment, or pass another `--db`.
- `hya exec: database <db> is in use by hya server pid <pid> at <url>, and
  <reason>; stop it (…) or pass another --db` (exit status 75): the command
  would normally go through that server, but this invocation cannot (for
  example `--pure`, or `workflow run --revision`). Drop that flag, stop the
  server (`hya serve stop --db <db>`), or use another `--db`. See
  [CLI: Database lock and the backend daemon](cli.md#database-lock-and-the-backend-daemon).

`<db>.lock` is released by the OS when its process exits, even on a crash or
SIGKILL. Do not delete it. A `<db>.server.json` left by a crash is ignored
and replaced by the next server.

## SQLite Database Is Locked

File-backed stores use WAL mode and a five-second busy timeout. If lock errors
continue:

- make sure another process is not holding a long write transaction
- use a separate database path for separate local experiments
- `hya exec --db` and `hya workflow` respect the server lock (they go through
  the server that holds the database), so a `database is locked` error points
  at a process outside hya, or an older hya, writing the file
- use an empty `--db ""` for in-memory one-off runs

## The Server Binds an Unexpected Port

Use an explicit bind address:

```sh
hya serve --bind 127.0.0.1:8080 --db hya.db
```

Use `127.0.0.1:0` only when you want the OS to choose an ephemeral port; hya
prints the actual listening address on startup.

## `403 permission_denied: request refused: Host "…" is not an allowed name`

The server answers only requests whose `Host` (or HTTP/2 `:authority`) names
`localhost`, `127.0.0.1`, or `[::1]` (any port), the host of a non-wildcard
`--bind`, or an `--allow-host` name. This stops DNS-rebinding web pages from
driving the backend (or reading `GET /v1/relay/link`) through your browser.
A request with no `Host` at all (an HTTP/1.0 client) gets `403 … names no
Host`. On the gRPC listener (`HYA_GRPC_BIND`) the same check answers
`PERMISSION_DENIED`.

You see this when you reach the server by another name on purpose — a LAN
address with `--bind 0.0.0.0:8080` or `--mdns`, a name in `/etc/hosts`, a
reverse proxy that forwards its own `Host`. Name it:

```sh
hya serve --bind 0.0.0.0:8080 --allow-host 192.168.1.20 --allow-host hya.lan
hya serve restart --allow-host hya.lan   # the daemon; a later restart keeps the names
```

A server bound to one specific address (`--bind 192.168.1.20:8080`) accepts
that address by itself. `hya serve status` lists the extra names (`hosts`).
Before this check, a server bound to `0.0.0.0` answered any `Host`; now each
LAN name must be listed. See [docs/cli.md](cli.md#allowed-host-names).

## `403 browser requests are not accepted over the relay`

A request that arrived through the secure relay carried an `Origin` or
`Sec-Fetch-*` header — it came from a web page, which may never drive a
remote backend through a bridge. Use the TUI or the WebUI of `hya --connect`
(its browser tab talks to the TUI, not to the bridge), `hya-client`, or
curl; none of them send those headers.

## `401 unauthenticated: the hya bridge needs its token`

A `hya bridge` (standalone, the TUI's `/connect-remote` child, or bare `hya
--connect`'s in-process one) requires its per-bridge token on the first
request of every connection, as `x-hya-bridge-token: <token>`. Take the token
from the bridge's stdout (`hya bridge token …`, or `token` in `--json`) and
send it (`curl -H "x-hya-bridge-token: $TOKEN" …`; for the TUI,
`HYA_SERVER_TOKEN=$TOKEN … --server <bridge url>`). An older TUI that does not
know the token gets this error from a newer `hya bridge`: use the TUI of the
same release. See [docs/relay.md](relay.md#connecting-from-a-client).

## `503 unavailable: remote backend is offline, or the relay link was rotated or is wrong`

The bridge (`hya bridge`, bare `hya --connect`, or the TUI's
`/connect-remote` child) reached the relay, but no backend answered for the
link's room. The TUI shows it as `Connected to the relay, but the remote
backend did not answer …`, as `Remote connection failed: …`, or on the
Project view's notice line (`Temporary session failed: unavailable: …`).
Causes, in order:

1. **The backend is not on the relay.** On the backend machine, run
   `hya serve relay status`; `hya serve relay connect <url>` (or
   `hya serve start --relay <url>`) joins it.
2. **The link was rotated or is wrong.** A rotated or mistyped key looks
   exactly like an offline backend (the proxy refuses the open before the
   backend sees it). Get the current link with `hya serve relay link` on the
   backend machine.
3. **The path to the relay drops streams.** Run `hya relay doctor <link>`
   (see below).

`Remote bridge exited (…)` means the bridge process itself ended; its last
line is in the parentheses. `/connect-remote` starts a new one and
`/disconnect-remote` returns to the local backend. `No project is open ·
choose a project or start a temporary session` is not an error: a remote
start creates no session until you pick a Project or press `t` in the
Project view. See [docs/relay.md](relay.md#connecting-from-a-client) and
[docs/tui.md](tui.md#remote-backends-connect-remote).

## Relay: `hya proxy`/`hya relay doctor` Cannot Reach Each Other

The secure relay (`hya proxy`, `hya relay doctor`) works through nginx,
Cloudflare Tunnel, Caddy, Tailscale, and direct TLS, but an intermediary can
still be misconfigured. Run `hya relay doctor <proxy-url-or-link>` first —
its report names the failing binding, the probe failure kind, and the
recommended `t=` value. See [docs/relay.md](relay.md#hya-relay-doctor) for
the full report shape, [docs/relay.md#troubleshooting](relay.md#troubleshooting)
for the table keyed by doctor output, and
[docs/relay.md#deployment-recipes](relay.md#deployment-recipes) for
complete, known-good configs (Cloudflare Tunnel, nginx, Caddy, Tailscale,
direct TLS).

## Process Agent E2E (`hya-e2e`) Fails

Track P tests spawn a real `hya` against a local FakeLlm. Common failures:

1. **Binary missing** — build first:
   ```sh
   cargo build -p hya-backend --bin hya
   cargo test -p hya-e2e -- --test-threads=1
   ```
2. **Port / process flakiness** — always use `--test-threads=1`.
3. **MCP `unknown tool: mcp__…`** — MCP must finish connecting before the tool
   call. The harness sets `HYA_DEFER_SIDEPLANES=0` for MCP fixtures and waits on
   `GET /v1/mcp` (Mcp `GetMcpStatus`) until the server reports `connected`. See
   [process-e2e.md](testing/process-e2e.md).
4. **Hyabundle “exact lowercase .hyabundle suffix”** — install paths must end in
   `.hyabundle` (use `materialize_public_bundle`, not the raw `.7z` fixture path).
5. **Weak-looking asserts** — oracles should check disk effects, tree depth, or
   follow-up FakeLlm **tool results**, not only request counts. Inventory:
   [agent-matrix.md](testing/agent-matrix.md).
