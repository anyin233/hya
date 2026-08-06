# Troubleshooting

## The TUI Does Not Start

Bare `hya` starts the TypeScript/OpenTUI frontend. Use a real terminal for the
interactive UI, or run a headless command:

```sh
hya-backend exec "summarize this repo"
```

If startup reports that adjacent `hya-ts` is missing or the TUI runtime cannot
be located, reinstall with `./install.sh` (see [Getting Started](getting-started.md)
for flags such as `--bin-dir` and permission preflight); installing only the
`hya` Cargo package is unsupported. If it reports `failed to launch Bun`,
install Bun or pass its path with `hya --bun <PATH>`.

### Startup loading overlay

When the shell is not yet ready, after **500 ms** a bottom-centered spinner
appears: first `Loading plugins...`, then `Finishing startup...` once plugins
are ready. The overlay stays on screen for at least **3 s** after it first
appears so the flash is readable
([`startup-loading.tsx`](../packages/hya-tui-ts/src/upstream/component/startup-loading.tsx)).

Set `HYA_FAST_BOOT` to any non-empty value to suppress the overlay entirely
(useful when measuring startup). See [Diagnosing Slow Startup](#diagnosing-slow-startup).

### Error boundary (crash screen)

An unhandled render error replaces the UI with a full-screen crash view
([`error-component.tsx`](../packages/hya-tui-ts/src/upstream/component/error-component.tsx)):
message and stack, themed for the detected light/dark mode, plus a clickable
**Reset TUI** control and **Exit**. Reset re-mounts the app without restarting
the process, so the backend session can survive. Ctrl+C on that screen exits.

## Diagnosing Slow Startup

Set `HYA_STARTUP_TRACE=1` (the only truthy values are exactly `1` or `true`,
case-insensitive) to emit structured startup marks on **stderr** from the Rust
launcher, the backend serve path, and the TypeScript TUI.

```sh
HYA_STARTUP_TRACE=1 hya . 2>trace.log
```

Each mark is one JSON line with wall-clock (and on the TS side, monotonic)
timestamps. Useful marks, roughly in emission order:

| Mark | Source | Notes |
| --- | --- | --- |
| `hya_ts_start` | `hya-ts` | Launcher entry. |
| `backend_spawn` | `hya-ts` | Only when the launcher auto-spawns `hya-backend`. |
| `backend_listen` | `hya-ts` / `hya-backend` | Backend announced its listen URL. |
| `bun_entry` | TS TUI | Bun entrypoint started. |
| `theme_resolved` | TS TUI | Terminal theme mode resolved. |
| `shell_paint` | TS TUI | Detail `immediate` by default; `routes_ready` when `HYA_SYNC_PLUGIN_START` gates shell routes. |
| `plugin_host_done` | TS TUI | Builtin plugin host finished start. |
| `sync_partial` | TS TUI | First partial sync. |
| `sync_complete` | TS TUI | Sync complete. |

Related comparison flags:

- `HYA_SHOW_TTFD` — on-screen time-to-first-draw overlay when set to `1`/`true`.
- `HYA_FAST_BOOT` — any non-empty value skips the loading overlay.

## Copy/Paste Does Not Work

Clipboard behaviour
([`clipboard.ts`](../packages/hya-tui-ts/src/upstream/clipboard.ts)):

- When `TMUX` or `STY` is set, OSC-52 clipboard sequences are wrapped in a
  tmux/screen passthrough. A **stale** `TMUX` variable outside a real tmux
  session can break that passthrough.
- When `WAYLAND_DISPLAY` is set, hya prefers `wl-copy` / `wl-paste` over X11
  tools (`xclip` / `xsel`). Install `wl-clipboard` on Wayland desktops.

## Provider Call Fails with `http: <status>: ...`

Upstream HTTP failures surface as `ProviderError::Http` with a message shaped
like `{status}: {body snippet}`
([`crates/hya-provider/src/http.rs`](../crates/hya-provider/src/http.rs)). The
response body is truncated to the **first 500 characters**, so a long HTML error
page is cut off.

**No automatic retry** is attempted at the provider layer for 429 or 5xx: the
turn fails immediately.

Once the SSE stream is open, any frame whose JSON carries an `error` object
aborts the stream with `Http(message)` **before** the frame reaches the protocol
decoder
([`crates/hya-provider/src/http/stream.rs`](../crates/hya-provider/src/http/stream.rs)).
Mid-stream provider errors therefore use the same error variant as non-2xx
responses.

## hya Uses the Offline Provider

If the response starts with `(hya dev provider)`, hya did not find a usable
live provider route. Check:

- `$XDG_CONFIG_HOME/hya/config.yaml`
- `$HOME/.config/hya/config.yaml`
- each provider has `kind`, `base_url`, and at least one model under `models`
- each provider has either an inline `api_key` or a saved token from
  `hya-backend login <provider> <token>`
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

Headless `exec`, `run`, goal mode, `rpc`, and `serve` install an automatic
permission responder. By default it allows reads, globs, grep, shell, MCP, and
edits that stay inside the active workdir after symlink-aware resolution. Edits
outside the workdir are rejected.

Use `--yolo` only when you intentionally want to auto-approve all tool actions,
including edits outside the workdir.

## Shell Output Is Truncated

Tool outputs are capped to protect model context. Large stdout/stderr strings
include a truncation marker. Narrow the command output or write results to a
file and read the specific section you need.

## `tail-session` Cannot Parse the Session Id

`tail-session` accepts any valid session id: a new `hysec_...` id, a legacy
`ses_...` display id, or a legacy raw UUID:

```sh
hya-backend tail-session hysec_ABCDEFGHIJKLMNOPQRST --db hya.db
```

If parsing fails, confirm the id came from `hya-backend sessions --db <PATH>`
for the same database path.

## Server SSE Emits `resync`

`GET /sessions/:id/stream` emits a `resync` SSE event if the broadcast receiver
lagged. The client should call:

```text
GET /sessions/:id/events?since_seq=<last_seen_seq>
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
hya-backend serve --bind 127.0.0.1:8080 --db hya.db
```

Use `127.0.0.1:0` only when you want the OS to choose an ephemeral port; hya
prints the actual listening address on startup.

## Process Agent E2E (`hya-e2e`) Fails

Track P tests spawn a real `hya-backend` against a local FakeLlm. Common failures:

1. **Binary missing** — build first:
   ```sh
   cargo build -p hya-backend --bin hya-backend
   cargo test -p hya-e2e -- --test-threads=1
   ```
2. **Port / process flakiness** — always use `--test-threads=1`.
3. **MCP `unknown tool: mcp__…`** — MCP must finish connecting before the tool
   call. The harness sets `HYA_DEFER_SIDEPLANES=0` for MCP fixtures and waits on
   `GET /mcp` status `connected`. See [process-e2e.md](testing/process-e2e.md).
4. **Hyabundle “exact lowercase .hyabundle suffix”** — install paths must end in
   `.hyabundle` (use `materialize_public_bundle`, not the raw `.7z` fixture path).
5. **Weak-looking asserts** — oracles should check disk effects, tree depth, or
   follow-up FakeLlm **tool results**, not only request counts. Inventory:
   [agent-matrix.md](testing/agent-matrix.md).
