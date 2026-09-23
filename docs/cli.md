# CLI Reference

`hya` is the single terminal entry point: one executable (built from the
`hya-backend` package, [`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs))
whose subcommands select the area being controlled. Since 0.38.0 there are no
other user-facing executables — the former `hya-backend` binary name and the
standalone `hya-updater` binary are gone.

| Area | Subcommands |
| --- | --- |
| Headless agent runs | `exec`, `run`, `-p/--prompt` (goal mode), `loop` |
| Server and wire protocols | `serve`, `rpc` |
| Sessions | `sessions`, `tail-session` |
| Providers and auth | `login`, `oauth`, `auth` (alias `providers`), `models` |
| Agents, bundles, Workflows | `agent`, `bundle`, `workflow` |
| Self-update TCB | `update` (`version`, `status`, `recover`, `apply`, `discard`, `init-roots`) |

```sh
cargo build -p hya-backend --bin hya   # ./target/debug/hya
hya --help                             # list every subcommand
hya <subcommand> --help                # flags for one area
```

## Global Options

```text
hya [--model <MODEL>] [--prompt <GOAL>] [--max-iterations <N>]
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
lines are accepted unchanged. They are never read after clap parse. hya
does not expose a CLI switch for verbose tracing today. Many operational notices
go to stderr; the serve readiness line
`hya server listening on <url>` is printed on **stdout** (see
[`serve`](#hya-serve)).

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
slash commands in their prompt UIs. The `/init` and `/review` prompt templates
are loaded from the trusted `hya/core-commands` [first-party
bundle](bundle-runtime.md#first-party-bundles).

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
hya workflow list
hya workflow info plan-impl-review
hya --db sessions.db workflow use plan-impl-review --session hysec_...
hya --db sessions.db workflow run --session hysec_... \
  --input request=verify-parser
hya --db sessions.db workflow state --session hysec_...
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

`hya bundle` manages bundles in two scopes, the same two tiers the runtime
loads:

| Scope | Flag | Where | Precedence |
| --- | --- | --- | --- |
| User (default) | `--user` | The installed-bundle registry, `$XDG_DATA_HOME/hya/bundles/registry.sqlite3` (fallback `~/.local/share/hya/bundles/registry.sqlite3`). | Shadows first-party bundles with the same id or namespace. |
| Project | `--project` | Bundle source directories under `./.hya/bundles/<dir>/` in the current directory (the directory `hya` is started from). | Highest: shadows user-installed and first-party bundles with the same id or namespace. |

`--user` and `--project` are mutually exclusive. The twelve bundles shipped with
hya have scope `builtin`: they are listed but cannot be installed over as presets
or removed.

| Command | What it does |
| --- | --- |
| `hya bundle install [--user\|--project] [-y] [--overwrite] <PACKAGE>` | Verify a `.hyabundle` and install it into the scope. Asks for confirmation unless `-y`. |
| `hya bundle install [--user\|--project] [-y] [--overwrite] --claude <SOURCE>` | Translate a Claude Code plugin and install it the same way. |
| `hya bundle remove [--user\|--project] [-y] <BUNDLE_ID>` | Remove a bundle from the scope. Asks for confirmation unless `-y`. Alias: `uninstall`. |
| `hya bundle verify [--user\|--project] [--overwrite] <PACKAGE>` | Run every install check against the scope and report what `install` would do. Writes nothing. |
| `hya bundle list [--user\|--project]` | List bundles. All scopes by default; a flag narrows to one scope. |
| `hya bundle info [--user\|--project] <BUNDLE_ID\|PACKAGE>` | Show metadata of a bundle by id (searching every scope unless narrowed) or of a package file. Declarations print one line each: `schema=…`, `process=<kind> command=…`, and `api=<METHOD> <scope> <path> id=<id>` (plus ` request_schema=<file>`, ` response_schema=<file>`, and ` description=…` when declared) for every [API endpoint](agent-bundle-authoring.md#api-endpoints-apis), for example `api=GET session /usage id=usage description=Per-model token usage of the session tree`. |
| `hya bundle info -f <PACKAGE>` | Show metadata of a package file. |
| `hya bundle search [--user\|--project] <QUERY>` | Filter bundles in every scope (or one) by id, agent id, or skill id. |
| `hya bundle schema [--user\|--project] <BUNDLE_ID\|PACKAGE>` | Show the URI-scheme extensions one bundle declares. |

```sh
hya bundle verify example.hyabundle            # check only; nothing installed
hya bundle install example.hyabundle           # user scope, asks [y/N]
hya bundle install --project -y example.hyabundle   # into ./.hya/bundles, no prompt
hya bundle list --project
hya bundle info hya/docs-example
hya bundle info example.hyabundle              # read a package file
hya bundle remove --project -y hya/docs-example
hya bundle uninstall hya/docs-example          # same as remove, user scope
hya bundle search --project docs               # search one scope
hya bundle schema hya/schema-demo              # one bundle's schemes
hya bundle schema schema-demo.hyabundle        # from a package file
```

### Confirmation

`install` and `remove` print a summary to stderr and ask `Proceed? [y/N]`.
Only `y` or `yes` (any case) continues; any other answer cancels. Closed or
empty stdin also cancels, so a script or CI job that omits `-y` fails safely
instead of hanging. A cancelled command exits 1 with
`bundle install cancelled` (or `bundle remove cancelled`) and changes nothing.
`-y`/`--yes` skips the prompt. An install whose content is already present
prints `unchanged` without asking.

The install summary names the bundle, version, kind, scope, target (registry
path or project directory), the action (`install`, `replace from=<version>`),
its Agents, and any bundle it removes through a namespace takeover:

```text
Install hya/docs-example 1.0.0 (AgentBundle) into user scope
  target: /home/me/.local/share/hya/bundles/registry.sqlite3
  action: install
  agents: docs-example
Proceed? [y/N]
```

### Install rules

Both scopes apply the same rules:

- The package must be a public `.hyabundle` (exact lowercase suffix). Private
  packages fail with `PRIVATE_ACTIVATION_UNSUPPORTED`.
- A bundle may not claim a trusted preset id or a built-in Agent id, and must
  form a valid catalog with the first-party bundles.
- Reinstalling identical content is `unchanged`. A higher version replaces the
  installed one.
- A downgrade fails with `BUNDLE_DOWNGRADE_REQUIRED`, and taking over another
  bundle's namespace fails with `NAMESPACE_CONFLICT`, unless `--overwrite` is
  given (the incumbent namespace owner is then removed).
- The same version with different content fails with `BUNDLE_CONTENT_CONFLICT`.
  In the user scope this always fails; in the project scope `--overwrite`
  replaces the directory.

A project install unpacks the package's declared source files (the manifest
plus every file it references) into `./.hya/bundles/<id with / replaced by __>/`,
for example `.hya/bundles/acme__tools/`. It replaces a bundle with the same id
in place, whatever its directory is named. If the target directory exists but
is not that bundle, the install fails with `PROJECT_BUNDLE_DIRECTORY_OCCUPIED`
and leaves it alone. Files are written to a staging directory under `./.hya/`
and renamed into place, so the runtime never loads a half-written bundle.
A project remove deletes the bundle's source directory, including
hand-authored ones, which is why it confirms first.

With `--claude <source>`, `install` accepts a local Claude Code plugin directory
or a marketplace reference `<marketplace-root>#<entry>`. The adapter emits an
agentless `Plugin` or an `AgentSetBundle` containing all imported agents, with
packaged Skills, supported hooks, MCP declarations, and their source files.
Imports follow the same namespace-conflict policy. See
[Claude plugin import](claude-plugin-import.md) for supported mappings and
explicitly rejected hook semantics.

### Output

`install` prints one line on stdout:

```text
installed|replaced <BUNDLE_ID> <VERSION> scope=user generation=<N>
installed|replaced <BUNDLE_ID> <VERSION> scope=project path=<DIR>
unchanged <BUNDLE_ID> <VERSION> scope=<user|project>
```

`remove` prints `removed <BUNDLE_ID> scope=user generation=<N>` or
`removed <BUNDLE_ID> scope=project path=<DIR>`.

`verify` prints `key=value` lines and exits 0 when `install` with the same
flags would succeed, or exits 1 with the same error `install` would report:

```text
verified <BUNDLE_ID> <VERSION>
format=public-v1
kind=<Plugin|AgentBundle|AgentSetBundle|WorkflowBundle>
source_digest=<sha256 hex>
prepared_digest=<hex>
scope=<user|project>
target=<registry path or project directory>
action=<install|replace from=VERSION|unchanged>
removes=<BUNDLE_ID>        # once per bundle a namespace takeover would remove
```

`verify` never creates the registry or `./.hya`.

`list` prints `NAME VERSION AGENT STATE KIND WORKFLOW SCOPE`, one row per
bundle, sorted by name. SCOPE is `builtin`, `user`, or `project`. STATE is
`active`, `shadowed` (a user bundle hidden by a project bundle with the same id
or namespace), or `unreadable (reinstall)` (a registry row written by another
hya version). A first-party bundle hidden by a user or project bundle is not
listed, matching what the runtime loads.

`info` prints `key=value` lines: `name`, `version`, `publisher`, `origin`
(`preset`, `first-party`, `installed`, or `project`), `scope`, `format`, `state`,
`immutable`, digests, `path` for project bundles, and one line per Agent,
Skill, Tool, MCP server, hook, extension, schema, and process declaration.
By id, `info` looks in the order the runtime resolves bundles (preset, project,
user, first-party) unless `--user` or `--project` narrows it. Given a path that
ends in `.hyabundle` and names a file, or `-f <PACKAGE>`, it reads the package
without installing it and prints `key: value` lines (`format`, `name`,
`version`, `publisher`, `origin: package`, digests, and the same resource
lines).

`list`, `info`, and `search` include the trusted `hya/core-agents`,
the five tool-family presets (`hya/base-tools`, `hya/extended-tools`,
`hya/network-tools`, `hya/channel-tools`, `hya/todo-tools`), and
`hya/core-skills`, `hya/core-commands`, and `hya/agent-channels` preset inventory
alongside first-party and installed packages. Trusted inventory rows are
immutable and not installable; public packages cannot acquire preset
privileges. A user or project bundle that overrides a first-party bundle takes
precedence; removing the override restores the first-party bundle, which itself
cannot be removed.

`search <QUERY>` covers exactly the bundles `list` shows: builtin, user, and
project bundles, including user bundles shadowed by a project bundle
(`shadowed`). `--user` or `--project` narrows it to one scope. It is a
case-insensitive substring match over bundle ids, agent ids, and skill ids
(both local and stable spellings such as `handbook` and
`bundle:hya/docs-example/skill/handbook`), printing one `bundle list`-shaped
`NAME VERSION AGENT STATE KIND WORKFLOW SCOPE` row per matching bundle, sorted by
bundle id then scope. `<QUERY>` is a required positional argument: omitting it or passing
only whitespace exits non-zero and prints the usage line. An unreadable
installed row stays searchable by its bundle id and prints the same degraded
`unreadable (reinstall)` row as `bundle list`. `search` is read-only and never
creates the bundle registry. When no bundle metadata matches — for example a
query naming another subcommand such as `schema` — it exits 0, prints every
bundle in the searched scope on stdout, and explains the fallback on stderr.

`schema <BUNDLE_ID|PACKAGE>` shows the URI-scheme extensions (`schemas:` in the
manifest) of one bundle. It prints a `SCHEME TOOL WRITABLE` header and one row
per declared scheme, sorted by scheme; a bundle that declares none prints only
the header. A bundle id resolves like `info` — preset, project, user, then
first-party — unless `--user` or `--project` narrows it; an unknown id exits 1
with `BUNDLE_NOT_FOUND`. A path ending in `.hyabundle` that names a file is
inspected without installing it. `schema` is read-only.

```text
$ hya bundle schema hya/schema-demo
SCHEME TOOL WRITABLE
db query false
```

The live, merged scheme table the runtime resolves (with the winning owner and
the chain of claimants across bundles) is `GET /v1/runtime/schemas`; see
[configuration](configuration.md#bundle-schemas).

Before the registry is touched, `install` stages the package on disk via
`stage_package`: the bytes land in
`<staging_root>/hya-bundle-stage-<pid>-<n>/package` with file mode `0600`
inside a directory mode `0700`. The stage is first built under a
`hya-bundle-building-` prefix and then atomically renamed into the
`hya-bundle-stage-` name, holding an exclusive flock for the staging lifetime.
`cleanup_orphaned_staging` reclaims unlocked leftovers from crashed installs, so
stale `hya-bundle-stage-*` directories are self-healing — do not delete them by
hand while an install is running.

A successful user-registry generation change
is loaded at root admission, root turn binding, root model-round boundaries,
and explicit catalog refresh. A running round keeps its captured snapshot;
subagents and Workflow members keep their inherited binding throughout the
activation. A failed rebind leaves the previous snapshot active. There is no
filesystem watcher or per-tool-call registry query.

`info -f` strictly inspects a package without mutating the registry or runtime
publication. Package paths require the exact lowercase `.hyabundle` suffix;
the bytes magic is still authoritative for public/private format detection.
Public packages are a closed `Plugin | AgentBundle | AgentSetBundle | WorkflowBundle`
payload. A Plugin carries resources without an Agent or Workflow. An AgentBundle
carries one Agent; an AgentSetBundle carries Agents and/or declarative channel
policies without a Workflow. A WorkflowBundle carries one compiled Workflow and
its exact reachable Agent closure. All kinds may remain process-free.

Use `extensions.process` for a native/Bun/Claude provider, `resources.mcp` for
managed MCP servers, and `extensions.files` for explicit support-file closure.
Resources execute from the validated package after source removal. Agentless
JavaScript Plugins use a generation-owned Bun process; agent-bearing JavaScript
bundles retain activation-scoped sidecars with selected entrypoints. Process
initialization must match declared resources before publication. See
[Bundle Runtime](bundle-runtime.md), [AgentBundle Authoring](agent-bundle-authoring.md),
and [WorkflowBundle packaging](workflows.md#packaging-a-workflowbundle).

Package publication validates the merged catalog after first-party shadowing,
the complete installed BundleCatalog, and reserved core Agent ids before atomic
generation publication. Each JavaScript activation materializes only the selected
Agent's captured Tool/Hook/Skill capability closure and exact-path-matched
JavaScript Extension entrypoints; inert staged files never imply activation.
Root admission, turn/round boundaries, and catalog refreshes publish installed
generations lazily; each existing TurnBinding itself remains immutable. Public installed bundles cannot grant
preset trust or expand their permission/resource planes. Private output reports
authentication as unverified, payload as opaque, and activation as unsupported.
`extensions.rust` packages raw native executable files when paired with
`extensions.process.kind: rust`; the first process argument names the bundled
executable. This selects the native process ABI without compiling source at
activation time.

## Bare `hya`

With no subcommand (and no `--prompt`), `hya` prints a guidance banner
and exits. No interactive frontend is bundled:

```text
hya <version> — a multi-agent coding agent
No interactive frontend is bundled. Try `hya serve`, `hya exec "<prompt>"`, `hya -p "<goal>"`, or `hya --help`.
```

It exits **0** on both a TTY and a non-TTY stdout. Scripts that pipe
`hya` with no arguments hit this branch and must not treat exit 0 as
“interactive session ready.”

## `--pure`

Global flag for `exec`, `run`, `rpc`, `-p` goal mode, `workflow`, and `serve`:
load no external project or user context — no `AGENTS.md`/context-file
discovery (startup-baked in direct modes, per-turn guidance on `serve`), no
MCP servers, no plugins, and no external skill directories (the built-in
skill catalog is the whole skill surface). Websearch keeps its own
configuration, and builtin tools are unaffected. Use it for reproducible
runs whose prompt context is exactly what you passed.

## `hya exec`

```sh
hya exec "summarize this repo"
hya exec --json "summarize this repo"
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

## `hya run`

```sh
hya run "summarize this repo"
hya run --format json "summarize this repo"
```

Compat-compatible alias for `exec`. Message words are joined with spaces.
Like `exec`, `run` persists only when the global `--db <PATH>` is supplied.
`--format json` and `--json` both emit event JSONL.

## `hya -p`

```sh
hya -p "make the workspace compile" --max-iterations 6
```

Runs goal mode with an in-memory store. Each iteration runs an agent turn, then
an independent evaluator judges the transcript. The run stops when the evaluator
returns `met=true`, a cap is reached, or cancellation is requested. Goal mode
does not persist to the global `--db` database.

## `hya serve`

```sh
hya serve --bind 127.0.0.1:8080 --db hya.db
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
`HYA_GRPC_BIND=<host:port>` additionally serves the same seventeen services over
gRPC (reflection enabled). See [Protocol guide](protocol/README.md),
[API reference](protocol/api-reference.md), and
[Server and Client](architecture/server-client.md).

## Auth and Catalog Commands

```sh
hya login <provider> <token>
hya oauth login --provider <name> --type <openai-codex|grok-build|aliases…> [--device] [--loopback] [--no-browser] [--browser] [--model <id>] [--base-url <url>]
hya oauth status [provider]
hya auth list
hya auth logout <provider>
hya providers list
hya providers logout <provider>
hya models [provider] [--verbose]
hya agent list [--all]
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

Every other provider must use `hya login <provider> <token>` or an
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
(`hya oauth login --provider … --type …`). No token material is printed.

**`models [provider]`.** Prints the sorted `provider/model` rows from the same
immutable startup snapshot used by the server and its clients. With `--verbose`,
each id is followed by a JSON line containing `id`, `provider`, and
`source= configured|discovered|offline`. Unfiltered offline output is exactly
`hya/offline`; a filter with no rows exits with `Provider not found: <id>`.
Provider declarations that resolved no rows do not fabricate output.

**`agent list`.** Default output is Compat-parity: only the built-in primary
agent, printed as `build (primary)` followed by its permission rules as
pretty-printed JSON. Pass `--all` to also list ordinary agents reachable from
the built-in catalog. Deliberate limitation: `agent list` **never**
inspects on-disk agent files under `.hya/`, `.claude/`, or `.opencode/`, nor
config-declared agents — it reflects the built-in catalog only. System agents
(compaction / title / summary) are excluded because they are not ordinarily
spawnable via catalog `can_spawn` reachability.

## Session and RPC Commands

```sh
hya sessions --db hya.db
hya rpc
```

`sessions` lists persisted sessions in a SQLite database, including sessions
created by `exec --db` and `exec --json --db`. Empty `--db` is remapped to the
durable XDG path (same as bare interactive startup), not in-memory. `rpc` reads
JSONL requests on stdin, accepts `{"type":"prompt","text":"..."}` and
`{"type":"quit"}`, and emits new session events plus a `{"type":"done"}` marker
using an in-memory store; `rpc` does not persist to the global `--db` database.

## `hya tail-session`

```sh
hya tail-session <session-id> --db hya.db
```

Replays a persisted session's event log as JSON lines. The `<session-id>`
accepts any valid `SessionId` form: `hysec_...`, `ses_...`, or legacy raw UUID.
Empty `--db` is remapped to the durable XDG path (not in-memory).

This command intentionally exits cleanly on broken pipe (exit 0), so shell
filters such as `head` and `grep -q` can close stdout without causing a panic.

## `hya update` (self-update TCB)

`hya update` verifies signed release metadata, stages immutable generations,
optionally smokes them, and activates only with explicit owner authorization.
It replaces the former standalone `hya-updater` binary. The commands are
implemented in the independent `hya-updater` library crate, and `hya`
dispatches them before composing any runtime: no config bootstrap, bundles,
providers, plugins, MCP, or session store are loaded. Global flags such as
`--model` or `--db` are accepted but ignored. See
[Secure self-update](self-update.md).

| Command | Purpose |
| --- | --- |
| `hya update version` | Print the updater package version and supported metadata protocol. |
| `hya update status --root DIR` | Show selector, accepted floor, and layout paths. |
| `hya update recover --root DIR` | Recover interrupted prepare/commit journal state. |
| `hya update apply --root DIR --metadata FILE --package DIR --platform TRIPLE [--smoke CMD] [--trust-roots FILE] [--owner-authorized-activation]` | Verify, stage, optionally smoke, and (owner-gated) activate. |
| `hya update discard --root DIR --sequence N` | Discard a staged-but-not-accepted candidate. |
| `hya update init-roots --path FILE --root KEY_ID=HEX32...` | Write a bootstrap `trust_roots.json` (operator only). |

```sh
hya update version
hya update status --root /var/lib/hya/updater
hya update recover --root /var/lib/hya/updater
hya update apply \
  --root /var/lib/hya/updater \
  --metadata release.metadata.json \
  --package ./package-dir \
  --platform x86_64-unknown-linux-gnu \
  --smoke smoke.sh
# owner-gated activation only:
hya update apply ... --owner-authorized-activation
# optional trust-roots override (default: <root>/trust_roots.json):
hya update apply ... --trust-roots /secure/media/trust_roots.json
hya update discard --root /var/lib/hya/updater --sequence 42
hya update init-roots \
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
| `hya` | **0** on success (including the bare guidance banner, `serve` graceful signal shutdown, and `tail-session` broken-pipe). | **1** with the full `anyhow` error chain printed to stderr on any error — CLI validation failures use the same path. |
