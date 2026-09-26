# CLI Reference

`hya` is the single terminal entry point: one executable (built from the
`hya-backend` package, [`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs))
whose subcommands select the area being controlled. Since 0.38.0 there are no
other user-facing executables — the former `hya-backend` binary name and the
standalone `hya-updater` binary are gone.

| Area | Subcommands |
| --- | --- |
| Interactive TUI and WebUI | bare `hya` on a terminal (see [Bare `hya`](#bare-hya)) |
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
     [--port <PORT>] [--yolo] [--db <PATH>] [COMMAND]
```

| Option | Meaning |
| --- | --- |
| `--model <MODEL>` | Override `default_model` from hya config and `HYA_MODEL`. |
| `-p, --prompt <GOAL>` | Run headless goal mode instead of a subcommand. |
| `--max-iterations <N>` | Iteration cap for goal mode. Defaults to `6` in the CLI. |
| `--port <PORT>` | WebUI port of [bare `hya`](#bare-hya) on `127.0.0.1`. Default `3250`; `0` picks a free port. Only valid without a subcommand and without `-p` (`hya --port 1 sessions` is an error); `hya serve --port` is the server's own flag. |
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
[`serve`](#hya-serve)). Bare `hya` on a terminal sends its stdout and
stderr to a log file instead (see [Bare `hya`](#bare-hya)).

### `--db` empty-string semantics

Empty `--db` is **not** always in-memory:

| Command path | Empty `--db` means |
| --- | --- |
| `exec`, `run`, `serve` | In-memory store (`open_store("")` → `SessionStore::connect_memory`). |
| bare `hya` (TUI + WebUI), `sessions`, `tail-session` | Remapped to `$XDG_STATE_HOME/hya/sessions.db`, falling back to `$HOME/.local/state/hya/sessions.db` (or `./.local/state/hya/sessions.db` when neither is set). The directory is created if missing. |

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

Run with no subcommand and no `--prompt` on a terminal, `hya` starts the
interactive frontends: the terminal TUI ([OpenTUI frontend](tui.md)) and the
WebUI ([browser-rendered TUI](tui-web.md)) on `http://127.0.0.1:3250`. Both
talk to one server that runs inside the `hya` process, so a session started in
the terminal shows up in every browser tab and the other way round.

```sh
hya                 # TUI here, WebUI on http://127.0.0.1:3250
hya --port 8000     # WebUI on http://127.0.0.1:8000
hya --port 0        # WebUI on a free port (the TUI shows which)
hya --yolo --model anthropic/claude-sonnet-4-6 --db ~/work.db
```

The terminal TUI's status bar, its sidebar `Context` box, and `/status` show
`WebUI http://127.0.0.1:<port>`. Open that address in a browser: each tab runs
its own TUI process against the same server. Quit the terminal TUI (`/exit`,
Ctrl+D, or Ctrl+C twice) to stop everything.

**Attaching to a running server.** One database has one server
([ADR-0022](adr/0022-one-writer-per-database.md)). If another process already
serves the database (a `hya serve --db`, a TUI that started its own backend,
or another bare `hya`), `hya` starts no server. Before it touches the
terminal it reads `<db>.server.json` next to the database, checks that
`GET <url>/v1/health` answers, and runs the WebUI host (still on `--port`) and
the terminal TUI against that server. The TUI gets `--attached-pid <pid>`, and
`/status` shows `Backend     attached to a running server · pid <pid>`.
Quitting stops only what this `hya` started (the TUI and the web host). The
other process's server keeps running. Its flags apply, not this launch's
`--model`, `--yolo`, or `--pure`; the log notes this. If the holder is still
starting, `hya` prints `hya: database <db> is in use by pid <pid>; waiting for
its server…` and waits up to 20 s. If no healthy server appears, it exits
**1** with, for example:

```text
Error: database /home/me/.local/state/hya/sessions.db is in use by pid 4242, which has published no server (waited 20 s); stop that process or pass another --db
```

**Requirements.** Bare `hya` starts the frontends only when both stdin and
stdout are terminals. It needs [Bun](https://bun.sh) (`$BUN`, else `bun` on
`PATH`) and the two Bun packages, which a release archive or `install.sh`
places next to the binary. Without a terminal it prints a guidance banner and
exits **0**, starting nothing:

```text
hya <version> — a multi-agent coding agent
Run `hya` in a terminal to start the TUI and the WebUI (http://127.0.0.1:3250; needs Bun). Without a terminal, try `hya serve`, `hya exec "<prompt>"`, `hya -p "<goal>"`, or `hya --help`.
```

Scripts that run `hya` with no arguments and no terminal get this banner and
must not treat exit 0 as "interactive session ready."

**What runs.**

1. The v1 server, in the `hya` process, on `127.0.0.1:<free port>`. It is
   composed exactly like [`hya serve`](#hya-serve) and honors the global
   `--db`, `--model`, `--yolo`, and `--pure`. An empty `--db` means the
   durable default `$XDG_STATE_HOME/hya/sessions.db` (as for `sessions`), so
   sessions survive restarts and `hya sessions` lists them. It prints no
   readiness line.
2. The web host: `bun <tui-web>/src/main.ts --host 127.0.0.1 --port <port>
   --cwd <cwd> -- bun <tui>/src/main.ts --server <server-url> --dir <cwd>`.
   `hya` waits up to 20 s for its `hya-tui-web listening on <url>` line.
   If the host fails (the port is in use, it crashes, or it prints nothing in
   time), `hya` still starts the TUI and passes the reason on.
3. The terminal TUI, attached to this terminal: `bun <tui>/src/main.ts
   --server <server-url> --dir <cwd>` plus `--web-url <url>` or
   `--web-error <reason>` (see [tui.md](tui.md#start-it)). A failed WebUI
   shows `WebUI unavailable: <reason> · hya --port <N>` in the status line
   and `/status`, and `WebUI unavailable` (warning color) in the status bar.

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

A server that fails to start (for example a broken config) is reported the
same way, followed by `hya: see <log file> for the server and WebUI log`.

**Log file.** While the TUI owns the terminal, `hya`'s own stdin reads
`/dev/null` and its stdout and stderr are appended to
`$XDG_STATE_HOME/hya/hya.log` (else `~/.local/state/hya/hya.log`), so server
notices never draw over the TUI. The log gets a start line per run, the
server URL (`hya: server listening on <url>`), every line the web host prints
(prefixed `[webui] `), and the shutdown steps. Processes the server starts
(MCP servers, plugins) inherit the log too. At start a log over 4 MiB is
moved to `hya.log.1`.

**Stopping.** When the terminal TUI exits, `hya` sends the web host SIGTERM
(SIGKILL after 8 s) and waits for it. The web host sends SIGHUP to every
browser tab's TUI and SIGKILLs any still running after 3 s. Then the server
drains running turns (up to 5 s, as `serve` does on a signal) and shuts down.
`hya` exits with the TUI's exit status. SIGINT, SIGTERM, or SIGHUP sent to
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
| TUI flags | `--server <url> --dir <cwd>` and exactly one of `--web-url <url>` / `--web-error <reason>` ([tui.md](tui.md#start-it)). |
| Web host readiness | First stdout line matching `hya-tui-web listening on <url>` ([tui-web.md](tui-web.md#usage)). |
| Log file | `<state dir>/hya/hya.log`, append-only; rotated once to `hya.log.1` above 4 MiB. |
| Attach | `<db>.lock` held and `<db>.server.json` healthy → no in-process server; TUI flags `--server <url> --dir <cwd> --attached-pid <pid>` ([`hya serve`](#hya-serve) "One server per database"). Wait for a starting holder: 20 s. |
| Exit status | The TUI's status; `128 + signal` for a signal to `hya` or a TUI killed by one; **1** for an error before start. |

To run the frontends by hand instead (development, a remote server), see
[tui.md](tui.md#start-it) and [tui-web.md](tui-web.md#usage).

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

**Which agent runs.** The new root Session's Agent id is resolved the same way
`serve` resolves one: config `default_agent` (see
[Configuration](configuration.md#agent-selection)) when set, otherwise the
built-in `build` agent. There is no per-invocation `--agent` flag yet. An
unresolvable `default_agent` (not a selectable agent id in the bound catalog)
fails the run with a clear `UnknownAgentId` error instead of silently falling
back to `build`. `hya run`, `hya -p` goal mode, `hya loop`, `hya rpc`, and
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
stop handling applies to `hya run`, `hya -p`, and `hya loop`; `hya rpc` and
`hya workflow` drain at their normal end. Example:

```sh
hya --db run.db exec --json "long task" & pid=$!
sleep 5; kill -INT $pid; wait $pid   # exit 130
hya --db run.db tail-session <session> | tail -1
# {"seq":12,…,"event":{"type":"message_finished",…,"finish":"cancelled","cause":"user_cancel"}}
```

## `hya run`

```sh
hya run "summarize this repo"
hya run --format json "summarize this repo"
```

Compat-compatible alias for `exec`. Message words are joined with spaces.
Like `exec`, `run` persists only when the global `--db <PATH>` is supplied,
and resolves its root Agent the same way (config `default_agent`, else the
built-in `build` agent — see [`hya exec`](#hya-exec)).
`--format json` and `--json` both emit event JSONL.

## `hya -p`

```sh
hya -p "make the workspace compile" --max-iterations 6
```

Runs goal mode with an in-memory store. Each iteration runs an agent turn, then
an independent evaluator judges the transcript. The run stops when the evaluator
returns `met=true`, a cap is reached, or cancellation is requested. Goal mode
does not persist to the global `--db` database. The worker Agent is resolved
the same way as `hya exec`'s root Agent (config `default_agent`, else `build`
— see [`hya exec`](#hya-exec)); the independent evaluator is a separate,
unaffected model selection (`--evaluator-model` / `goal.evaluator_model`).

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
| `--db <PATH>` | SQLite path. Empty string uses an in-memory store. A file database is locked for this process (see "One server per database" below); a second `serve` on it exits **75**. |

**Readiness contract.** After the listener is bound, the process prints exactly:

```text
hya server listening on <url>
```

That string is a stability contract: harnesses, supervisors, and client SDKs
parse this exact line from merged stdout/stderr to discover the base URL. Do not
change its wording. Source: [`serve.rs`](../crates/hya-backend/src/serve.rs).

**One server per database.** With a file `--db`, `serve` takes an exclusive
lock on the database before it opens it, and publishes a discovery file once
it listens, so a TUI or bare `hya` can attach to it instead of opening the
same file a second time ([ADR-0022](adr/0022-one-writer-per-database.md)).

| File | Contract |
| --- | --- |
| `<db>.lock` | Exclusive advisory lock (`flock`), taken without waiting before the store opens and held until the process exits; the OS releases it on a crash or SIGKILL. Contents: the owner's pid. Never deleted. |
| `<db>.server.json` | Written atomically after the listener is bound: `{"url": "http://127.0.0.1:<port>", "pid": <u32>, "version": "<hya version>", "startedAt": <unix ms>}`. An unspecified bind address (`0.0.0.0`, `::`) is published as loopback. Removed on a clean shutdown (after the drain); a file left by a crash is ignored and replaced by the next owner. |

`<db>` is the `--db` path with its directory resolved (symlinks and `..`), so
different spellings of one file share one lock. An in-memory store (`--db ""`,
the default for `serve`) takes no lock. A second `serve` on a database that
another process holds fails before it starts, with exit status **75**
(`EX_TEMPFAIL`) and one line on stderr:

```text
hya serve: database /home/me/.local/state/hya/sessions.db is already in use by hya server pid 4242 at http://127.0.0.1:53211 (hya 0.41.0); connect to it (`hya-tui --server http://127.0.0.1:53211`, or run bare `hya`, which attaches), stop it, or pass another --db
```

If the holder has not published its URL yet (it is still starting, or it is
not a server), the message names the pid from `<db>.lock` instead. Clients
that attach check the discovery file with `GET <url>/v1/health`
(`{"ok": true, "version": …}`).

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
hya models [provider] [--verbose] [--refresh]
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
an alias for `auth`.

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
| `hya` | **0** on success (including the bare guidance banner, `serve` graceful signal shutdown, and `tail-session` broken-pipe). **130** / **143** when `exec`/`run`/`-p`/`loop` was stopped by SIGINT / SIGTERM (after the drain). Bare `hya` on a terminal exits with the terminal TUI's status, or `128 + signal` (130 / 143 / 129) when `hya` was stopped by SIGINT / SIGTERM / SIGHUP. | **1** with the full `anyhow` error chain printed to stderr on any error — CLI validation failures use the same path. |
