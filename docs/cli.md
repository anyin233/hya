# CLI Reference

`hya` is the backend executable and the backend-only command entry point. Its
subcommands select the area being controlled; the optional frontend package
adds the TUI and WebUI that bare `hya` launches. Since 0.38.0 there are no
other user-facing executables — the former `hya-backend` binary name and the
standalone `hya-updater` binary are gone.

| Area | Subcommands |
| --- | --- |
| Interactive TUI and WebUI | bare `hya` when the separately installed frontend is present (see [Bare `hya`](#bare-hya)) |
| Headless agent runs | `exec`, `-p/--prompt` (goal mode), `loop` |
| Server and wire protocols | `serve` (and `serve start`/`status`/`stop`/`restart` for the backend daemon) |
| Sessions | `sessions`, `tail-session` |
| Providers and auth | `provider` (alias `providers`: `add`, `list`, `remove`, `logout`), `login`, `oauth`, `models` |
| Agents, bundles, Workflows | `agent`, `bundle`, `workflow` |
| Update | `update` installs the latest backend and frontend releases; `--backend-only` or `--tui-only` selects one side (also `--version`, `--prefix`, `--force`; see [`docs/install.md`](install.md)) |
| Secure relay | `proxy`, `bridge`, `relay doctor`, `serve --relay`, `serve relay connect\|disconnect\|status\|link\|rotate`, bare `hya --connect` (see [`docs/relay.md`](relay.md)) |

```sh
cargo build -p hya-backend --bin hya   # ./target/debug/hya
hya --help                             # list every subcommand
hya <subcommand> --help                # flags for one area
```

## Global Options

```text
hya [--model <MODEL>] [--effort <LEVEL>] [--prompt <GOAL>] [--max-iterations <N>]
     [--port <PORT>] [--backend <URL>] [--connect [<LINK>]] [--relay-ca <PEM>]
     [--transport <auto|grpc|ws>] [--allow-host <HOST>]... [--resume [<ID>]] [--yolo] [--db <PATH>] [COMMAND]
```

| Option | Meaning |
| --- | --- |
| `--model <MODEL>` | Override `default_model` from hya config and `HYA_MODEL`. A model may include an explicit `#effort` suffix. |
| `--effort <LEVEL>` | Append a validated reasoning effort suffix to `--model` (for example `--model openai/gpt --effort high`); errors when `--model` already contains `#`. |
| `-p, --prompt <GOAL>` | Run headless goal mode instead of a subcommand. |
| `--max-iterations <N>` | Iteration cap for goal mode. Defaults to `6` in the CLI. |
| `--port <PORT>` | WebUI port of [bare `hya`](#bare-hya) on `127.0.0.1`. Default `3250`; `0` picks a free port. Only valid without a subcommand and without `-p` (`hya --port 1 sessions` is an error); `hya serve --bind` sets the server's address. |
| `--backend <URL>` | [Bare `hya`](#bare-hya) only: use this running server (`http://host:port`) instead of the database's backend daemon — no discovery, no auto-start. An unreachable URL is an error (exit **1**). |
| `--connect [<LINK>]` | [Bare `hya`](#bare-hya) only: reach a remote backend through a relay link with an in-process bridge; no local daemon. `-` reads the link from the terminal without echo, no value reads `$HYA_RELAY_LINK`. See [Secure relay](relay.md#connecting-from-a-client). |
| `--relay-ca <PEM>` | With `--connect` only: extra trusted CA certificates for the relay's TLS. |
| `--transport <BINDING>` | With `--connect` only: `auto`, `grpc`, or `ws`, overriding the link's `t=`. |
| `--allow-host <HOST>` | [Bare `hya`](#bare-hya) without `--backend`/`--connect`: accept this Host name on the daemon it starts, besides `localhost`, `127.0.0.1`, and `[::1]` (repeatable; see [Allowed Host names](#allowed-host-names)). |
| `--resume [<ID>]` | [Bare `hya`](#bare-hya) only: the terminal TUI opens that session and unarchives it; without an id it opens a picker of the sessions of the Project that contains the current directory, archived ones included. Before another flag it takes no id (`hya --resume --port 0`). |
| `--yolo` | Auto-approve every tool action. This applies to headless and server composition. |
| `--db <PATH>` | SQLite database path. Semantics of an empty value depend on the command (see below). |
| `--pure` | Load no external project or user context: no `AGENTS.md` discovery, MCP servers, plugins, or external skills (see [`--pure`](#--pure)). Kept for Compat/OpenCode command lines. |

`--pure` keeps its whole meaning (load no external project or user context);
it is also what Compat/OpenCode command lines that pass it expect. hya does not
expose a CLI switch for verbose tracing today. Many operational notices go to
stderr; the serve readiness line `hya server listening on <url>` is printed on
**stdout** (see [`serve`](#hya-serve)). Bare `hya` on a terminal sends its
stdout and stderr to a log file instead (see [Bare `hya`](#bare-hya)).

### `--db` empty-string semantics

Empty `--db` is **not** always in-memory:

| Command path | Empty `--db` means |
| --- | --- |
| `exec`, `serve` | In-memory store (`open_store("")` → `SessionStore::connect_memory`). |
| bare `hya` (TUI + WebUI), `serve start`/`status`/`stop`/`restart`, `sessions`, `tail-session` | Remapped to `$XDG_STATE_HOME/hya/sessions.db`, falling back to `$HOME/.local/state/hya/sessions.db` (or `./.local/state/hya/sessions.db` when neither is set). The directory is created if missing. |

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

### Database lock and the backend daemon

A database file has one writer at a time
([ADR-0022](adr/0022-one-writer-per-database.md)): the process holding
`<db>.runtime-owner.lock` (see [`hya serve`](#hya-serve)). Usually that is the backend
daemon ([ADR-0023](adr/0023-persistent-backend-daemon.md)). Every command that
writes a file database checks the lock first:

- **A server holds it** (it published `<db>.server.json`): the command goes
  through that server over `/v1`, so the session is the server's and every
  TUI or WebUI tab on it sees the session live. If the command cannot be
  expressed there, it exits **75** before it writes anything.
- **Nobody holds it:** the command takes the lock and holds it until it exits
  (the OS releases it on a crash). A `hya serve --db` or daemon started
  meanwhile exits 75, and bare `hya`/a TUI waits for the lock (up to 60 s)
  before it starts one. `hya serve stop` sends the holder SIGTERM, which stops
  the command as a signal would (exit 143).
- **A process holds it but has not published a server** (a daemon still
  starting, or another command below): exit **75** with
  `hya <command>: database <db> is in use by pid <pid> and it does not serve HTTP yet; try again or stop it`.

In-memory stores (`--db ""`, the default of `exec`/`serve`) and SQLite
URIs are never locked.

| Command | Store | Server holds the file `--db` | Nobody holds it |
| --- | --- | --- | --- |
| `exec` | `--db`, else in-memory | Through the server: a new root session in the current directory, one prompt turn, the same stdout. `--model` sets the session's model, `--yolo` its permission mode. `--pure` exits 75. | Lock held for the run |
| `workflow use`, `workflow state` | `--db`, else in-memory | Through the server (`/v1/sessions/{id}/workflow`) | Lock held for the command |
| `workflow run` | `--db`, else in-memory | Through the server; a run without `--session` creates the session there. `--revision`, `--pure`, and `--yolo` exit 75 (no `/v1` field for them). | Lock held for the run |
| `workflow list`, `workflow info` | In-memory always | Not affected | Not affected |
| `sessions archive`/`unarchive` | Durable default or `--db` | Through the server (`PATCH /v1/sessions/{id}`) | Lock held for the write |
| `sessions` (list), `tail-session` | Durable default or `--db` | Read directly, never write, no lock | Read directly, no lock |
| `-p` goal mode, `loop` | A private temporary database per run | Not affected (`--db` is ignored) | Not affected |
| `serve` | `--db`, else in-memory | Exits 75 | Takes the lock (it is the server) |
| `agent`, `bundle`, `models`, `provider`, `update` | No session store | Not affected | Not affected |

The 75 line for a command the server cannot run names the server and the way
out:

```text
hya exec: database /home/me/work.db is in use by hya server pid 4242 at http://127.0.0.1:53211, and --pure cannot apply to a running server (it loaded its own context); stop it (`hya serve stop --db /home/me/work.db`) or pass another --db
```

**What differs when a command goes through the server.** The server's
configuration, providers, plugins, and context apply, not the command's own.
The command has no `--pure` or `--yolo` runtime of its own. It rejects the
permission asks and questions of its session tree, as it would in process.

- `exec`: the command returns when the lead's turn ends. Team members,
  subagents, or a synthesis turn still running on the server keep running and
  are not drained. `--json` output is polled every 250 ms and ends with the
  same final flush from the durable log, in ascending `seq` order. SIGINT or
  SIGTERM cancels the turn on the server (`cause: user_cancel`) and exits 130
  or 143. A second SIGINT exits at once. A turn that another client cancels
  exits 1.
- `workflow run`: the server returns once the run is admitted, and the
  command polls the session's workflow state until the run ends. Stopping the
  command (Ctrl-C) does not stop the run on the server. Errors read `<code>:
  <message>` from the `/v1` error model.

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

`use`, `run`, and `state` on a file `--db` hold its lock, or go through the
server that owns it (see
[Database lock and the backend daemon](#database-lock-and-the-backend-daemon)).

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
| Project | `--project` | Bundle source directories under `./.hya/bundles/<dir>/` in the current directory. The runtime loads them for sessions of the registered Project that has this directory as a root (not from the directory a server was started in). | Highest: shadows user-installed and first-party bundles with the same id or namespace. |

`--user` and `--project` are mutually exclusive. The eleven bundles shipped with
hya have scope `builtin`: they are listed but cannot be installed over as presets
or removed.

| Command | What it does |
| --- | --- |
| `hya bundle install [--user\|--project] [-y] [--overwrite] <PACKAGE>` | Verify a `.hyabundle`, run its declared self-check, and install it into the scope. Asks for confirmation unless `-y`. Then, if a backend runs for `--db`, prove the bundle is active there ([Self-proof and activation](bundle-runtime.md#self-proof-and-activation)); exit 1 when it is not. |
| `hya bundle install [--user\|--project] [-y] [--overwrite] --claude <SOURCE>` | Translate a Claude Code plugin and install it the same way. |
| `hya bundle remove [--user\|--project] [-y] <BUNDLE_ID>` | Remove a bundle from the scope. Asks for confirmation unless `-y`. Alias: `uninstall`. A running backend for `--db` refreshes at once. |
| `hya bundle disable <BUNDLE_ID>` | Disable a bundle by id in every scope: it stays installed but publishes nothing (agents, skills, tools, MCP servers, workflows, APIs, permission modes, TUI extension). First-party bundles can be disabled; the trusted presets (`hya/core-agents`, the tool libraries, …) cannot. A running backend refreshes at once. `list` shows it `disabled`. |
| `hya bundle enable <BUNDLE_ID>` | Enable a disabled bundle again. |
| `hya bundle verify [--user\|--project] [--overwrite] <PACKAGE>` | Run every install check, including the bundle's declared self-check, against the scope and report what `install` would do. Writes nothing. |
| `hya bundle list [--user\|--project]` | List bundles. All scopes by default; a flag narrows to one scope. |
| `hya bundle info [--user\|--project] <BUNDLE_ID\|PACKAGE>` | Show metadata of a bundle by id (searching every scope unless narrowed) or of a package file. Declarations print one line each: `schema=…`, `process=<kind> command=…`, and `api=<METHOD> <scope> <path> id=<id>` (plus ` request_schema=<file>`, ` response_schema=<file>`, and ` description=…` when declared) for every [API endpoint](agent-bundle-authoring.md#api-endpoints-apis), for example `api=GET session /usage id=usage description=Per-model token usage of the session tree`. |
| `hya bundle info -f <PACKAGE>` | Show metadata of a package file. |
| `hya bundle search [--user\|--project] <QUERY>` | Filter bundles in every scope (or one) by id, agent id, or skill id. |
| `hya bundle schema [--user\|--project] <BUNDLE_ID\|PACKAGE>` | Show the URI-scheme extensions one bundle declares. |

```sh
hya bundle verify example.hyabundle            # check only (incl. its self-check); nothing installed
hya bundle install example.hyabundle           # user scope, asks [y/N]
hya bundle install --project -y example.hyabundle   # into ./.hya/bundles, no prompt
hya bundle list --project
hya bundle info hya/docs-example
hya bundle info example.hyabundle              # read a package file
hya bundle remove --project -y hya/docs-example
hya bundle disable hya/docs-example            # keep it installed, publish nothing
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
  packages fail with `invalid bundle package format`.
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
or namespace). A first-party bundle hidden by a user or project bundle is not
listed, matching what the runtime loads. Old prepared-format registry records
fail with `please reinstall`; see [Prepared install format](bundle-runtime.md#prepared-install-format).

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
installed registry fails with `please reinstall`, as in `bundle list`.
`search` is read-only and never creates the bundle registry. When no bundle metadata matches — for example a
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

Run with no subcommand and no `--prompt` on a terminal, `hya` starts the
interactive frontends: the terminal TUI ([OpenTUI frontend](tui.md)) and the
WebUI ([browser-rendered TUI](tui-web.md)) on `http://127.0.0.1:3250`. Both
talk to one **backend daemon**: a `hya serve` that runs on its own, in the
background, for the database ([ADR-0023](adr/0023-persistent-backend-daemon.md)).
A session started in the terminal shows up in every browser tab and the other
way round, and either can resume the other's sessions.

```sh
hya                 # TUI here, WebUI on http://127.0.0.1:3250
hya --port 8000     # WebUI on http://127.0.0.1:8000
hya --port 0        # WebUI on a free port (the TUI shows which)
hya --db ~/work.db  # the daemon of another database
hya --backend http://127.0.0.1:8080   # a server you run yourself
hya --connect -     # a remote backend through a relay link (pasted, not echoed)
hya --resume        # pick a session to resume (archived ones included)
hya --resume hysec_abc123   # resume that session (and unarchive it)
```

The terminal TUI's status bar, its sidebar `Context` box, and `/status` show
`WebUI http://127.0.0.1:<port>`. Open that address in a browser: each tab runs
its own TUI process against the same daemon. Quit the terminal TUI to stop
the TUI and the WebUI: Ctrl+C twice or `/exit` also **archive** its session;
Ctrl+D or `/to-background` leave it running on the daemon, not archived.
Closing a browser tab leaves the tab's session running too (a tab offers no
`/to-background`; its Ctrl+D only says so). **The daemon keeps running**, so
the next `hya` (or TUI) starts at once and finds every session; `hya serve
stop` stops it ([Backend daemon](#backend-daemon)). `hya --resume` (or
`/resume` in any TUI or tab) brings an archived session back
([tui.md](tui.md#quit-and-keep-running-or-archive)).

**Finding the daemon.** One database has one server
([ADR-0022](adr/0022-one-writer-per-database.md)). Before it touches the
terminal, `hya` reads `<db>.server.json` next to the database and checks that
the pid is alive and `GET <url>/v1/health` answers. If so it uses that server
(whoever started it: another `hya`, a TUI, `hya serve start`, or a
`hya serve --db` you run in the foreground). Otherwise it starts a daemon
exactly like [`hya serve start`](#backend-daemon) and waits up to 60 s for it
to answer. Flags that shape a server (`--model`, `--yolo`, `--pure`) apply
only to a daemon this launch starts, and then to every client of it until it
stops; the log notes when they do not apply. If the database is held by a
process that serves nothing (still starting after 60 s, or not a server),
`hya` exits **1** before it touches the terminal:

```text
Error: database /home/me/.local/state/hya/sessions.db is held by pid 4242, which serves no reachable server (waited 60 s); stop it (`hya serve stop --force --db /home/me/.local/state/hya/sessions.db`) or pass another --db
```

A running daemon of another hya version is used as is; the log and the TUI's
status line suggest `hya serve restart`.

**`--backend <URL>`.** Use that server instead: no discovery, no daemon
start. If `GET <URL>/v1/health` does not answer, `hya` exits **1** with
`Error: no hya server answers at --backend <URL> (GET <URL>/v1/health); start
one (`hya serve start`) or drop --backend`. The TUIs then get only
`--server <URL>` (no `--db`), so they never replace that server, and
`/status` shows `Backend     daemon · pid <pid> · via --backend/--server`.

**`--connect [<LINK>|-]`.** Use a remote backend through the
[secure relay](relay.md#connecting-from-a-client) instead: no discovery, no
daemon. `hya` starts a relay bridge in its own process on a free loopback
port and hands that URL to both TUIs as a fixed `--server`, adding
`--remote --server-label "remote: <relay>/<room>"` (the header, sidebar, and
`/status` name the remote, not the loopback URL; the TUI starts without a
local Project, see [tui.md](tui.md#projects)). `--connect -` prompts for the
link on the terminal with echo turned off, and `--connect` alone reads
`HYA_RELAY_LINK`; both keep the link — the credential — out of process
listings (a link given as the value works, with a warning). A relay that
cannot be reached or a link the backend rejects stops `hya` with exit status
**1** before it touches the terminal; an offline backend does not (the TUI
shows `unavailable: remote backend is offline, or the relay link was rotated
or is wrong …` until it comes back; a rotated link looks the same, since the
proxy answers it like an offline room). The
bridge lives as long as `hya`. It requires its per-bridge token on every
connection ([`hya bridge`](#hya-bridge)); `hya` gives it to both TUIs (and the
WebUI host) as `HYA_SERVER_TOKEN` in their environment, never in argv, and
removes `HYA_RELAY_LINK` from its own environment and from every child's.
`--connect` conflicts with `--backend`.
`--relay-ca <PEM>` (extra trusted CA certificates for a relay behind a private
CA) and `--transport auto|grpc|ws` (the relay binding, overriding the link's
`t=`) configure that bridge, like `hya bridge`'s flags of the same names;
both need `--connect`. The TUIs also get `--hya <this binary>`, the `hya`
their `/connect-remote` runs `hya bridge` with (a TUI started by
`--connect` has no local backend, so `/disconnect-remote` explains that
instead of going back to one).

**When the daemon goes away.** Every TUI bare `hya` started (the terminal
one and every WebUI tab) knows the database (`--db`), and the server says
why it stops ([tui.md](tui.md#when-the-server-goes-away)). After `hya serve
stop` they stay disconnected (`Backend stopped (hya serve stop) · /reconnect
starts it again`) until `/reconnect` in one of them, or a new client, starts
the daemon again; the others attach to it. After `hya serve restart` they
attach to the new daemon. After a crash they find the next server or start
one. Either way they switch to it and reload the open session. Turns that
were running on the old server end with it. A new WebUI tab tries the URL in
its command first and falls back to the database's daemon (found or started)
when it does not answer.

**Requirements.** Bare `hya` starts the frontends only when both stdin and
stdout are terminals. It needs the separately installed frontend package,
including Bun (`$BUN`, else `bun` on `PATH`) and the TUI/WebUI directories. A
backend-only install therefore prints a guidance banner telling the user to
install the frontend with `hya update --tui-only`; `hya serve`, `hya exec`, and other
headless commands do not need it. Without a terminal it prints the same
guidance banner and exits **0**, starting nothing:

```text
hya <version> — a multi-agent coding agent
Run `hya` in a terminal to start the TUI and the WebUI (needs the optional frontend package). Without a terminal, try `hya serve`, `hya exec "<prompt>"`, `hya -p "<goal>"`, or `hya --help`.
```

Scripts that run `hya` with no arguments and no terminal get this banner and
must not treat exit 0 as "interactive session ready."

**What runs.**

1. The backend: the database's daemon (found or started, see above), or the
   `--backend` URL. An empty `--db` means the durable default
   `$XDG_STATE_HOME/hya/sessions.db` (as for `sessions`), so sessions survive
   restarts and `hya sessions` lists them. The path is made absolute.
2. The web host: `bun <tui-web>/src/main.ts --host 127.0.0.1 --port <port>
   --cwd <cwd> -- bun <tui>/src/main.ts --server <server-url> --dir <cwd>
   --db <db> --hya <this hya> --web-tab` (no `--db`/`--hya` with
   `--backend`; `--web-tab` tells a tab's TUI it runs in a browser tab). `hya`
   waits up to 20 s for its `hya-tui-web listening on <url>` line. If the
   host fails (the port is in use, it crashes, or it prints nothing in time),
   `hya` still starts the TUI and passes the reason on.
3. The terminal TUI, attached to this terminal: the same TUI command without
   `--web-tab`, plus `--web-url <url>` or `--web-error <reason>`, and
   `--resume [<id>]` when given (see
   [tui.md](tui.md#start-it)). A failed WebUI shows `WebUI unavailable:
   <reason> · hya --port <N>` in the status line and `/status`, and
   `WebUI unavailable` (warning color) in the status bar.

`<cwd>` is the directory `hya` was started in: the workspace of both
frontends.

**Where the packages come from.** Each package is looked up in this order;
the first directory that has `src/main.ts` wins:

| Package | 1. Override | 2. Installed next to the binary | 3. Source checkout |
| --- | --- | --- | --- |
| TUI | `HYA_TUI_DIR` | `<prefix>/lib/hya/tui` for `<prefix>/bin/hya` | `packages/hya-tui` |
| WebUI host | `HYA_TUI_WEB_DIR` | `<prefix>/lib/hya/tui-web` | `packages/hya-tui-web` |

The installed location is checked for the path `hya` was started as and, if
that is a symlink, for its target. The source checkout is the one the binary
was built from. An override must contain `src/main.ts`; `hya` does not fall
through to the next place then. The chosen directory must also have its
dependencies installed (`node_modules/`); in a source checkout run
`bun install --frozen-lockfile` in `packages/hya-tui` and
`packages/hya-tui-web`.

**Errors before start.** A missing Bun, a missing package, or missing
dependencies stop `hya` with exit status **1** and one message on stderr,
before it touches the terminal, for example:

```text
Error: Bun is required for the TUI and the WebUI but was not found on PATH: install it from https://bun.sh or set BUN=<path>. Other subcommands (`hya serve`, `hya exec`, …) do not need it.
Error: HYA_TUI_DIR=/opt/tui has no src/main.ts (the TUI)
```

A daemon that fails to start (for example a broken config) is reported the
same way, with the last lines of its log `<db>.server.log`.

**Log files.** While the TUI owns the terminal, `hya`'s own stdin reads
`/dev/null` and its stdout and stderr are appended to
`$XDG_STATE_HOME/hya/hya.log` (else `~/.local/state/hya/hya.log`), so notices
never draw over the TUI. The log gets a start line per run (with the backend
URL and database), which daemon it used (`hya: started the backend daemon pid
<pid> at <url> (db <db>, log <log>)` or `hya: using the running backend daemon
pid <pid> at <url> …`), every line the web host prints (prefixed `[webui] `),
and the shutdown steps. At start a log over 4 MiB is moved to `hya.log.1`.
The daemon writes to its own log, `<db>.server.log` (see
[Backend daemon](#backend-daemon)); processes it starts (MCP servers,
plugins) inherit that one.

**Stopping.** When the terminal TUI exits, `hya` sends the web host SIGTERM
(SIGKILL after 8 s) and waits for it. The web host sends SIGHUP to every
browser tab's TUI and SIGKILLs any still running after 3 s. The daemon is
not stopped. `hya` exits with the TUI's exit status. SIGINT, SIGTERM, or SIGHUP sent to
`hya` (a closed terminal sends SIGHUP) first stop the TUI (SIGTERM, then
SIGKILL after 8 s) and then do the same cleanup; `hya` exits with
`128 + signal` (130, 143, 129). The web host runs in its own process group,
so Ctrl+C in the terminal reaches only the TUI and `hya`.

**Interfaces.**

| Contract | Definition |
| --- | --- |
| `--port <PORT>` | `u16`, default `3250`, `0` = free port. Bare invocation only. |
| `HYA_TUI_DIR`, `HYA_TUI_WEB_DIR` | Directory of the TUI / web host package (must contain `src/main.ts` and `node_modules/`). |
| `BUN` | Bun executable; must exist when set. Else `bun` on `PATH`. |
| `--backend <URL>` | `http://` or `https://` URL of a running server; bare invocation only; must answer `GET /v1/health`. |
| `--connect [<LINK>\|-]` | A relay link, `-` (read from the terminal, not echoed), or no value (`HYA_RELAY_LINK`); bare invocation only; conflicts with `--backend`. |
| `--relay-ca <PEM>`, `--transport <auto\|grpc\|ws>` | Only with `--connect`: the in-process bridge's extra CA file and relay binding. |
| `--allow-host <HOST>` | Repeatable; passed to a daemon this launch starts (not to a running one); conflicts with `--backend` and `--connect`. See [Allowed Host names](#allowed-host-names). |
| TUI environment | `HYA_SERVER_TOKEN=<bridge token>` with `--connect` (removed otherwise); never `HYA_RELAY_LINK`. |
| `--resume [<ID>]` | Bare invocation only (else an error); passed to the terminal TUI as `--resume [<ID>]`. |
| TUI flags | `--server <url> --dir <cwd> --db <db> --hya <hya>` (only `--server <url> --dir <cwd>` with `--backend`; `--server <bridge-url> --dir <cwd> --hya <hya> --remote --server-label <label>` with `--connect`); the terminal TUI adds exactly one of `--web-url <url>` / `--web-error <reason>` and `--resume [<id>]` when given ([tui.md](tui.md#start-it)); the WebUI tabs' command adds `--web-tab` instead. |
| Web host readiness | First stdout line matching `hya-tui-web listening on <url>` ([tui-web.md](tui-web.md#usage)). |
| Log files | `<state dir>/hya/hya.log` (bare `hya`) and `<db>.server.log` (the daemon), append-only; each rotated once to `.1` above 4 MiB. |
| Daemon | Found: `<db>.server.json` whose pid is alive and whose `/v1/health` answers. Else started like `hya serve start` (60 s wait). Never stopped by bare `hya`; after `hya serve stop` its TUIs start it again only on `/reconnect`. |
| Exit status | The TUI's status; `128 + signal` for a signal to `hya` or a TUI killed by one; **1** for an error before start. |

To run the frontends by hand instead (development, a remote server), see
[tui.md](tui.md#start-it) and [tui-web.md](tui-web.md#usage).

## `--pure`

Global flag for `exec`, `-p` goal mode, `workflow`, and `serve`:
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

Runs one headless turn and prints the rendered transcript. The session works
in the current directory: its workdir is the caller's cwd as an absolute path.
When `exec` runs in process (not routed through a server that already holds
`--db`), it ensures a Project for that cwd before binding the run's Agent
catalog — the same find-or-create-in-one-transaction the server does for a
routed session's workdir — so the directory's `.hya/bundles` project bundles,
project plugins, and skills apply through the Project catalog scope, and the
new session's `project` field names it. `-p` goal mode, `loop`, and
a `workflow` command that creates a new Session all ensure a Project the same
way. The command uses the global `--db <PATH>` SQLite store when supplied; otherwise it uses an in-memory
store. With `--db`, the database stores the full canonical event log for replay,
which can contain more sensitive data than the rendered transcript. `--json`
prints the canonical event stream as JSONL. A file `--db` is locked for the
run; when a server (the backend daemon) already holds it, `exec` runs the turn
through that server instead, still with the caller's cwd as the workdir (see
[Database lock and the backend daemon](#database-lock-and-the-backend-daemon)).

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

**Which agent runs.** The new root Session's Agent id is resolved the same way
`serve` resolves one: config `default_agent` (see
[Configuration](configuration.md#agent-selection)) when set, otherwise the
built-in `hya-main` agent. There is no per-invocation `--agent` flag yet. An
unresolvable `default_agent` (not a selectable agent id in the bound catalog)
fails the run with a clear `UnknownAgentId` error instead of silently falling
back to `hya-main`. `hya -p` goal mode, `hya loop`, and
`hya workflow run` (when it creates a new Session) all resolve the root Agent
the same way.

**Stopping and end of run.** Nothing keeps streaming after `exec` returns.
When the lead's turn ends, `exec` drains every other in-flight turn — team
members, subagents, and a quiescence-synthesis turn that may have started on
the lead — before it flushes the trajectory, so the log (and `--json` stdout)
ends on terminal events: each open assistant message gets `message_finished
{ finish: "cancelled", cause }`, open tool parts get a `tool_error`, and
members are archived (`reason: shutdown`) so a later run on the same `--db`
can wake them by mailing their handle; the lead is never archived. The cause is `shutdown`, or `leader_failed` when the lead's turn
failed (then the exit status is 1).

| Signal during the run | What happens | `cause` | Exit status |
| --- | --- | --- | --- |
| SIGINT (Ctrl-C) | Drain: every in-flight turn in every session is cancelled and closed; the transcript/trajectory is still printed | `user_cancel` | **130** |
| second SIGINT while draining | Exit immediately; the next start's crash recovery closes what is left (`cause: interrupted`) | — | **130** |
| SIGTERM | Drain as above | `shutdown` | **143** |

The drain waits at most **5 s** (`DRAIN_DEADLINE`) for cancelled turns to
close themselves, then closes any still-open message itself. A process killed
outright (SIGKILL, crash) leaves turns open; the next `hya` process that opens
the same `--db` closes them at startup with `cause: interrupted`, once. The same
stop handling applies to `-p` goal mode and `hya loop`; `hya workflow` drains at
its normal end. Example:

```sh
hya --db run.db exec --json "long task" & pid=$!
sleep 5; kill -INT $pid; wait $pid   # exit 130
hya --db run.db tail-session <session> | tail -1
# {"seq":12,…,"event":{"type":"message_finished",…,"finish":"cancelled","cause":"user_cancel"}}
```


## `hya -p`

```sh
hya -p "make the workspace compile" --max-iterations 6
```

Runs goal mode with an in-memory store. Each iteration runs an agent turn, then
an independent evaluator judges the transcript. The run stops when the evaluator
returns `met=true`, a cap is reached, or cancellation is requested. Goal mode
does not persist to the global `--db` database. The worker Agent is resolved
the same way as `hya exec`'s root Agent (config `default_agent`, else `hya-main`
— see [`hya exec`](#hya-exec)); the independent evaluator is a separate,
unaffected model selection (`--evaluator-model` / `goal.evaluator_model`).

## `hya serve`

```sh
hya serve --bind 127.0.0.1:8080 --db hya.db
```

Starts the HTTP/SSE API from [`../crates/hya-server`](../crates/hya-server).

**No working directory.** `hya serve` never resolves a request against the
directory it was started in ([ADR-0024](adr/0024-project-model-and-client-chosen-workspace.md)).
Clients say where to work: a session's workdir comes from `CreateSession`
(a `workdir`, a Project, or a temporary scratch directory), and rpcs that work
on a directory (files, VCS, worktrees, PTY) need an absolute
`directory` field in the request, else they fail with
`invalid_argument`. Catalog listings without one show the global view (see
[the protocol guide](protocol/README.md#base-url-and-scoping)). The only
startup-directory reads left are project bundles and plugins under
`./.hya/` (see [Bundle Commands](#bundle-commands)); a backend daemon starts
in your home directory, so for it that is `~/.hya/` (run `hya serve --db
<db>` in a project yourself to serve that project's). Bare `hya` passes its
cwd to the TUI as `--dir`, and `hya exec`/`-p`/`loop` record the
caller's cwd as their session's workdir.

| Flag | Meaning |
| --- | --- |
| `--bind <ADDR>` | Socket address. Defaults to `127.0.0.1:8080`; use `127.0.0.1:0` for an ephemeral port. |
| `--allow-host <HOST>` | Also accept requests whose `Host` names `HOST` (a host name or IP address, no port; repeatable; also after `start`/`restart`). See [Allowed Host names](#allowed-host-names). |
| `--db <PATH>` | SQLite path. Empty string uses an in-memory store. A file database is locked for this process (see "One server per database" below); a second `serve` on it exits **75**. |
| `--relay <URL>` | Join the secure relay published at this public URL (`https://host[:port][/prefix]`, or `http://…` for plaintext on a LAN or tailnet) and print the relay link once on stderr as `hya relay link: <link>`. The link is a **secret**: whoever holds it controls this backend. See [Hosting a backend on a relay](relay.md#hosting-a-backend-on-a-relay). |
| `--relay-transport auto\|grpc\|ws` | Relay binding (default `auto`); also the link's `t=`. Needs `--relay`. |
| `--relay-ca <PEM>` | Extra trusted CA certificates for the relay's TLS. Needs `--relay`. |
| `--relay-ephemeral` | A throwaway relay identity instead of `<db>.relay-identity.json`: the link dies with the process. Needs `--relay`. |
| `--relay-heartbeat <SECONDS>` | Relay heartbeat interval (default 15). |

**Readiness contract.** After the listener is bound, the process prints exactly:

```text
hya server listening on <url>
```

That string is a stability contract: harnesses, supervisors, and client SDKs
parse this exact line from merged stdout/stderr to discover the base URL. Do not
change its wording. Source: [`serve.rs`](../crates/hya-backend/src/serve.rs).
With `--relay` the relay is joined before this line, and the link follows it
on stderr, once, as `hya relay link: <link>` plus a one-line secrecy note.

### Allowed Host names

Every HTTP request (and every gRPC call, by its `:authority`) must name an
allowed host, or it gets `403
{"error":{"code":"permission_denied","message":"request refused: Host \"…\" is
not an allowed name for this server (allowed: …); …"}}` before any route runs.
This stops **DNS rebinding**: a web page whose name resolves to 127.0.0.1
still sends its own name as `Host`, so it cannot drive the backend or read
`GET /v1/relay/link` through your browser.

| Accepted | Why |
| --- | --- |
| `localhost`, `127.0.0.1`, `[::1]`, on any port | Loopback clients (the TUI, `hya-client`, curl, a relay bridge's `127.0.0.1:<port>`). The port is not compared: a bridge forwards its own port. |
| The host of `--bind` when it is not a wildcard (`--bind 192.168.1.20:8080` accepts `192.168.1.20`) | The address you chose to serve on. |
| Each `--allow-host <HOST>` (case-insensitive, a trailing dot ignored, IPv6 with or without brackets) | Names you reach the server by on purpose. |

A request with no `Host` (HTTP/1.0) from a network peer is refused too;
requests through the secure relay pass the same check. `--allow-host` is
also a bare `hya` flag (for the daemon it starts), is passed to the daemon
by `hya serve start`, is recorded in the discovery file as `allowHosts`, and
is kept by `hya serve restart` unless new names are given; `hya serve
status` shows them (`hosts`).

**LAN migration.** A wildcard bind (`--bind 0.0.0.0:8080`) used to
answer any `Host`; now only loopback names pass until you list the LAN names
clients use:

```sh
hya serve --bind 0.0.0.0:8080 --allow-host 192.168.1.20 --allow-host hya.lan
```

See [troubleshooting](troubleshooting.md#403-permission_denied-request-refused-host--is-not-an-allowed-name).

**One server per database.** With a file `--db`, `serve` takes an exclusive
runtime-owner lock before initializing the store, and publishes a discovery file once
it listens, so a TUI or bare `hya` can attach to it instead of opening the
same file a second time ([ADR-0022](adr/0022-one-writer-per-database.md)).

| File | Contract |
| --- | --- |
| `<db>.runtime-owner.lock` | Exclusive advisory lock (`flock`), reserved without waiting before daemon store initialization, then held by the runtime owner and held until the process exits; the OS releases it on a crash or SIGKILL. Contents: the owner's decimal PID and newline, cleared before release. Never deleted. |
| `<db>.server.stop` | Written atomically by `hya serve stop` / `restart` just before their SIGTERM: `{"pid": <lock holder>, "reason": "stop" \| "restart"}`. The server reads it when a termination signal arrives, uses it only when `pid` is its own, and deletes it; the reason becomes the last frame of every client stream (`serverStopping`, see below). Deleted by whoever takes the lock. |
| `<db>.server.json` | Written atomically after the listener is bound: `{"url": "http://127.0.0.1:<port>", "pid": <u32>, "version": "<hya version>", "startedAt": <unix ms>}`. While the server is joined to a relay it also has `"relay": {"proxyUrl", "transport", "ephemeral", "ca"?, "heartbeatSecs"?}` (public settings only, never the link; read by `hya serve restart`), and with `--allow-host` names `"allowHosts": ["<host>", …]` (normalized; kept by `hya serve restart`). An unspecified bind address (`0.0.0.0`, `::`) is published as loopback. Removed on a clean shutdown (after the drain); a file left by a crash is ignored and replaced by the next owner. |
| `<db>.relay-identity.json` | The relay identity (room key, Noise static key, link PSK), mode 0600, created on the first relay join and kept across restarts; see [relay.md](relay.md#hosting-a-backend-on-a-relay). |

`<db>` is the `--db` path with its directory resolved (symlinks and `..`), so
different spellings of one file share one lock. An in-memory store (`--db ""`,
the default for `serve`) takes no lock. A second `serve` on a database that
another process holds fails before it starts, with exit status **75**
(`EX_TEMPFAIL`) and one line on stderr:

```text
hya serve: database /home/me/.local/state/hya/sessions.db is already in use by hya server pid 4242 at http://127.0.0.1:53211 (hya 0.41.0); connect to it (`hya-tui --server http://127.0.0.1:53211`, or run bare `hya`, which attaches), stop it, or pass another --db
```

If the holder has not published its URL yet (it is still starting, or it is
not a server), the message names the pid from `<db>.runtime-owner.lock` instead. Clients
that attach check the discovery file with `GET <url>/v1/health`
(`{"ok": true, "version": …}`).

While a server shuts down, `GET /v1/health` answers **503** `unavailable`
and every open event stream (`StreamSessionEvents`, `StreamGlobalEvents`,
over SSE and gRPC) ends at once, so connected clients never hold the shutdown
open and notice the loss right away. The last frame of each stream is
`serverStopping {reason}`: `stop` (`hya serve stop`), `restart` (`hya serve
restart`), or `signal` (any other SIGTERM/SIGINT/SIGHUP, for example Ctrl+C
on a foreground `hya serve`); see
[Server shutdown](protocol/README.md#server-shutdown).

### Backend daemon

`hya serve start|status|stop|restart` manage the backend daemon of a
database: a `hya serve` that runs on its own, outlives every client, and is
shared by all of them ([ADR-0023](adr/0023-persistent-backend-daemon.md)).
Bare `hya` and the TUI start it on demand; these commands control it by
hand. Plain `hya serve` (no action) still serves in the foreground.

```sh
hya serve start                 # start the daemon of the default database (or find it)
hya serve status                # url, pid, version, db, uptime; exit 1 when none runs
hya serve stop                  # graceful stop; waits until it released the database
hya serve stop --force          # SIGKILL after --timeout (default 30 s)
hya serve restart --db ~/work.db
hya serve start --json          # machine-readable (the TUI uses this)
hya serve start --relay https://relay.example.com   # a daemon on a secure relay
hya serve relay status          # the running backend's relay (see below)
```

`--db` defaults to the durable `$XDG_STATE_HOME/hya/sessions.db` for these
actions (plain `serve` keeps its in-memory default) and may come before or
after the action. The path is made absolute.

| Action | Behavior | Output | Exit |
| --- | --- | --- | --- |
| `start [--json]` | If a server of the database answers (discovery file, live pid, healthy), report it. Else run `hya serve --bind 127.0.0.1:0 --db <db>` (plus this command's `--model`, `--yolo`, `--pure`, `--allow-host`, and relay flags) **detached**: its own session (`setsid`), working directory your home directory (`$HOME` when it exists, else `/`; never the caller's, since the backend serves every client wherever it runs), stdin `/dev/null`, stdout and stderr appended to `<db>.server.log` (rotated to `.1` above 4 MiB). Wait up to 60 s until it answers. If its start exits 75 (another client's daemon won the race, or the last one is still shutting down), wait for that server, or start again once the lock is free. | `started hya server pid <pid> at <url> (db <db>, log <log>)` or `hya server pid <pid> already running at <url> (hya <version>, db <db>)`. `--json`: `{"url", "pid", "version", "startedAt", "db", "log", "started"}` (`started` is true only when this call started it). A server of another hya version adds `note: the running server is hya X, this is hya Y; run `hya serve restart` to switch` on stderr. | **0**; **1** when the daemon exits with an error (its log tail is printed) or does not answer in 60 s |
| `status [--json]` | Read the discovery file and probe the server. | `hya server pid <pid> running at <url>` and `version`, `db`, `uptime`, `log` lines. `--json`: `{"url", "pid", "version", "startedAt", "uptimeMs", "db", "log", "relay"?, "allowHosts"?}` (text: `relay` and `hosts` lines when set). | **0** running; **1** with `no hya server is running on <db>` (or `hya server pid <pid> holds <db> but does not answer (starting or stopping)`) on stderr |
| `stop [--force] [--timeout <s>]` | Write the stop request (`<db>.server.stop`, reason `stop`), SIGTERM to the lock holder (pid from `<db>.runtime-owner.lock`, else the discovery file), then wait until the lock is free. The server drains turns (5 s) and ends every client stream with `serverStopping {reason: "stop"}`. `--force`: SIGKILL when it has not stopped within `--timeout` (default 30). | `stopped hya server pid <pid> (db <db>)` and `connected TUIs stay disconnected until /reconnect, or until a new hya client starts the next server`; `killed …` with `--force`; or `no hya server is running on <db>`. | **0** (also when nothing ran); **1** when it did not stop in time without `--force` |
| `restart [--json] [--force] [--timeout <s>]` | Stop the old daemon with reason `restart`, wait for it to release the database, then start this binary as a fresh daemon. Preserve the old relay and allowed hosts unless overridden. | A new ready line, or the `start --json` shape plus `restarted: true`; old streams end with `serverStopping {reason: "restart"}`. | **0** once the new daemon is healthy; **1** if stop or start fails |
| `relay connect\|disconnect\|status\|link\|rotate` | Control the running backend's relay connector over its loopback-only `RelayControl` rpcs; see [the command table](relay.md#hosting-a-backend-on-a-relay). | `status`: `relay <state>` plus detail lines (`--json`: `RelayStatus`); `link`: the link alone; `connect`/`rotate`: `hya relay link: <link>`. | **0**; **1** when no server runs or the rpc fails (not joined for `link`, a bad URL for `connect`) |

Daemon discovery reads the PID metadata written by the store's runtime-owner
lock holder and checks process liveness and HTTP health. It never takes a lock.
Concurrent `start` commands report the same writer; a losing daemon child may
exit **75**, while its starter attaches to the winner. A stopped owner's PID is
cleared before the single lock is released.

A restart has a short period with no listener. The new daemon binds its own
port and publishes a fresh URL, PID, and `startedAt`; clients rediscover it.
There is no queued handoff response. A restart invoked by the daemon's own
shell tool may cancel that call along with its turn; run daemon control from
an external shell.

`start` and `restart` accept the relay flags of plain `hya serve`
(`--relay`, `--relay-transport`, `--relay-ca`, `--relay-ephemeral`,
`--relay-heartbeat`). The daemon joins the relay at start but never prints
 the link to its log; `start` reads it over loopback and prints
`hya relay link: <link>` on stderr. When a server was already running, `start --relay` changes nothing
and says so (use `hya serve relay connect`).
`status` shows a `relay` line (`--json`: `relay`) while joined.

#### Restoring interrupted sessions

On every graceful daemon stop (including SIGTERM, SIGINT, and SIGHUP), the
engine refuses new turns, cancels all active turns, and records one
`pending_resume` row per interrupted, non-archived root session in the same
transaction as its cancellation. Child sessions stay under the resident
supervisor's recovery. Unanswered permission and
question requests are cancelled with the turn. Their tools are never marked
successful merely because an answer arrives after cancellation.

The next `start` appends one user-role restore prompt, then starts a fresh
turn with normal project guidance, busy tracking, and cancellation. The row
is deleted when the assistant message starts; a crash before that point
starts the turn on the next launch without repeating the prompt. Missing or
archived sessions are skipped. A restore failure is logged with the session
id and does not prevent other restores or server startup. The prompt says the
previous turn was interrupted by a restart and lists unfinished tool names
and call IDs. Completed results stay in the transcript. The model decides
what to redo; interrupted tools may already have had side effects.

```sh
hya serve stop --db ./work.db
hya serve start --db ./work.db --json  # restore interrupted sessions
# Or perform both operations with the newly built binary:
./target/debug/hya serve restart --db ./work.db
```

`serve check`, `restart --verify`, `restart --exe`, listener inheritance,
generation pins, rollback launch, and parked owners have been removed
([ADR-0031](adr/0031-hot-reload-and-stop-start-restart.md)).

A stop is a stop: connected TUIs start nothing after `hya serve stop` (or a
plain signal). They show `Backend stopped (hya serve stop) · /reconnect
starts it again` and stay disconnected until `/reconnect` in one of them, or
a new client (`hya`, a TUI, `hya serve start`) starts the daemon, which they
then attach to. After `restart` they wait up to 60 s for the new daemon,
attach to it, and reload their own code, so the TUI and WebUI tabs pick up
new TUI features too ([tui.md](tui.md#hot-update-after-hya-serve-restart)).
Only a daemon that goes away without saying why (a crash,
`kill -9`) makes them find or start the next one by themselves
([tui.md](tui.md#when-the-server-goes-away)).

**Signal handling.** SIGTERM, SIGINT, and SIGHUP handlers are installed
**before** the listen line is printed (an e2e-harness ordering requirement: a
harness that sees the URL may signal immediately). A signal first drains the
engine — every in-flight turn in every session (roots and members) is
cancelled with `cause: shutdown` and closes its messages and tool parts within
the 5 s drain deadline, resident members go terminal, each lead is parked
`idle`, and new turns are refused — then the graceful axum shutdown and
spawn-supervisor teardown run, so the process terminates normally with exit
code **0** rather than dying by signal. A `/v1` turn cancel
(`POST /v1/sessions/{id}/turns/{turn}/cancel`) closes that session's turn with
`cause: user_cancel`, and `SessionInfo.busy` is true while any engine turn runs
on the session (including a resident wake or synthesis turn the client did not
start). This
matters for supervisors (systemd, `docker stop`) and for test harnesses that
assert a clean exit.

**Startup trace.** When `HYA_STARTUP_TRACE` is `1` or `true` (case-insensitive),
serve also emits JSON startup phase marks on stderr (`backend_start`,
`store_open`, `runtime_resolved`, `interrupted_turns_recovered`,
`store_recovery`, `engine_runtime`, `residents_recovered`, `engine_built`, and
`backend_listen` after the listen line), for example
`{"hya_startup":true,"mark":"backend_listen","wall_ms":…,"detail":"<url>"}`.
See [Diagnosing Slow Startup](troubleshooting.md#diagnosing-slow-startup).

The server serves exactly one HTTP contract — `hya.v1` — under `/v1`
(HTTP/JSON + SSE + WebSocket). The former native `/sessions/*` routes and the
Compat-compatible legacy/v2 route groups are deleted. The same port also
serves the eighteen services over gRPC (HTTP/2 without TLS; requests with
`content-type: application/grpc*`), from the same server state. Optional and
legacy: `HYA_GRPC_BIND=<host:port>` adds an extra listener serving the same
server (it prints `hya grpc listening on http://<addr>` after the readiness
line; a bind failure is reported on stderr and the server starts without
it). See [Protocol guide](protocol/README.md),
[API reference](protocol/api-reference.md), and
[Server and Client](architecture/server-client.md).

## `hya provider`

`hya provider` sets up model providers from the command line. `add` is the
onboarding path. It asks for the base URL, the protocol, and the API key. It
then fetches the provider's model list with that key, and saves the provider
only when the list comes back. `list` shows what is configured, and `remove`
deletes a provider. They write the same files as the TUI Provider View
(`/key`).

```sh
hya provider add [--name <id>] [--base-url <url>] [--protocol <protocol>] [--api-key <key>] [-y]
hya provider list [--refresh]
hya provider remove <id> [-y]
hya provider logout <id>
```

`hya providers` is an alias of `hya provider`. It replaces the removed
`hya auth list|logout`: `list` shows saved keys and `logout` deletes one.

### `hya provider add`

Any value not given as a flag is asked for on the terminal, in this order:

1. **Base URL**: the API root that lists models at `<base>/models`, for
   example `https://api.openai.com/v1` or `https://api.anthropic.com/v1`. A
   trailing `/` is dropped. Only `http(s)` URLs with a host and no credentials
   are accepted.
2. **Protocol**: pick a number or type a name. The default is
   `anthropic-messages` when the URL contains `anthropic`, and `openai-chat`
   otherwise.
3. **API key**: typed with echo off on a terminal. Leave it empty for
   endpoints that need no key.
4. After the model list is fetched: **Provider name**, the `<id>` in
   `<id>/<model>`. The default comes from the host: `api.openai.com` gives
   `openai`, `api.12th.day` gives `12th`, and an IP address or `localhost`
   gives `local`.

| Protocol | `config.yaml` `kind` | Requests go to |
| --- | --- | --- |
| `openai-chat` | `openai` | `<base>/chat/completions`, Bearer key |
| `openai-responses` | `openai-response` | `<base>/responses`, Bearer key |
| `anthropic-messages` (alias `anthropic-message`) | `anthropic` | `<base>/messages`, `x-api-key` |

The model list is fetched exactly as it is for a saved provider
(`GET <base>/models`, bounded to 8 s). The first 20 ids are printed. If the
fetch fails, for example because the key is rejected, the endpoint is
unreachable, or it has no `/models`, `add` asks `Save the provider anyway?`.
A closed stdin answers no, the command exits 1, and nothing is written. If the
id is already configured, `add` asks before replacing it. With `--name`, it
asks before any other prompt. `-y` replaces without asking.

Saving writes:

| What | Where |
| --- | --- |
| `providers.<id>` with `kind`, `base_url`, and (new) `models: []` | the active `config.yaml` (edited in place; comments and other keys kept) |
| the API key (`type: api`) | `~/.config/hya/auth/<id>.yaml` (mode 0600), never `config.yaml`. Replacing a provider without a key removes its old saved key. |
| the fetched model list | the model cache, the same one `hya models --refresh` fills |

If a backend daemon of the database is running, `add` then calls its
`POST /v1/providers/{id}/refresh`, so the new models are live without a
restart. A failure there is a warning; `hya serve restart` also applies the
change.

Example session (input you type is shown after each prompt):

```text
$ hya provider add
Base URL (e.g. https://api.openai.com/v1): https://api.12th.day/v1
Protocol:
  1) openai-chat         OpenAI Chat Completions  (<base>/chat/completions)
  2) openai-responses    OpenAI Responses         (<base>/responses)
  3) anthropic-messages  Anthropic Messages       (<base>/messages)
Choose [1]: 3
API key (input hidden; empty for none):
Fetching models from https://api.12th.day/v1/models …
Found 127 models:
  cline-free/deepseek-v4.1-flash, cline-free/muse-spark-1.3-contributor
  …
  … and 107 more (`hya provider list`)
Provider name [12th]:
Saved provider `12th` (anthropic-messages, https://api.12th.day/v1) to ~/.config/hya/config.yaml
Saved its key to ~/.config/hya/auth/12th.yaml
Use a model with `hya --model 12th/cline-free/deepseek-v4.1-flash`, or set `default_model: 12th/cline-free/deepseek-v4.1-flash` in config.yaml
```

The same without prompts, for scripts:

```sh
hya provider add --name 12th --base-url https://api.12th.day/v1 \
  --protocol anthropic-messages --api-key "$KEY" --yes
```

`--api-key` is visible in process listings. On a shared machine, pipe the key
instead: `printf '%s\n' "$KEY" | hya provider add --name … --base-url … --protocol …`.

### `hya provider list`

Prints one block per `providers.<id>` in `config.yaml`: the id, the protocol
(or the raw `kind` for `openai-codex`, `grok-build`, and `google`), the base
URL, and where the key comes from (`saved key`, `oauth`, `config api_key`, or
`no key`). Below that it lists every model as `<id>/<model>`. Models come
from the model cache merged with `models:` entries, the same catalog
`hya models` prints. `--refresh` fetches every provider's list first, through
the database's running backend when one runs (see `models --refresh` below).

A `models:` entry that names a fetched model overrides it field by field, and
unset fields keep the fetched values
([ADR-0029](adr/0029-config-model-entries-override-the-model-cache.md)). Such
rows end in `(config override)`. Rows the endpoint does not list, but an entry
declares, end in `(config only)`. Keys saved for ids that `config.yaml` does
not declare come last.

```text
12th  anthropic-messages  https://api.12th.day/v1  saved key
  12th/MiniMaxAI/MiniMax-M2.5
  12th/claude-sonnet-5  (config only)
  12th/glm-5.3  (config override)
  …

Saved keys without a provider in config.yaml (`hya provider logout <id>` deletes one):
  old-proxy
```

### `hya provider logout`

`hya provider logout <id>` deletes the saved key `auth/<id>.yaml` and keeps
the provider's declaration, which is then used with its inline `api_key` if it
has one. The provider can also be missing from `config.yaml`, which cleans up
a leftover key. If no key is saved, it exits 1. A running backend rebuilds the
provider without the key (`DELETE /v1/auth/{id}`).

### `hya provider remove`

`hya provider remove <id>` asks for confirmation, unless you pass `-y`. It
then deletes `providers.<id>` from `config.yaml`, the saved key
`auth/<id>.yaml`, and the provider's model-cache rows. It warns about every
value outside `providers` that still names a model of the provider, such as
`default_model: <id>/…` or `agents.<agent>.model`. A running backend of the
database drops the provider's route and models through
`DELETE /v1/auth/{id}`. That call rebuilds the provider, and a provider no
longer in `config.yaml` loses its route. Removing an id that is not
configured exits 1.

## Auth and Catalog Commands

```sh
hya login <provider> <token>
hya oauth login --provider <name> --type <openai-codex|grok-build> [--loopback] [--no-browser] [--browser] [--model <id>] [--base-url <url>]
hya oauth status [provider]
hya models [provider] [--verbose] [--refresh]
hya agent list
```

`login` writes a plain provider token under `~/.config/hya/auth`. Prefer
`oauth login` for ChatGPT Codex and Grok Build subscription auth — it runs the
OAuth flow in Rust, stores a refreshable credential bundle, and upserts the
provider route into `config.yaml`.

**`--type` values.** Interactive OAuth accepts only two provider
implementations, spelled exactly as their `kind` label: `openai-codex` and
`grok-build`.
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
`OAuthLoginOptions`: `provider`, `oauth_type`, `loopback`,
`no_browser`, `model`, `base_url` (the `auth_dir` / `config_path` fields are
test-only overrides).

Saved credentials take precedence over inline `api_key` values. `hya provider
list` shows every saved key and `hya provider logout <id>` deletes one (see
[`hya provider`](#hya-provider)).

**`oauth status [provider]`.** Prints non-secret per-provider status only —
credential kind (`api` vs oauth), OAuth type when present, `expires` /
`status=ok|EXPIRED`, and ChatGPT/Grok `account=` id when known. For expired
OAuth credentials it also prints a ready-to-copy re-login line
(`hya oauth login --provider … --type …`). No token material is printed.

**`models [provider]`.** Prints the sorted `provider/model` rows of the
effective catalog: each provider's remote models from the model cache
(`$XDG_CACHE_HOME/hya/model_cache.db`) merged per model id with its
`config.yaml` `models:` entries (see [Configuration — Model cache and config
overrides](configuration.md#model-cache-and-config-overrides)). Providers with
no cached rows, and discovery-only providers, fetch their remote list first.
`--refresh` fetches the remote list of every provider (or only `provider`'s)
into the cache before printing; a failed fetch is reported on stderr as
`hya: <provider>: model list <result>: <error>` and keeps the old rows.

When a backend daemon of the database (`--db`) is running, `--refresh` asks it
to fetch instead: one `POST /v1/providers/{id}/refresh` (`RefreshProvider`)
per provider declared in `config.yaml` (only `provider` when given). The
backend fetches each list once, writes the model cache, rebuilds its catalog,
and emits a live `catalogUpdated` frame, so its TUI and WebUI clients re-read
`GET /v1/models` and offer the refreshed models without a restart. The command
then prints from the cache the backend wrote. Without a running backend it
fetches the lists itself. A backend that answers with an error fails the
command.

```sh
hya serve start
hya models openai --refresh   # the open TUI's /model list now shows the new models
```

With `--verbose`, each id is followed by a JSON line with `id`, `provider`,
`source` (`remote`, `config`, `override`, or `offline`), and — each only when
the model's metadata (config entry or remote model list) declares it —
`name`, `context`, `output`, and `reasoning`:

```sh
$ hya models openrouter --verbose
openrouter/vendor/model-a:free
{"context":131072,"id":"vendor/model-a:free","name":"Model A (free)","output":8192,"provider":"openrouter","reasoning":true,"source":"override"}
```

Unfiltered offline output is exactly `hya/offline`; a filter with no rows exits
with `Provider not found: <id>`. Provider declarations that resolved no rows do
not fabricate output.

**`agent list`.** Prints the built-in catalog's selectable agents, one block
per agent: `id (mode) [builtin|bundle]`, then its description when it has one.
Deliberate limitation: `agent list` **never** inspects on-disk agent files under
`.hya/`, `.claude/`, or `.opencode/`, nor config-declared agents — it reflects
the built-in catalog only. System agents (compaction / title / summary) are
excluded because they are not ordinarily spawnable via catalog `can_spawn`
reachability.

## Session Commands

```sh
hya sessions --db hya.db
hya sessions --all            # archived root sessions too
hya sessions --archived       # only archived root sessions
hya sessions archive <session-id>
hya sessions unarchive <session-id>
```

`sessions` lists persisted sessions in a SQLite database, including sessions
created by `exec --db` and `exec --json --db`, one per line:
`<id>  events=<n>  started_ms=<ms>`, plus `  archived_ms=<ms>` on an archived
session. Empty `--db` is remapped to the durable XDG path (same as bare
interactive startup), not in-memory; `--db` may also follow `archive` and
`unarchive`.

Archived root sessions are hidden by default: the TUI archives its session
when you quit it gracefully, and `--resume` brings it back (see
[Archived sessions](protocol/README.md#archived-sessions)). `--all` lists them
too and `--archived` lists only them (the two flags conflict).
`archive <id>` archives a root session and `unarchive <id>` brings one back;
both print `archived <id>` / `unarchived <id>` and succeed when the session
already is in that state. Archiving a subagent child session or an unknown id
fails (exit 1). When a server holds the database (bare `hya`'s daemon or
`hya serve`), the change goes through that server's `UpdateSession`, so
connected clients see it live; if the holder does not serve HTTP yet, the
command exits 75. Otherwise it takes the database lock and writes the
`session_archived` / `session_unarchived` event directly (no session hooks
run then).

## `hya tail-session`

```sh
hya tail-session <session-id> --db hya.db
```

Replays a persisted session's event log as JSON lines. The `<session-id>`
accepts any valid `SessionId` form: `hysec_...`, `ses_...`, or legacy raw UUID.
Empty `--db` is remapped to the durable XDG path (not in-memory).

This command intentionally exits cleanly on broken pipe (exit 0), so shell
filters such as `head` and `grep -q` can close stdout without causing a panic.

## `hya update`

`hya update [--backend-only | --tui-only] [--version VERSION] [--force]
[--prefix DIR]` installs or reinstalls the latest backend and frontend
releases into the running hya's prefix. `--backend-only` updates only the
backend (`bin/hya`, bundles, Bun adapter); `--tui-only` updates only the
frontend (Bun, TUI, WebUI) and never installs `bin/hya`. The two flags conflict.
`--version` pins each selected side. A backend-only installation can run
headless commands without the frontend, while bare `hya` tells the user to run
`hya update --tui-only` when the frontend is missing. See
[Install and update](install.md).

`hya update version` prints the updater's package version (the backend release
version). `hya` dispatches every `update` command before composing any runtime:
no config bootstrap, bundles, providers, plugins, MCP, or session store are
loaded in the installer process. `--db` selects the daemon to refresh or restart;
`--model` does not change the running daemon during installation.

After a successful `hya update`, the running daemon for `--db` (the default
session database when omitted) reloads its bundles if only bundle contents
changed. A changed backend executable runs the installed `hya --db <db> serve
restart`: stop drains interrupted turns, start restores them with a restore
prompt, and connected TUIs automatically discover the new URL and reconnect.
No running daemon is started solely by an update. Unchanged installs and
frontend-only updates leave the daemon running.

## `hya proxy`

```sh
hya proxy --port 8766
```

Runs the relay proxy (docs/relay.md): a blind Noise rendezvous a backend and
a client reach each other through. Dispatched before any runtime
composition — no config, providers, plugins, MCP, or session store — like
`hya update`.

**Readiness contract.** After the listener is bound, the process prints
exactly:

```text
hya proxy listening on <scheme>://<addr><prefix>
```

`<scheme>` is `https` with `--tls-cert`/`--tls-key`, else `http`; `<addr>` is
the bound socket address; `<prefix>` is the normalized `--path-prefix`, or
empty. Source: [`proxy_cmd.rs`](../crates/hya-backend/src/proxy_cmd.rs).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--host <HOST>` | `0.0.0.0` | Bind host. |
| `--port <PORT>` | `8766` | Bind port; `0` picks a free port. |
| `--tls-cert <PEM>` / `--tls-key <PEM>` | none | TLS certificate/key; both or neither. |
| `--path-prefix <PREFIX>` | none | Serve both bindings under a path prefix. |
| `--trust-forwarded <HEADER>` | off | Identify clients by the address in exactly one forwarding header instead of the socket address: `cf-connecting-ip`, `x-real-ip`, or `x-forwarded-for` (its rightmost entry, appended by your hop). Only behind a hop that sets that header; see the [recipes](relay.md#deployment-recipes). |
| `--max-rooms`, `--max-streams-per-room`, `--idle-timeout-secs`, `--stream-rate-bytes-per-sec` | library defaults | The four `ProxyLimits` knobs an operator chooses; every other limit keeps its library default (durations in whole seconds). See [docs/relay.md](relay.md#hya-proxy). |
| `--drain-timeout-secs <N>` | `10` | How long shutdown waits for streams and connections to drain. |

**Shutdown.** SIGINT or SIGTERM stops accepting new connections, drains
existing relay streams (`UNAVAILABLE`), and exits **0**.

Deployment recipes for Cloudflare Tunnel, nginx, Caddy, Tailscale, and direct
TLS are in [docs/relay.md](relay.md#deployment-recipes).

## `hya bridge`

```sh
printf '%s\n' "$LINK" | hya bridge -
hya bridge - --json --exit-with-stdin   # for a parent process (the TUI)
```

The client side of the [secure relay](relay.md#connecting-from-a-client): a
loopback HTTP address that reaches the remote backend behind a relay link,
end to end encrypted. Point a TUI at it with `--server <url> --remote`.
Dispatched before any runtime composition, like `hya proxy`.

**Readiness contract.** Once listening (after choosing the relay binding and
checking the link), stdout gets two lines,
`hya bridge listening on http://127.0.0.1:<port>` and
`hya bridge token <token>`, or with `--json` exactly one:
`{"url":"http://127.0.0.1:<port>","room":"<room_id>","proxy":"<redacted relay>","label":"remote: <relay>/<room_id>","token":"<token>"}`.
Status lines go to stderr (`hya bridge: …`) and never hold the token or the
link. Source: [`bridge.rs`](../crates/hya-backend/src/bridge.rs).

**Bridge token.** `<token>` (64 lowercase hex characters, new for every
bridge) must be sent as `x-hya-bridge-token: <token>` on the first request of
every connection (send it on every request); without it the bridge answers
`401 {"error":{"code":"unauthenticated",…}}` and opens no relay stream. The
header is stripped before the request reaches the backend. A TUI takes it
from `HYA_SERVER_TOKEN` (`HYA_SERVER_TOKEN=<token> bun packages/hya-tui/src/main.ts
--remote --server <url>`); `/connect-remote` and bare `hya --connect` pass it
themselves. Details: [relay.md](relay.md#connecting-from-a-client).

| Flag | Default | Meaning |
| --- | --- | --- |
| `<LINK>` | `$HYA_RELAY_LINK` | The relay link; `-` reads one line from stdin (recommended: an argument is visible in process listings, and `hya` warns). |
| `--listen <ADDR>` | `127.0.0.1:0` | Loopback listen address; non-loopback addresses are refused. |
| `--relay-ca <PEM>` | none | Extra trusted CA certificates. |
| `--transport auto\|grpc\|ws` | the link's `t=` | Relay binding override. |
| `--json` | off | JSON readiness line. |
| `--exit-with-stdin` | off | Exit when stdin reaches end of file. |

**Exit codes:** **0** after SIGINT, SIGTERM, SIGHUP, or (with
`--exit-with-stdin`) end of stdin; **1** for a bad link or flag, a
non-loopback `--listen`, a relay no binding reaches, or a link the backend
rejects.

## `hya relay doctor`

```sh
hya relay doctor https://relay.example.com/hya
```

Probes a relay path (a proxy URL or a `hya://`/`hya+insecure://` link — a
link's secret is never printed) and recommends a `t=` value. See
[docs/relay.md](relay.md#hya-relay-doctor) for the full report shape and the
advice table per failure kind.

| Flag | Meaning |
| --- | --- |
| `--relay-ca <PEM>` | Extra trusted CA certificates. |
| `--timeout <SECS>` | Deadline for each probe (default 5). |
| `--measure-idle` | Bounded (130s) idle-cut measurement; needs a link with a live room. |
| `--json` | Emit the report as JSON. |

**Exit codes:** **0** when at least one binding (gRPC or WebSocket) works,
**1** when neither does.

## Exit Codes

| Binary | Success | Failure / notes |
| --- | --- | --- |
| `hya` | **0** on success (including the bare guidance banner, `serve` graceful signal shutdown, `serve stop` with nothing running, `proxy` and `bridge` graceful SIGINT/SIGTERM shutdown, and `tail-session` broken-pipe). **75** from `serve` on a database another process holds. **1** from `serve status` and `serve relay …` when no server runs (and from `serve relay …` when its rpc fails). **130** / **143** when `exec`/`-p`/`loop` was stopped by SIGINT / SIGTERM (after the drain). Bare `hya` on a terminal exits with the terminal TUI's status, or `128 + signal` (130 / 143 / 129) when `hya` was stopped by SIGINT / SIGTERM / SIGHUP. | **1** with the full `anyhow` error chain printed to stderr on any error — CLI validation failures use the same path; `hya relay doctor` also exits **1** (with its report still printed) when neither relay binding works. |
