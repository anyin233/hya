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
| `--db <PATH>` | SQLite database path. Semantics of an empty value depend on the command (see below). |
| `--resume <SESSION>` | Resume a session in the interactive TUI. Accepts any valid `SessionId` form: `hysec_...`, `ses_...`, or legacy raw UUID. |
| `--print-logs` | Compat-compatible global flag. Parsed, then ignored (no-op). |
| `--log-level <LEVEL>` | Compat-compatible global flag. `LEVEL` must be one of `DEBUG`, `INFO`, `WARN`, `ERROR` (clap rejects any other value). The value is discarded after parsing — it does not enable logging. |
| `--pure` | Compat-compatible global flag. Parsed, then ignored (no-op). |

`--print-logs`, `--log-level`, and `--pure` exist only so Compat/OpenCode command
lines are accepted unchanged. They are never read after clap parse. hya-backend
does not expose a CLI switch for verbose tracing today. Many operational notices
go to stderr; the serve readiness line
`hya server listening on <url>` is printed on **stdout** (see
[`serve`](#hya-backend-serve)).

### `--db` empty-string semantics

Empty `--db` is **not** always in-memory:

| Command path | Empty `--db` means |
| --- | --- |
| `exec`, `run`, `serve` | In-memory store (`open_store("")` → `SessionStore::connect_memory`). |
| Bare interactive startup, `sessions`, `tail-session` | Remapped to `$XDG_STATE_HOME/hya/sessions.db`, falling back to `$HOME/.local/state/hya/sessions.db` (or `./.local/state/hya/sessions.db` when neither is set). The directory is created if missing. |

`resolve_interactive_db` performs that remap so `hya --continue` / `hya -s` can
resume across restarts. An explicit `--db ""` is **not** distinguishable from
the clap default on those remapped commands, so it still lands on the durable
path. To force an in-memory store for interactive work, use the `hya` frontend's
`HYA_DB=` empty override (see below) rather than `--db ""` on `hya-backend`.

When `--db <PATH>` is a non-empty path, hya persists the canonical event log, not
just the rendered transcript. The SQLite file can contain prompts, tool
arguments, tool results, reasoning deltas, command metadata, absolute workdir
paths, and other replay data. The file is plain SQLite; encryption and
permissions are the caller’s responsibility and file mode follows the process
umask, so place it in a private directory.

`--resume` is interactive-only and cannot be combined with `--prompt` or a
subcommand. Bare `hya-backend --resume <ID>` launches `hya --session <ID>`.
When `--prompt` is present, it takes precedence over subcommand dispatch.

## `hya` frontend

```sh
hya [OPTIONS] [PROJECT] [COMMAND]
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
| `-c, --continue` | Continue the most recently updated root session from the persisted store. |
| `-s, --session <ID>` | Resume an exact session id (`hysec_…` / `ses_…`). |
| `--fork` | Fork the continued or selected session. |
| `--prompt <TEXT>` | Submit an initial prompt. |
| `--agent <NAME>` | Select the initial agent. |
| `--model <PROVIDER/MODEL>` | Select the initial model. |

### Validation rules

- `--import` cannot be combined with any subcommand (`bundle`, `oauth`, `login`,
  `auth`, …). Doing so prints
  `<invocation>: --import cannot be used with a subcommand` to stderr and exits
  with code 1.
- `--fork` requires `--continue` or `--session`. Without one of those, the
  launcher prints
  `<invocation>: --fork requires --continue or --session` and exits 1.

Examples:

```sh
hya .
hya -c
hya --continue
hya -s hysec_...
hya --session hysec_...
hya --server http://127.0.0.1:8787
hya --import compat
```

Owned backends started by `hya` persist sessions to
`$XDG_STATE_HOME/hya/sessions.db` (or `~/.local/state/hya/sessions.db`). Override
with `HYA_DB=/path/to.db`, or set `HYA_DB=` empty for an in-memory store. This
empty override is the intentional way to force in-memory interactive runs when
`hya-backend`'s remapped empty `--db` would otherwise open the durable path.

`hya-ts` exposes the same launcher surface for diagnostics. Normal use should
invoke `hya` so help and errors retain canonical branding. In the TUI, press
`Ctrl-P` for the authoritative command list and `Ctrl-X` for leader-key actions.

## TUI Slash Commands

The TypeScript TUI registers built-in slash commands and keybinds. The full
command / keybind / alias table lives in
[TUI Keybindings](tui-keybindings.md) (including which-key and every default
chord). This section is the CLI-facing summary so readers of this reference can
find and use the commands without leaving the CLI docs entirely.

Common slash commands (aliases in parentheses):

| Command | Aliases | Effect (summary) |
| --- | --- | --- |
| `/sessions` | `/resume`, `/continue` | Open the session-list dialog |
| `/new` | `/clear` | Start a new session (home route) |
| `/models` | `/mo`, `/model` | Open the model picker (`/mo` biases fuzzy match away from `/move`) |
| `/agents` | | Open the agent picker |
| `/mcps` | | MCP enable/disable dialog |
| `/variants` | `/think` | Model variant picker (hidden when the model has none) |
| `/status` | | Status dialog |
| `/themes` | | Theme list |
| `/help` | | Help dialog |
| `/exit` | `/quit`, `/q` | Exit the app |
| `/rename` | | Rename the current session |
| `/timeline` | | Jump-to-message dialog |
| `/fork` | | Fork from a selected message |
| `/compact` | `/summarize` | Compact / summarize the session |
| `/undo` | | Abort an in-flight turn if the session is not idle, then revert at the last user message before the current revert point (repeatable walks backwards). **Overwrites the prompt buffer** with that message’s text parts and re-attaches its file parts (any draft text already typed is lost). |
| `/redo` | | Redo after a revert |
| `/timestamps` | `/toggle-timestamps` | Toggle message timestamps |
| `/thinking` | `/toggle-thinking` | Cycle thinking-block visibility |
| `/copy` | | Copy the full transcript to the clipboard |
| `/export` | | Export options dialog; default filename `session-<id8>.md` |
| `/editor` | | Open `$VISUAL` or `$EDITOR` on the prompt buffer |
| `/skills` | | Skill selector; inserts `/<skill> ` into the prompt |
| `/diff` | | Open the git diff viewer |

The **leader** key defaults to `ctrl+x`. Leader chords are written as
`<leader>…` (for example `<leader>n` for new session). `ctrl+p` opens the full
command palette. See [TUI Keybindings](tui-keybindings.md) for every command id
and default binding.

### Backend-provided commands

In addition to the frontend-registered slash commands above, the backend serves
a built-in command catalog from
[`command_catalog.rs`](../crates/hya-server/src/compat/command_catalog.rs) over
`GET /api/command` (and the Compat `/command` surface).

**Expandability.** Every built-in is constructed with `expandable: false`.
Server-side `expand_prompt` only expands entries with `expandable: true`
(user-defined commands and skills). For the eight built-ins below, admitting a
slash command therefore uses the literal admitted text `/<name>` or
`/<name> <arguments>` — **not** the stored AGENTS.md / review template body.
Catalog construction still substitutes the current workdir for `${path}` inside
the *stored* init/review template strings, but that body is not applied by
`expand_prompt` while `expandable` remains false.

| Command | Description | Catalog notes |
| --- | --- | --- |
| `/init` | Guided AGENTS.md setup | Built-in, **not** expandable. Stored template includes `${path}` → workdir at list time; admission still falls back to literal `/init` + args. |
| `/review` | Review changes `[commit\|branch\|pr]`, defaults to uncommitted | Built-in, **not** expandable. Catalog sets `subtask: true` metadata. Same literal-admission rule as `/init`. |
| `/help` | Show help | Built-in, not expandable; template is the literal `/help`. |
| `/model $ARGUMENTS` | Switch the active model | Built-in, not expandable; template is `/model $ARGUMENTS`. |
| `/clear` | Start a fresh session | Built-in, not expandable; template is `/clear`. |
| `/sessions` | Switch session | Built-in, not expandable; template is `/sessions`. |
| `/think $ARGUMENTS` | Set reasoning effort | Built-in, not expandable; template is `/think $ARGUMENTS`. |
| `/workflow $ARGUMENTS` | Inspect or run workflows | Built-in, not expandable; template is `/workflow $ARGUMENTS`. |

User-defined commands from config and on-disk command sources are merged with
this list via upsert: a user-defined command with the same name **overrides** the
built-in of that name. Every user-defined command is constructed with
`expandable: true` **unconditionally** (`CommandInfo::command` hardcodes it);
frontmatter / inline maps have no `expandable` field — writing `expandable` in
`opencode.json` or markdown frontmatter is ignored. Server-side `expand_prompt`
therefore expands those templates.

### Keybindings

The frontend ships a named keybind registry in
[`packages/hya-tui-ts/src/upstream/config/keybind.ts`](../packages/hya-tui-ts/src/upstream/config/keybind.ts)
(`Definitions`: **173** named entries including `leader` and chord defaults).
Each registry entry is `{ default, description }`.

- The special entry `leader` defaults to `ctrl+x`. Bindings written as
  `<leader>x` are leader chords.
- Major namespaces include app, session, pane, model, agent, messages, prompt,
  input, diff, theme, terminal, and which-key.
- The binding schema accepts per-definition overrides under a `keybinds` map
  (Definition names from `keybind.ts`), and `false` or `"none"` unbind a name —
  see [Configuration → TUI Configuration](configuration.md#tui-configuration).
  **Shipped launcher caveat:** the current entrypoint applies defaults only via
  `resolve({}, { terminalSuspend: … })` and loads **no** on-disk TUI config file;
  `config.yaml` has no `tui` / `keybinds` key. Until a host supplies a non-empty
  config object, users cannot override or unbind factory chords from disk.

**Command palette.** The `command.palette.show` command (default `ctrl+p`,
keybind id `command_list`) lists every reachable non-hidden command together
with its currently bound keys, grouped by category, with a **Suggested** group
first. Commands marked hidden (for example `/variants` when the active model has
no variants) are omitted from the palette. Use the palette as the authoritative
live list of bound keys; the static tables in
[TUI Keybindings](tui-keybindings.md) document factory defaults.

### Prompt autocomplete

In the prompt input:

- Typing **`@`** opens completion over workspace files, subagents (offered as
  `@<name>`), and reference aliases (also `@<name>`).
- Typing **`/`** as the **first character** of the prompt (column 0 of the
  buffer, with no whitespace before the cursor) opens slash-command completion.
  The `/` trigger is position-sensitive; it does not open mid-line.

The `/` list also includes server-provided commands from the backend catalog:

- Entries whose `source` is `mcp` render with a trailing **`:mcp`** label so they
  are distinguishable from local commands.
- Entries whose `source` is `skill` are **hidden** from the `/` list; open them
  through `/skills` instead.

## Workflow Commands

```sh
hya-backend workflow list
hya-backend workflow info plan-impl-review
hya-backend --db sessions.db workflow use plan-impl-review --session hysec_...
hya-backend --db sessions.db workflow run --session hysec_... \
  --input request=verify-parser
hya-backend --db sessions.db workflow state --session hysec_...
```

`list` and `info` read the merged project, user, installed, and immutable
first-party Workflow catalog. `use` persists an exact source/revision identity
in an existing Session and therefore requires `--session`. `state` also requires
`--session` and replays that Session from the selected database.

`run [NAME]` executes the explicit name, or the Session selection when `NAME`
is omitted with `--session`. A run without `--session` creates a new Session and
requires `NAME`. Repeat `--input KEY=VALUE` for declared inputs; values split on
the first `=`. `--revision` (alias `--expected-revision`) fences selection/run
against a canonical compiler revision, and `--json` emits the shared typed
command result.

Inside the TUI, `/workflow list|info|use|run|state` uses the same app control
path and bypasses parent-model admission. Progress arrives through normal
Session Events and `session.updated` synchronization.

## Bundle Commands

```sh
hya bundle info -f example.hyabundle
hya bundle install example.hyabundle
hya bundle list
hya bundle info hya/docs-example
hya bundle uninstall hya/docs-example
```

These are the canonical bundle commands. `hya` delegates once to `hya-ts`,
which forwards the bundle subcommand once to `hya-backend`; invoking
`hya-backend bundle ...` directly exposes the same backend implementation.

`install` reports whether the package was installed, replaced, or unchanged,
along with bundle identity, version, closed payload kind, and registry generation.
`list` reports name, version, packaged Agents, state, package kind, and Workflow
id for the merged immutable first-party and installed catalog. `info` also
reports publisher, origin, format, immutability, digests, and packaged resource
ids when available. The first-party WorkflowBundle is read-only and cannot be
replaced or uninstalled. Repeating an install with the same digest is
idempotent; replacement and removal publish through atomic registry operations.

Before the registry is touched, `install` stages the package on disk via
`stage_package`: the bytes land in
`<staging_root>/hya-bundle-stage-<pid>-<n>/package` with file mode `0600`
inside a directory mode `0700`. The stage is first built under a
`hya-bundle-building-` prefix and then atomically renamed into the
`hya-bundle-stage-` name, holding an exclusive flock for the staging lifetime.
`cleanup_orphaned_staging` reclaims unlocked leftovers from crashed installs, so
stale `hya-bundle-stage-*` directories are self-healing — do not delete them by
hand while an install is running.

The separate registry is
`$XDG_DATA_HOME/hya/bundles/registry.sqlite3`, falling back to
`~/.local/share/hya/bundles/registry.sqlite3`. A successful generation change
is loaded lazily before a new root turn binds and when the TUI/catalog is
refreshed. In-flight and child turns remain pinned to their existing catalog;
a failed candidate leaves the previous snapshot active. There is no filesystem
watcher or per-round/tool-call registry query.

`info -f` strictly inspects a package without mutating the registry or runtime
publication. Package paths require the exact lowercase `.hyabundle` suffix;
the bytes magic is still authoritative for public/private format detection.
Public packages are a closed `AgentBundle | WorkflowBundle` payload. An
AgentBundle carries one Agent. A WorkflowBundle carries one compiled Workflow
and its exact reachable Agent closure. Either kind may remain process-free or
include only its declared prompt/resource/Extension closure for
self-contained selected JavaScript entrypoints in an activation-scoped Bun
Compat sidecar; no helper/import closure is supported. Undeclared directory
files are ignored and unreferenced archive files are rejected; activation never
executes the authoring tree. See [AgentBundle Authoring](agent-bundle-authoring.md),
[WorkflowBundle packaging](workflows.md#packaging-a-workflowbundle), and the
[static](examples/bundle.hya.md), [transient Bun](examples/bun-transient/),
[resident Bun](examples/bun-resident/), and [disjoint Bun](examples/bun-disjoint/)
examples. Package publication validates collisions against the immutable
first-party catalog, the complete installed BundleCatalog, and reserved core
Agent ids before atomic generation publication. Each activation materializes
only the selected Agent's captured Tool/Hook/Skill capability closure and
exact-path-matched JavaScript Extension entrypoints; staged-but-unselected
Extensions never activate.
New root turns and catalog refreshes publish the installed generation lazily while
existing TurnBindings remain pinned. Private output reports authentication as
unverified, payload as opaque, and activation as unsupported in 0.36.0.
Raw Rust extensions and Bundle-declared MCP remain unsupported; the sidecar
does not run an agent loop or add a permission plane.

## Bare `hya-backend`

With no subcommand (and no `--prompt`), `hya-backend` is the interactive path.

**On a TTY:** starts an in-process HTTP/SSE backend bound to an ephemeral
loopback port (`127.0.0.1:0`) and hands the terminal to the `hya` frontend.
Frontend resolution order: `HYA_FRONTEND_BIN`, else the newest of
`target/release/hya` and `target/debug/hya` under the workspace, else `hya` on
`PATH`. For this path the empty default `--db` is remapped to
`$XDG_STATE_HOME/hya/sessions.db` (see [Global Options](#--db-empty-string-semantics)).

**On a non-TTY stdout:** does **not** start a backend or frontend. It prints

```text
hya <version> — a multi-agent coding agent
The hya frontend needs a terminal. Try `hya-backend exec "<prompt>"`, `hya-backend -p "<goal>"`, or `hya-backend --help`.
```

then exits **0**. Scripts that pipe `hya-backend` with no arguments hit this
branch and must not treat exit 0 as “interactive session ready.”

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

**Readiness contract.** After the listener is bound, the process prints exactly:

```text
hya server listening on <url>
```

That string is a stability contract: `hya-sdk`'s `ServerHandle` parses this exact
line from merged stdout/stderr to discover the base URL. Do not change its
wording. Source: [`serve.rs`](../crates/hya-backend/src/serve.rs),
[`hya-sdk` server](../crates/hya-sdk/src/server.rs).

**Signal handling.** SIGTERM, SIGINT, and SIGHUP handlers are installed
**before** the listen line is printed (an e2e-harness ordering requirement: a
harness that sees the URL may signal immediately). Those signals trigger a
graceful axum shutdown followed by spawn-supervisor teardown, so the process
terminates normally with exit code **0** rather than dying by signal. This
matters for supervisors (systemd, `docker stop`) and for test harnesses that
assert a clean exit.

**Startup trace.** When `HYA_STARTUP_TRACE` is `1` or `true` (case-insensitive),
serve also emits a JSON `backend_listen` startup mark on stderr after the listen
line, for example
`{"hya_startup":true,"mark":"backend_listen","wall_ms":…,"detail":"<url>"}`.

The server mounts native `/sessions/*` routes plus Compat-compatible legacy
and v2 route groups. See
[`compat-parity.md`](compat-parity.md) for exact compatibility status.

## Auth and Catalog Commands

```sh
hya-backend login <provider> <token>
hya-backend oauth login --provider <name> --type <openai-codex|grok-build|aliases…> [--device] [--loopback] [--no-browser] [--browser] [--model <id>] [--base-url <url>]
hya-backend oauth status [provider]
hya-backend auth list
hya-backend auth logout <provider>
hya-backend providers list
hya-backend providers logout <provider>
hya-backend models [provider] [--verbose]
hya-backend agent list [--all]
```

`login` writes a plain provider token under `~/.config/hya/auth`. Prefer
`oauth login` for ChatGPT Codex and Grok Build subscription auth — it runs the
OAuth flow in Rust, stores a refreshable credential bundle, and upserts the
provider route into `config.yaml`.

**`--type` values and aliases.** Interactive OAuth accepts only two provider
implementations:

| Canonical | Accepted aliases |
| --- | --- |
| `openai-codex` | `openai_codex`, `codex` |
| `grok-build` | `grok_build`, `grok`, `xai-oauth` |

Every other provider must use `hya-backend login <provider> <token>` or an
inline `api_key` in config.

**Device vs loopback.** For `openai-codex`, the default matches Codex CLI:
**device-code with URL/code printed** (no auto-open browser). Use `--browser` to
open the verification URL, or `--loopback` for localhost PKCE. `--loopback` is
**openai-codex only** — passing it with `--type grok-build` (or any non-codex
type) is rejected with an error, not ignored. `--browser` and `--no-browser` are
mutually exclusive.

The loopback flow binds a local HTTP listener and uses redirect URI
`http://localhost:1455/auth/callback` with locally generated S256 PKCE plus a
`state` parameter. Port **1455** must be free and reachable from the browser.
Prefer the default device-code flow on headless or remote machines.

**Timeout and options.** The whole interactive login has a **600-second
(10 minute)** default timeout. If the user does not complete device or loopback
approval in that window, the command fails and must be rerun. Flags map to
`OAuthLoginOptions`: `provider`, `oauth_type`, `device`, `loopback`,
`no_browser`, `model`, `base_url` (the `auth_dir` / `config_path` fields are
test-only overrides).

Saved credentials take precedence over inline `api_key` values. `providers` is
an alias for `auth`. Catalog discovery already runs once during each process
startup, so there is no `models --refresh` command or second refresh path.

**`oauth status [provider]`.** Prints non-secret per-provider status only —
credential kind (`api` vs oauth), OAuth type when present, `expires` /
`status=ok|EXPIRED`, and ChatGPT/Grok `account=` id when known. For expired
OAuth credentials it also prints a ready-to-copy re-login line
(`hya-backend oauth login --provider … --type …`). No token material is printed.

**`models [provider]`.** Prints the sorted `provider/model` rows from the same
immutable startup snapshot used by the server, SDK, and TUI. With `--verbose`,
each id is followed by a JSON line containing `id`, `provider`, and
`source= configured|discovered|offline`. Unfiltered offline output is exactly
`hya/offline`; a filter with no rows exits with `Provider not found: <id>`.
Provider declarations that resolved no rows do not fabricate output.

**`agent list`.** Default output is Compat-parity: only the built-in primary
agent, printed as `build (primary)` followed by its permission rules as
pretty-printed JSON. Pass `--all` to also list ordinary agents reachable from
the build-embedded catalog. Deliberate limitation: `agent list` **never**
inspects on-disk agent files under `.hya/`, `.claude/`, or `.opencode/`, nor
config-declared agents — it reflects the embedded catalog only. System agents
(compaction / title / summary) are excluded because they are not ordinarily
spawnable via catalog `can_spawn` reachability.

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
created by `exec --db` and `exec --json --db`. Empty `--db` is remapped to the
durable XDG path (same as bare interactive startup), not in-memory. `rpc` reads
JSONL requests on stdin, accepts `{"type":"prompt","text":"..."}` and
`{"type":"quit"}`, and emits new session events plus a `{"type":"done"}` marker
using an in-memory store; `rpc` does not persist to the global `--db` database.

## `hya-backend tail-session`

```sh
hya-backend tail-session <session-id> --db hya.db
```

Replays a persisted session's event log as JSON lines. The `<session-id>`
accepts any valid `SessionId` form: `hysec_...`, `ses_...`, or legacy raw UUID.
Empty `--db` is remapped to the durable XDG path (not in-memory).

This command intentionally exits cleanly on broken pipe (exit 0), so shell
filters such as `head` and `grep -q` can close stdout without causing a panic.

## `hya-updater` (independent self-update TCB)

`hya-updater` is a separate binary from `hya-backend`. It verifies signed
release metadata, stages immutable generations, optionally smokes them, and
activates only with explicit owner authorization. See
[Secure self-update](self-update.md).

```sh
cargo build -p hya-updater --bin hya-updater
./target/debug/hya-updater version
./target/debug/hya-updater status --root /var/lib/hya/updater
./target/debug/hya-updater recover --root /var/lib/hya/updater
./target/debug/hya-updater apply \
  --root /var/lib/hya/updater \
  --metadata release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --smoke smoke.sh
# owner-gated activation only:
./target/debug/hya-updater apply ... --owner-authorized-activation
./target/debug/hya-updater discard --root /var/lib/hya/updater --sequence 42
```

Network download is outside the TCB. Pass a local package directory or
`file://` path. `install.sh` remains break-glass recovery.

## Exit Codes

| Binary | Success | Failure / notes |
| --- | --- | --- |
| `hya` (shim) | On success, `exec()`s `hya-ts` and replaces the process image, so **`hya-ts` owns the final exit code**. | Prints `hya: failed to resolve current executable: …` or `hya: failed to launch \`<path>\`: …` to stderr and exits **1**. |
| `hya-ts` | Propagates the Bun child's exit code truncated to `u8`. | Uses **1** when the child has no code (for example it died by signal). The termination-signal path returns **1** after killing the child process group. Any launcher error returns **1** after printing `<invocation-name>: <error>` to stderr. Forwarded backend subcommands (`bundle` / `oauth` / `login` / `auth`) propagate `hya-backend`'s exit code verbatim. |
| `hya-backend` | **0** on success (including non-TTY bare banner, `serve` graceful signal shutdown, and `tail-session` broken-pipe). | **1** with the full `anyhow` error chain printed to stderr on any error — CLI validation failures use the same path. |
| `hya-updater` | **0** on success. | **1** after printing `hya-updater: <error>` to stderr. |
