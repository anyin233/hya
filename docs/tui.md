# OpenTUI frontend

The `packages/hya-tui` frontend is the terminal client of hya. Running
`hya` in a terminal starts it together with an in-process server and the
WebUI (see [Start it](#start-it)); from a source checkout it can also start its
own `hya serve` or connect to a running one with `--server`. It uses OpenTUI for
display and input while the backend remains the owner of sessions, event
history, tool execution, and permissions. The screen is one main column (the transcript of the open
session, pending interactions, the status line, and the input) plus a
sidebar with the session list, todos, and session context that you can show
or hide (see [Layout](#layout)). Assistant replies render as Markdown with
highlighted code blocks; reasoning is collapsed to one `Thinking` line; each
tool call is a card with its state, a one-line summary, and an expandable
body; a subagent's `task` card shows the child's status and opens its session
read-only (see [Messages](#messages)). When the agent or one of its subagents
needs a permission decision or asks a question, a prompt docked above the
input shows the call and its options; press `1`, `2`, or `3` (see
[Permission and question prompts](#permission-and-question-prompts)).
Shift+Tab or `/permissions` switches the session's permission mode
(`manual`, `yolo`, or a mode an installed bundle provides); the status bar
shows the mode in effect (see [Permission modes](#permission-modes)).
Models and Workflows have dedicated views, and `/key` opens the full-screen
[Provider View](#provider-view) (providers, keys, model lists, model tests,
and model metadata); the API command view exposes the other HTTP/JSON operations
in `hya.v1`. The input is a multi-line editor with input history; it also
runs `!command` shell turns, completes `@file` references, and opens a
command menu on `/` (see [Composer](#composer)). Tab completes slash commands
using the TUI and server command catalogs. One persistent instruction line
stays below the input at the bottom of the screen and changes with the
current view. `?` on an empty input (or `/help`) lists every key and
command (see [Key help](#key-help)).

## Start it

Run `hya` in a terminal. It starts a server inside the `hya` process, the
WebUI on `http://127.0.0.1:3250` (`hya --port <N>` picks another port, `0` a
free one), and this TUI attached to the terminal. The TUI and every WebUI tab
share the same server and sessions. Quitting the TUI stops the WebUI and the
server. Bare `hya` needs Bun and finds the TUI under `lib/hya/tui` next to the
binary (a release archive or `install.sh` puts it there) or in the source
checkout it was built from. See [Bare `hya`](cli.md#bare-hya) for the lookup
order, the log file, and signals.

```sh
hya                   # TUI + WebUI on http://127.0.0.1:3250
hya --port 8000       # WebUI on another port
```

With a WebUI, the status bar shows `WebUI http://127.0.0.1:3250`, the
sidebar's `Context` box shows `WebUI    127.0.0.1:3250`, and `/status` shows a
`WebUI` row. If the WebUI could not start (for example because the port is
taken), the TUI still works: the status line shows
`WebUI unavailable: port 3250 is in use · hya --port <N>`, the status bar
shows `WebUI unavailable` in the warning color, and `/status` repeats the
reason. `/status` shows `Backend     in the hya process (bare hya)`. If
another process already serves the database, bare `hya` attaches to that
server instead of starting its own, and `/status` shows
`Backend     attached to a running server · pid <pid>` (see [Bare
`hya`](cli.md#bare-hya), "Attaching to a running server").

### Run it with Bun (development)

For development, or to attach to a server elsewhere, run the TUI directly
with Bun 1.4.2 (the version the repository pins; the Solid setup is verified
on it) in a terminal supported by OpenTUI. From a clone:

```sh
cd packages/hya-tui
bun install --frozen-lockfile
```

Then, from the repository root, one command starts the TUI and its backend:

```sh
cargo build -p hya-backend --bin hya        # once; or put a released hya on PATH
HYA_BIN=target/debug/hya bun packages/hya-tui/src/main.ts --dir "$PWD"
```

Without `--server` the TUI first looks for a server that already runs on its
database ([ADR-0022](adr/0022-one-writer-per-database.md)). It reads
`<db>.server.json` next to the database (`{url, pid, version, startedAt}`,
written by that server) and attaches when the pid is alive and
`GET <url>/v1/health` answers `ok`. It then starts nothing and stops
nothing, and `/status` shows
`Backend     attached to a running server · pid <pid> · db <db>`. So a second
TUI on the same database shares the first one's sessions and live events
(streamed turns, renames, asks) instead of writing the file as a second
server. The attached TUI depends on that server: if its owner quits, the
attached TUI loses the connection (restart it to start a new server).

Otherwise the TUI starts its own backend: it finds the `hya`
binary, runs `hya serve --bind 127.0.0.1:0 --db <db>` in `--dir` with the
TUI's environment, reads the URL from the server's readiness line
(`hya server listening on <url>`, see [`hya serve`](cli.md#hya-serve)),
connects, and stops the server when the TUI exits — Ctrl+C twice, Ctrl+D,
`/exit`, or a signal (SIGINT, SIGTERM, SIGHUP, which is also what the WebUI
host sends when its browser tab closes). Stopping sends SIGTERM (the server
drains running turns for up to 5 s) and SIGKILL after 6 s, and the TUI
waits for the process to exit, so no `hya serve` is left behind. The
server's stdout and stderr never reach the screen; `/status` shows the
started backend's pid, binary, and database
(`Backend     started by this TUI · pid <pid> · <bin> · db <db>`). If that
`hya serve` exits with status 75 because another process holds the database
(two TUIs started at the same moment, or the holder is still starting), the
TUI waits up to 20 s for the holder's discovery file and attaches. If none
appears it fails with
`database <db> is in use by another hya process that serves no reachable server`.

The binary is looked up in this order:

1. `--hya <path>`
2. the `HYA_BIN` environment variable
3. `hya` on `PATH`

A path given by `--hya` or `HYA_BIN` must exist; the TUI does not fall back
to the next source then. If no binary is found, or the server exits (or
prints no readiness line within 60 s) before it is ready, the TUI prints the
reason and the last lines of the server's output, and exits with status 1
before it takes over the terminal, for example:

```text
hya-tui: could not start the backend: hya serve exited with code 1 before it was ready
--- hya serve output (last lines) ---
Error: invalid config: ...
```

To use a backend you run yourself (another machine, a shared server, or a
custom `hya serve` command line), pass its URL; the TUI then starts nothing
and stops nothing:

```sh
cargo run --locked -p hya-backend --bin hya -- serve --bind 127.0.0.1:8080 --db "$HOME/hya-sessions.db"
bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080 --dir "$PWD"
```

| Flag | Meaning |
| --- | --- |
| `--server <url>` | Base HTTP URL of a running `hya serve`. Without it the TUI starts its own backend. |
| `--dir <path>` | Workspace directory: the TUI makes the Project that contains it active at start (see [Projects](#projects)), new sessions of that Project work in it, and it is the started backend's working directory. Default: the TUI's working directory. |
| `--hya <path>` | `hya` binary to start (first in the lookup order above). Only without `--server`. |
| `--db <path>` | SQLite database of the backend, relative to `--dir`: the TUI attaches to the server already running on it, else starts one. Default: `$XDG_STATE_HOME/hya/sessions.db`, else `~/.local/state/hya/sessions.db` — the store `hya sessions` reads, so sessions survive restarts. Only without `--server`. |
| `-c`, `--continue` | Open the most recently updated top-level session of the Project that contains `--dir`, whatever its workdir inside the Project (subagent sessions are opened from their parent). |
| `--remote` | The backend runs on another machine, so `--dir` names nothing there: start without an active Project. The first prompt or `/new` is refused until a Project is chosen; a temporary session needs none. |
| `-s`, `--session <id>` | Open that session. Cannot be combined with `--continue`. |
| `--web-url <url>` | Show this WebUI address (status bar `WebUI <url>`, sidebar `Context` row, `/status`). Bare `hya` passes it; an HTTP(S) URL. |
| `--web-error <reason>` | Show `WebUI unavailable: <reason> · hya --port <N>` in the status line and `/status`, and `WebUI unavailable` in the status bar. Bare `hya` passes it when the WebUI could not start. Cannot be combined with `--web-url`. |
| `--attached-pid <pid>` | With `--server` only: the server belongs to another process (pid) that bare `hya` attached to; `/status` shows `attached to a running server · pid <pid>`. Bare `hya` passes it. |
| `-h`, `--help` | Print the flags and the binary lookup order. |

Without `--continue` or `--session` no session is open at start; the first
prompt (or `/new`) creates one, and `/sessions` (or the sidebar) reaches the
earlier ones. Two TUIs on the same database share one server, so they see
the same sessions live; give one `--db` for a separate store. The
backend's offline echo model is sufficient for a first run; configure a
provider in the backend for live model calls.

Type a plain prompt and press Enter. The frontend creates a session when none
is open, admits the prompt as a turn, and streams the reply into the
transcript as it arrives (see [Streaming, queued prompts, and turn
status](#streaming-queued-prompts-and-turn-status)). For example, type `summarize this repository`,
then `/models` to inspect available routes, and `/open 1` to return to the
first session. Press Ctrl+C twice (or Ctrl+D on an empty input, or type
`/exit`) to exit and restore the terminal; a backend the TUI started stops
with it. Next time, `--continue` picks the conversation up again:

```sh
HYA_BIN=target/debug/hya bun packages/hya-tui/src/main.ts --dir "$PWD" --continue
```

To add a provider or set its API key, type `/key`: the full-screen
[Provider View](#provider-view) lists the providers, adds one through a short
pop-up (name, protocol, base URL, key), and fetches and tests its models.
Changes apply to the running backend at once; no restart is needed.

## Projects

A Project (ADR-0024) is a named list of root directories on the backend
machine; the first root is the primary root. Every non-temporary session
belongs to one, and the TUI always has at most one **active Project**: the
one new sessions go to and the directory scope (`x-hya-directory`, the
`directory` of VCS, MCP, rule, and agent-model calls) follows.

- **Local start.** At start the TUI calls `EnsureProjectForPath(--dir)`: the
  Project whose root contains `--dir` (the longest matching root wins), else
  a new Project named after `--dir` with `--dir` as its only root. That
  Project becomes active and the scope stays `--dir`. If the call fails (an
  older backend), no Project is active and new sessions send `--dir` alone;
  the server then places them the same way.
- **New sessions.** `/new` (and the first prompt) creates a Project session
  in the active Project: with `workdir = --dir` when `--dir` lies inside one
  of its roots, else without a workdir, so the server uses the primary root.
  A temporary session has no Project; the server creates a scratch workdir
  for it (`$XDG_CACHE_HOME/hya/scratch/<session>`), and the active Project
  stays for the next `/new`.
- **Switching.** Switching to a Project makes it active, sets the scope to
  `--dir` when it lies inside the Project, else to its primary root, and
  opens the Project's most recently updated top-level session — or creates
  one when it has none. Opening a top-level session of another Project (for
  example from `/sessions`) makes that Project active too; opening a
  temporary session scopes requests to its scratch workdir.
- **`--continue`** opens the newest top-level session of the ensured
  Project, so a session started in a subdirectory of the same Project is
  found too.
- **`--remote`.** No Project is ensured and none is active. A prompt or
  `/new` without one is refused with `No project is open · choose a project
  or start a temporary session` on the status line.
- **Live list.** The Project list (`ListProjects`, with each Project's
  `busy` flag: a session of it runs a turn) is read with the catalogs and
  re-read on every `projectsUpdated {}` frame of the global stream (live
  only, no seq, empty `session`), debounced like `catalogUpdated` (120 ms,
  at most 400 ms), so a burst is one re-read.

Interface (`src/client.ts`, `src/state/projects.ts`, `src/app/controller.ts`):

| Call | Route |
| --- | --- |
| `listProjects()` | `GET /v1/projects` (all pages) |
| `getProject(id)` | `GET /v1/projects/{id}` |
| `createProject({name, roots})` | `POST /v1/projects` |
| `updateProject(id, {name?, roots?})` | `PATCH /v1/projects/{id}` |
| `deleteProject(id)` | `DELETE /v1/projects/{id}` |
| `resolveProject(path)` | `GET /v1/projects/resolve?path=` (`undefined` when no Project contains it) |
| `ensureProjectForPath(path)` | `POST /v1/projects/ensure` `{path}` → `{project, created}` |
| `listSessions({projectId?})` | `GET /v1/sessions?projectId=` |
| `createSession(agent, model, placement)` | `POST /v1/sessions` with `kind` `SESSION_KIND_PROJECT` plus `projectId`/`workdir`, or `SESSION_KIND_TEMPORARY` alone |
| `setDirectory(path)` | changes the scope of every later request and stream |

Store fields: `projects` (`ProjectInfo[]`), `activeProjectId`, `remote`.
Controller actions: `newSession(agent?, model?)`,
`newTemporarySession(agent?, model?)`, `switchProject(id)`,
`refreshProjects()`.

## Commands and keys

| Input | Effect |
| --- | --- |
| Plain text + Enter | Admit a prompt in the current session; create one if needed. |
| Ctrl+J, Alt+Enter, Shift+Enter | Insert a newline instead of sending (Shift+Enter only where the terminal reports it; see [Composer](#composer)). |
| Up / Down | On the input's first / last line: the previous / next submitted input. |
| `!<command>` + Enter | Run the command as a shell turn in the current session (see [Shell turns](#shell-turns)). |
| `@<text>` | Show matching file paths; Up/Down select, Tab or Enter inserts `@<path>`, Esc closes (see [File references](#file-references)). |
| `/` at the start of the input | Open the command menu; fuzzy-filters as you type the name (see [Command menu](#command-menu)). |
| `1` `2` `3`, Up/Down + Enter | With a permission prompt shown and an empty input: Allow once, Always allow, Deny. On a question prompt the digits pick its options (see [Permission and question prompts](#permission-and-question-prompts)). |
| Esc | Close the command menu or the file list; else, with vim mode on and the input in insert mode, switch to normal mode (see [Vim mode](#vim-mode)); else, with a prompt shown and an empty input, deny the permission / reject the question; else, in a subagent's read-only view, return to the parent session; else cancel the running turn; else clear the input. |
| Ctrl+C | Clear the input and show `Press Ctrl+C again to quit`; a second Ctrl+C within 2 s quits. |
| Ctrl+D | Quit when the input is empty (otherwise delete the character under the cursor). |
| `/exit`, `/quit` | Quit. |
| `/new [agent] [model]` | Create a session in the active Project (in `--dir` when it lies inside the Project, else in its primary root), using the first visible agent and its model by default. |
| `/sessions` | Open the sessions picker: a `New session` row, then every session (subagent sessions nested under their parent); Enter opens, F2 renames, Ctrl+D deletes with confirmation (see [Pickers](#pickers)). |
| `/open <id or number>` | Switch sessions directly. Numbers count in the sidebar's order (subagent sessions under their parent). Opening a subagent's session shows it read-only (see [Subagents](#subagents)). |
| `/models`, `/model [provider/model]` | View catalog, or open the model picker (rows tagged by provider); `/model <provider/model>` switches directly. With no session yet, a picker or direct choice is remembered for the next one (see [Pickers](#pickers)). |
| `/agent [name]` | Open the agent picker (visible agents, tagged with their default model); `/agent <name>` switches directly. With no session yet, the choice is remembered for the next one. |
| `/rename <title>` | Rename the current session (`UpdateSession`); see also the sessions picker's F2 (see [Session titles](#session-titles)). |
| `/permissions [mode]` | Open the permission mode picker, or with a mode id switch to it directly (see [Permission modes](#permission-modes)). |
| Shift+Tab | Switch to the next permission mode: `manual` → `yolo` → bundle modes → `manual`. Switching to `yolo` asks for a confirmation the first time. In an open list (the command menu, the file list, a picker) it moves the highlight up instead. |
| `/key` | Open the full-screen [Provider View](#provider-view): list providers, add one, set or remove a key, fetch a provider's models, test a model, add a model or edit its metadata. No arguments. |
| `/diff` | Open the full-screen [Diff view](#diff-view): the working tree diff, split per file. |
| `/mcp` | Open the full-screen [MCP servers](#mcp-servers) view: server status, tools, connect/disconnect, login. |
| `/rules` | Open the full-screen [Saved Rules](#saved-rules) view: saved permission decisions, delete. |
| `/agent-models` | Open the full-screen [Agent Models](#agent-models) view: per-agent default model, pick or clear. |
| `/workflows`, `/workflow select <name>`, `/workflow run [name]` | View sources and selected state; select or start a Workflow in the selected session. |
| `/interactions` | View pending permissions and questions. |
| `/approve <id>`, `/deny <id>` | Respond to a permission request for this run only (`persist: false`); the keyboard fallback of the prompt, which shows the id. |
| `/answer <id> <text>` | Answer a question request. |
| `/cancel` or Esc | Cancel the running turn: the status line shows `Cancelling…`, then `Cancelled · Ready`. |
| `/refresh` or Ctrl+R | Reload sessions, messages, interactions, models, Workflows, and the command catalog (commands and skills). |
| `/sidebar [on\|off]` or Ctrl+B | Show or hide the sidebar. Without an argument it toggles what is visible now. |
| `/thinking [on\|off]` or Ctrl+O | Expand or collapse every reasoning (`Thinking`) block. |
| `/tools [on\|off]` or Ctrl+G | Expand or collapse every tool call card (see [Tool calls](#tool-calls)). |
| `/theme` | Pick the color theme: moving the highlight previews it, Enter keeps it and saves it to the preferences file, Esc restores the previous one (see [Themes](#themes)). |
| `/copy` | Copy the last assistant reply's text to the clipboard with OSC 52; the status line says `Copied N chars` (see [Copy](#copy)). |
| Mouse drag over text | Select it (theme selection color); on release it is copied with OSC 52 (see [Copy](#copy)). |
| `/editor`, Ctrl+X Ctrl+E | Edit the input in `$VISUAL` / `$EDITOR` (fallback `vi`); the edited text comes back into the input, unsent (see [External editor](#external-editor)). |
| `/vim [on\|off]` | Turn vim mode in the input on or off, saved in the preferences file; `-- INSERT --` / `-- NORMAL --` on the status bar (see [Vim mode](#vim-mode)). |
| `/notifications [on\|off]` | Turn desktop notifications on or off, saved in the preferences file (see [Desktop notifications](#desktop-notifications)). |
| `/compact` | Compact the session's context now (`CompactSession`); the status line shows `Compacting…`, then `Compacted · <strategy>`. |
| `/summarize` | Summarize the session into a new message (`SummarizeSession`). |
| `/undo` | Revert the last prompt: it and every later message leave the transcript, the files its tools changed are restored, and the prompt goes back into an empty input. Again = one prompt further back (see [Undo, redo, and fork](#undo-redo-and-fork)). |
| `/redo`, Ctrl+X R | Undo the pending `/undo` (messages and files come back); only until the next prompt, which makes the revert permanent. Ctrl+X U is `/undo` and Ctrl+X F is `/fork`; the chord works whatever the input holds. |
| `/fork` | Pick where to fork the session (the latest message, or before one of its prompts); Enter creates the fork, switches to it, and puts the picked prompt in the input. |
| `/todos` | Show the session's todo list (`GetSessionTodo`) in the main panel. |
| `/status` | Show the server URL, backend version, directory, session (and `Forked from <title>` for a fork), agent, model, permission mode, and the backend (started by this TUI with its pid, binary, and database, in the `hya` process under bare `hya`, or external with `--server`); under bare `hya` also the WebUI address or why it is unavailable. |
| `/init`, `/review` | Server built-in commands from the backend command catalog, run as `CommandTurn`s. |
| `/<skill> [args]` | Run a discovered skill as a `CommandTurn` (see [Skill commands](#skill-commands)). |
| `/api` | List the HTTP operations from the generated operation catalog (`src/operations.json`, written with `docs/protocol/openapi.json` by `cargo run -p xtask -- gen-api`). |
| `/api METHOD /v1/path [JSON]` | Send a scoped HTTP/JSON request and show its JSON response. |
| `/help`, `?` | Open the key and command help overlay (`?` only on an empty input; with text it types `?`). See [Key help](#key-help). |
| Tab | Complete a slash command name (or, in the command menu, the highlighted entry) or a supported argument; repeat Tab to cycle argument matches. |
| PgUp / PgDn | Scroll the transcript one page (the view height minus two rows). |
| Ctrl+Home / Ctrl+End | Jump to the top of the transcript / to the newest line, which the view then follows again. Plain Home / End do the same while the input is empty; with text in the input they move the cursor. |
| Mouse wheel | Scroll the transcript. |
| Click on a `Thinking` line | Expand or collapse that one reasoning block. |
| Click on a tool card | Expand or collapse that one card; on a `task` card, open the subagent's session read-only. |

`/sessions` also shows the sidebar when the terminal is too narrow for it, so
the list it refreshes is on screen.

The bottom instruction row is separate from the status message above the
input. Status updates and completion suggestions can change without erasing
the next-step instruction.

Other slash commands are forwarded to the backend as `CommandTurn`s, so
custom commands and skills from the server catalog (`ListCommands`, which
already includes skills tagged `source: "skill"`) remain usable in this
frontend. Tab suggestions and the command menu also use that catalog.
Argument completion covers agents, sessions, models, Workflows, pending
interaction IDs, permission modes, and HTTP operations from the
generated OpenAPI catalog. Suggestions are refreshed with `/refresh` or
Ctrl+R, and whenever the session or directory changes.

The API command accepts `GET`, `POST`, `PUT`, `PATCH`, and `DELETE`; the optional
body must be JSON. `GET` has no body. Include query parameters directly in the
path. For example:

```text
/api GET /v1/health
/api GET /v1/sessions
/api PATCH /v1/sessions/hysec_... {"title":"Review"}
```

It only accepts paths beginning `/v1/`, so a command cannot redirect the
client to another origin. The catalog marks server-streaming operations with
`[stream]`; the one-shot API command does not consume those streams. Session
SSE is connected automatically when a session is open. PTY WebSocket sessions
need a WebSocket client; the command view can still call their JSON setup
routes. See the [protocol guide](protocol/README.md) for those frames.

## Key help

`?` on an empty input, or `/help`, opens a help overlay over the screen: a
filterable list of every key and command, one row each, grouped and tagged —
`[composer]`, `[vim]`, `[transcript]`, `[turns]`, `[prompts]`, `[modes]`,
`[pickers]`, `[providers]` (the [Provider View](#provider-view)'s keys),
`[views]`, `[app]` for keys, then every slash command tagged by where it
comes from: `[local]` (this TUI), `[server]` (the backend's command
catalog), or `[skill]`. The highlighted row's full description shows,
wrapped, under the list. Type to filter (a key such as `ctrl+b`, a command
such as `/compact`, a group such as `views`, or any word of a description);
Up/Down scroll; Esc (or Enter) closes it and the input has the focus again.
With text in the input, `?` types a question mark.

The rows are generated from the key binding tables (`keyBindings` and the
input's `composerKeyBindings` in `src/keys/bindings.ts`, the `/sessions`
picker's row actions, the Provider View's `providerKeyRows` in
`src/state/providers.ts`) and the merged command list, so the overlay cannot
list a key the TUI does not have or miss one it does; the prompt and picker
keys come from `src/commands/help.ts` next to those state machines. Keys
that only a real terminal can send are marked: Shift+Enter reads
`terminal only` because xterm.js (the WebUI) and terminals without the
kitty keyboard protocol or modifyOtherKeys send a plain Enter for it — use
Ctrl+J or Alt+Enter there.

When the TUI cannot reach its backend, the main panel shows the same key
list as plain text instead.

## Layout

```text
hya · <session> · <agent> <provider/model> · <server>   ┌─Sessions───────────┐
mode manual · …/work · ⎇ main                           │▸ 1. Review         │
                                                        │   build · ◌ waiting│
┃ your prompt                                           │                    │
                                                        └────────────────────┘
● build · fake/model                                    ┌─Todos──────────────┐
◌ bash  cargo test · awaiting approval                  │○ write tests       │
⠹ 0:07 · Running bash cargo test · Esc to interrupt     │◐ fix the bug       │
                                                        └────────────────────┘
┌─Permission─────────────────────────────────────────┐  ┌─Context────────────┐
│bash  cargo test                                    │  │Session  hysec_…    │
│asked by build                                      │  │Agent    build      │
││ $ cargo test                                      │  │Model    fake/model │
│▸ 1  Allow once                                     │  │Messages 2          │
│  2  Always allow  bash: cargo test                 │  │Dir      …/work     │
│  3  Deny                                           │  │Server   127.0.0.1:…│
│1-3 or ↑↓ Enter · Esc denies · perm_… · mode manual │  └────────────────────┘
└────────────────────────────────────────────────────┘
Connected to hya 0.41.0 · /help for commands
┌────────────────────────────────────────────────────┐
│ Message, /command, !shell, or @file                │
└────────────────────────────────────────────────────┘
Enter a prompt · /new creates a session · /help …
```

The main column holds, from top to bottom: the header line (session, agent,
model, server, in the accent color), the status bar (permission mode,
directory, git branch, a compact todo count while the sidebar is hidden,
connection state — see
[Working indicator, status bar, and todo panel](#working-indicator-status-bar-and-todo-panel)),
the transcript (or the panel of the current view: models, Workflows, keys,
API, help), the working indicator while a turn this client admitted runs,
the pending block (asks of other sessions), the permission or question
prompt, the one-line yolo confirmation while it is asked, the status line,
the bordered input, and the instruction line. The permission mode picker
(`/permissions`) is drawn over the screen near the top while it is open.

- **Sidebar.** Three titled boxes on the right: `Sessions` (the list; `▸`
  marks the open one; a subagent's session is one `↳ N. <agent>` line nested
  under its parent, `· running` while it works, `· ◌ waiting` while a
  permission or question of that session waits for an answer — opening a
  session syncs its row to the fresh `GetSession` read, so a stale `running`
  from before it was opened does not linger, and its own stream's turn-end
  frame clears it live if the turn was already running when it was opened;
  `state/store.ts` `openSession()` / `setSessionBusy()`), `Todos` (the
  live todo list — see
  [Working indicator, status bar, and todo panel](#working-indicator-status-bar-and-todo-panel)),
  and `Context` (session, agent, model, the merged transcript's message
  count, directory, server). It is 32 columns wide (at most 40% of a narrow
  terminal, at least 20). By default it follows the width: shown at 110
  columns or more, hidden below, so an 80-column terminal gets the full
  width for the transcript. Ctrl+B or `/sidebar` pins it shown or hidden at
  any width; `/sidebar on` and `/sidebar off` set it explicitly. The status
  line confirms the change (`Sidebar shown · Ctrl+B toggles`).
- **Prompt.** A pending permission request or question of the open session
  or one of its subagent sessions is a prompt box (warning-colored border)
  above the status line; see
  [Permission and question prompts](#permission-and-question-prompts).
- **Pending block.** While permission requests (`!`) or questions (`?`) of
  *other* sessions wait (sessions not in the open session's tree), a
  `Pending (N)` box appears above the prompt with up to three of them
  (`! <title> · <n>. <session> · <id>`: which session asks, by its `/open`
  number and title) and the commands that answer them; `/open <n>` goes to
  that session to answer with its prompt. They arrive live — see
  [Asks of other sessions](#asks-of-other-sessions). `/interactions` lists
  every detail. It disappears when nothing else is pending.
- **Keys and the browser.** Ctrl+B, Ctrl+O, and Ctrl+G are not reserved by
  browsers, so they also work in the WebUI (`packages/hya-tui-web`). Ctrl+B is tmux's
  default prefix; inside tmux press it twice (tmux passes the second one
  through) or use `/sidebar`. Ctrl+B would otherwise move the input cursor
  left; the Left arrow still does.
- **Focus.** The input keeps the keyboard focus. Mouse clicks (on the
  transcript, a `Thinking` line, a tool card, or the sidebar) never move it (the renderer
  runs with `autoFocus: false`).

The colors come from the theme in effect (see [Themes](#themes)). The
default `hya` theme:

| Name | Value | Used for |
| --- | --- | --- |
| `bg` | `#11151b` | Screen and transcript background. |
| `panel` | `#1c2530` | Boxes, user message blocks, code blocks, the input. |
| `fg` | `#e8edf3` | Text. |
| `muted` | `#9caab9` | Status line, instructions, `Thinking` lines, model names, queued prompts. |
| `accent` | `#73c8e8` | Header, user message bar, assistant name, headings, list markers. |
| `border` | `#405366` | Box borders and titles. |
| `error` | `#f07878` | Error notices and failed tool calls. |
| `warning` | `#e5c07b` | Length-limit and cancel notices. |

Tool cards add `toolColors.done` `#a5d6a7` (the ✓ of a finished call and an
idle or done subagent) and `diffColors`: added rows `#a5d6a7`, removed rows
`#f07878`, hunk and file headers `#82aaff`, context rows `#9caab9` (muted).
A running spinner uses `accent`, a failed call `error`, a call waiting for a
permission answer `warning`, a pending one `muted`.

Code block tokens use `syntaxColors` (keyword `#c792ea`, string `#a5d6a7`,
number `#f78c6c`, comment `#7a8a9c`, function `#82aaff`, type `#ffcb6b`,
operator `#89ddff`) and inline code `#f2a97a`.

## Themes

The TUI ships four built-in color themes, so it stays readable on light
terminals and for users who need more contrast. The choice is saved in the
TUI preferences file and used by every later start (including the TUI bare
`hya` starts). A WebUI tab runs its own TUI process, which reads the same
file when it starts.

| Name | Kind | Look |
| --- | --- | --- |
| `hya` | dark | The default: slate background, cyan accent (the palette in [Layout](#layout)). |
| `light` | light | Light background (`#f7f9fb`) with dark text (`#1f2933`), for bright terminals. |
| `contrast` | dark | Black background, white text, saturated accents. |
| `ember` | dark | Warm dark theme: brown background, amber accent. |

**Usage.** `/theme` (no arguments) opens the [picker](#pickers) with one
row per theme, `[dark]`/`[light]` tagged; `●` marks the theme in effect.
Moving the highlight (Up/Down, Tab/Shift+Tab, typing a filter) repaints the
whole screen in the highlighted theme at once — the transcript, Markdown,
highlighted code, tool cards, boxes, and the status line. Enter keeps it,
writes it to the preferences file, and shows `Theme → <label>`; Esc (or
Ctrl+C) closes the picker and restores the theme in effect when it opened,
writing nothing. If the file cannot be written, the theme still applies for
this run and the status line says `Theme → <label> · not saved: <reason>`.

```text
/theme            # ↓ previews Light, Enter keeps it
cat ~/.config/hya/tui.json
{
  "theme": "light"
}
```

### Preferences file

The TUI keeps its own settings (not the backend's `config.yaml`) in one
JSON object:

| Location (first that applies) | |
| --- | --- |
| `$HYA_TUI_CONFIG` | A full file path; overrides the default (tests, several profiles). |
| `$XDG_CONFIG_HOME/hya/tui.json` | When `XDG_CONFIG_HOME` is set and not empty. |
| `~/.config/hya/tui.json` | Otherwise. |

```ts
interface TuiPreferences {
  theme?: string          // a built-in theme name: "hya" (default), "light", "contrast", "ember"
  vim?: boolean           // vim mode in the input (/vim); default false
  notifications?: boolean // desktop notifications (/notifications); default true
}
```

- The file is read once at start, before the first frame. A missing file
  means the defaults. An unreadable file, invalid JSON, or a JSON value that
  is not an object is ignored, and the status line says
  `Ignored unreadable TUI preferences <path>`; an unknown theme name says
  `Unknown theme <name> in <path>; using hya`. A key whose value has the
  wrong type is ignored.
- A change (`/theme`'s Enter, `/vim`) merges the changed key into what is on disk —
  keys this TUI does not know are kept — and writes a temporary file in the
  same directory, then renames it over the file, so a crash never leaves a
  half-written file. The directory is created when missing.

**Interfaces for components.** `src/theme.ts` exports the palette of the
theme in effect as Solid stores: `colors` (`bg`, `panel`, `fg`, `muted`,
`accent`, `border`, `error`, `warning`, `selection` — the mouse-selection
background), `toolColors` (`done`), `diffColors`
(`add`, `remove`, `hunk`, `context`), and `syntaxColors` (`keyword`,
`string`, `number`, `comment`, `function`, `type`, `operator`,
`inlineCode`). Read them where they are used — in JSX (`fg={colors.muted}`),
a function called from JSX, a memo, or an effect — so a theme switch
repaints; a module-level copy (`const c = colors.fg`) is a snapshot that
never updates. `themeName()` is the reactive name of the theme in effect,
`currentTheme()` its `ThemeDefinition`, `setTheme(name)` switches (returns
`false` for an unknown name), `themes` lists the built-ins, and
`syntaxStylesFor(theme)` gives the Markdown/tree-sitter scope styles.
`components/Markdown.tsx` keeps one OpenTUI `SyntaxStyle` per theme and, on
a switch, sets it and rebuilds the blocks so fenced-code boxes repaint too.
A new theme is one more `ThemeDefinition` entry in `themes` with every key
of the four groups (`test/theme.test.ts` checks it).

## Working indicator, status bar, and todo panel

**Working indicator.** While a turn this client admitted runs, one muted
line sits below the transcript, above the pending block and the
permission/question prompt dock (so the dock a pending ask needs still gets
the last word before the input): a spinner, the elapsed time (`m:ss`, or
`h:mm:ss` past an hour), the current activity, an optional `Queued N`, and
`Esc to interrupt`. The activity, highest priority first:

| Activity | When |
| --- | --- |
| `Waiting for approval` / `Waiting for an answer` | A permission or question prompt of the open session's tree is pending (the same ask the prompt dock shows). |
| `Waiting for subagent <agent>` | The streaming message's last block is a `task` card whose child session is starting or running, with no ask of its own yet. |
| `Running <tool> <summary>` | The last block is a tool call still running (or its arguments still streaming); the summary is the same one-line summary as its tool card. |
| `Thinking…` | The last block is reasoning still streaming, or the message has no blocks yet (between the turn's start and its first part). |
| `Writing…` | The last block is answer text still streaming. |

While a streaming assistant message has no blocks yet, its header's `●`
marker is the spinner too, so a slow first token still shows the turn is
alive before the working line's own elapsed clock is very interesting.

**Status bar.** One muted line under the header: with
[vim mode](#vim-mode) on, first the input's mode (`-- INSERT --` muted,
`-- NORMAL --` in the accent color, followed by a half-typed command such
as `2d`); then the permission mode
(`mode <mode>`, from `SessionInfo.permissionMode`, colored per mode — see
[Permission modes](#permission-modes)), the context occupancy (`ctx 42%`),
the session's token total (`12.3k tok`), the workspace directory
(shortened, keeping the tail), the git branch (`GetVcsStatus`, refreshed
when a session opens and after a turn ends; omitted when unknown or the
directory is not a repository), the WebUI that bare `hya` serves
(`WebUI http://127.0.0.1:3250`, or `WebUI unavailable` in the warning color;
see [Start it](#start-it)), a compact todo count (`Todos <completed>/
<total>`) shown only while the sidebar is hidden (the sidebar's own `Todos`
box already lists them), and `reconnecting` (warning color) while the
session event stream is down. Segments with no data are omitted rather than
shown empty; on a narrow terminal the least essential segments (from the
end) drop first, then the whole line clips, so it always fits the terminal
width. The header line above it already carries agent, model, session, and
server, so the status bar does not repeat them.

- **`ctx N%`** is the prompt the latest provider round sent against the
  context window of the model that served it (the rule in the protocol
  guide's [Usage and context occupancy](protocol/README.md#usage-and-context-occupancy)):
  `roundUsage.input + roundUsage.cacheRead + roundUsage.cacheWrite` of the
  newest assistant message that has `roundUsage`, divided by the
  `ModelSummary.contextLimit` of its `model`; while a turn runs, the newest
  `tokensRecorded` frame with a non-empty `message` replaces it at once.
  Rounded to a whole percent; muted below 80 %, the warning color from 80 %,
  the error color from 95 %. Hidden when the model's limit is unknown
  (`contextLimit` `0` or absent — set `limit.context` for the model in the
  backend config, see [Model limits](configuration.md#model-limits)) or no
  round has reported usage.
- **`<n> tok`** is `SessionInfo.usage` summed — `input + cacheRead +
  cacheWrite + output` (everything billed for the session, title and summary
  side calls included). The open session is re-read after a `tokensRecorded`
  frame (debounced like the transcript) to keep it current. Counts are
  `uint64` decimal strings on the wire; they show as `950`, `12.3k`, `123k`,
  `1.2M`. Hidden while the total is zero or unknown.

The sidebar's `Context` box repeats both when known: `Context  42% ·
42k/100k` (prompt tokens / window) and `Tokens   42.3k`. Under bare `hya` it
ends with a `WebUI` row: the address without the scheme, or `unavailable`.

**Todo panel.** The sidebar's `Todos` box is seeded from `GetSessionTodo`
when a session opens and then kept current by the session stream: every
todo tool call that changes the list (`todo__update_status`,
`todo__update_content`, …) sends a durable `todoUpdated { items }` frame
with the whole new list, which replaces the box's rows at once (no re-read). Each item is one line, a status
glyph and its text: pending `○` (muted), in progress `◐` (accent),
completed `✓` (green), blocked `✗` (muted — the `TodoStatus` enum has no
`cancelled` status, so `blocked` takes the glyph and color that status would
otherwise use). The box shows at most 6 items, then a `+N more` row, so a
long list cannot push the `Context` box below the visible area. `/todos`
still opens the full-panel view (same glyphs) for a longer list.

## Notices

**Compaction.** A `compactionApplied { untilSeq, strategy, message,
foldedCount, manual }` frame renders as a muted transcript divider,
`── context compacted · 12 messages · manual · local summary ──`: the number
of messages folded behind the summary (omitted when the event has none),
`manual` for a client-requested `/compact` (`CompactSession`) or
`/summarize` (omitted for a compaction that fired mid-turn), and always the
strategy in words (`state/format.ts` `strategyText()`): `native` (provider-
native), `local_summarizer` → `local summary` (a model-written summary —
also every manual compaction), `snap_compact` → `snapshot` (a local dense
archive, no model call), `handoff` → `handoff` (a model-written handoff
document). The divider sits right before `message`, the system message that
holds the summary, once the transcript has it (until then, right after the
newest message); the summary follows it as a muted notice, without the
internal `HYA_COMPACTED_CONTEXT` marker line. For example, `/compact` after
a short exchange shows:

```text
── context compacted · 2 messages · manual · local summary ──

Summary: the user asked for …
```

`/compact`'s status line reads the same way: `Compacting…`, then
`Compacted · <strategy in words>` (e.g. `Compacted · local summary`).

**Compactions from before the session was opened.** Opening a session
(at start with `--session`/`--continue`, `/open`, `/sessions`, a subagent's
view) shows a divider for every compaction already in its history, at the
same place: right before each summary message. These are derived from the
transcript itself — every compaction appends its summary as a system
message starting with `HYA_COMPACTED_CONTEXT` (all five mechanisms do) —
so they cost no extra request. The strategy and folded count live only in
the durable `CompactionApplied` event, which the TUI does not replay from
the start of the log, so a history divider reads just:

```text
── context compacted ──
```

A live `compactionApplied` for a summary already shown takes that divider's
place (same position, full text) — a summary never gets two dividers.

**Engine system messages.** A message with the system role (for example a
`TEAM QUIESCED …` coordination notice) renders as a muted notice line, not
an assistant header block — no `●`, no agent or model.

**Connection and version.** A lost stream connection shows `Stream
reconnecting: <error>` in the status line while it retries (also reflected
in the status bar's `reconnecting`); a version mismatch between this TUI and
the backend's bootstrap version appends `backend <version> ≠ tui <version>`
to the initial `Connected to hya …` status.

## Messages

Each message in the transcript is drawn by role:

- **User** prompts are panel-colored blocks with a heavy accent bar (`┃`) on
  the left. A queued prompt (see below) uses a muted bar and text and a
  `queued` tag on its right.
- **Assistant** messages start with a header, `● <agent> · <provider/model>`:
  the agent name in the accent color, the model muted. The v1 server does not
  fill `MessageInfo.agent` and `.model` yet, so the header shows the open
  session's agent and model. Then come the parts, in order, and at most one
  notice.

| Part or finish | Shown as |
| --- | --- |
| Text | Markdown (below). |
| Reasoning | One muted line, `▸ Thinking · N words` (`Thinking…` while it is the part still streaming). Expanded: `▾ Thinking · N words`, then the text in muted italics beside a bar. |
| Tool call | A card: `<icon> <tool>  <summary>` and the duration on the right, collapsed by default. See [Tool calls](#tool-calls). A `task` call is a subagent card; see [Subagents](#subagents). |
| Attachment | `↳ attachment · <name>`. |
| `FINISH_REASON_STOP`, `FINISH_REASON_TOOL_CALLS` | Nothing: a normal finish is not noteworthy. |
| `FINISH_REASON_LENGTH` | `! Reply stopped at the output length limit` (warning color). |
| `FINISH_REASON_CANCELLED` | `! Cancelled` (warning color). |
| `FINISH_REASON_ERROR` or an `error` on the message | `✗ <code>: <message>` (error color), for example `✗ provider_error: http status 400: bad request`; `✗ Turn failed` when no error text was recorded. The error shows as soon as `errorReported` arrives. |

**Markdown.** Assistant text is rendered by OpenTUI's built-in `<markdown>`
renderable (`@opentui/core` 0.5.12): headings (accent, bold, `#` hidden),
**bold**, *italic*, strikethrough, `inline code`, links (the label followed by
the URL in parentheses, since terminals may not support hyperlinks), bullet
and numbered lists with nested indentation, task lists, block quotes (a bar on
the left), tables, horizontal rules, and fenced code blocks. A fenced block is
a panel-colored box with the language name on its first row; its tokens are
highlighted by tree-sitter in OpenTUI's parser worker. Highlighting covers the
grammars bundled with `@opentui/core`: TypeScript, JavaScript (and their JSX
variants), Markdown, and Zig. Other languages render as plain text on the
panel color. Nothing is downloaded at run time.

While a reply streams, the renderer keeps its last blocks provisional, so an
unclosed code fence shows its lines as code so far and an unclosed `**` shows
as plain text until it closes. When the reply finishes, its final text is
parsed again from the start.

**Reasoning.** Reasoning parts arrive as `reasoning` parts (durable deltas;
see the protocol guide). They are collapsed by default. Ctrl+O or `/thinking`
expands or collapses all of them (and forgets per-block choices); a click on
one `Thinking` line toggles just that block. The word count is the reasoning
text split on white space. Only provider routes that stream reasoning produce
these parts (for example `openai-response`; the `openai-compatible` decoder
ignores reasoning).

**Scrolling.** The transcript follows the newest line while you are at the
bottom. Scroll up (PgUp, the mouse wheel, Ctrl+Home) and it stays where you
left it; when more content arrives below, a `↓ New messages below · End
jumps` hint appears at the bottom right. End (with an empty input), Ctrl+End,
or scrolling back to the bottom clears the hint and resumes following.
Submitting a prompt jumps to the bottom. Opening a session starts at its
bottom. The transcript shows the newest 200 messages.

### Tool calls

Every tool call of an assistant message is a card. The header is one line:

```text
✓ read  src/main.rs · lines 1-40 of 212                          3ms
⠹ bash  cargo test -p hya-core
◌ bash  rm -rf target · awaiting approval
✗ read  missing.txt
  File not found: /work/missing.txt
```

- **State icon.** `○` pending (the model is still streaming the
  arguments), a spinner (`⠋⠙⠹…`, accent) while it runs, `◌` (warning color)
  while a permission request for this call waits (its interaction's
  `payload.callId` is the card's call id), `✓` (green) done, `✗` (error
  color) failed.
- **Tool name** in bold, then a **summary** (muted) that depends on the tool
  (below), clipped to the width, and the **duration** on the right once the
  call is done (`42ms`, `1.5s`, `12s`, `1m 5s`).
- A failed call adds its error message on the next line in the error color,
  collapsed or not.

**Expanding.** Cards are collapsed by default; expanded, the body shows under
the header beside a bar. Ctrl+G or `/tools` expands or collapses all of them
(`/tools on`, `/tools off`; with no argument it toggles) and forgets
per-card choices, like `/thinking`. A click on one card toggles just that
card; the input keeps the focus. The cards of a `!command` shell turn start
expanded, so you see the output you asked for. A body longer than 12 lines
keeps its first 5 and last 6 lines around a `… N lines hidden` row.

| Tool (canonical name) | Summary | Expanded body |
| --- | --- | --- |
| `bash` (hidden alias `shell`) | The command (first line), then `· exit N` for a non-zero exit and `· timed out` | `$ <command>`, the output (muted), the exit status (error color) |
| `read` | `<path> · lines A-B of N` (from the output's display metadata; before that, from `offset` / `limit`) | The text with line numbers |
| `edit` | `<path> · +A -D` | The diff: the output's `metadata.diff` (unified diff), else rows derived from the arguments (`edits[].oldText`/`newText`, `lines`; compat `oldString`/`newString`) |
| `write` | `<path> · N lines` | The content, every row an addition |
| `apply_patch` (alias `patch`) | The files, `· +A -D` | The patch envelope: file headers, `@@` hunks, `+`/`-`/context rows |
| `grep` | `"<pattern>" in <path> (<glob>) · N matches` | `file:line: text` per match |
| `glob`, `find` | `<pattern> in <path> · N files` | The paths |
| `ls` | `<path> · N entries` | The listing |
| `lsp` | `<operation> <file>:<line>:<character>` | The output |
| `todo__read`, `todo__update_status`, `todo__update_content` (and older `todo*`) | `N todos · D done` | The list, `☐` pending, `▸` in progress, `!` blocked, `✓` completed |
| `webfetch` (alias `fetch`) | The URL | The output |
| `websearch` (alias `search`) | `"<query>"` | The output |
| `skill` | The skill name | — |
| `ask_user` (alias `question`) | `<header>: <question>` of the first question | The answers |
| `task` | `<subagent_type> · <description>` | A subagent card (below) |
| anything else (MCP `server__tool`, plugin tools) | The arguments as compact JSON | The output text |

Diff rows are colored: `+` added (green), `-` removed (red), hunk and file
headers blue, context muted. While a call's arguments still stream (state
`PENDING`, `inputJson` not complete), the summary reads the main string field
(`command`, `path`, `pattern`, `url`, `query`, …) out of the partial JSON.

Cards appear and update as the stream frames arrive, before the projection
is re-read (see [Stream frames and the transcript](#stream-frames-and-the-transcript)).

### Subagents

A `task` call spawns a subagent in its own child session. Its card shows the
child's status and what it last did, and it always shows these lines:

```text
✓ task  general · survey the repo                                  7ms
│ ⠹ running  ↳ read notes.txt · lines 1-1 of 1
│ click to view · /open hysec_…
```

- **Link.** The card finds its member (`MemberInfo`) by the tool call's
  `callId` (`memberUpdated.callId`), else by the child session in the task
  output (`outputJson.metadata.sessionId`). Resident spawns record no call
  id, so the second rule is the one that usually applies.
- **Status.** A finished member status wins (`✓ done`, `✗ failed`,
  `! cancelled`). Otherwise the child session's `busy` flag says `running`
  (spinner) or `idle` (`✓`, its turn ended; a resident subagent waits for
  mail). Before anything is known it is `○ starting`. `✗ failed` also shows
  when the child's newest reply failed.
- **Waiting.** While the child session has a pending permission request
  (question), the status reads `◌ waiting for approval` (`◌ waiting for an
  answer`) in the warning color, and the parent view shows the ask as a
  prompt labelled with the subagent (see
  [Subagent asks](#subagent-asks)).
- **Latest activity** after `↳`: the member's finish `summary` when it has
  one, else the child's newest tool call (`<tool> <summary>`) or the first
  line of its newest text.
- **Source.** The TUI reads each child of the open session (its members and
  the children named by `task` outputs) with `GET /v1/sessions/{child}`
  (`busy`, `agent`) and `GET /v1/sessions/{child}/messages` (activity). It
  reads them after every projection read and `memberUpdated` frame, at most
  once per 1.5 s, and repeats every 1.5 s while a child is busy or this
  client's turn runs. It does not subscribe to child streams. The same round
  re-reads the session list, so the sidebar's nesting and `· running` stay
  current, and the pending interactions (`GET /v1/interactions`), so a
  subagent's ask reaches the parent's prompt within one round.

**Child view.** A click on the task card, `/open <child session id>`, or
`/open <number>` of its sidebar row opens the child session read-only: a
`Viewing subagent <agent> · Esc returns · read-only` banner sits above its
transcript, and the input's placeholder and the footer say so. Enter on a
prompt or `!command` keeps the text and shows
`Read-only: this is a subagent's session · Esc returns to the parent`;
slash commands still run. Esc, when no list is open, opens the parent session
again (status `Back to the parent session`); the text you typed stays, and a
second Esc clears it. Opening another session this way resets the parent's
overlay and prompt queue like any session switch; the parent's turn keeps
running on the server, and its transcript is re-read on return.

## Streaming, queued prompts, and turn status

The assistant reply appears chunk by chunk while the model streams it. When
the reply is complete, the transcript shows the server's stored copy of it;
the text does not repeat or flicker when that happens.

You can type the next prompt while a turn is running. Press Enter and the
prompt appears dimmed at the end of the transcript, tagged `queued`. The working line counts the waiting prompts
(`Queued 1`). When the running turn ends, the frontend sends
the oldest queued prompt; several queued prompts go one per turn, in the
order you typed them. The server has no prompt queue of its own. It rejects
a prompt with `409 session_busy` while a turn runs, and it releases the
session shortly after the reply finishes. So the frontend retries a busy
prompt a few times with a short backoff (about 100 ms growing to 1 s). If the
session is still busy after that (for example, another client started a
turn), the prompt stays queued and is sent after the next turn end seen on the
stream. Opening another session drops the queued prompts of the previous one.
A queued prompt is still sent after a cancelled or failed turn.

The status line above the input shows the turn state. While the
[working line](#working-indicator-status-bar-and-todo-panel) shows a
running turn, the progress texts that repeat it (`Sending prompt…`,
`Running · <turn id>…`, `Running shell · …`, `Queued · N waiting`) are left
out of the status line, which stays empty until another message (a command
result, an error, `Ready`) arrives:

| Status | Meaning |
| --- | --- |
| `Sending prompt…` | The prompt is being admitted (`CreateTurn` in flight). |
| `Session busy · retrying (N)` | The server answered `409 session_busy`; the prompt is retried. |
| `Running · <turn id>[ · N queued]` | The turn runs; `N` prompts wait. Hidden while the working line shows. |
| `Session busy · N queued prompt(s) wait(s) for the running turn` | Retries ran out; the prompts wait for the next turn end. |
| `Ready` | The turn finished. `Ready · reply stopped at the length limit` when the model hit its output limit. |
| `Cancelling…` | Esc or `/cancel` sent `CancelTurn`; the turn has not ended yet. |
| `Cancelled · Ready` | The turn was cancelled (Esc or `/cancel`). |
| `Running shell · <command>` | A `!command` shell turn runs. |
| `Press Ctrl+C again to quit` | The first Ctrl+C; it goes back to the previous status after 2 s. |
| `Error · <code>: <message>` | The turn failed, for example `Error · provider_error: http status 400: …`. `Error · turn failed` when the backend recorded no error text. |

A failed assistant message also shows its error in the transcript, as a line
under its header, in the error color:

```text
● build · openai/gpt-5
✗ provider_error: http status 400: bad request
```

## Composer

The input at the bottom of the main column is a multi-line editor (OpenTUI's
built-in `<textarea>`). It keeps the keyboard focus. Its placeholder is
`Message, /command, !shell, or @file`.

**Writing.** Enter sends the whole input: a prompt, a `/command`, or a
`!command`. Ctrl+J inserts a newline in every terminal and in the WebUI;
Alt+Enter does too. Shift+Enter inserts a newline only in terminals that
report it as a separate key (the kitty keyboard protocol, which OpenTUI
requests at startup, or modifyOtherKeys). xterm.js, and so the
WebUI, sends a plain Enter for Shift+Enter, so there it sends the input. A
bracketed paste inserts its text, line breaks included, and never sends it.
The box grows with its content up to 8 rows (wrapped lines count), then
scrolls. Newlines stay in the prompt text, so the transcript shows the lines
as typed. Editing keys: Left/Right, Up/Down between lines, Home/End to the
start/end of the current line, Ctrl+Left/Right or Alt+Left/Right by word,
Ctrl+A / Ctrl+E to the start/end of the logical line, Backspace, Delete,
Alt+Backspace deletes the previous word (Ctrl+W too, outside a browser, which
reserves it), Ctrl+U / Ctrl+K delete to the line start/end, Ctrl+- undo.

**History.** Every sent input (prompts, `!commands`, `/commands`) is kept for
the life of the TUI process, up to 200 entries; it is not saved to disk.
Up on the first line of the input shows the previous entry; Down on the last
line shows the next one, and past the newest entry it restores what you were
typing before. Any edit ends history navigation. Repeated sends of the same
input are stored once.

**Esc.** Esc closes the file list if it is open. With a permission or
question prompt shown and an empty input, it then denies the permission or
rejects the question (see
[Permission and question prompts](#permission-and-question-prompts)). In a
subagent's read-only view it then returns to the parent session (see
[Subagents](#subagents)).
Otherwise, while a turn
admitted by this TUI runs, it cancels that turn (like `/cancel`): the status
shows `Cancelling…`, then `Cancelled · Ready`, and the transcript shows
`! Cancelled`. Text you typed meanwhile stays. With no turn running, Esc
clears the input. With [vim mode](#vim-mode) on, Esc in insert mode first
switches to normal mode (after closing an open list) and does nothing else;
Esc in normal mode has the meanings above.

**Quitting.** The renderer does not quit on Ctrl+C by itself. The first
Ctrl+C clears the input (on an empty input it only arms) and shows
`Press Ctrl+C again to quit`; a second Ctrl+C within 2 s quits. Any other key
in between disarms it. Ctrl+D on an empty input quits; with text it deletes
the character under the cursor. `/exit` and `/quit` quit. Quitting destroys
the renderer, which restores the terminal, and exits with code 0.

Example:

```text
explain these two functions:          ← Ctrl+J
- parse_args                          ← Ctrl+J
- run                                 ← Enter sends all three lines
```

### Shell turns

An input that starts with `!` runs the rest of the line as a shell command in
the open session (a session is created first if none is open). The input box
shows the shell mode while you type: its border turns the warning color and
its title reads `! shell`. The command goes through the prompt queue like a
prompt, so it waits while a turn runs.

The backend runs it as a `ShellTurn`: its builtin `bash` tool runs the command
in the session's working directory, with no model round, under the session's
agent. You typed the command, so it never asks: it runs without a permission
prompt in every permission mode (`manual`, `yolo`, and bundle modes; a bundle
mode's approver is not consulted). An explicit deny rule still blocks it
(the card shows the error), and a plugin's `tool.execute.before` hook can
still veto it. Commands the model runs through `bash` are unchanged: they ask
as the mode says.
`CreateTurn` returns only when the command has finished; meanwhile the status
reads `Running shell · <command>`.

The backend records the turn as two messages: a user message with the fixed
text `The following tool was executed by the user`, and an assistant message
with one `bash` tool call. The transcript shows the user message as
`!<command>` and the tool call as a `bash` card (see
[Tool calls](#tool-calls)) that starts expanded:

```text
┃ !echo hello

● build · openai/gpt-5
◌ bash  echo hello · awaiting approval
```

and, once approved and finished:

```text
✓ bash  echo hello                                               4ms
│ $ echo hello
│ hello
```

The command comes from this TUI's own shell turns (before the part carries
its input), or from the tool call's `inputJson` (`{"command": …}`); the
output is the tool call's `outputJson`. Esc cancels a running shell command;
the turn then reads `Cancelled · Ready`.

### File references

Type `@` and at least one character (at the start of the input or after a
space) to see up to 8 files and directories under `--dir` whose relative
path contains the text. The list is a `Files` box above the input; the
selected row is marked `▸` in the accent color. Up/Down move the selection;
Tab or Enter replaces the `@text` token with `@<relative path>` and a space;
Esc closes the list until you edit the token again. The lookup runs 120 ms
after the last keystroke. Matching is case-sensitive (the server's glob), and
the best matches come first: file name starts with the text, then file name
contains it, then only the path does; shorter paths first. Slash-command lines
(`/…`) have no file references.

The reference is plain text: the prompt carries `@src/main.rs` as typed. For
most files nothing else happens — the agent reads the file with its tools if
it needs it. A reference to an image file (`.png`, `.jpg`/`.jpeg`, `.gif`,
`.webp`) is different: see [Attachments](#attachments) below.

### Attachments

An `@path` reference to an image file — typed, completed from the `@file`
list, or turned from a pasted path (see below) — is read from disk (relative
to the session's workdir, else `--dir`) and sent as an image attachment
alongside the prompt text (`PromptTurn.attachments`,
[Prompt attachments](protocol/README.md#prompt-attachments-images)); the
`@path` text itself stays in the prompt unchanged, since it is useful context
for the model and the server never reads it back from disk. Referencing the
same image twice sends it once.

Pasting a path to an image file — a terminal pastes a file dragged into the
window as its path, quoted or with escaped spaces when it has spaces — inserts
an `@path ` mention instead of the raw text, so it is treated exactly like a
typed reference; an ordinary text paste (prose, a path to a non-image file, a
path that does not exist) is inserted as-is.

Before you press Enter, every pending image reference in the input shows as
its own row above the composer: `[image] shot.png · 240 KB`, or `[image]
shot.png · <reason>` in the warning color when it fails validation — not a
supported type, larger than 10 MiB, the turn's attachments together over
20 MiB, the file cannot be read, or the current model's `imageInput` is
`false` ([Providers and keys](protocol/README.md#providers-and-keys),
`ModelSummary.imageInput`; absent means unknown and is allowed). Enter refuses
to send while any row has an error — the status line names the file and the
reason, and nothing is sent — so a bad reference never reaches the server.

Once sent, the user message's transcript row shows each attachment under the
text: `↳ attachment · shot.png · image/png · 240 KB` (an `AttachmentPart`
listing never carries the bytes; size is shown once the server records it,
either in the initial response or right after, via a live `partsAdded` stream
frame appended to the message).

### Command menu

Typing `/` at the start of the input (before any other character) opens a
`Commands` box above it, the same overlay position and key handling as the
`@file` list. Each row shows the name, its argument hint, its description
(truncated to width), and its source in brackets: `[local]` (this TUI's own
registry), `[command]` (a custom or built-in server command, `/init` and
`/review` among them), or `[skill]` (a discovered skill — see
[Skill commands](#skill-commands) below). The list is fuzzy-filtered as you
keep typing the name: an exact match ranks first, then a prefix match, then a
substring match, then any name whose letters appear in order (a subsequence
match); ties break alphabetically. Up/Down move the highlight; Esc closes the
menu and keeps the typed text.

Tab always completes the highlighted name and a trailing space, so you keep
typing its arguments. Enter's behavior depends on the highlighted command's
argument hint: with no hint, or one written `[in brackets]` (an optional
argument, for example `/new [agent] [model]` or `/sidebar [on|off]`), Enter
runs the command as is. Any other hint (`/open <id|number>`, `/key
set|remove <provider>`) names a required first argument, so Enter behaves
like Tab: it completes the name and waits for you to type the argument.

Local and backend (command or skill) names are merged and deduplicated by
name; a local name always wins a clash with a backend name (the registry
looks up local commands before falling back to the backend, so a local
command is what actually runs either way). The list refreshes with
`/refresh`/Ctrl+R and whenever the session or directory changes, the same as
Tab completion.

### Skill commands

A discovered skill runs as `/<skill> [args]`, the same as any other backend
command: the TUI sends `{command: {command, arguments}}` (`CommandTurn`);
the backend catalog (`crates/hya-server/src/support/command_catalog.rs`)
resolves the name against custom commands and skills together, expands the
skill's template with the arguments (`$1`, `$ARGUMENTS`) server-side, and
runs the result as a normal prompt turn. The transcript shows what you typed,
`/<skill> args`, in place of the backend's expanded prompt text — the same
idea as a `!command` shell turn showing `!<command>` (see
[Shell turns](#shell-turns)) — then the agent's streamed reply as usual.
`CreateTurn` returns the user message id as the turn id for a command turn
(unlike a shell turn), so the TUI remembers the typed `/name args` by that id
(`state/store.ts` `commandDisplay`, `state/messages.ts`
`commandUserView`) and shows it once the projection carries that message.

### Copy

The TUI copies text to the system clipboard with an OSC 52 escape sequence
(`ESC ] 52 ; c ; <base64> BEL`): it writes the text into its own output and
the terminal puts it on the clipboard. That works in the WebUI (xterm.js)
and in local terminals that allow OSC 52 (kitty, WezTerm, iTerm2 with
"Applications in terminal may access clipboard", tmux with
`set-clipboard on`), also over SSH, since the sequence travels with the
output. A terminal that ignores OSC 52 copies nothing.

**Usage.**

- **Mouse selection.** Drag over text in the transcript (any text on the
  screen is selectable): the selected cells get the theme's `selection`
  background while the text keeps its color. On release the selected text
  is copied and the status line says `Copied N chars`. A plain click selects
  nothing and copies nothing (clicks on `Thinking` lines and tool cards keep
  toggling them).
- **`/copy`** copies the text of the newest assistant reply that has text
  (its text blocks joined by a blank line; reasoning and tool calls are left
  out) and says `Copied N chars`; with no reply yet it says
  `Nothing to copy: no assistant reply yet`.

When the renderer knows the terminal refuses OSC 52 (its capability probe
says so), nothing is sent and the status line says
`Copy failed: this terminal does not accept OSC 52 clipboard writes`.

**Interfaces.** `AppActions.copyText(text): boolean` (commands/registry.ts)
calls `CliRenderer.copyToClipboardOSC52(text)` (clipboard target `c`);
`composer/clipboard.ts` `copyNotice(text, sent)` is the status text (`N`
counts code points). The selection handler is `useSelectionHandler` in
`app/App.tsx` (OpenTUI emits `selection` when a drag ends);
`components/selection.ts` `paintSelection(root, color)` sets `selectionBg`
on every text renderable (Markdown and code blocks included) on each left
mouse press. The browser specs observe the sequence by registering an
xterm.js OSC 52 handler on the page hook (`window.hyaTerm.term.parser`,
see [tui-web.md](tui-web.md#page-test-hook-windowhyaterm)).

### External editor

For long prompts, the input can be edited in your own editor.

**Usage.** Ctrl+X then Ctrl+E (or Ctrl+X then E; the readline/zsh chord,
browser-safe), or `/editor`. After Ctrl+X the status line shows
`Ctrl+X · Ctrl+E opens the external editor · U undo · R redo · F fork`
(see [Undo, redo, and fork](#undo-redo-and-fork)); any other next key drops the
chord and is handled as usual. The TUI writes the input to a temporary file
(`$TMPDIR/hya-prompt-XXXXXX/prompt.md`), suspends its renderer (the editor
gets the whole terminal; the TUI's screen comes back afterwards), and runs
the editor on it. When the editor exits with status 0 the file's text
replaces the input — it is **not** sent; press Enter to send it — and the
status line says `Edited in the external editor · Enter sends`. One trailing
newline the editor adds is dropped. The temporary directory is removed.

```sh
EDITOR="code -w" hya          # VS Code; -w waits for the tab to close
VISUAL=nvim hya-tui ...       # VISUAL wins over EDITOR
```

| Failure | Status line | Input |
| --- | --- | --- |
| The editor exits non-zero | `Editor <name> exited with status N · input unchanged` | kept |
| The binary is not found | `Editor <name> not found · input unchanged` | kept |

**Interfaces.**

| Environment variable | Meaning |
| --- | --- |
| `VISUAL` | Editor command, used first. |
| `EDITOR` | Used when `VISUAL` is unset or blank. |
| (neither) | `vi`. |

The value is split like a shell word list — blanks separate words, `'…'`
and `"…"` quote, a backslash escapes — but not run through a shell, and the
file path is appended as the last argument (`code -w /tmp/…/prompt.md`).
`composer/editor.ts` exports `splitCommand`, `editorCommand(env)`, and
`editText(text, { env, suspend, resume, spawn? })`, which never throws and
returns `{ ok: true, text }` or `{ ok: false, error }`; the controller's
`openEditor()` (`AppActions.openEditor`) runs it with
`CliRenderer.suspend()` / `resume()` and the composer's registered input
(`controller.attachComposer`). The key is the `externalEditor` binding (the
second key of the `chord` binding Ctrl+X) in `keys/bindings.ts`.

### Vim mode

Vim-style modal editing for the input, for people whose fingers expect it.
Off by default.

**Usage.** `/vim` toggles it (`/vim on`, `/vim off` set it); the choice is
saved as `vim` in the [preferences file](#preferences-file) and applies to
every later start. When on, the status bar starts with `-- INSERT --`
(muted) or `-- NORMAL --` (accent color), plus a half-typed command
(`-- NORMAL -- 2d`); the cursor is a bar in insert mode and a block in
normal mode. The input starts in insert mode, where every key works as
usual. Esc switches to normal mode (the cursor steps back onto the last
character, as in vim). After a send, the next input starts in insert mode
again.

Normal mode (a count before a motion or command repeats it: `3w`, `2dd`,
`d2w`; `3G` goes to line 3):

| Keys | Action |
| --- | --- |
| `h` `j` `k` `l` (Backspace = `h`) | Left, down, up, right; `j`/`k` keep the column. |
| `w` `b` `e` | Next word start, previous word start, word end (letters/digits/`_` and punctuation runs are words). |
| `0` `^` `$` | Line start, first non-blank, last character. |
| `gg` `G` | First / last line (with a count: that line). |
| `i` `a` `I` `A` `o` `O` | Insert before / after the cursor, at the first non-blank / the line end, on a new line below / above. |
| `x` (Delete = `x`) | Delete the character under the cursor. |
| `dd` `D` | Delete the line(s); delete to the line end. |
| `d` + `w` `e` `b` `h` `l` `0` `^` `$` `j` `k` | Delete over the motion (`dw` stops at the line end; `dj`/`dk` take lines). |
| `cc` `C` `s` `S`, `c` + motion | Change: like delete, then insert mode (`cw` changes to the word end, like `ce`). |
| `yy`, `y` + motion | Yank (copy) into the register. |
| `p` `P` | Put the register after / before the cursor (lines: below / above). |
| `u`, Ctrl+R | Undo / redo (the input's own undo history; each normal-mode edit is one step). |
| Enter | Send the input. |
| Esc | With a count or operator pending: cancel it. Otherwise the usual Esc (below). |

Other printable keys do nothing in normal mode (they never type, so `?`
does not open help there — `/help` or `i` then `?` does). Ctrl and Alt keys
(Ctrl+C, Ctrl+D, Ctrl+B, …), arrows, Tab, Shift+Tab, PgUp/PgDn keep their
usual meaning in both modes. The register is internal (not the system
clipboard; use [Copy](#copy) for that).

**Esc precedence** (vim on), first match wins:

1. The picker or the one-line yolo confirmation is open: it takes Esc.
2. The command menu or the file list is open: Esc closes it.
3. Insert mode: Esc switches to normal mode — nothing else, even while a
   turn runs or a prompt is shown.
4. Normal mode with a count or operator pending: Esc cancels it.
5. Normal mode: today's Esc — deny/reject a shown prompt when the input is
   empty, return from a subagent view, cancel the running turn, clear the
   input.

So cancelling a running turn from insert mode is Esc Esc. With a prompt
shown and an empty input, digits, Up/Down, and Enter still answer the
prompt in normal mode (the prompt dock sees keys before normal mode does).

```text
/vim                     → -- INSERT --
fix the parser bug       ← typed
Esc 0 w cw the lexer Esc → "fix the lexer bug"   -- NORMAL --
Enter                    → sent; the next input starts in -- INSERT --
```

**Interfaces.** The `vim` preference (`boolean`, [Preferences
file](#preferences-file)); `AppState.vim`, `vimMode` (`"insert" |
"normal"`), `vimPending` (state/store.ts `setVim`, `setVimMode`).
`composer/vim.ts` is the pure state machine: `initialVimState()` and
`vimKey(state, { text, cursor }, key)`, which returns `{ type: "pass" }`
(the key goes on to the usual handling) or
`{ type: "handled", state, edit?, cursor?, command? }` (`command`:
`undo`, `redo`, `submit`). `components/Composer.tsx` applies an `edit`
with the textarea's `replaceText` (keeping its undo history) and runs
`undo()` / `redo()` on it.

### Desktop notifications

The TUI can ask the terminal for a desktop notification when it needs your
attention and you are not looking: a turn of the open session finishing
(success or error), or a permission/question ask arriving for it. On by
default.

**Usage.** `/notifications` toggles it (`/notifications on`,
`/notifications off` set it); the choice is saved as `notifications` in the
[preferences file](#preferences-file). A notification is sent only while
**both** are true: the preference is on, and the terminal is unfocused
(tracked through the terminal's own focus reporting — most terminals
support it; one that does not simply never reports a blur, so nothing is
ever sent). Focusing the terminal again does not resend anything already
missed. A subagent's turn ending or ask never notifies — only the open
(root-viewed) session's own, and an ask of a session outside the open tree
([Asks of other sessions](#asks-of-other-sessions)); keeps the rule simple,
since a subagent's work already shows in its parent's task card. Each ask
notifies at most once, even when both streams carry it.

The message: `hya` as the title, and one of:

| Event | Body |
| --- | --- |
| Turn finished | `Turn finished · <session title>` (no ` · …` before the session has one) |
| Turn failed | `Turn failed: <error>` |
| Permission ask | `Permission needed: <what it asks about>` |
| Question ask | `Question: <the question's title>` |
| Ask of another session | the same, then ` · in <n>. <session>` |

**Interfaces.** Two escape sequences, both sent for the same event (some
terminals understand one, some the other): OSC 9 (`ESC ] 9 ; <body> BEL`)
and OSC 777 (`ESC ] 777 ; notify ; <title> ; <body> BEL`). `src/notify.ts`:
`shouldNotify({ notifications, focused })`, `sanitizeNotificationText(text,
maxLength?)` (strips control characters, truncates with `…`),
`notificationBody(kind, detail)`, and `notificationSequence(body, title?)`
build the sequence; `app/controller.ts`'s `sendNotification` calls them and
writes through `TerminalAccess.notify(sequence)` (`app/run.tsx`: straight to
`process.stdout`, since OSC sequences have no visible effect and OpenTUI has
no other "write this sequence" entry point). Focus tracking is OpenTUI's
(`CliRenderer` "focus"/"blur" events, driven by the terminal's CSI `?1004`
reporting), read through `TerminalAccess.onFocusChange` into
`AppState.focused`; `AppState.notifications` mirrors the preference.
`app/turns.ts`'s `TurnRunnerOptions.onEnd` is the turn-finished hook
(skipped for a user-cancelled turn); the ask hook is in `applyEvent`'s
handling of a fresh `permissionRequested`/`questionRequested` of the open
session's own stream (a descendant's ask never reaches it — see
`state/prompts.ts` `askFrameRoute`) and in `onGlobalFrame` for another
session's ask; both go through `notifyAsk(id, kind, detail)`, which
remembers the ids it handled.

The WebUI (ADR-0021, [tui-web.md](tui-web.md#desktop-notifications)) maps
both sequences to a browser `Notification`, generically — the host does not
know they are hya's.

## Permission and question prompts

The backend asks before some tool calls run (under the default permission
model: `bash`, `edit`, `write`, network reads, MCP and plugin tools; see
[Configuration — Permissions](configuration.md#permissions)), and the
`ask_user` tool asks you questions. The TUI shows each of these pending
interactions as a prompt docked above the status line, so you can answer
without typing its id. The agent's turn waits until you answer.

### Permission prompt

```text
┌─Permission · 1 of 2──────────────────────────────────────────┐
│edit  src/main.rs · +1 -1                                     │
│asked by build                                                │
││ - let x = 1;                                                │
││ + let x = 2;                                                │
│▸ 1  Allow once                                               │
│  2  Always allow  tool: edit                                 │
│  3  Deny                                                     │
│1-3 or ↑↓ Enter · Esc denies · perm_… · mode manual           │
└──────────────────────────────────────────────────────────────┘
```

- **Title row.** The waiting call as its tool card would summarize it
  (`<tool>  <summary>`); an ask that is not tied to a tool call (for example
  an external directory) shows the interaction title, `<action> <resource>`.
- **Who asks.** `asked by <agent>` for the open session, `asked by subagent
  <agent> · <task description>` for a subagent's session.
- **Details**, beside a bar, rendered like the tool card body from the
  call's arguments (`payload.input`): `bash` the command (`$ …`); `edit`,
  `write`, `apply_patch` the diff (added rows green, removed rows red);
  `read`, `webfetch` and other path or URL tools the path or URL; anything
  else the compact JSON arguments; without a call, the resource. At most 8
  lines (the first 3 and last 4 around `… N lines hidden`).
- **Options.**

| Key | Option | Sends |
| --- | --- | --- |
| `1` | Allow once: this call runs. | `{permission: {allowed: true, persist: false}}` |
| `2` | Always allow: this call runs, and the backend stops asking for what the muted text names (`payload.always`, shown as `<action>: <patterns>`; the resource when the backend sends no patterns). | `{permission: {allowed: true, persist: true}}` |
| `3` | Deny: the call fails with a permission error (its card shows `✗`), and the model continues with that result. | `{permission: {allowed: false, persist: false}}` |

  An Always allow grant applies to every session of that backend (and
  survives a permission mode switch). The backend saves it and reloads it
  when it restarts; `/rules` lists and deletes saved grants, and a deleted
  grant stops applying at once. For native tools it covers the exact subject (the
  same command, the same path); see
  [Tools and permissions](architecture/tools-and-permissions.md).

### Question prompt

```text
┌─Question─────────────────────────────────────────────────────┐
│Color: Which color do you want?                               │
│asked by build                                                │
│▸ 1  red                                                      │
│  2  blue                                                     │
│  3  Other…  type the answer in the input, Enter sends        │
│  4  Reject                                                   │
│1-4 or ↑↓ Enter · type an answer + Enter · Esc rejects · q_…  │
└──────────────────────────────────────────────────────────────┘
```

The first row is `<header>: <question>`. Each option sends
`{question: {answer: "<label>"}}`. For a free-text answer, type it into the
input and press Enter: it sends `{question: {answer: "<text>"}}` instead of a
prompt (a `/command` still runs as a command). Choosing `Other…` only points
you at the input. `Reject` (or Esc) sends `{question: {rejected: true}}`;
`ask_user` then reports the question as unanswered. Only the first question
of a multi-question `ask_user` call is shown (the backend answers one per
interaction).

### Keys

A prompt takes keys only while the input is empty, so text you are typing
can never answer one by accident: with text in the input, digits, Enter, and
Esc edit or send the input as usual, and the hint row reads `Clear the input
to answer with 1-3 · or /approve <id>` (a question's reads `Enter sends the
input as the answer`). The text you typed stays in the input while a prompt
is shown and after you answer it.

Key order, first match wins:

1. The permission mode picker, while open, takes every key but Ctrl+C; the
   one-line yolo confirmation takes Enter, Esc, and Shift+Tab (see
   [Permission modes](#permission-modes)), so they never answer the prompt.
2. An open list (the `/` command menu or the `@file` list) takes Up/Down,
   Tab, Shift+Tab (highlight up), Enter, and Esc.
3. The prompt, with an empty input: `1`–`9` choose that option at once;
   Up/Down move the highlight (`▸`, accent color); Enter chooses the
   highlighted option; Esc declines. With text in the input, a question takes
   Enter as its answer.
4. The composer: history, sending, Esc's other meanings (return from a
   subagent view, cancel the turn, clear the input), and Shift+Tab (switch
   the permission mode, also while a prompt is shown).

**Esc declines, it never approves.** On a permission prompt Esc is Deny
(`allowed: false`, not saved); on a question it is Reject. Denying is the
safe default: nothing runs that you did not allow, and the model sees the
refusal and can continue or ask differently. There is no "answer later"
state; to leave a prompt waiting, type in the input (the prompt stays and
ignores keys) or open another view. A click on an option chooses it too.

### Several asks

Asks are shown one at a time, oldest first; the box title counts them
(`Permission · 1 of 2`). Answering one shows the next. The session's own
tool calls ask one after another; several asks wait at once when subagents
ask too.

An answer hides the prompt at once. The ask also closes when it is resolved
elsewhere: another client answered it, or a switch of the session tree to
the `yolo` permission mode allowed it (an `interactionResolved` frame, or the
next listing). If the ask was already answered, the status line reads
`Already answered elsewhere · <title>`; if the request fails, the prompt
comes back with `Answer failed: …`.

### Subagent asks

A subagent runs in a child session and asks in its own name. The TUI
subscribes to the open session's stream with `includeDescendants=true`, so
a subagent's `permissionRequested` / `questionRequested` /
`interactionResolved` frames (any depth, `event.session` = the asking
session) arrive on it the moment they are raised — there is no polling
delay. Frames sent before the subscription are not replayed, so the TUI
reads `GET /v1/interactions` once after every (re)subscribe and after a
`resync`; other listings happen only with a full refresh (start, Ctrl+R).
The open session's prompt queue holds the asks of the open session and of
every session below it (children by `SessionInfo.parent`, and the open session's
members and `task` outputs before the session list knows them), so a
subagent's ask appears in the parent view, labelled `asked by subagent
<agent> · <task>`. The subagent's `task` card shows `◌ waiting for
approval`, and its sidebar row `· ◌ waiting`. Opening the subagent's
read-only view shows the same prompt there (only that subtree's asks); you
can answer in either view. Asks of unrelated sessions stay in the
[pending block](#layout); they arrive on the global stream
([Asks of other sessions](#asks-of-other-sessions)).

### Asks of other sessions

A permission request or question can come from a session this TUI does not
have open: a session of another TUI or WebUI tab on the same server, or a
headless run (`hya run`, the HTTP API). The TUI shows it the moment it is
raised, names the session it belongs to, and never answers it by accident:
its prompt only appears once you open that session.

**Usage.** When such an ask arrives:

- the [pending block](#layout) lists it as `! <title> · <n>. <session> ·
  <id>` (`?` for a question), where `<n>` is the session's number in the
  sidebar and `/open`;
- the status line says `Permission needed in <n>. <session> · /open <n> to
  answer there` (`Question in …` for a question);
- while the terminal is unfocused, a [desktop
  notification](#desktop-notifications) says `Permission needed: <title> ·
  in <n>. <session>`.

`/open <n>` (or `/sessions`) opens that session; its prompt appears as
usual and `1`/`2`/`3` answer it there. `/approve <id>`, `/deny <id>`, and
`/answer <id> <text>` still answer from anywhere; the pending line shows
which session the id belongs to first. An ask answered elsewhere (in the
other tab) disappears at once. A session created since the last listing
is listed again when its first ask arrives, so the line can name it (a
session the listing does not return — another directory — is named by its
id, and `/open <id>` still works).

Example: this TUI views session 1 while another tab's session 2 asks to run
a command:

```text
╭Pending (1)──────────────────────────────────────────────────────────╮
│ ! bash echo hi · 2. Fix the build · perm_01a0…                      │
│ /open <n> answers there · /approve <id> · /deny <id> · …            │
╰─────────────────────────────────────────────────────────────────────╯
Permission needed in 2. Fix the build · /open 2 to answer there
```

**Interfaces.** From start, the TUI keeps one subscription to
`GET /v1/events/stream?interactionsOnly=true` (`StreamGlobalEvents`, SSE
`StreamFrame`s of every session). The server leaves out every session's
engine events (text, tools, messages, status) and their `resync` frames, so
only the ask planes' `permissionRequested {interaction}`, `questionRequested
{interaction}`, `interactionResolved {request}` (never durable), and
`catalogUpdated {}` (live, no seq, empty `session`) arrive — no history is
replayed and, unlike `streamSession`, no `resync` is expected on this
filtered stream. `state/prompts.ts` `globalAskRoute(event, context)` returns
`ignore` for anything but the three ask frames, `tree` for an ask of the
open session's tree (its own stream carries it too; applying it again is
harmless — asks are kept by id), and `other` otherwise; `app/controller.ts`
`onGlobalFrame` applies both through `store.applyAsk` and, for a new `other`
ask, sets the status notice (`state/format.ts` `otherAskNotice`) and
notifies. A `catalogUpdated` frame is checked first (before `globalAskRoute`,
which would otherwise `ignore` it) and does not touch the pending list; see
[Catalog updates](#catalog-updates) below. The stream is not a history: after
every (re)subscribe the TUI reads `GET /v1/interactions` once (a `resync` on
this stream is not expected, but the handler still re-lists on one
defensively). It reconnects after 800 ms, doubling to at most 15 s while it
keeps failing (a backend without the route); its failures do not touch the
status bar's connection state, which the session stream owns.
`state/format.ts` `askSessionLabel(sessionId, sessions)` renders
`<n>. <title or id>` (or the bare id when the list does not have it).

### Catalog updates

`catalogUpdated {}` (live-only: no seq, empty `session`) marks a change to
the provider/model catalog — a provider added, edited, or refreshed, a key
set or removed, or startup model discovery finishing
(`docs/protocol/README.md` "Live and durable frames"). It arrives on the
global stream (above) and, separately, on the open session's own stream
(`app/controller.ts` `applyEvent`); either one triggers a re-read of
`GET /v1/models` and `GET /v1/providers` only (`refreshCatalogOnly()`,
lighter than a full catalog `refresh()`, so it does not also disturb the
session list or interactions), applied with `store.setProviderCatalog()`.
Both handlers share one debounce (`app/debounce.ts createDebounce`, the same
120 ms / 400 ms window as the projection re-read), so a burst — both
streams delivering the same frame, or a `catalogUpdated` arriving right
after the Provider View's own post-call reload — coalesces into a single
re-read; there is no double flicker. The `/model` picker (built from
`state.models`) therefore shows a provider added over the HTTP API, from
another client, without the TUI restarting or a manual refresh.

The only polling left is the child-session round for `task` cards (their
status and latest activity, `GetSession` + `ListMessages` of each child,
every 1.5 s while a child is busy or a turn runs): durable child events stay
on the child's own stream.

The keyboard commands keep working as a fallback: `/approve <id>`,
`/deny <id>`, `/answer <id> <text>`, and `/interactions` (the prompt's hint
row shows the id).

## Permission modes

A permission mode decides how the open session tree (the session and its
subagents) answers permission checks: `manual` asks you (the
[permission prompt](#permission-prompt)), `yolo` allows every tool call
without asking (including calls a rule denies), and a bundle mode lets an
installed bundle's approver answer first and asks you only for what it
leaves open. The mode lives on the backend, on the root session; see
[Configuration — Session permission modes](configuration.md#session-permission-modes)
for the semantics. The TUI switches it without a restart, shows it in the
status bar, and notes every switch in the transcript.

### Switching

- **Shift+Tab** switches to the next mode: `manual` → `yolo` → the bundle
  modes in the backend's listing order → `manual`. The listing is read with
  the other catalogs at start and on `/refresh`; without it (an older
  backend) the cycle is `manual` ↔ `yolo`. Shift+Tab also works while a
  permission prompt is shown. In an open list (the `/` command menu, the
  `@file` list, the picker) it moves the highlight up instead and the mode
  does not change.
- **`/permissions`** opens a picker with every mode from
  `GET /v1/permission-modes`: its title, `[source]` (`builtin`, or the id
  of the bundle that declares it), and description; `●` marks the mode in
  effect, which is also highlighted. Type to filter (every word must match
  the title, id, source, or description), Up/Down (or Shift+Tab/Tab) move,
  Enter switches, Esc closes; a click on a row switches too. While the
  picker is open the input does not take keys; closing it gives the input
  the focus back.
- **`/permissions <mode>`** switches directly (Tab completes the mode ids),
  for example `/permissions yolo` or `/permissions acme/approver/careful`.

```text
┌─Permission mode──────────────────────────────────────────────────────┐
│ Filter ▏  3 of 3                                                     │
│ ▸ ● Manual     [builtin]       Ask the user before actions that need │
│     Yolo       [builtin]       Allow every action without asking, in │
│     Echo only  [e2e/approver]  Approve echo commands; ask for the re │
│ ↑↓ select · Enter chooses · Esc closes · type to filter              │
└──────────────────────────────────────────────────────────────────────┘
```

**Confirming yolo.** The first switch to `yolo` in a TUI process shows one
line above the status line and waits:

```text
⚠ Enable yolo? Every tool call runs without asking · Enter confirms · Esc cancels
```

Enter switches; Esc keeps the current mode (`Permission mode unchanged ·
manual`); Shift+Tab skips `yolo` and goes on to the next mode of the cycle
(with only the built-ins, that is where you started, so nothing changes);
any other key cancels and then does what it normally does, so you cannot
type into `yolo` by accident. This line takes Enter and Esc before a shown
permission prompt does. After one confirmed switch, later switches to `yolo`
in the same process do not ask again. Shift+Tab never lands in `yolo`
without this confirmation.

**Pending asks.** Switching to `yolo` makes the backend allow (once) every
permission ask of the tree that is still waiting, so the prompt closes and
the tool runs: the TUI re-reads the pending interactions right after the
switch instead of waiting for the `interactionResolved` frames. The switch
applies from the next permission check, including in a turn that is
already running.

**No session yet.** Before any session exists (a fresh directory), the
choice is remembered — the status bar shows it and the status line reads
`Permission mode → <mode> · applies when the session is created` — and it
is sent right after the next session is created (the first prompt, `/new`,
or a command that creates one), before the prompt is admitted. Opening an
existing session instead shows that session's own mode.

### Display

| Mode | Status bar | Color |
| --- | --- | --- |
| `manual` | `mode manual` | normal text (`fg`) |
| `yolo` | `mode ⚠ yolo` | `error` (`#f07878`) |
| bundle mode | `mode <title>` (the listing's title, else the id) | `accent` (`#73c8e8`) |

The `mode` word and the rest of the status bar stay muted. Every switch —
from this TUI or another client (a `sessionUpdated` frame with
`permissionMode`) — adds one muted notice line to the transcript,
`Permission mode → yolo` (a bundle mode: `Permission mode → <title>
(<id>)`), and the status line confirms it (`Permission mode → ⚠ yolo ·
Shift+Tab cycles · /permissions lists`). A permission prompt's hint row ends
with the mode (`… · perm_… · mode manual`). An unknown or unavailable mode
leaves the mode unchanged and shows `Permission mode failed: …
invalid_argument: …`.

### Bundle modes

A bundle declares modes with `permission_modes:` and answers them with a
`permission.approve` hook (see
[Agent bundle authoring — Permission modes](agent-bundle-authoring.md#permission-modes-permission_modes)).
Installed (for example `hya bundle install --project -y approver.hyabundle`,
or a source directory under `.hya/bundles/<dir>/` where `hya serve` runs),
its modes appear in the picker as `<title> [<bundle id>]` and in the
Shift+Tab cycle after `yolo`. With one active, the approver decides first;
when it defers, the TUI shows the usual permission prompt. Worked example:
a bundle `e2e/approver` whose mode `echo-only` allows `echo …` commands —
`/permissions`, type `echo`, Enter: the status bar reads `mode Echo only`,
a model's `echo hi` call runs without a prompt, and its `ls` call asks.

## Undo, redo, and fork

`/undo` takes back the last prompt: the prompt and every message after it
leave the transcript, the files the turn's `edit`, `write`, `patch`, and
`bash` tools changed are written back to what they were before
(`RevertSession`; see the protocol guide's
[Revert and redo](protocol/README.md#revert-and-redo) for what the backend
keeps and its size limits), and the prompt goes back into the input so you
can edit and resend it. `/redo` undoes that until the next prompt; `/fork`
copies the session into a new one, at its end or before a picked prompt.

**Usage.**

- **`/undo`** reverts the last visible prompt; `/undo` again goes one prompt
  further back. The status line summarizes the files:
  `Reverted · 2 files restored · 1 deleted` (`deleted`: the turn created
  the file), then every file that could not be restored with its reason,
  `skipped big.bin (too_large)` or `failed /etc/x (permission denied)`.
  Paths inside the session's directory are shown relative to it.
- **Keys.** Ctrl+X U undoes, Ctrl+X R redoes, and Ctrl+X F opens the fork
  picker (Ctrl+X Ctrl+U / Ctrl+R / Ctrl+F work too). They act whatever the
  input holds — after `/undo` it holds the reverted prompt, so typing
  `/redo` would first need it cleared, while Ctrl+X R does not. The help
  overlay lists them in the `Turns` group.
- **The input.** The reverted prompt goes into the input only when the input
  is empty, or still holds, untouched, the prompt a previous `/undo` or
  `/fork` put there (so `/undo` twice leaves the older prompt in it). Text
  you typed is never replaced; the status line then ends with
  `the input kept your text`.
- **While a revert is pending** the transcript ends with a line in the
  warning color:
  `↶ 2 messages reverted · /redo or Ctrl+X R restores them · the next prompt makes it permanent`.
  It follows the session live: a revert or redo from another client (a
  `sessionReverted` frame) updates it, and the next prompt or `!command`
  (its `messageStarted`) removes it.
- **`/redo`** works only while that line is shown. It brings the messages
  and files back (`Restored · 2 files restored`) and empties the input if it
  still holds exactly the reverted prompt. After the next prompt it says
  `Nothing to redo · /redo works after /undo, until the next prompt`.
- **A running turn.** The backend refuses a revert while a turn runs
  (`409 session_busy`): `Undo refused: a turn is running · wait for it to
  finish or press Esc to cancel it`. With no earlier prompt, `/undo` says
  `Nothing to undo: …` with the server's reason. In a subagent's read-only
  view both are refused.
- **`/fork`** opens a picker: `Fork at the latest message` first
  (highlighted), then the session's prompts newest first, tagged `#1` (the
  oldest) upward; typing filters. Enter on the first row copies every
  message; on a prompt, the new session holds the messages strictly before
  it and the prompt goes into the (empty) input. The TUI switches to the new
  session (`Forked before “<prompt>” · the prompt is in the input`, or
  `Forked at the latest message`); the backend titles it `<source title>
  (fork)` (the source id when the source is untitled). The sidebar's `Context` box and `/status` show where it came
  from: `Forked   from <source title>`. Messages hidden by a pending revert
  are never copied.

For example, after the model wrote `notes.txt` in reply to `write notes`:

```text
/undo      → Reverted · 1 deleted · the prompt is back in the input
             (notes.txt is gone; the input holds "write notes")
/redo      → Restored · 1 file restored   (notes.txt is back)
```

**Interfaces.**

| Action | Call | Body | Reads |
| --- | --- | --- | --- |
| `/undo` | `POST /v1/sessions/{id}/revert` | `{}` | `RevertSessionResponse {session, files}`: `session.revert {messageId, text, hiddenMessages, files}`, `files[] {path, action, reason}`; then `GET /v1/sessions/{id}/messages` |
| `/redo` | `POST /v1/sessions/{id}/revert` | `{undo: true}` | `{session (no revert), files}`; then the messages |
| `/fork` Enter | `POST /v1/sessions/{id}/fork` | `{}` (head) or `{messageId}` | `ForkSessionResponse {session, promptText}`; `session.forkedFrom {session, messageId}`; then `GET /v1/sessions` and the new session is opened |
| Live | session stream | — | `sessionReverted {messageId, undone, files}` (durable): the overlay is dropped and the session row and transcript are re-read; a later durable `messageStarted` clears `revert` locally |

## Pickers

`/model`, `/agent`, and `/sessions` (with no argument) open the same
reusable modal picker `/permissions` uses (see
[Permission modes — Switching](#switching) for the shared filter/move/select
keys). Rows are loaded from the catalog already held by the TUI (`refresh()`
at start and `/refresh`/Ctrl+R), so a picker opens with no loading state.

- **`/model`** lists every model from `GET /v1/models`, `[tag]`ged with its
  provider id and, when the route advertises one, its context window
  (`128k ctx`); `●` marks the open session's model. Enter sends
  `UpdateSession {model}` and shows `Model → <provider>/<model>`.
  `/model <provider/model>` still switches directly, with Tab completion.
- **`/agent`** lists visible (non-`hidden`) agents from `GET /v1/agents`,
  tagged with the agent's default `provider/model` and its one-line
  description; `●` marks the open session's agent. Enter sends
  `UpdateSession {agent}` and shows `Agent → <name>`. `/agent <name>` still
  switches directly.
- **No session yet.** Before any session exists, a `/model`/`/agent` choice
  (picker or direct form) is remembered — the status line reads
  `Model → <id> · applies when the session is created` (`Agent → …` for the
  agent) — and is used for the next `CreateSession` in place of the usual
  default, the same way a chosen [permission mode](#permission-modes)
  applies once the session exists.
- **`/sessions`** opens a picker with a `New session` row first, then every
  session as a tree (top-level sessions, subagent sessions nested under
  their parent and `[subagent]` tagged — see [Subagents](#subagents)),
  showing the agent, model, and a relative update time (`3m`, `2h`) in the
  detail column, and `● running` while busy. `●` marks the open session.
  Enter on the `New session` row runs `/new`; Enter on any other row opens
  it.

```text
┌─Sessions──────────────────────────────────────────────────────────────┐
│ Filter ▏  3 of 3                                                      │
│ ▸   New session          [new]       Create a session with the curr… │
│   ● Fix the flaky test              build · fake/model · 3m           │
│       ↳ Explore the auth code [subagent]  explore · fake/model · 1m  │
│ Enter opens · F2 renames · Ctrl+D deletes · Esc closes · type to fil… │
└──────────────────────────────────────────────────────────────────────┘
```

### Row actions

The `/sessions` picker's highlighted row also takes two keys the plain
filter never sees (never Ctrl+R, which means refresh):

- **F2** renames it: the picker switches to a one-line editable field seeded
  with the row's current label (`New title <text>▏`); type to edit, Enter
  sends `UpdateSession {title}` and shows `Renamed to <title>` (an empty
  title cancels with a status message), Esc returns to the list without
  changing anything. A rename reopens the picker so browsing continues, its
  row already showing the new title.
- **Ctrl+D** deletes it: the picker switches to a one-line confirmation
  (`Delete "<title>"? Enter confirms · Esc cancels`); Enter sends
  `DeleteSession` and shows `Deleted session <id>`, Esc returns to the list
  with nothing changed. Deleting the open session opens the next top-level
  session (or shows no session, if none is left); the confirmation applies
  the same way whether or not the row is the open session, so the open
  session is never deleted without it.

`state/picker.ts`'s `PickerAction` (`{id, key, ctrl?, label, prompt: "value"
| "confirm", confirmText?}`) and the `"rename"`/`"confirm"` picker modes are
a small, backward-compatible extension of the picker used by `/permissions`:
a picker with no `actions` behaves exactly as before. See
[Code layout — The picker](#code-layout) for the API.

### Session titles

The header (`hya · <title or id> · <agent> <provider/model> · <server>`),
the sidebar's `Sessions` box, and the `/sessions` picker all show the
session's `title` when the backend has set one (`/rename`, the picker's F2,
or the backend's own auto-generated title once it lands), falling back to
the raw id. A `sessionUpdated {title}` frame (see
[Stream frames and the transcript](#stream-frames-and-the-transcript))
updates all three live, with no extra refresh — including a title set by
another client or generated by the backend after the first turn.

## Provider View

`/key` opens the Provider View: a full-screen place to configure the model
providers of the backend the TUI talks to. It lists every provider with its
protocol, where its key comes from, its auth status, and how many models it
has; a provider opens into its models. From there you add a provider, set or
remove its API key, pull its latest model list, test a model, and add a
model or edit a model's metadata. Every change applies to the running
backend at once (the server rebuilds that provider's route and catalog; see
[Protocol guide — Providers and keys](protocol/README.md#providers-and-keys)),
so there is nothing to restart, and the `/model` picker lists the new models
right away. The key itself is only ever sent to the backend: the view shows
bullets while you type it and never displays a saved key.

The view replaces the older `/keys`, `/key set|remove <provider>`, and
`/login <provider>` commands; `/key` takes no arguments.

### Screens

- **List** (`Providers`): one row per provider — `PROVIDER` (the id),
  `PROTOCOL` (`openai`, `openai-response`, `anthropic`, `google`, …;
  `offline` for the built-in `hya` row, listed last), `KEY` (`saved key` in
  `auth/<id>.yaml`, `oauth`, `config key` for an inline `api_key`, or
  `no key`), `STATUS` (`ready`, `no key`, `key rejected`, `auth required`,
  `offline`), and `MODELS` (the count).
- **Detail** (`Providers › <id>`): the header line
  `gw · openai · https://gw.example/v1 · saved key · ready · 3 models`, then
  one row per model — `MODEL` (the provider-local id), `NAME` (a display
  name, when it differs), `SOURCE`, `CTX / OUT` (context and output limits,
  `64k / 4.1k`, `—` when unknown), and `reasoning` when the model takes
  reasoning effort variants. `SOURCE` says where the row comes from:
  `remote` (the provider's model list, kept in the model cache), `config`
  (only in `config.yaml`), `override` (both; the `config.yaml` entry wins
  field by field), or `offline`. The last model test's result shows under the
  rows.

Below the rows: the running call (spinner, label, elapsed seconds,
`Esc cancels`), a notice with the last outcome (green for success, red for a
failure), the filter while one is set, and the key line for the screen.
At about 80 columns the columns shrink (long model ids end in `…`) and the
key line wraps.

### Keys

| Key | List | Detail |
| --- | --- | --- |
| Up / Down | Move over providers | Move over models |
| Enter | Open the provider | — |
| `a` | Add a provider (the pop-up below) | — |
| `k` | Set or replace the highlighted provider's API key | The open provider's |
| `x` | Remove the provider's saved key (asks first) | The open provider's |
| `r` | Fetch the provider's latest model list | The open provider's |
| `t` | — | Test the highlighted model |
| `m` | — | Add a model by hand |
| `e` | — | Edit the highlighted model's metadata |
| `d` | — | Delete the highlighted model's `config.yaml` entry (asks first) |
| `/` | Filter the rows (Enter keeps the filter, Esc clears it) | Filter the models |
| Esc | Cancel a running call; else clear the filter; else close the view | Cancel a running call; else clear the filter; else back to the list |

Ctrl+C closes the view and keeps its quit meaning (press again to quit). The
built-in `hya` provider takes no key and has no list to fetch or edit. `x`
works on a `saved` or `oauth` key; a key written inline in `config.yaml` is
left alone. `d` works on a `config` or `override` row; a `remote` row has no
entry to delete. While a call runs, moving still works and other actions say
`Busy: … · Esc cancels`. The help overlay (`?`, group `providers`) lists the
same keys.

### Pop-ups

A pop-up asks one field at a time over the view. Enter checks the field and
moves on (the last field submits); Esc cancels at any step. Text fields take
typing, Backspace, Ctrl+U (clear), and pasting; a choice field takes Up/Down
or its digit; a key field shows only bullets. A field that fails a check
stays open with the reason in red, and so does a pop-up whose call the server
refused (`invalid_argument: …`), on the field the error names.

- **Add provider** (`a`, `Add provider · 1/4` … `4/4`): **Name** (the
  provider id: 1–64 letters, digits, `-`, `_`; not `hya`; not an existing
  id), **Protocol** (`1 openai` — OpenAI-compatible Chat Completions,
  `2 openai-response` — OpenAI Responses, `3 anthropic`, `4 google`),
  **Base URL** (`http://` or `https://`, no credentials), **API key**
  (optional: Enter with none skips, for local endpoints). Enter on the key
  adds the provider and fetches its models: the view opens the new provider
  with `Added gw · 2 models fetched`, or `Added gw · model fetch failed
  (unavailable): …` — a failed fetch still adds the provider, so you can fix
  the key (`k`) or the endpoint and press `r`. When the open session (or,
  without one, the next new session) would run on `hya/offline`, the
  `/model` picker opens over the view with the new provider's first model
  highlighted; Enter switches the session (or remembers the choice for the
  next one), Esc keeps the offline model.
- **API key** (`k`): one hidden field; Enter saves it to `auth/<id>.yaml`
  (`Saved the key of gw · applies now`, plus the fetch outcome when the
  provider had no cached models).
- **Add model** (`m`, `1/5` … `5/5`): **Model id** (as the provider serves
  it; `/` and `:` are fine), **Display name**, **Context limit**, **Output
  limit** (digits only, at most 4294967295; the output limit may not exceed
  the context limit), **Reasoning** (`default` keeps what the provider
  reports, `on`, `off`). Only the fields you fill in are saved into the
  provider's `models:` in `config.yaml`; the row shows `config` (or
  `override` for a model the remote list also has).
- **Edit model** (`e`): the same fields but the id, filled with the model's
  current values. Only the fields you change are sent: a field left as it
  opened is not written, so values the server merely shows for a model
  without real metadata (a fallback context limit, `reasoning`) never end up
  in `config.yaml`. Clearing a field (Ctrl+U) removes that field from the
  entry, so the remote value (if any) shows again; choosing `default`
  reasoning removes a `reasoning: true|false`. An edit that changes nothing
  says `No changes to <id>` and sends nothing. Saving makes a remote model's
  row `override`.
- **Confirm** (`x`, `d`): one line — Enter does it, Esc cancels.

### Testing a model

`t` sends one message, `hi`, to the highlighted model with at most 1 output
token (16 on Responses protocols), no tools, and no system prompt. The call
may take up to 60 seconds: the running line counts the seconds, the view
stays usable, and Esc stops waiting (`Test cancelled`). The result line reads
`✓ gw/alpha replied · 412 ms · finish length · "Hi"` — a `length` finish is a
normal reply here — or `✗ gw/alpha failed · 90 ms · http_401: …` with the
provider's error code (`http_<status>`, `transport`, `timeout`,
`unknown_model`, `incompatible`, `decode`, `auth_expired`,
`provider_error`). Nothing is written to any session.

### Example

Add a local OpenAI-compatible server and switch the session to it:

1. `/key`, then `a`.
2. Name `local`, Enter; Enter (`openai`); base URL
   `http://127.0.0.1:8000/v1`, Enter; no key, Enter.
3. The view opens `Providers › local` with `Added local · 3 models fetched`;
   the session was on `hya/offline`, so the model picker opens — choose a
   model, Enter.
4. `t` on a model: `✓ local/qwen replied · 38 ms · finish length · "Hi"`.
5. `e` on it, Display name `Qwen`, Context limit `32768` (Ctrl+U clears the
   value it opened with), Enter through the rest: the row shows
   `Qwen · override · 33k / —`, and the provider's entry in `config.yaml`
   now reads (formatting may differ; only the two changed fields are written):

   ```yaml
   providers:
     local:
       kind: openai
       base_url: http://127.0.0.1:8000/v1
       models:
         - id: qwen
           name: Qwen
           limit:
             context: 32768
   ```

6. Esc, Esc: back in the chat.

### Provider View interfaces

The view uses the v1 routes of
[Protocol guide — Providers and keys](protocol/README.md#providers-and-keys);
after every write it re-reads `GET /v1/providers` and `GET /v1/models` (with
the rest of the catalog), so its rows and the `/model` picker are current.
The server's `catalog.updated` notice does not reach v1 event streams, so
the TUI does not wait for it.

| Action | Call | Body | Reads |
| --- | --- | --- | --- |
| Open, after each write | `GET /v1/providers`, `GET /v1/models` | — | `ProviderSummary` (`id`, `kind`, `baseUrl`, `keySource`, `auth`, `modelCount`); `ModelSummary` (`providerId`, `modelId`, `displayName`, `contextLimit`, `outputLimit`, `reasoning`, `source`) |
| Add provider | `PUT /v1/providers/{id}` | `{kind, baseUrl, apiKey?}` (no `apiKey` when the key was skipped) | `ProviderUpdate.discovery` (`ok`, `result`, `errorMessage`, `modelCount`) |
| Refresh (`r`) | `POST /v1/providers/{id}/refresh` | `{}` | `ProviderUpdate.discovery` |
| Set key (`k`) | `PUT /v1/auth/{id}` | `{apiKey}` | `discovery` when the models were fetched too |
| Remove key (`x`) | `DELETE /v1/auth/{id}` | — | — |
| Add / edit model (`m`, `e`) | `PUT /v1/providers/{id}/models` | `{modelId, displayName?, contextLimit?, outputLimit?, reasoning?}` with only the fields filled in (add) or changed (edit); limits as numbers (uint32); a cleared field is sent as `""` / `0` (removed); `reasoning` omitted for `default` | `ProviderUpdate` |
| Delete override (`d`) | `DELETE /v1/providers/{id}/models?modelId=<percent-encoded>` | — | `ProviderUpdate` |
| Test (`t`) | `POST /v1/providers/{id}/test` | `{modelId}` | `TestProviderModelResponse` (`ok`, `text`, `finishReason`, `errorCode`, `errorMessage`, `latencyMs`) |
| Pick a model after adding | `PATCH /v1/sessions/{id}` | `{model: "provider/model"}` | `SessionInfo` (no session: remembered for the next `CreateSession`) |

A failed call shows the server's `code: message` (for example
`invalid_argument: …` or `not_found: …`) without the method and path.

## Diff view

`/diff` opens a full-screen view of the working tree diff: `git diff HEAD`
plus every untracked file, split back into one entry per file. The file list
sits on the left (path and `+N -M`), the highlighted file's colored diff on
the right — same line colors as a tool card's diff (add/remove/hunk).

### Keys

Up/Down, PgUp/PgDn, Home/End, and the mouse wheel scroll the open file's
body. `n` / `p` (or `]` / `[`) move to the next / previous file. `r` reloads
the diff (after editing files outside the TUI, for example). Esc closes the
view; Ctrl+C closes it too and keeps its quit meaning. The help overlay
(`?`, group `diff`) lists the same keys.

With no changes the body says `No changes`; outside a git repository (or
when the backend directory is not one) it says `Not a git repository` — the
same signal the status bar's git branch uses (`GetVcsStatus`), since the
diff route itself does not distinguish the two.

### Diff view interfaces

| Action | Call | Reads |
| --- | --- | --- |
| Open, `r` reload | `GET /v1/vcs/diff?directory=<dir>` | `GetVcsDiffResponse.diff`: one unified-diff text, split client-side on `diff --git` headers into per-file rows (`raw`/`paths` are not sent, so the whole tree's diff is always read; the backend also accepts `paths` to restrict it) |

## MCP servers

`/mcp` opens a full-screen view of every configured MCP server: its
connection state, tool count, and (when failed) its error.

### Keys

Up/Down move the highlight; Enter opens the highlighted server's tool list
(`MCP › <name>`); `c` connects it now, `x` disconnects it; `r` refreshes;
`/` filters by name or state. `a` starts a login for a server that needs one
(`authRequired`): the authorization URL is copied to the clipboard (OSC 52,
the same action `/copy` uses) and shown, then a one-line pop-up takes the
callback code — Enter completes the login, Esc cancels the pop-up only (the
server keeps needing a login). Esc on the list closes the view; Ctrl+C
closes it too and keeps its quit meaning. The help overlay (`?`, group
`mcp`) lists the same keys.

### MCP view interfaces

| Action | Call | Reads |
| --- | --- | --- |
| Open, `r` refresh | `GET /v1/mcp?directory=<dir>` | `McpServerStatus[]` (`name`, `state`, `tools`, `error`, `authRequired`) |
| `c` connect | `POST /v1/mcp/{name}/connect` | `McpServerStatus` |
| `x` disconnect | `POST /v1/mcp/{name}/disconnect` | `McpServerStatus` |
| `a` start login | `POST /v1/mcp/{name}/auth` | `{authorizationUrl}` |
| Code pop-up Enter | `POST /v1/mcp/{name}/auth/complete` | `{code}` → `McpServerStatus` |

## Saved Rules

`/rules` opens a full-screen list of saved permission decisions (the rules a
persisted "always allow" answer writes). Each row shows the effect, the
tool it matches, the pattern, the Project it is scoped to, and how long ago
it was saved, e.g. `allow  bash  git status  global  · 2m ago`. The pattern
column tells apart the three grant shapes the server reports
(`docs/protocol/README.md` "Saved permission rules"): the exact command for
a `bash` grant, `*` for an action-wide grant (every command of that tool, or
every action for a non-`bash` tool), and empty for a tool-wide grant — shown
blank, not folded into `*`, so it stays distinct from an action-wide grant.
`tool` itself falls back to `*` only when the server leaves it empty (not
expected in practice). The project column (`state/rules.ts`
`ruleProjectLabel()`) shows the Project's name when `store.projects` has it,
else the raw `projectId`, and `global` for a rule that applies to every
session and Project (ADR-0026: every rule except an `ExternalDirectory`
grant, which is scoped to the Project it was saved under). The saved time is
relative (`state/catalog.ts` `relativeTime()`,
`state/rules.ts` `ruleTimeText()`: `Ns`/`Nm`/`Nh`/`Nd ago`), `—` for a rule
saved before creation times were recorded.

### Keys

Up/Down move the highlight; `d` asks to confirm, Enter on the confirm line
deletes the rule (`DELETE /v1/permissions/rules/{id}`); `r` refreshes; `/`
filters. Esc cancels a running call, then the pending delete, then the
filter, then closes the view; Ctrl+C closes it too and keeps its quit
meaning. The help overlay (`?`, group `rules`) lists the same keys.

Every saved rule is an "always allow" grant, so the backend reports it as
`allow` with the time it was saved (no time for rules saved before times
were recorded). The same list shows in every directory (`directory` is
accepted and ignored): most rows are global and apply to every session and
Project, but an `ExternalDirectory` grant (ADR-0026) is scoped to one
Project, shown by its `projectId`. Deleting one takes effect at once — the
next matching call asks again.

### Saved Rules interfaces

| Action | Call | Reads |
| --- | --- | --- |
| Open, `r` refresh | `GET /v1/permissions/rules?directory=<dir>` (paginated) | `SavedRule[]` (`id`, `permission`, `tool`, `pattern`, `timeCreated`, `projectId`) |
| `d` then Enter | `DELETE /v1/permissions/rules/{id}?directory=<dir>` | — |

## Agent Models

`/agent-models` opens a full-screen list of every catalog agent's base
model: its mode (`primary`/`subagent`), the effective `provider/model`, and
which tier resolved it (`session`, `configured`, `remembered`, `default`).

### Keys

Up/Down move the highlight. Enter on a `settable` agent opens the shared
model picker (the same one `/model` uses) to choose its remembered default;
`c` clears a set preference. An agent with direct model or category
configuration cannot take a remembered preference — Enter and `c` on it (or
`c` with no preference set) show why instead of acting. `r` refreshes, `/`
filters. Esc closes the view; Ctrl+C closes it too and keeps its quit
meaning. The help overlay (`?`, group `agentmodels`) lists the same keys.

### Agent Models interfaces

| Action | Call | Body | Reads |
| --- | --- | --- | --- |
| Open, `r` refresh | `GET /v1/agent-models?directory=<dir>` | — | `AgentModelState[]` (`agentId`, `mode`, `hidden`, `configured`, `settable`, `preference`, `preferenceAvailable`, `effective`, `source`) |
| Enter → picker Enter | `PUT /v1/agent-models/{agentId}` | `{directory, preference: {providerId, modelId}}` | `AgentModelState` |
| `c` clear | `PUT /v1/agent-models/{agentId}` | `{directory}` (no `preference`) | `AgentModelState` |

## Interface definitions

The frontend uses the existing HTTP/JSON+SSE transport. Every request carries
`x-hya-directory: <absolute --dir path>`; JSON uses protojson lower camel case,
string encoded 64-bit values, and the error envelope documented in the
[protocol guide](protocol/README.md). These are the first-class calls:

| Method and route | Request | Response read by the TUI |
| --- | --- | --- |
| `GET /v1/bootstrap` | No body | `Bootstrap` (`location`, `agents`, `models`, `interactions`) |
| `GET /v1/sessions` | No body | `ListSessionsResponse.sessions: SessionInfo[]` (every session of the directory, subagent sessions included; `parent` nests them in the sidebar and the `/sessions` picker, `busy` marks `· running`, `timeUpdated` feeds the picker's relative time). Re-read with each child-session round (see [Subagents](#subagents)). |
| `POST /v1/sessions` | `{agent: string, model: string, workdir: string}` | `CreateSessionResponse.session: SessionInfo` |
| `GET /v1/sessions/{id}` | No body | `SessionInfo` (including `permissionMode`, read by `/status`; `parent`, which makes the view read-only; `members: MemberInfo[]`, the subagent rows the task cards link to; `usage: TokenUsage`, the status bar's token total, re-read after `tokensRecorded`). For a child session: `busy` and `agent` for its task card. |
| `PATCH /v1/sessions/{id}` | `{title?: string, model?: string, agent?: string, permissionMode?: string}` (`UpdateSession`; `/model`, `/agent`, `/rename`, the `/sessions` picker's F2, and a permission mode switch each send one field; `permissionMode` is `manual`, `yolo`, or `<bundle-id>/<mode-id>`) | `SessionInfo`; after a switch its `permissionMode` is the mode shown. An unknown or unavailable mode fails with `invalid_argument`. |
| `DELETE /v1/sessions/{id}` | No body (`DeleteSession`; the `/sessions` picker's Ctrl+D, confirmed first) | Empty response; the TUI re-reads the session list and, if the deleted session was open, opens the next top-level one. |
| `GET /v1/agents` | No body (`ListAgents`; read with the catalogs and by `/agent`) | `ListAgentsResponse.agents: AgentSummary[]` (`name`, `model`, `description`, `hidden`); the `/agent` picker drops `hidden` rows. |
| `GET /v1/permission-modes` | No body (`ListPermissionModes`; read with the catalogs and by `/permissions`; a `404` from an older backend counts as an empty list) | `ListPermissionModesResponse.modes: [{id, title, description, source}]` — built-ins first; `source` is `builtin` or the bundle id. Feeds the Shift+Tab cycle, the picker rows, and bundle mode titles. |
| `GET /v1/sessions/{id}/messages` | No body | `ListMessagesResponse.messages: MessageInfo[]` (`roundUsage` and `model` of the newest assistant message give the status bar's `ctx N%`); tool cards read `parts[].toolCall` (`ToolCallPart {callId, tool, state, inputJson, outputJson, durationMs, errorCode, errorMessage}`). For a child session: its latest activity. `parts[].attachment` is an `AttachmentPart {name, mime?, path?, size?}` (never the bytes) — see [Attachments](#attachments). |
| `POST /v1/sessions/{id}/compact` | `{}` (`CompactSession`) | `CompactSessionResponse {compactedUntilSeq, strategy}` for `/compact` |
| `POST /v1/sessions/{id}/summarize` | No body (`SummarizeSession`) | `SummarizeSessionResponse {summaryMessage}` for `/summarize` |
| `POST /v1/sessions/{id}/revert` | `{}` (`/undo`) or `{undo: true}` (`/redo`) (`RevertSession`) | `RevertSessionResponse {session, files}`; `SessionInfo.revert` drives the pending-revert line (see [Undo, redo, and fork](#undo-redo-and-fork)) |
| `POST /v1/sessions/{id}/fork` | `{}` or `{messageId}` (`ForkSession`, `/fork`) | `ForkSessionResponse {session, promptText}`; `SessionInfo.forkedFrom` is shown in the sidebar and `/status` |
| `GET /v1/sessions/{id}/todo` | No body (`GetSessionTodo`) | `TodoList.items: TodoItem[]` for `/todos` and to seed the sidebar's `Todos` box when a session opens; `todoUpdated` frames keep it current. |
| `GET /v1/vcs?directory=<--dir>` | No body (`GetVcsStatus`) | `VcsStatus.branch` for the status bar's git branch; read when a session opens and after a turn ends. Never errors on a non-repository directory (`branch` comes back empty, so the segment is omitted). |
| `POST /v1/sessions/{id}/turns` | `{prompt: {text: string, attachments?: PromptAttachment[]}}`; `PromptAttachment {name, mime?, data, path?}`, `data` standard base64 of the file bytes — see [Attachments](#attachments) | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{command: {command: string, arguments: string}}` for other slash commands | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{shell: {command: string, agent: string, model?: {providerId: string, modelId: string}}}` for `!command` (the session's agent and model) | `CreateTurnResponse.turn: TurnInfo` once the command has finished; `id` is the shell turn's assistant message. |
| `POST /v1/sessions/{id}/turns/{turn}/cancel` | `{}` | `TurnInfo`. Esc and `/cancel` send the admitted turn id (the user message id). The server cancels whatever runs in the session, so a shell turn whose id is not known yet is sent as `current`. |
| `GET /v1/fs/find?pattern=**/*<text>*&limit=50` | No body (`FindFiles`, scoped by `x-hya-directory`) | `FindFilesResponse.paths: string[]` (relative paths) for `@file` suggestions. |
| `GET /v1/sessions/{id}` | No body | `SessionInfo.lastSeq` when a session is opened (the stream's first `sinceSeq`). |
| `GET /v1/sessions/{id}/events/stream?sinceSeq=N&includeDescendants=true` | SSE | `StreamFrame` with `event` or `resync`; `N` is the last applied durable seq. `includeDescendants=true` adds the ask frames of every subagent session below (see [Subagent asks](#subagent-asks)). |
| `GET /v1/events/stream?sinceSeq=18446744073709551615` | SSE | `StreamFrame`s of every session, live-only (no durable event passes the watermark); the TUI reads only ask/resolve frames (see [Asks of other sessions](#asks-of-other-sessions)). |
| `GET /v1/sessions/{id}/events?sinceSeq=N&limit=500` | No body | `ListEventsResponse.events` / `nextSeq`, paged, to fill the gap after each stream (re)connect and `resync`. |
| `GET /v1/interactions` | No body (every type, every session; read at start, on a full refresh, after every stream (re)subscribe and `resync`, and after a permission mode switch — never polled) | `ListInteractionsResponse.interactions: Interaction[]`, oldest first. The TUI reads `id`, `session` (the asking session, a subagent's child session included), `type` (`INTERACTION_TYPE_PERMISSION` / `_QUESTION`), `title`, `detail` (a question's header), `options` (a question's option labels), and a permission's `payload`: `action`, `resource`, `always` (what Always allow covers), `callId` (marks the waiting tool card, `◌ … · awaiting approval`), `tool` and `input` (the prompt's details). A listed question has no options or header; the TUI keeps those from its live `questionRequested` frame, else reads them from the waiting `ask_user` call in the transcript. |
| `POST /v1/interactions/{id}/respond` | Prompt: `{permission: {allowed: boolean, persist: boolean}}`, `{question: {answer: string}}`, or `{question: {rejected: true}}`. `/approve`, `/deny`: `persist: false`. | `RespondInteractionResponse.applied` (`false`: already resolved elsewhere) |
| `GET /v1/models` | No body | `ListModelsResponse.models: ModelSummary[]` (`id`, `providerId`, `modelId`, `displayName`, `contextLimit`, `outputLimit`, `reasoning`, `source`, `imageInput`); the `/model` picker tags rows by `providerId`; `contextLimit` (a uint64 string, `0`/absent = unknown) is the status bar's `ctx N%` denominator; the [Provider View](#provider-view) lists a provider's rows with their `source`; `imageInput: false` refuses attachments locally before a turn is sent (see [Attachments](#attachments); absent means unknown and is allowed). |
| `GET /v1/providers` | No body | `ListProvidersResponse.providers: ProviderSummary[]` (`id`, `kind`, `baseUrl`, `keySource`, `auth`, `modelCount`): the Provider View's list. |
| `GET /v1/commands` | No body | `ListCommandsResponse.commands: CommandSummary[]` (includes skills, tagged `source: "skill"`) for slash completion and the command menu. |
| `PUT /v1/providers/{id}`, `POST …/refresh`, `PUT …/models`, `DELETE …/models?modelId=`, `POST …/test` | See [Provider View interfaces](#provider-view-interfaces) | `ProviderUpdate` / `TestProviderModelResponse` |
| `PUT /v1/auth/{provider_id}` | `{apiKey: string}` (Provider View `k`) | `{status, provider?, discovery?}`; the key value is sent only to the backend. |
| `DELETE /v1/auth/{provider_id}` | No body (Provider View `x`) | `{provider?}` |
| `GET /v1/workflows` | No body | `ListWorkflowsResponse.workflows: WorkflowSummary[]` |
| `GET /v1/sessions/{id}/workflow` | No body | `WorkflowState` |
| `POST /v1/sessions/{id}/workflow` | `{select: {name: string}}` or `{run: {name: string}}` | `SubmitWorkflowCommandResponse` |

The one-row footer sits directly below the input panel. Its content is selected
from the current view; it makes no HTTP request (the Provider View draws its
own key line; see [Provider View](#provider-view)):

| View or state | Bottom instruction |
| --- | --- |
| Chat | `Enter a prompt · /new creates a session · /help lists commands · / opens the command menu` |
| Models | `Next: /model <provider/model> to switch this session · /key opens the Provider View · /help` |
| Workflows | `Next: /workflow select <name> or /workflow run [name]` |
| Interactions | `Next: /approve <id>, /deny <id>, or /answer <id> <text>` |
| Chat in a subagent's session | `Read-only subagent view · Esc returns to the parent · click a task card or /open <n> to switch` |
| API | `Next: /api GET /v1/health · /help for command syntax` |
| Help | `Enter a prompt or choose a /command · Tab completes` |
| Todos | `Next: /refresh to reload the list · /help` |
| Status | `Next: /model, /agent, or /rename to change what's shown · /help` |

### Stream frames and the transcript

The server projection (`ListMessages`, `MessageInfo.parts`) is the
authoritative transcript. Stream frames feed a transient overlay that shows
what the projection cannot show yet, mainly the live text of the in-flight
round. The overlay is never persisted and is rebuilt from the stream. The
rules follow the protocol guide's
[Live and durable frames](protocol/README.md#live-and-durable-frames):

| Frame (`StreamEvent` field) | Kind | Effect in the TUI |
| --- | --- | --- |
| `messageStarted {message, role}` | durable | Overlay message with its role; projection re-read (debounced: 120 ms after the last such frame, but at least every 400 ms while frames keep coming). |
| `partStarted {message, part, kind}` (`text`, `reasoning`) | live or durable | Overlay part. A part id the overlay already has is not a new part. |
| `partStarted {message, part, kind: "tool_call", tool, callId}` | durable | Overlay tool part in state `PENDING` (a `○` card). |
| `partAppended {message, part, textDelta}` | live (assistant text) or durable (reasoning, tool arguments, user text) | Appends `textDelta` to the part (for a tool part, to its argument JSON so far). No projection re-read. |
| `toolStateChanged {message, part, callId, state, tool, inputJson, outputJson, durationMs, errorCode, errorMessage}` | durable | Sets the tool part's state and every field the frame carries, keeping the others (`inputJson` replaces the appended fragments). An empty `callId` is a direct part overwrite; the part keeps its call id. A part the overlay never saw is started. Merged with the projection by part id: the overlay's part is shown only while its state is further along (`PENDING` < `RUNNING` < `OK`/`ERROR`). |
| `memberUpdated {member, child, agent, description, status, summary, callId, depth}` | durable (parent session) | Folded into the open session's member rows by `member` (partial frames keep known fields); triggers a child-session round. |
| `partReplaced {message, part, text}` | live (plugin rewrite) or durable (end of round) | Sets the part's whole text, replacing the live deltas. |
| `partCompleted {message, part}` | live or durable | No overlay change; a durable one triggers a projection re-read. |
| `partsAdded {message, parts}` | durable | Appends each `parts[].attachment` to the message as an `attachment` part, skipping any part id already there (a reconnect/gap-fill duplicate); triggers a projection re-read. Sent for a prompt's image attachments (see [Attachments](#attachments)), right after the user message. |
| `errorReported {message, code, errorMessage}` | durable | Stored as the message's error. Shown in the transcript and, at turn end, in the status line. |
| `messageFinished {message, finish, cause}` | durable | The turn ends at the first assistant `messageFinished` after the turn's user message whose `finish` is not `FINISH_REASON_TOOL_CALLS`. Then the projection is re-read. |
| `permissionRequested {interaction}`, `questionRequested {interaction}` | live | The ask is added to the pending list at once (a prompt appears); its options and header are remembered by id. With `includeDescendants=true` a subagent's asks arrive here too (`event.session` = the child): they change only the pending list, never the open session's transcript. Other frames of another session are ignored. |
| `interactionResolved {request}` | live | The ask is removed at once (its prompt closes); also for a subagent's ask. |
| `sessionUpdated {permissionMode}` | durable (root session) | The tree's mode changed (this TUI's switch echoed, or another client's): the open session's `permissionMode` is updated, and a `Permission mode → …` notice is added unless the transcript already announced that mode. |
| `sessionUpdated {title, agent, model}` | durable | Patches the session's row (and, if it is the open one, the header and sidebar) at once — a `/rename`/`/model`/`/agent` from another client, or the backend's auto-generated title (see [Session titles](#session-titles)) — instead of waiting for the next catalog refresh. |
| `compactionApplied {untilSeq, strategy, message, foldedCount, manual}` | durable | Appended to `state.dividers` (once per seq) and spliced into the transcript right before `message`, the summary, or right after the message that was newest at the time until the summary is read (see [Notices](#notices)). A summary message (system role, `HYA_COMPACTED_CONTEXT` first line) without such a divider — a compaction from before the session was opened — gets a derived `── context compacted ──` divider (`state/messages.ts` `withDividers`, id `compaction-<message id>`). |
| `tokensRecorded {message, model, usage}` | durable | With a non-empty `message`: the newest round, the live source of `ctx N%` (`state.liveRound`). Any `tokensRecorded` also re-reads the open session (debounced) for `SessionInfo.usage`. |
| `todoUpdated {items}` | durable | Replaces the sidebar's todo list with `items` (the whole list). |
| `sessionReverted {messageId, undone, files}` | durable | A revert or redo (this TUI's or another client's): the overlay is dropped (it may hold hidden messages) and the session row (`revert`) and the transcript are re-read. A later durable `messageStarted` committed the revert: `revert` is cleared locally (see [Undo, redo, and fork](#undo-redo-and-fork)). |
| `resync {lastSeq}` | — | Live parts that were mid-stream stop taking deltas until their durable `partReplaced`; `ListEvents` fills the gap; the projection is re-read. |

- **Sequence numbers.** The client keeps the last applied durable `seq` as a
  decimal string and compares with `BigInt`, so 64-bit values stay exact. A
  durable frame at or below it is a duplicate and is ignored. Live frames have
  no `seq` and are always applied.
- **(Re)connect.** The stream is subscribed with
  `sinceSeq = last applied seq`. Before any frame is read, `ListEvents` pages
  are replayed through the same fold. The stream does not replay history, so
  this fills the gap. After a reconnect, the projection is re-read too. A
  prompt is admitted only once the stream is subscribed (up to 3 s wait), so
  no frame of its turn is missed.
- **Handover.** The displayed transcript is the projection with the overlay
  merged by message id and part id. The overlay's text wins for a part that is
  still streaming. Overlay parts and messages that are not in the projection
  yet follow the projected ones. A projected message with a `finish` is shown
  exactly as projected, and the overlay drops it in the same store update. The
  live text and the durable text are identical, so the handover does not
  flicker. Projection reads complete in order: an older read never replaces a
  newer one.
- **Turn id.** `CreateTurn` returns the user message id as `TurnInfo.id`. It
  is used for `/cancel` and to find the turn's end. A reply that finishes
  before `CreateTurn` returns ends the turn as soon as the response arrives.
- **Session switch.** Opening a session aborts the old stream and resets the
  overlay, the prompt queue, and the turn state. Frames of any other session
  are ignored.
- **Rendering cost.** Frames are folded at once, but the overlay is published
  to the store at most once per 16 ms. A fast delta stream therefore renders
  about once per display frame, not once per chunk. Each message's view model
  (`state/messages.ts`) is cached per message object; projected messages and
  unchanged overlay messages keep their identity, so a delta rebuilds only the
  view of the message it changed. Messages and their parts are components
  keyed by id: a delta updates the existing Markdown renderable of that part
  instead of recreating it.

List requests follow the server's `page.nextCursor` using the
`page.cursor` and `page.limit` query keys. `GET /v1/auth` is an unpaginated
names-only list. The generic `/api` command sends the supplied JSON unchanged to
the named `/v1` route; its full request and response schemas are in the
[generated API reference](protocol/api-reference.md).
For non-2xx responses with an empty or invalid JSON body, the frontend reports
`METHOD /v1/path: HTTP <status> <status text>`; a structured error envelope
continues to show its code and message.

## Code layout

The frontend is written with [`@opentui/solid`](https://github.com/anomalyco/opentui)
(Solid JSX over `@opentui/core`). `@opentui/core`, `@opentui/solid`, and
`solid-js` are pinned to exact versions in `package.json` and must move
together.

| Path | Role |
| --- | --- |
| `src/main.ts` | Entry. Registers the Solid JSX transform (`@opentui/solid/preload`), parses flags, then dynamically imports the app. |
| `src/cli.ts` | `parseArguments()` (`--server`, `--dir`, `--hya`, `--db`, `--continue`, `--session`, `--help`) and the `usage` text (which also names `HYA_TUI_CONFIG`). |
| `src/prefs.ts` | The TUI preferences file ([Themes — Preferences file](#preferences-file)): `preferencesPath()` (`HYA_TUI_CONFIG`, XDG, home), `loadPreferences()` (never throws; `warning` for an unusable file), `savePreferences()` (merge + atomic rename), `TuiPreferences`. |
| `src/launch.ts` | One-command launch: `resolveHyaBinary()` (`--hya`, `HYA_BIN`, `PATH`), `parseReadyLine()`, `defaultDatabase()`, `startBackend()` (spawn `hya serve`, drain its output, wait for readiness, `stop()` with SIGTERM then SIGKILL), `initialSessionId()` (`--continue` / `--session`), `BackendError`. |
| `src/client.ts` | Typed v1 HTTP/JSON+SSE client (`HyaClient` with `streamSession` and `streamGlobal`, `SseDecoder`, `parseApiCommand`). |
| `src/state/store.ts` | `createAppStore()`: the single store. It holds the server projection (sessions, messages, interactions, models, agents, providers, workflows, backend commands, todos, stream cursor, the open session's subagent members, what was last read about each child session), the published streaming overlay, the prompt queue, the turn state (`running`, `turnId`), and UI state (view, status, the open Provider View's state, sidebar mode, terminal columns, the reasoning switch and per-part toggles, the tool-card switch and per-card toggles, the highlighted prompt option (`promptSelection`, by ask id), whether the input holds text (`draft`), the jump-to-bottom tick, the `/status` text, the backend version from bootstrap, the `/name args` display text of command turns by user message id). Each field is a Solid signal, and only the store's mutation methods change it. |
| `src/state/overlay.ts` | `TranscriptOverlay`: the pure fold of stream frames by message and part id (seq filter, live/durable handover, `resync` handling, turn-end lookup). `mergeTranscript()` merges it over the projection. |
| `src/state/messages.ts` | The transcript view model: `transcriptViews()` (projection + overlay + waiting queued prompts), `messageView()` (role, agent/model, typed blocks, finish notice; cached per message object), `finishNotice()`, `reasoningLabel()`, `reasoningExpanded()`, `toolExpanded()`; transcript notices spliced in by `withDividers()`, including the dividers derived from compaction summaries in the history. |
| `src/state/tools.ts` | The tool-card view model: `toolCard()` (status, per-tool summary, body lines with tones, duration, error, task info), `toolStatus()`, `formatDuration()`, `clipLines()`, `diffLines()`, `partialField()`. |
| `src/state/modes.ts` | Permission modes: `modeCycle()` (Shift+Tab order), `nextMode()`, `requestMode()` and `confirmKey()` (the yolo confirmation state machine), `modeDisplay()` (status bar text and tone), `modeNotice()`, `modeRows()` (picker rows), `effectiveMode()`, `isShiftTab()`. |
| `src/state/picker.ts` | The reusable modal picker's pure state (API below): `createPicker()`, `pickerMatches()`, `pickerRows()`, `pickerHighlighted()`, `pickerKey()`, `pickerWindow()`, and the `PickerRow` / `PickerAction` / `PickerSpec` / `ActivePicker` types; `"rename"`/`"confirm"` row-action modes (F2/Ctrl+D on `/sessions`, [Pickers — Row actions](#row-actions)). |
| `src/state/providers.ts` | The [Provider View](#provider-view)'s pure state: `initialProviderView()`, `providerViewKey()` (screens, filter, busy), the pop-up forms (`addProviderForm()`, `setKeyForm()`, `addModelForm()`, `editModelForm()`, `formKey()`, `formPaste()`, `withSecretLength()`), validation (`validateProviderId()`, `validateBaseUrl()`), row text (`providerLine()`, `modelLine()`, `providerDetailHeader()`, `tokenCount()`, `discoveryNotice()`, `testResultText()`), `providerKeyRows` (footer hint and help), and `defaultModelRef()`. |
| `src/app/providers.ts` | `createProviderController()`: the Provider View's calls (one at a time, Esc aborts), the `SecretEntry` behind key fields, the catalog re-read after every write, and the `/model` prompt after adding a provider while the next turn would run on `hya/offline`. |
| `src/state/catalog.ts` | `/model`/`/agent`/`/sessions` picker row builders: `modelRows()` (tagged by provider), `agentRows()` (visible agents, tagged by default model), `sessionRows()` (the `New session` row + `sessionTree()`, relative time), `relativeTime()`. |
| `src/app/modes.ts` | `createModeSwitcher()`: `cycle()` (Shift+Tab), `request(mode)`, `key()` (the confirmation's keys), `applyPending()` (a mode chosen before any session, sent after `CreateSession`); sends `UpdateSession {permissionMode}`, re-lists interactions, reports in the status line. |
| `src/state/prompts.ts` | Permission and question prompts: `promptQueue()` (asks of the open session's tree), `treeSessionIds()`, `promptView()` (headline, asker, details from `toolCard()`, options), `currentPrompt()`, `promptKey()` (option keys), `respondBody()`, `mergeInteractions()` (listing + live frames + answered ids), `waitingKind()`, `askFrameRoute()` (the session stream) and `globalAskRoute()` (the global stream). |
| `src/app/prompts.ts` | `answerPrompt()`: send a choice's `RespondInteraction`, hide the ask, report the outcome in the status line. |
| `src/state/members.ts` | Subagents: `foldMember()`, `taskLink()` (card → member and child session), `childStatus()`, `childActivity()`, `childSessionIds()`. |
| `src/state/layout.ts` | Sidebar rules: `layoutBreakpoints`, `sidebarVisible()`, `toggledSidebar()`, `sidebarWidth()`, and `parseSwitch()` for `on`/`off` arguments. |
| `src/state/scroll.ts` | `ScrollFollow` (the "new messages below" hint), `atBottom()`, `pageStep()`. |
| `src/state/format.ts` | Pure text for the header, sidebar (session list with `sessionTree()` nesting, context box), pending lines, the status bar (`statusBarSegments()`, `contextUsage()`, `sessionTokens()`, `formatTokens()`), the compaction divider (`compactionText()`), and the non-chat views. |
| `src/app/controller.ts` | `createController()`: refreshes, the session SSE loop (subscribe, `ListEvents` gap-fill, `resync`), the global SSE loop for other sessions' asks (`onGlobalFrame`, backoff), batched overlay flushes, the debounced projection re-read (`app/debounce.ts`), child-session rounds for subagent cards, `returnToParent()`, session creation, prompt submission (refused in a subagent's read-only view), command dispatch, the Provider View (`providerKey`, `providerPaste`, `closeProviders`; app/providers.ts), and `savePreferences` (the `preferencesPath` option; `actions.savePreferences(patch)` for commands). It writes results into the store. |
| `src/app/turns.ts` | `createTurnRunner()`: the client-side prompt queue, `409 session_busy` retry, and turn-end detection and status text. |
| `src/app/revert.ts`, `src/state/revert.ts` | [Undo, redo, and fork](#undo-redo-and-fork): `createRevertController()` (`undo()`, `redo()`, `fork()`, the input prefill rule); `revertSummary()`, `revertIndicator()`, `forkRows()`, `forkSourceText()`, `sessionRow()` (a fresh session row over the open one, dropping a `revert` it no longer has). |
| `src/app/App.tsx`, `src/app/run.tsx`, `src/app/context.ts` | Root layout (main column + sidebar), startup (the started backend, the preferences file and saved theme, then the renderer) and the single `shutdown()` every exit path runs (restore the terminal, stop the backend, exit), and the `AppContext` (store, controller, server URL, and `ui` handles such as the transcript's scroll actions) that components read with `useApp()`. |
| `src/components/` | `Header`, `MainPanel` (transcript or view panel), `Transcript` (scrollbox, follow/hint), `MessageView` (`MessageItem`, user/assistant messages, blocks, reasoning, tool cards and `task` subagent cards, `KeyedFor`), `Spinner` (the shared spinner clock), `Markdown` (the `<markdown>` wrapper, `SyntaxStyle`, code-block boxes), `Panel`, `PendingBlock` (other sessions' asks), `PromptDock` (the permission / question prompt), `ModeConfirm` (the one-line yolo confirmation), `Picker` (the modal picker), `ProviderView` (the full-screen Provider View and its pop-up forms), `Sidebar`, `StatusLine`, `Composer` (the `<textarea>` editor, its height, history, Esc / Ctrl+C / Ctrl+D, the shell-mode border, the `@file` list, the `/` command menu, Tab completion, key actions, routing keys and pastes to an open Provider View, the [vim mode](#vim-mode) adapter, the Ctrl+X chord), `selection.ts` (`paintSelection`, the theme's mouse-selection color; [Copy](#copy)), `Footer`. |
| `src/composer/` | Pure composer logic: `history.ts` (`InputHistory`), `quit.ts` (`createQuitGuard`, the Ctrl+C double press), `escape.ts` (`escapeAction`), `shell.ts` (`shellCommand`, `isShellInput`), `mention.ts` (`mentionAt`, `insertMention`, `findPattern`, `rankPaths`), `vim.ts` (`vimKey`, the [vim mode](#vim-mode) state machine), `editor.ts` (`editText`, `editorCommand`, `splitCommand`; [External editor](#external-editor)), `clipboard.ts` (`copyNotice`; [Copy](#copy)). |
| `src/commands/` | The slash-command registry (`registry.ts`), the built-in commands (`native.ts`), the key and command help (`help.ts`: `helpRows()`, `helpPickerRows()`, `composerKeyLabel()`, `keyHelpText()`, generated from the binding tables), and the command menu's merge/fuzzy-filter/argument-hint logic (`menu.ts`: `mergeCommandEntries`, `filterCommands`, `requiresArgument`). |
| `src/keys/bindings.ts` | The global key binding table (`keyBindings`, including `cycleMode` on Shift+Tab / CSI Z) and the textarea overrides (`composerKeyBindings`: Enter submits; Ctrl+J, Shift+Enter, Alt+Enter insert a newline; Home/End). |
| `src/completion.ts`, `src/instructions.ts`, `src/api.ts`, `src/theme.ts` | Tab completion and `SecretEntry` (the Provider View's key fields), footer instructions, the `/api` operation catalog (reads `src/operations.json`, generated by `gen-api` so the package ships without the repository's docs; `test/api-catalog.test.ts` checks it matches `docs/protocol/openapi.json` and that no source file imports from outside the package), and the themes: the reactive palette (`colors`, `toolColors`, `diffColors`, `syntaxColors`), `themes`, `themeName()`, `currentTheme()`, `setTheme()`, and `syntaxStylesFor()`, the Markdown/tree-sitter scope styles ([Themes](#themes)). |

The Solid transform has two parts. `bunfig.toml` preloads
`@opentui/solid/preload` for `bun test` and for `bun src/...` run inside the
package. `tsconfig.json` sets `"jsx": "preserve"` and
`"jsxImportSource": "@opentui/solid"`. Bun reads `bunfig.toml` only from the
directory it runs in, so `src/main.ts` imports the preload itself. It then
loads `.tsx` modules and `solid-js` with a dynamic `import()`. Keep static
imports in `main.ts` free of Solid code. Without the preload, Bun resolves
`solid-js` to its non-reactive server build.

**The picker.** `components/Picker.tsx` is a reusable modal list for
choosing one value (`/permissions`, `/model`, `/agent`, and `/sessions` all
use it). Open one from a command handler with `actions.openPicker(spec)`
(or `controller.openPicker`):

```ts
interface PickerRow {
  id: string          // value handed to onSelect (a mode id, model id, session id, …)
  label: string       // main text
  detail?: string     // muted text after the tag (a description)
  tag?: string        // shown as [tag] (a source, a provider, a kind)
  current?: boolean   // the value in effect: marked ●, highlighted when the picker opens
}
interface PickerAction {
  id: string           // outcome id passed to onAction, e.g. "rename", "delete"
  key: string           // OpenTUI key name, e.g. "f2", "d" — never a plain printable character
  ctrl?: boolean
  label: string         // hint text, e.g. "F2 rename"
  prompt: "value" | "confirm"   // "value" edits the row's label inline; "confirm" shows a yes/no line
  confirmText?: string  // "confirm" only; "{label}" is replaced by the row's label
}
interface PickerSpec {
  title: string       // box title
  rows: PickerRow[]
  hint?: string       // bottom row; default "↑↓ select · Enter chooses · Esc closes · type to filter"
  actions?: PickerAction[]   // row actions on the highlighted row (S9: /sessions F2/Ctrl+D)
  onSelect(row: PickerRow): void | Promise<void>   // runs after the picker closed; a throw shows "Error: …"
  onAction?(id: string, row: PickerRow, value?: string): void | Promise<void>   // after a row action committed
  onHighlight?(row: PickerRow): void   // the highlight moved to another row (a live preview, /theme); not on open
  onCancel?(): void                    // closed without a choice (Esc, Ctrl+C); undo a preview here
}

actions.openPicker({
  title: "Permission mode",
  rows: modeRows(modes, effectiveMode(store.state)),
  onSelect: (row) => actions.requestPermissionMode(row.id),
})
```

While a picker is open (`store.state.picker`), the composer's editor is
unfocused and its key handler sends every key but Ctrl+C to
`controller.pickerKey()`, which applies `pickerKey(state, key)` from
`state/picker.ts`: printable characters extend the filter (Backspace
shortens it, Ctrl+U clears it; an empty filter highlights the current row
again), Up/Down and Shift+Tab/Tab move with wrap-around, Enter selects, Esc
closes. At most `pickerMaxRows` (10) rows show; the window follows the
highlight. A click on a row selects it (`controller.choosePickerRow`).
Ctrl+C closes the picker and keeps its quit meaning. Selecting or closing
returns the focus to the input. When a key moves the highlight to another
row (`pickerHighlighted(state)` changes), the controller calls
`spec.onHighlight(row)`; Esc and Ctrl+C (`controller.closePicker`) call
`spec.onCancel()`, a selection or a committed row action does not. `/theme`
uses the pair for its live preview.

A key matching one of `spec.actions` switches the picker into `"rename"`
(`prompt: "value"`: an editable line seeded with the row's label; Enter
commits, Esc returns to the list) or `"confirm"` (`prompt: "confirm"`: a
one-line yes/no; Enter commits, Esc returns to the list) mode instead of
extending the filter. Committing (`controller.pickerKey()` sees a `"commit"`
outcome) closes the picker and runs `spec.onAction(id, row, value)`, the
same way selecting runs `onSelect`; the handler can call
`actions.openPicker` again to keep browsing (`/sessions`' F2 does, so a
rename reopens the picker on the updated row).

To add a slash command, add a `CommandSpec` to `nativeCommandSpecs` in
`src/commands/native.ts`:

```ts
{
  name: "/title",
  description: "Rename the current session",
  argumentHint: "<text>",
  complete: ({ words, current, head }, context) => [],   // optional argument completion
  run: async ({ store, client, actions }, { args, argumentsText }) => { /* … */ },
}
```

The name becomes Tab-completable and appears in the command menu and the
help overlay automatically (source `[local]`; it wins a name clash with a
backend command). An `argumentHint` written `[in brackets]` is optional (the command
menu's Enter runs it as is); anything else is treated as required (Enter
completes the name and waits). Add a row to the command table above. Unregistered `/names` still go to the backend as
`CommandTurn`s (custom commands and skills; see
[Skill commands](#skill-commands)). To add a key, append a
`KeyBinding` to `src/keys/bindings.ts`, handle its action in
`components/Composer.tsx`, and give the action a help group in
`src/commands/help.ts` (`actionGroups`; the build fails until it has one). A binding's `matches(key, context)` may depend on
`context.composerEmpty` (plain Home/End scroll only while the input is
empty). Do not bind a core action only to a
browser-reserved shortcut (see `docs/tui-web.md`). The renderer runs with
`exitOnCtrlC: false`; Ctrl+C is the composer's `quit` action (the double
press in `src/composer/quit.ts`). Editing keys of the input are
`composerKeyBindings`, merged over OpenTUI's textarea defaults by key.

## Verify locally

```sh
cd packages/hya-tui
bun install --frozen-lockfile
bun run typecheck
bun test
```

Then check the rendered TUI in the browser from `packages/hya-tui-web`
(`bun run typecheck && bun test ./test && bunx playwright test`; see
[tui-web.md](tui-web.md)). `e2e/hya-tui.spec.ts` and
`e2e/hya-tui-commands.spec.ts` cover the layout, colors, commands, key
entry, narrow widths, and Ctrl+C. `e2e/hya-tui-layout.spec.ts` covers the
main column and sidebar at the default viewport and at about 80 columns
(Ctrl+B, `/sidebar`), the prompt dock, and the pending block of another session's ask. `e2e/hya-tui-messages.spec.ts`
covers user and assistant styling, Markdown and code highlighting, reasoning
(Ctrl+O, `/thinking`, click), error, length, and cancel notices, and
scrolling (PgUp/PgDn, End, Ctrl+End, the wheel, the new-messages hint).
`e2e/hya-tui-streaming.spec.ts` uses the fake model to cover streaming text,
queued prompts, and the turn status line (`Ready`, provider errors).
`e2e/hya-tui-commands-menu.spec.ts` covers the `/` command menu (open,
fuzzy filter, sources, Up/Down, Tab, Esc, Enter's argument-hint rule), skill
commands (a fixture `SKILL.md` under `.hya/skills/<name>/`), `/compact`,
`/rename`, and `/status`. `e2e/hya-tui-tools.spec.ts` covers tool cards (read, bash, edit/write diff
colors, a failed call, the running spinner, Ctrl+G, `/tools`, a click) and a
`task` subagent card (child status and activity, sidebar nesting, the
read-only child view, Esc back, `/open`), also at about 80 columns.
`e2e/hya-tui-composer.spec.ts` covers the composer:
Ctrl+J / Alt+Enter newlines and box growth up to 8 rows, Shift+Enter in the
browser, bracketed paste, cursor editing, history, Esc (clear, and cancel of
a hanging fake-model turn), Ctrl+C once and twice, Ctrl+D, `/exit`,
`!echo hello` (the shell indicator, running without a prompt, then its
output), `/approve <id>` answering a model's pending bash ask (the waiting
card), and `@file` suggestions at the default width and about 80 columns.
`e2e/hya-tui-prompts.spec.ts` covers the permission and question prompts
under the default permission model: a bash ask (`1`, arrows + Enter, typed
digits going to the input), Always allow (`2`, a second identical call runs
without asking), Deny (`3`) and Esc, an edit ask's diff, two queued asks
(`1 of 2`), `ask_user` options, a free-text answer, and Reject, a subagent's
ask in the parent view (its task card and sidebar row waiting), a
`!command` shell turn that shows no prompt, and about 80 columns.
`e2e/hya-tui-permission-modes.spec.ts` covers permission modes: Shift+Tab
through xterm.js, the yolo confirmation (Esc, Enter, no second ask), the
status bar colors, the transcript notice, a bash call under `yolo` without a
prompt and under `manual` with one, a pending ask closed by switching to
`yolo`, the `/permissions` picker (sources, filter, Up/Down, Shift+Tab, Esc,
focus back to the input, `/permissions <mode>`), Shift+Tab in the command
menu, a mode chosen before the session exists, a bundle mode from a project
bundle whose Bun `permission.approve` hook allows `echo` and defers `ls`,
`!command` shell turns running without a prompt in `manual` and `yolo`,
and about 80 columns.
`e2e/hya-tui-launch.spec.ts` covers the one-command launch: no `--server`
(`HYA_BIN` = the binary under test, an isolated HOME/XDG from the
`workspace` fixture), a prompt end to end, `/exit` and a closed tab
(SIGHUP) leaving no `hya serve` process (checked by pid), `--continue`, a
fresh start, a missing binary, and a server that fails to start.
`e2e/hya-bare.spec.ts` runs bare `target/debug/hya --port <free port>` on the
host's PTY (packages found in the workspace): the terminal TUI shows
`WebUI http://127.0.0.1:<port>` (also at about 80 columns), a second page at
that address runs a TUI on the same server (a session from the terminal is
in its `/sessions`), a busy port shows the `WebUI unavailable` notice while
prompts still work, `/exit`, SIGTERM, and SIGHUP (closed tab) leave no web
host, tab TUI, or server behind (checked by pid and port), the log file
holds the server and web host lines, and a missing TUI package or Bun exits
1 with a clear message.
`e2e/hya-tui-help.spec.ts` covers the help overlay (`?`, `/help`, filter,
Esc, `?` inside text, about 80 columns). `e2e/hya-tui-theme.spec.ts` covers
`/theme` (rows, the live preview, Esc restoring, Enter saving to a
`HYA_TUI_CONFIG` temp file, a restarted TUI starting in the saved theme, an
existing transcript repainting, about 80 columns); the `hya.ts` `tui`
fixture points every TUI at the backend's isolated config directory unless
a spec sets `HYA_TUI_CONFIG`, so a developer's own `tui.json` never changes
a spec's colors. `e2e/hya-tui-status.spec.ts` also
covers `ctx N%` and the token total (fake-model usage and a
`contextLimit`), `todoUpdated` (no todo re-read, through the logging proxy
in `e2e/proxy.ts`), and the `/compact` divider, live and after reopening the
session (a new TUI with `--session`, `/new` then `/open`, about 80
columns); `e2e/hya-tui-prompts.spec.ts`
checks that a subagent's ask arrives on the `includeDescendants` stream
with no interactions listing in between, and that an ask of a session run
headless over the HTTP API (`hya.ts` `headlessTurn`) shows live in the
pending block with its session, then `/open <n>` answers it there (default
and about 80 columns); `e2e/hya-tui-notifications.spec.ts` checks that ask's
single desktop notification.
