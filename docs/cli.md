# CLI Reference

The backend CLI/API binary is `hya-backend`, defined in
[`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs).

## Global Options

```text
hya-backend [--model <MODEL>] [--prompt <GOAL>] [--max-iterations <N>]
     [--yolo] [--db <PATH>] [--resume <SESSION>] [COMMAND]
```

| Option | Meaning |
| --- | --- |
| `--model <MODEL>` | Override `default_model` from hya config and `HYA_MODEL`. |
| `-p, --prompt <GOAL>` | Run headless goal mode instead of the TUI or a subcommand. |
| `--max-iterations <N>` | Iteration cap for goal mode. Defaults to `6` in the CLI. |
| `--yolo` | Auto-approve every tool action. This applies to TUI, headless, and server composition. |
| `--db <PATH>` | SQLite database path. Empty string uses an in-memory store. Used by the TUI, `serve`, headless `exec`/`run`, `sessions`, and `tail-session`; goal mode and `rpc` stay in-memory. |
| `--resume <SESSION>` | Resume a session in the interactive TUI. Accepts any valid `SessionId` form: `hysec_...`, `ses_...`, or legacy raw UUID. |
| `--print-logs`, `--log-level`, `--pure` | Accepted Compat-compatible global flags. |

`--resume` is interactive-only and cannot be combined with `--prompt` or a
subcommand. Bare `hya-backend --resume <ID>` launches `hya --session <ID>`.
When `--prompt` is present, it takes precedence over subcommand dispatch.

When `--db <PATH>` is supplied, hya persists the canonical event log, not just
the rendered transcript. The SQLite file can contain prompts, tool arguments,
tool results, reasoning deltas, command metadata, absolute workdir paths, and
other replay data. The file is plain SQLite; encryption and permissions are the
caller’s responsibility and file mode follows the process umask, so place it in a
private directory.

## `hya` frontend

```sh
hya [PROJECT] [OPTIONS]
```

`hya` is the canonical Unix entrypoint. It delegates to the adjacent `hya-ts`
launcher, which starts the TypeScript/OpenTUI frontend and an owned local
`hya-backend`. Use `--server` to attach to an existing backend instead.

| Option | Meaning |
| --- | --- |
| `PROJECT` | Project directory. Defaults to the current directory. |
| `--server <URL>` | Attach to an existing backend. |
| `--backend-bin <PATH>` | Override the backend executable. |
| `--bun <PATH>` | Override the Bun executable. |
| `--import <SOURCE>` | Import configuration. The supported source is `compat`. |
| `--continue` | Continue the latest session. |
| `--session <ID>` | Resume an exact session id. |
| `--fork` | Fork the continued or selected session. |
| `--prompt <TEXT>` | Submit an initial prompt. |
| `--agent <NAME>` | Select the initial agent. |
| `--model <PROVIDER/MODEL>` | Select the initial model. |

Examples:

```sh
hya .
hya --continue
hya --session hysec_...
hya --server http://127.0.0.1:8787
hya --import compat
```

`hya-ts` exposes the same launcher surface for diagnostics. Normal use should
invoke `hya` so help and errors retain canonical branding. In the TUI, press
`Ctrl-P` for the authoritative command list and `Ctrl-X` for leader-key actions.

## `hya-backend exec`

```sh
hya-backend exec "summarize this repo"
hya-backend exec --json "summarize this repo"
```

Runs one headless turn and prints the rendered transcript. The command uses the
global `--db <PATH>` SQLite store when supplied; otherwise it uses an in-memory
store. With `--db`, the database stores the full canonical event log for replay,
which can contain more sensitive data than the rendered transcript. `--json`
prints the canonical event stream as JSONL.

## `hya-backend run`

```sh
hya-backend run "summarize this repo"
hya-backend run --format json "summarize this repo"
```

Compat-compatible alias for `exec`. Message words are joined with spaces.
Like `exec`, `run` persists only when the global `--db <PATH>` is supplied.
`--format json` and `--json` both emit event JSONL.

## `hya-backend -p`

```sh
hya-backend -p "make the workspace compile" --max-iterations 6
```

Runs goal mode with an in-memory store. Each iteration runs an agent turn, then
an independent evaluator judges the transcript. The run stops when the evaluator
returns `met=true`, a cap is reached, or cancellation is requested. Goal mode
does not persist to the global `--db` database.

## `hya-backend serve`

```sh
hya-backend serve --bind 127.0.0.1:8080 --db hya.db
```

Starts the HTTP/SSE API from [`../crates/hya-server`](../crates/hya-server).

| Flag | Meaning |
| --- | --- |
| `--bind <ADDR>` | Socket address. Defaults to `127.0.0.1:8080`; use `127.0.0.1:0` for an ephemeral port. |
| `--hostname <HOST>` | Compat-compatible alias for the host part of `--bind`. |
| `--port <PORT>` | Compat-compatible alias for the port part of `--bind`. |
| `--mdns` | Bind to `0.0.0.0` when no hostname is supplied. hya does not advertise mDNS yet. |
| `--mdns-domain <NAME>` | Accepted for Compat CLI compatibility. |
| `--cors <ORIGIN>` | Accepted for Compat CLI compatibility; hya mirrors CORS origins globally. |
| `--db <PATH>` | SQLite path. Empty string uses an in-memory store. |

The server mounts native `/sessions/*` routes plus Compat-compatible legacy
and v2 route groups. See
[`compat-parity.md`](compat-parity.md) for exact compatibility status.

## Auth and Catalog Commands

```sh
hya-backend login <provider> <token>
hya-backend oauth login --provider <name> --type <openai-codex|grok-build> [--device] [--loopback] [--no-browser] [--browser] [--model <id>] [--base-url <url>]
hya-backend oauth status [provider]
hya-backend auth list
hya-backend auth logout <provider>
hya-backend providers list
hya-backend providers logout <provider>
hya-backend models [provider] [--verbose] [--refresh]
hya-backend agent list
```

`login` writes a plain provider token under `~/.config/hya/auth`. Prefer
`oauth login` for ChatGPT Codex and Grok Build subscription auth — it runs the
OAuth flow in Rust, stores a refreshable credential bundle, and upserts the
provider route into `config.yaml`. For `openai-codex`, the default matches
Codex CLI: **device-code with URL/code printed** (no auto-open browser). Use
`--browser` to open the verification URL, or `--loopback` for localhost PKCE.
Saved credentials take precedence over inline `api_key` values. `providers` is
an alias for `auth`. `models --refresh` is accepted for Compat compatibility
but does not fetch a remote catalog.

The same auth/oauth commands are available on canonical `hya` (forwarded to
`hya-backend`, using the same credential store):

```sh
hya oauth login --provider codex --type openai-codex
hya oauth login --provider grok --type grok-build --no-browser
hya oauth status
hya login anthropic "$ANTHROPIC_API_KEY"
hya auth list
```

## Session and RPC Commands

```sh
hya-backend sessions --db hya.db
hya-backend rpc
```

`sessions` lists persisted sessions in a SQLite database, including sessions
created by `exec --db` and `exec --json --db`. `rpc` reads JSONL requests on
stdin, accepts `{"type":"prompt","text":"..."}` and `{"type":"quit"}`, and
emits new session events plus a `{"type":"done"}` marker using an in-memory
store; `rpc` does not persist to the global `--db` database.

## `hya-backend tail-session`

```sh
hya-backend tail-session <session-id> --db hya.db
```

Replays a persisted session's event log as JSON lines. The `<session-id>`
accepts any valid `SessionId` form: `hysec_...`, `ses_...`, or legacy raw UUID.

This command intentionally exits cleanly on broken pipe, so shell filters such
as `head` and `grep -q` can close stdout without causing a panic.
