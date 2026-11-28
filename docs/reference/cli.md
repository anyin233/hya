# CLI Reference

`hya` is a single command-line binary. Running `hya` with no subcommand opens the interactive terminal UI and browser WebUI; passing a subcommand runs headless tasks, manages the background server, configures providers, or controls bundles and workflows.

---

## Quick command map

| Area | Commands |
| --- | --- |
| **Interactive UI** | `hya`, `hya --resume`, `hya --port <N>`, `hya --backend <URL>`, `hya --connect [<LINK>\|-]` |
| **Editor integration** | `hya acp` |
| **Headless runs** | `hya exec`, `hya -p` (`--prompt`), `hya loop` |
| **Background server** | `hya serve`, `hya serve start\|status\|stop\|restart` |
| **Remote relay** | `hya proxy`, `hya bridge`, `hya relay doctor`, `hya serve relay connect\|disconnect\|status\|link\|rotate` |
| **Providers & auth** | `hya provider add\|list\|remove\|logout`, `hya login`, `hya oauth login\|status`, `hya models` |
| **Sessions** | `hya sessions`, `hya sessions archive\|unarchive`, `hya tail-session` |
| **Agents, Bundles & Workflows** | `hya agent list`, `hya bundle ...`, `hya workflow ...` |
| **Offline v1 → v2 Migration** | `hya-migration check`, `hya-migration run [--dry-run] [--backup-dir <DIR>]`, `hya-migration rollback --backup <DIR>` |
| **Self-update** | `hya update`, `hya update version` |

---

## Global options

```text
hya [OPTIONS] [COMMAND]
```

| Flag | Default | Description |
| --- | --- | --- |
| `--model <MODEL>` | Config `default_model` | Override the model for this run (`provider/model` or `provider/model#effort`). |
| `--effort <LEVEL>` | Model/agent default | Reasoning effort (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`). Cannot be combined with a `#` suffix in `--model`. |
| `-p, --prompt <GOAL>` | — | Run headless **Goal mode** instead of opening the UI. |
| `--max-iterations <N>` | `6` | Maximum worker/judge rounds for `-p` Goal mode. |
| `--port <PORT>` | `3250` | Browser WebUI port on `127.0.0.1` when running bare `hya` (`0` picks any free port). |
| `--backend <URL>` | Auto-daemon | Connect bare `hya` to an existing server URL instead of finding or starting a local daemon. |
| `--connect [<LINK>\|-]` | — | Connect bare `hya` to a remote backend over a `hya://` relay link (`-` prompts without echoing; empty reads `$HYA_RELAY_LINK`). |
| `--relay-ca <PEM>` | — | Extra trusted TLS root certificate PEM file (with `--connect` or `serve --relay`). |
| `--transport <MODE>` | `auto` | Relay transport binding: `auto`, `grpc`, or `ws`. |
| `--allow-host <HOST>` | Loopback only | Extra `Host` header name accepted by the server (repeatable, e.g. `--allow-host hya.lan`). |
| `--resume [<ID>]` | — | Reopen a specific session ID in bare `hya`, or open the session picker when `<ID>` is omitted. |
| `--yolo` | `false` | Auto-approve every tool and command without asking. |
| `--bare` | `false` | Run with `BuiltinBundleProfile::Bare` across `hya`, `hya serve` (including `start`/`restart` and daemon auto-start), `hya exec`, `-p`, `hya loop`, `hya acp`, `hya agent list`, and `hya workflow`: loads zero built-in bundles (`bundles/builtin/`), exposes the 8 standard bare tools (`read`, `write`, `edit`, `bash`, `grep`, `todo`, `rewrite`, `skill`) plus built-in `code-mode` (without `agent()`), uses the minimal `hya-bare` default agent, and retains `AGENTS.md`, disk skills, MCP, and user/project bundle/plugin adapters. Combine with `--pure` (`hya exec --bare --pure`) to also skip external `AGENTS.md`, disk skills, MCP, and user/project plugins. |
| `--pure` | `false` | Ignore external context (`AGENTS.md`, MCP servers, user/project plugins, disk skills, and `code-mode` sidecar). Can be combined with `--bare`. |
| `--db <PATH>` | Command-dependent | SQLite database file path (`""` or `":memory:"` for in-memory on `exec` and foreground `serve`). |

---

## Interactive launch (bare `hya`)

Running `hya` on an interactive terminal finds (or starts) the background daemon for `~/.local/state/hya/sessions.db`, starts the WebUI on `http://127.0.0.1:3250`, and opens the terminal UI in your current directory. Passing `hya --bare` starts the auto-launched daemon with `--bare` (zero built-in bundles, `hya-bare` default agent, standard bare tools, and zero built-in TUI bundles).

If `stdin` or `stdout` is not a terminal (for example inside a pipe or CI script), bare `hya` prints a short version and usage banner and exits `0`.

### `hya acp` (Editor integration)

```sh
hya [--db <PATH>] [--model <MODEL>] acp
```

Runs the [Agent Client Protocol (ACP v1)](../guide/editor-integration.md) JSON-RPC 2.0 server on `stdin`/`stdout` for editors like Zed, attaching to (or starting) the local database's Hya daemon (`>= 0.50.0`).

---

## Headless commands

### `hya exec`

Run a single prompt non-interactively and print the reply. Pass `-` as the prompt to read from `stdin`.

```sh
hya exec [OPTIONS] <PROMPT | ->
```

| Flag | Description |
| --- | --- |
| `--json` | Stream committed `Event` envelopes as newline-delimited JSON (JSONL) on `stdout`. |
| `--include-subagents` | With `--json` (in-process full mode), include events from spawned child and grandchild subagent sessions. |
| `--time-budget <SECS>` | Wall-clock budget in seconds; injects progress, wrap-up, and final-window notices and caps per-call `bash` waits. |

Global flags such as `--bare` and `--pure` can be placed either before or after `exec` (e.g., `hya exec --bare "fix tests"` or `hya --bare --pure exec "benchmark prompt"`).

### `hya -p` / `--prompt` (Goal mode)

```sh
hya -p "<GOAL>" [--max-iterations <N>] [--evaluator-model <MODEL>]
```

Runs iterative turns in memory through the `hya/goal` native bundle's `GoalDriver` until an independent evaluator (`goal.evaluate` hook or tool-free verifier model) confirms `<GOAL>` is met or `--max-iterations` (default `6`) is reached. Requires `hya/goal` to be enabled; fails with an actionable error in `--bare` mode or when `hya/goal` is disabled in `config.yaml` (`bundles.builtin.disable: [hya/goal]`). Per-bundle evaluator model defaults can be configured in `~/.config/hya/bundles/hya%2Fgoal/config.yml` under `goal.evaluator_model`.

### `hya loop`

```sh
hya loop --target "<OBJECTIVE>" [--until "<CMD>" | --while "<CMD>"] [--budget <N>] [--evaluator-model <PROVIDER/MODEL>]
```

Iterates toward `--target` up to `--budget` rounds (default `10`, clamped to `1..=100`; `--max-iterations` is an alias). With `--until`, stops when `<CMD>` exits `0` (continues on exit `1`); `--while` inverts that check. Any exit code other than `0` or `1` (or timeout) aborts the loop with a non-zero exit status as a broken condition.

`hya loop` is provided by the built-in native bundle `hya/loop` (`LoopDriver`, dispatched through `HostRuntime::loop_driver()`). Running with `--bare` or with `hya/loop` disabled in `config.yaml` (`bundles.builtin.disable: [hya/loop]`) exits with an actionable error instead of running silently without a driver.

---

## Server & Daemon commands (`hya serve`)

### Foreground server

```sh
hya serve [--bind 127.0.0.1:8080] [--db ./sessions.db] [--bare] [--allow-host <HOST>]... [--relay <URL>]
```

Serves both `/v2` HTTP/JSON+SSE+WebSocket and `hya.v2` gRPC on `--bind` (default `127.0.0.1:8080`, in-memory DB unless `--db` is given). Prints `hya server listening on <url>` on `stdout` when ready. Adding `--bare` starts the server on `BuiltinBundleProfile::Bare` with zero `bundles/builtin/` loaded (`hya-bare` default agent, standard bare tools, and an empty built-in bundle catalog).

### Background daemon lifecycle

Uses `~/.local/state/hya/sessions.db` unless `--db <PATH>` is given:

| Command | Description |
| --- | --- |
| `hya serve start [--bare] [--json]` | Start the detached background daemon if not already running. Accepts `--bare`, `--relay`, `--relay-transport`, `--relay-ca`, `--relay-ephemeral`, `--relay-heartbeat`, and `--allow-host`. |
| `hya serve status [--json]` | Print URL, PID, version, database path, uptime, log path, and relay status (exits `1` if stopped). |
| `hya serve stop [--force] [--timeout <SECS>]` | Drain active turns and stop the daemon cleanly (default timeout `45`s; `--force` sends `SIGKILL` after timeout). |
| `hya serve restart [--bare] [--json] [--force] [--timeout <SECS>]` | Stop the running daemon and start a fresh one, preserving relay and `--allow-host` settings and restoring interrupted sessions. |

---

## Relay commands

| Command | Description |
| --- | --- |
| `hya proxy [--host 0.0.0.0] [--port 8766] [--tls-cert <PEM> --tls-key <PEM>] [--path-prefix <PATH>] [--trust-forwarded <HEADER>]` | Run the stateless, blind `hya.relay.v1` rendezvous proxy (`--max-rooms`, `--max-streams-per-room`, `--idle-timeout-secs`, `--stream-rate-bytes-per-sec`). |
| `hya relay doctor <PROXY_URL_OR_LINK> [--relay-ca <PEM>] [--timeout <SECS>] [--measure-idle] [--json]` | Probe TLS, gRPC, and WebSocket relay paths end to end and recommend a `t=` transport setting. |
| `hya bridge [<LINK>\|-] [--listen 127.0.0.1:0] [--transport auto\|grpc\|ws] [--relay-ca <PEM>] [--json] [--exit-with-stdin]` | Open a local loopback HTTP bridge to a remote backend over Noise `NKpsk0` (prints URL and `x-hya-bridge-token`). |
| `hya serve relay connect <URL> [--transport ...] [--relay-ca ...] [--ephemeral] [--json]` | Attach a running daemon to a relay and print its secret `hya://` link. |
| `hya serve relay status [--json]` | Show the running daemon's relay state, room ID, redacted link, and active stream count. |
| `hya serve relay link` | Print the full secret `hya://` link on `stdout`. |
| `hya serve relay rotate [--json]` | Generate a new pre-shared key, immediately revoking all previous links. |
| `hya serve relay disconnect [--json]` | Disconnect the running daemon from the relay. |

---

## Provider, Auth & Model commands

| Command | Description |
| --- | --- |
| `hya provider add [--name <ID>] [--base-url <URL>] [--protocol <PROTO>] [--api-key <KEY>] [-y]` | Interactive or scripted wizard to add an `openai-chat`, `openai-responses`, or `anthropic-messages` provider and fetch its models (`hya providers` is an alias). |
| `hya provider list [--refresh]` | List configured providers, auth sources, and resolved models. |
| `hya provider logout <ID>` | Delete the saved credential file `~/.config/hya/auth/<id>.yaml`. |
| `hya provider remove <ID> [-y]` | Remove the provider from `config.yaml`, delete its saved key, and clear cached models. |
| `hya login <PROVIDER> <TOKEN>` | Save a static API token to `~/.config/hya/auth/<provider>.yaml` (`0600`). |
| `hya oauth login --provider <ID> --type <openai-codex\|grok-build> [--browser\|--no-browser] [--loopback]` | Sign in via subscription OAuth (default device-code flow; `--loopback` uses `localhost:1455` PKCE for `openai-codex`). |
| `hya oauth status [<PROVIDER>]` | Show non-secret OAuth status and token expiry. |
| `hya models [<PROVIDER>] [--verbose] [--refresh]` | List resolved `provider/model` entries (`--refresh` updates the remote model cache; `--verbose` prints JSON metadata lines). |
| `hya agent list` | List built-in selectable agents and their roles. |

---

## Session commands

| Command | Description |
| --- | --- |
| `hya sessions [--all \| --archived] [--db <PATH>]` | List active root sessions newest-first (`--all` includes child subagent and archived sessions; `--archived` shows only archived roots). |
| `hya sessions archive <SESSION_ID> [--db <PATH>]` | Archive a root session. |
| `hya sessions unarchive <SESSION_ID> [--db <PATH>]` | Unarchive a root session. |
| `hya tail-session <SESSION_ID> [--db <PATH>]` | Dump a session's complete durable event log as JSONL. |

---

## Workflow & Bundle commands

### `hya workflow`

Backed by the built-in ABI v2 native bundle `hya/workflow` (`bundles/builtin/workflow/`). Returns an actionable error when run under `--bare` or when `hya/workflow` is disabled in `config.yaml`.

```sh
hya workflow list [--json]
hya workflow info <NAME> [--json]
hya [--db <DB>] workflow use <NAME> --session <SESSION_ID> [--revision <REV>]
hya [--db <DB>] workflow run [<NAME>] [--session <SESSION_ID>] [--input KEY=VALUE]... [--revision <REV>] [--json]
hya [--db <DB>] workflow state --session <SESSION_ID> [--json]
```

**Bundle API surface (`hya/workflow`):**

| Method & Scope | Path | Payload / Response |
| --- | --- | --- |
| `GET` (global) | `/v2/bundles/hya%2Fworkflow/api/workflows?directory=<abs>` | `{ "workflows": WorkflowSummary[] }` |
| `GET` (session) | `/v2/sessions/{id}/bundles/hya%2Fworkflow/state` | `WorkflowProjection` (`extensions["hya/workflow"]`) |
| `POST` (session) | `/v2/sessions/{id}/bundles/hya%2Fworkflow/command` | Body: `{ list }` \| `{ state }` \| `{ info: { name } }` \| `{ select: { name, expectedRevision? } }` \| `{ run: { name?, expectedRevision?, inputs?, run?, delivery?: "started"\|"finished" } }` |

### `hya bundle`

Manage `.hyabundle` packages in `--user` scope (default, `~/.local/share/hya/bundles/registry.sqlite3`) or `--project` scope (`./.hya/bundles/`):

| Command | Description |
| --- | --- |
| `hya bundle install [--user\|--project] [-y] [--overwrite] <PACKAGE>` | Verify, self-check, and install a `.hyabundle` package. |
| `hya bundle install [--user\|--project] [-y] [--overwrite] --claude <SOURCE>` | Convert and install a Claude Code plugin directory or `<marketplace>#<entry>`. |
| `hya bundle remove [--user\|--project] [-y] <BUNDLE_ID>` | Uninstall a bundle (`uninstall` is an alias). |
| `hya bundle enable <BUNDLE_ID>` / `disable <BUNDLE_ID>` | Enable or disable a bundle across scopes without uninstalling it. |
| `hya bundle refresh` | Reload edited project bundles and `config.yml` files in the running daemon. |
| `hya bundle pack <SOURCE_DIR> <OUTPUT.hyabundle>` | Build a deterministic public `.hyabundle` archive from a bundle source directory. |
| `hya bundle verify [--user\|--project] [--overwrite] [--strict] [--protocol-smoke] <PACKAGE_OR_DIR>` | Dry-run install and self-proof checks against a `.hyabundle` archive or source directory (`--strict` checks schema/descriptors/secret hygiene; `--protocol-smoke` verifies JS JSON-RPC `initialize`/`shutdown` parity). |
| `hya bundle list [--user\|--project]` | List builtin, user, and project bundles. |
| `hya bundle info [--user\|--project] <BUNDLE_ID \| -f PACKAGE>` | Inspect bundle digests, agents, skills, tools, hooks, MCP servers, and APIs. |
| `hya bundle search [--user\|--project] <QUERY>` | Case-insensitive search across bundle, agent, and skill IDs. |
| `hya bundle schema [--user\|--project] <BUNDLE_ID \| PACKAGE>` | Show custom URI schemes (`schemas:`) declared by a bundle. |

> **Publication vs. activation:** Package publication validates the merged catalog after first-party shadowing, the complete installed `BundleCatalog`, and reserved core Agent ids before atomic generation publication. At runtime, each JavaScript activation materializes only the selected Agent's captured Tool/Hook/Skill capability closure and exact-path-matched JavaScript Extension entrypoints.

---

## Offline Migration (`hya-migration`)

The standalone `hya-migration` binary (shipped alongside `bin/hya`) upgrades v1 SQLite databases (`sessions.db`), `config.yaml`, `tui.toml`/`tui.json`, `.hya/bundles/`, and `registry.sqlite3` to Hya v2 with automatic SHA-256 backup snapshots and verified rollback. See **[Migrating from Hya v1 to v2](../guide/migration-v1-to-v2.md)** for full target flags, backup manifest format, and mapping tables.

```sh
hya-migration [--json] check [--db <PATH>] [--config <PATH>] [--tui-toml <PATH>] [--tui-json <PATH>] [--registry <PATH>] [--bundles-dir <DIR>]...
hya-migration [--json] run [--dry-run] [--backup-dir <DIR>] [TARGET_FLAGS...]
hya-migration [--json] rollback --backup <DIR>
```

---

## Database lock & exit codes

Only **one writer** may hold a file-backed SQLite database (`<db>.runtime-owner.lock`) at a time:
- When a daemon holds `<db>` (and published `<db>.server.json`), commands like `hya --db <db> exec`, `hya workflow run`, and `hya sessions archive` automatically route through that running server over `/v2`!
- Commands with flags a running server cannot honor per-call (such as `exec --bare`, `exec --pure`, or a second `hya serve` on the same DB) refuse cleanly with exit code **`75`**.
- Read-only commands (`hya sessions`, `hya tail-session`, `hya workflow list|info`) never take the write lock and work anytime.

| Exit code | Meaning |
| --- | --- |
| `0` | Success (including clean daemon `SIGTERM`/`SIGINT` drain). |
| `1` | General runtime, validation, provider, or usage error. |
| `75` | Database is owned by another process (`EX_TEMPFAIL`), or flag cannot route through the running server. |
| `130` | Interrupted by `Ctrl+C` (`SIGINT`). |
| `143` | Terminated by `SIGTERM`. |
