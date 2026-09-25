# Troubleshooting

## Bare `hya` Exits After Guidance

The `hya` binary bundles no interactive frontend: bare `hya` (no subcommand)
prints a version banner plus guidance and exits. Run the backend headlessly, or
start the OpenTUI frontend from a checkout; it starts (and stops) its own
`hya serve`, found through `--hya <path>`, `HYA_BIN`, or `PATH`:

```sh
hya exec "summarize this repo"

# From the repository root
bun packages/hya-tui/src/main.ts --dir "$PWD"
```

Running the server yourself in a second terminal is optional — for a
shared server, another machine, or custom `serve` flags:

```sh
# Terminal 1
hya serve --bind 127.0.0.1:8080
# Terminal 2
bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080
```

## TUI Cannot Start Its Backend

Without `--server` the TUI starts `hya serve` itself. If that fails it
prints the reason and the last lines of the server's output, then exits
with status 1:

- `hya binary not found: …` — no `--hya`, no `HYA_BIN`, and no `hya` on
  `PATH`, or the path given does not exist. Build one
  (`cargo build -p hya-backend --bin hya`) and pass
  `--hya target/debug/hya`, or connect to a running server with `--server`.
- `hya serve exited with code N before it was ready` — the server failed
  at startup; the output lines below it say why (a config error, an
  unusable `--db` path, …). Run `hya serve --bind 127.0.0.1:0` in the same
  directory to see the full output.
- `hya serve did not print its readiness line within 60 s` — the server
  hung during startup; see [Diagnosing Slow Startup](#diagnosing-slow-startup).

`/status` in the TUI shows the backend it started (pid, binary, database).

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

## SQLite Database Is Locked

File-backed stores use WAL mode and a five-second busy timeout. If lock errors
continue:

- make sure another process is not holding a long write transaction
- use a separate database path for separate local experiments
- use an empty `--db ""` for in-memory one-off runs

## The Server Binds an Unexpected Port

Use an explicit bind address:

```sh
hya serve --bind 127.0.0.1:8080 --db hya.db
```

Use `127.0.0.1:0` only when you want the OS to choose an ephemeral port; hya
prints the actual listening address on startup.

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
