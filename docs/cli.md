# CLI Reference

The backend CLI/API binary is `hya-backend`, defined in
[`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs).

## Global Options

```text
hya-backend [--model <MODEL>] [--prompt <GOAL>] [--max-iterations <N>]
     [--yolo] [--db <PATH>] [COMMAND]
```

| Option | Meaning |
| --- | --- |
| `--model <MODEL>` | Override `default_model` from hya config and `HYA_MODEL`. |
| `-p, --prompt <GOAL>` | Run headless goal mode instead of a subcommand. |
| `--max-iterations <N>` | Iteration cap for goal mode. Defaults to `6` in the CLI. |
| `--yolo` | Auto-approve every tool action. This applies to headless and server composition. |
| `--db <PATH>` | SQLite database path. Semantics of an empty value depend on the command (see below). |
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
| `sessions`, `tail-session` | Remapped to `$XDG_STATE_HOME/hya/sessions.db`, falling back to `$HOME/.local/state/hya/sessions.db` (or `./.local/state/hya/sessions.db` when neither is set). The directory is created if missing. |

`resolve_interactive_db` performs that remap so the session-backed subcommands
see the same durable store across restarts. An explicit `--db ""` is **not**
distinguishable from the clap default on those remapped commands, so it still
lands on the durable path. Pass a real `--db <PATH>` for intentional locations.

When `--db <PATH>` is a non-empty path, hya persists the canonical event log, not
just the rendered transcript. The SQLite file can contain prompts, tool
arguments, tool results, reasoning deltas, command metadata, absolute workdir
paths, and other replay data. The file is plain SQLite; encryption and
permissions are the caller’s responsibility and file mode follows the process
umask, so place it in a private directory.

The same database also stores backend-owned per-Agent model preferences. The
default durable database therefore remembers per-Agent model selections across
restarts; separate explicit `--db` paths have independent preferences, and an
in-memory store is intentionally non-durable.

When `--prompt` is present, it takes precedence over subcommand dispatch.

## Backend Command Catalog

The backend serves a built-in command catalog from
[`command_catalog.rs`](../crates/hya-server/src/support/command_catalog.rs) over
`GET /v1/commands` (Catalog `ListCommands` on the `hya.v1` contract; the former
Compat `/api/command` surface is deleted). Clients surface these entries as
slash commands in their prompt UIs.

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

## Bundle Commands

```sh
hya-backend bundle info -f example.hyabundle
hya-backend bundle install example.hyabundle
hya-backend bundle install --claude ./my-claude-plugin
hya-backend bundle list
hya-backend bundle search goal-loop
hya-backend bundle info hya/docs-example
hya-backend bundle uninstall hya/docs-example
```

These are the canonical bundle commands, implemented by `hya-backend` directly.

`install` reports whether the package was installed, replaced, or unchanged,
along with bundle identity, version, closed payload kind, and registry generation.
With `--claude <source>`, `install` accepts a local Claude Code plugin directory:
the bundled Claude adapter translates it offline into an `AgentBundle`
(identity `claude/<name>`, namespace = sanitized name, skills from
`agents/`, `skills/`, and `commands/`, MCP from `.mcp.json`), which installs
through the same namespace-conflict policy (`DenyConflicts` by default;
`--overwrite` replaces the incumbent).
`list` reports name, version, packaged Agents, state, package kind, and Workflow
id for the merged immutable first-party and installed catalog. `info` also
reports publisher, origin, format, immutability, digests, and packaged resource
ids when available. The first-party WorkflowBundle is read-only and cannot be
replaced or uninstalled. Repeating an install with the same digest is
idempotent; replacement and removal publish through atomic registry operations.

`search <QUERY>` filters the same merged first-party and installed catalog with
a case-insensitive substring match over bundle ids, agent ids, and skill ids
(both local and stable spellings such as `handbook` and
`bundle:hya/docs-example/skill/handbook`), printing one `bundle list`-shaped
`NAME VERSION AGENT STATE KIND WORKFLOW` row per matching bundle, sorted by
bundle id. `<QUERY>` is a required positional argument: omitting it or passing
only whitespace exits non-zero and prints the usage line. An unreadable
installed row stays searchable by its bundle id and prints the same degraded
`unreadable (reinstall)` row as `bundle list`. `search` is read-only and never
creates the bundle registry. When no bundle metadata matches — for example a
query naming another subcommand such as `schemas` — it exits 0, prints the full
catalog on stdout, and explains the fallback on stderr.

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
is loaded lazily before a new root turn binds and when the catalog is
refreshed. In-flight and child turns remain pinned to their existing catalog;
a failed candidate leaves the previous snapshot active. There is no filesystem
watcher or per-round/tool-call registry query.

`info -f` strictly inspects a package without mutating the registry or runtime
publication. Package paths require the exact lowercase `.hyabundle` suffix;
the bytes magic is still authoritative for public/private format detection.
Public packages are a closed `Plugin | AgentBundle | AgentSetBundle | WorkflowBundle` payload. A Plugin carries resources without an Agent or Workflow. An
AgentBundle carries one Agent; an AgentSetBundle carries one or more Agents without a Workflow. A WorkflowBundle carries one compiled Workflow
and its exact reachable Agent closure. All kinds may remain process-free. Agent-bearing packages may
include only their declared prompt/resource/Extension closure for
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

With no subcommand (and no `--prompt`), `hya-backend` prints a guidance banner
and exits. No interactive frontend is bundled:

```text
hya <version> — a multi-agent coding agent
No interactive frontend is bundled. Try `hya-backend serve`, `hya-backend exec "<prompt>"`, `hya-backend -p "<goal>"`, or `hya-backend --help`.
```

It exits **0** on both a TTY and a non-TTY stdout. Scripts that pipe
`hya-backend` with no arguments hit this branch and must not treat exit 0 as
“interactive session ready.”

## `--pure`

Global flag for `exec`, `run`, `rpc`, `-p` goal mode, `workflow`, and `serve`:
load no external project or user context — no `AGENTS.md`/context-file
discovery (startup-baked in direct modes, per-turn guidance on `serve`), no
MCP servers, no plugins, and no external skill directories (the embedded
builtin skill catalog is the whole skill surface). Websearch keeps its own
configuration, and builtin tools are unaffected. Use it for reproducible
runs whose prompt context is exactly what you passed.

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

`--json` streams live: envelopes print as the engine broadcasts them (the
database is written per-event regardless), with an initial catch-up pass for
anything persisted before the stream attached and a final tail flush from the
durable log. The printed set is exactly what `tail-session` replays for the
session — sequence numbers and timestamps included — so an abnormally
terminated run still leaves a usable partial trajectory on stdout before the
nonzero exit surfaces. Ordering is bus-arrival order: almost always ascending,
but concurrent writers (the turn loop, resident batches, mailbox commits) can
interleave, so the final tail flush may append a late lower seq.

When no command-line model override is present, a new headless root Session
uses the selected Agent's effective default from that database. Direct/category
Agent configuration remains higher precedence. A command-line model override
applies only to that invocation and is not written as an Agent preference.

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

That string is a stability contract: harnesses, supervisors, and client SDKs
parse this exact line from merged stdout/stderr to discover the base URL. Do not
change its wording. Source: [`serve.rs`](../crates/hya-backend/src/serve.rs).

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

The server serves exactly one HTTP contract — `hya.v1` — under `/v1`
(HTTP/JSON + SSE + WebSocket). The former native `/sessions/*` routes and the
Compat-compatible legacy/v2 route groups are deleted. Setting
`HYA_GRPC_BIND=<host:port>` additionally serves the same sixteen services over
gRPC (reflection enabled). See [Protocol guide](protocol/README.md),
[API reference](protocol/api-reference.md), and
[Server and Client](architecture/server-client.md).

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
immutable startup snapshot used by the server and its clients. With `--verbose`,
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
# optional trust-roots override (default: <root>/trust_roots.json):
./target/debug/hya-updater apply ... --trust-roots /secure/media/trust_roots.json
./target/debug/hya-updater discard --root /var/lib/hya/updater --sequence 42
./target/debug/hya-updater init-roots \
  --path /var/lib/hya/updater/trust_roots.json \
  --root KEY_ID=HEX32
```

Network download is outside the TCB. Pass a local package directory or
`file://` path. `install.sh` remains break-glass recovery. `init-roots`
requires `--path` and at least one `--root KEY_ID=HEX32` (repeatable).
`apply --trust-roots` overrides `<root>/trust_roots.json`. The complete
flag list is in [Secure self-update](self-update.md).

## Exit Codes

| Binary | Success | Failure / notes |
| --- | --- | --- |
| `hya-backend` | **0** on success (including the bare guidance banner, `serve` graceful signal shutdown, and `tail-session` broken-pipe). | **1** with the full `anyhow` error chain printed to stderr on any error — CLI validation failures use the same path. |
| `hya-updater` | **0** on success. | **1** after printing `hya-updater: <error>` to stderr. |
