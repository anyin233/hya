# OpenTUI frontend

The `packages/hya-tui` frontend is the terminal client of hya. Running
`hya` in a terminal starts it together with the WebUI, both connected to the
database's backend daemon, which `hya` starts when none runs (see
[Start it](#start-it)); from a source checkout it can also be run directly, or
connect to a server you name with `--server` or directly to its gRPC listener
with `--grpc`. It uses OpenTUI for
display and input while the backend remains the owner of sessions, event
history, tool execution, and permissions. The default screen places Projects
on the left, the conversation and its input in the middle, and separate
Sessions, Todos, and Context panes on the right. These panes, the compact
Context line, and the full-screen `/project` view are provided by the trusted
first-party `hya/basic-tui-components` bundle; behavior is unchanged when it is
running. The full layout is editable, and the side panes follow terminal width
unless pinned (see [Layout](#layout)).
Assistant replies render as Markdown with
highlighted code blocks; reasoning is collapsed to one `Thinking` line; each
tool call is a borderless block with its tool name shown as a plain heading in the
normal foreground color, its state, a one-line summary, and an expandable
body; a subagent's `task` card shows the child's status and opens its session
read-only (see [Messages](#messages)). When the agent or one of its subagents
needs a permission decision or asks a question, a prompt docked above the
input shows the call and its options; press `1`, `2`, or `3` (see
[Permission and question prompts](#permission-and-question-prompts)).
`/permissions` switches the session's permission mode
(`manual`, `yolo`, or a mode an installed bundle provides); `/permissions`
shows the mode in effect (see [Permission modes](#permission-modes)).
Models and Workflows have dedicated views, and `/key` opens the full-screen
[Provider View](#provider-view) (providers, keys, model lists, model tests,
and model metadata); the API command view exposes the other HTTP/JSON operations
in `hya.v1`. The message composer is a multi-line editor with its own history;
it also runs `!command` shell turns and completes `@file` references. `/` on
an empty composer opens a separate [command pane](#command-pane), with its
own input and history. Tab completes commands from the TUI and server
catalogs. One persistent instruction line
stays below the input at the bottom of the screen and changes with the
current view. `?` on an empty input (or `/help`) lists every key and
command (see [Key help](#key-help)).

## Start it

Run `hya` in a terminal. It connects to the **backend daemon** of the
database (starting one when none runs), then starts the WebUI on
`http://127.0.0.1:3250` (`hya --port <N>` picks another port, `0` a free one)
and this TUI attached to the terminal. The TUI and every WebUI tab use the
same daemon, database, and workspace directory, so each sees, and can
resume, the sessions the other started. Quitting the TUI stops the WebUI; the
daemon keeps running for the next start (`hya serve stop` stops it,
[ADR-0023](adr/0023-persistent-backend-daemon.md)). Bare `hya` needs Bun and
finds the TUI under `lib/hya/tui` next to the binary (a release archive or
`install.sh` puts it there) or in the source checkout it was built from. See
[Bare `hya`](cli.md#bare-hya) for the lookup order, the log files, `--backend`,
and signals.

```sh
hya                   # TUI + WebUI on http://127.0.0.1:3250
hya --port 8000       # WebUI on another port
hya serve status      # the daemon both use
```

The sidebar's `Context` section and the explicitly opened `/status` view show the
WebUI address. If the WebUI could not start (for example because its port is
taken), `/status` shows the reason. Conversation has no persistent connection
or metadata banner. `/status` shows the daemon as
`Backend     daemon · pid <pid> · db <db> · started <N>m ago`.

### Run it with Bun (development)

For development, or to attach to a server elsewhere, run the TUI directly
with Bun 1.4.2 (the version the repository pins; the Solid setup is verified
on it) in a terminal supported by OpenTUI. From a clone:

```sh
cd packages/hya-tui
bun install --frozen-lockfile
```

If `node_modules` lives on another disk, keep `node_modules` in its resolved
path. OpenTUI's Bun preload recognizes Solid's client renderer by that path;
a link to a directory named only `hya-tui-node-modules` can load Solid's server
renderer and leave the terminal blank. For a fresh checkout, run this from the
repository root before `bun install`:

```sh
mkdir -p "$HOME/data/hya-tui/node_modules"
ln -s "$HOME/data/hya-tui/node_modules" packages/hya-tui/node_modules
(cd packages/hya-tui && bun install --frozen-lockfile)
readlink -f packages/hya-tui/node_modules
```

The last command should end in `/node_modules`. The TUI command and its
`--server` and `--grpc` options work the same way with this setup; no server
API or configuration field changes. If WebUI dependencies also live on another
disk, use the same layout for `packages/hya-tui-web/node_modules` (for example,
`$HOME/data/hya-tui-web/node_modules`) so its TypeScript imports resolve.

Then, from the repository root, one command starts the TUI (and, if none
runs, the backend daemon):

```sh
cargo build -p hya-backend --bin hya        # once; or put a released hya on PATH
HYA_BIN=target/debug/hya bun packages/hya-tui/src/main.ts --dir "$PWD"
```

Without `--server` or `--grpc` the TUI uses the backend daemon of its database
([ADR-0022](adr/0022-one-writer-per-database.md),
[ADR-0023](adr/0023-persistent-backend-daemon.md)):

1. It reads `<db>.server.json` next to the database (`{url, pid, version,
   startedAt}`, written by the server) and uses that server when the pid is
   alive and `GET <url>/v1/health` answers `ok`. Any server of the database
   counts: a daemon, a `hya serve --db` you run yourself, another bare `hya`'s.
2. Otherwise it finds the `hya` binary and runs `hya serve start --json
   --db <db>` in `--dir` (a relative `--db` resolves against it) with the
   TUI's environment. That starts `hya serve` detached in your home directory
   (its own session; output to `<db>.server.log`), waits until it
   answers, and prints where it is (see [Backend daemon](cli.md#backend-daemon)).
   Two TUIs that start at the same moment end up on one daemon: the database
   lock lets only one start.

The TUI never stops the daemon: Ctrl+C twice, Ctrl+D, `/exit`,
`/to-background`, and a signal (SIGINT, SIGTERM, SIGHUP, which is also what
the WebUI host sends when its browser tab closes) quit only the TUI (what
each does with the open session: [Quit and keep running, or
archive](#quit-and-keep-running-or-archive)). So a second TUI on the same database
shares the first one's sessions and live events (streamed turns, renames,
asks), and the next start is instant. `/status` shows
`Backend     daemon · pid <pid> · db <db> · started <N>s ago`.

The binary is looked up in this order:

1. `--hya <path>`
2. the `HYA_BIN` environment variable
3. `hya` on `PATH`

A path given by `--hya` or `HYA_BIN` must exist; the TUI does not fall back
to the next source then. If no binary is found, or the daemon cannot be
started (it exits with an error, or does not answer within 60 s), the TUI
prints the reason and the last lines of the output, and exits with status 1
before it takes over the terminal, for example:

```text
hya-tui: could not reach or start the hya server: hya serve start exited with code 1
--- hya serve start output (last lines) ---
Error: hya serve (daemon) exited with exit status: 1 before it was ready; see /home/me/.local/state/hya/sessions.db.server.log
```

To use a backend you run yourself (another machine, a shared server, or a
custom `hya serve` command line), pass its URL:

```sh
cargo run --locked -p hya-backend --bin hya -- serve --bind 127.0.0.1:8080 --db "$HOME/hya-sessions.db"
bun packages/hya-tui/src/main.ts --server http://127.0.0.1:8080 --dir "$PWD"
```

Add `--db` (the database behind that URL) to let the TUI fall back to that
database's daemon: at start when the URL does not answer, and later when the
server goes away. Bare `hya` passes `--server <daemon url> --db <db> --hya
<hya>` to every TUI it starts, WebUI tabs included, so a new tab still
connects after the daemon was restarted on another port. `/status` shows
`Backend     daemon · pid <pid> · via --backend/--server` for a `--server`
without `--db`.

To use the same `hya.v1` server over gRPC, pass the listener's `HOST:PORT`.
The server accepts gRPC on its main HTTP port (h2c); `HYA_GRPC_BIND` can add
another listener. This is useful when a client needs the gRPC transport while
keeping the same sessions, events, and permissions. For example, in two
terminals from the repository root:

```sh
HYA_GRPC_BIND=127.0.0.1:22104 hya serve --bind 127.0.0.1:22103 --db "$HOME/hya-sessions.db"
bun packages/hya-tui/src/main.ts --grpc 127.0.0.1:22104 --dir "$PWD"
```

`--grpc` takes only a host and port (an optional `grpc://` prefix is accepted).
It cannot be combined with `--server` or `--db`; the TUI connects to that
fixed listener and never starts a local daemon. `--dir`, `--session`,
`--continue`, and `--resume` work as with `--server`. Requests use the
`hya.v1` RPC matching each operation in the [v1 API catalog](protocol/api-reference.md);
responses are the corresponding protobuf messages. The two live event calls
are `Events.StreamSessionEvents` with `{session, since_seq,
include_descendants}` and `Events.StreamGlobalEvents` with
`{interactions_only: true}`. See the [protocol guide](protocol/README.md)
for the message and event payload definitions. `/connect-remote` can still
switch this TUI to an HTTP relay bridge during the session.

| Flag | Meaning |
| --- | --- |
| `--server <url>` | Base HTTP URL of a running `hya serve`. Without it the TUI uses the database's daemon. |
| `--grpc <host:port>` | Direct hya.v1 gRPC listener (h2c). Mutually exclusive with `--server` and `--db`; no local daemon is started. |
| `--dir <path>` | Workspace directory: the TUI makes the Project that contains it active at start (see [Projects](#projects)), new sessions of that Project work in it, and it is the `directory` scope of every scoped request. A daemon the TUI starts does not run in it: it starts in your home directory (the backend has no working directory of its own). Default: the TUI's working directory. |
| `--hya <path>` | `hya` binary that starts the daemon and that `/connect-remote` runs `hya bridge` with (first in the lookup order above). |
| `--db <path>` | SQLite database whose daemon to use, relative to `--dir`. Default without `--server` or `--grpc`: `$XDG_STATE_HOME/hya/sessions.db`, else `~/.local/state/hya/sessions.db` — the store `hya sessions` reads, so sessions survive restarts. With `--server`: the database behind that URL; the TUI falls back to its daemon when the URL does not answer or the server goes away. |
| `-c`, `--continue` | Open the most recently updated top-level session of the Project that contains `--dir` that is not archived, whatever its workdir inside the Project (subagent sessions are opened from their parent). Unlike a plain launch, it never reopens an archived session. |
| `--remote` | The backend runs on another machine, so `--dir` names nothing there: start without an active Project (and without a new session). The first prompt or `/new` is refused until a Project is chosen; a temporary session needs none. |
| `--server-label <text>` | Show this text instead of the server URL in the sidebar `Context` section and `/status` (`Server      <text> · via <url>`). Bare `hya --connect` passes `remote: <relay>/<room>`, because `--server` is then only the local relay bridge's loopback address ([relay.md](relay.md#connecting-from-a-client)). |
| `-s`, `--session <id>` | Open that session. Cannot be combined with `--continue`. |
| `--resume [id]` | Open that session and unarchive it (`PATCH {archived:false}`). Without an id (the next argument starts with `-`, or there is none), open a picker of the active Project's top-level sessions (every session without an active Project), archived ones included and tagged `[archived]`, newest first; Enter resumes (and unarchives) the highlighted one, Esc starts a new session instead. Cannot be combined with `--continue` or `--session`. |
| `--web-tab` | This TUI runs in a WebUI tab: `/to-background` is not offered and Ctrl+D only shows `Close the tab to leave this session running` (closing the tab already leaves the session running). Bare `hya` adds it to its web host's tab command; pass it yourself in the command of a web host you start by hand (see [tui-web.md](tui-web.md#usage)). |
| `--web-url <url>` | Show this WebUI address in the sidebar `Context` row and `/status`. Bare `hya` passes it; an HTTP(S) URL. |
| `--web-error <reason>` | Show `WebUI unavailable: <reason> · hya --port <N>` in `/status`. Bare `hya` passes it when the WebUI could not start. Cannot be combined with `--web-url`. |
| `-h`, `--help` | Print the flags and the binary lookup order. |

### Sessions on start and exit

Without `--continue`, `--session`, or `--resume`, a local TUI opens the
active Project's most recently updated saved conversation, including one
archived by `/exit`. A conversation with a pending permission or question
request takes priority over a newer saved chat, so you see its transcript
and the numbered answer choices immediately. The TUI unarchives the opened
conversation. If this Project has no saved conversation, it creates a new
session with the default agent and model (without any model, the first prompt
creates it instead). A `--remote` start without an active Project creates
none: it opens the [Project view](#project-view) instead. Type `/new` for a
fresh conversation; `/sessions` shows prior chats, including archived ones.

The TUI code can live in a different checkout from the Project whose history
you want. Set `--dir` to the **Project work directory**, not the directory
containing the frontend source. For example:

```sh
bun /path/to/hya/packages/hya-tui/src/main.ts \
  --server http://127.0.0.1:8080 --dir /path/to/project
```

Empty sessions do not
pile up: the TUI creates that session (and every `/new` one) *ephemeral*,
and the backend daemon deletes it about 5 s after no TUI shows it any more
while it is still unused — after `/new`, `/open`, `/sessions`, a `/fork`
switch, `/exit`, a WebUI tab closing, or the TUI being killed. The first
prompt or `!` shell command (from any client), a title (a `/rename`, or the
automatic title after a first prompt), archiving, or a `/fork` from it keeps
it for good. A session another TUI still shows is never deleted, whichever
TUI created it, and quitting never waits for a delete (see
[Ephemeral sessions](protocol/README.md#ephemeral-sessions) and ADR-0023).
Other TUIs drop its row when the daemon deletes it.

Two TUIs on the same database share one daemon, so they see the same
sessions live; give one `--db` for a separate store. The backend's offline
echo model is sufficient for a first run; configure a provider in the backend
for live model calls.

Type a plain prompt and press Enter. The prompt is admitted as a turn of the
open session and the reply streams into the transcript as it arrives (see
[Streaming, queued prompts, and turn status](#streaming-queued-prompts-and-turn-status)).
For example, type `summarize this repository`, then `/models` to inspect
available routes, and `/open 1` to return to the first session. Press Ctrl+C
twice (or type `/exit`) to quit and archive the session, or Ctrl+D on an
empty input (`/to-background`) to quit and leave it running. Next time,
Plain launch opens the saved conversation again, `--resume` offers a picker
instead (archived or not), and `--continue` picks up the newest one that is
not archived:

```sh
HYA_BIN=target/debug/hya bun packages/hya-tui/src/main.ts --dir "$PWD" --resume
```

### Quit and keep running, or archive

How you leave a TUI decides what happens to its open session on the
backend daemon (ADR-0023). Archiving is only a flag
([Archived sessions](protocol/README.md#archived-sessions)): it hides the
session from the default list, the sidebar, and `--continue`, and never
cancels a running turn, which finishes on the daemon.

| Way out | Open session |
| --- | --- |
| Ctrl+C twice, `/exit`, `/quit` (graceful) | Archived at once (`PATCH /v1/sessions/{id} {"archived": true}`); in a subagent's read-only view, its root session is archived. |
| Ctrl+D on an empty input, `/to-background` (terminal only) | Left as is: it keeps running on the daemon, not archived. |
| Closing a WebUI tab, SIGTERM/SIGHUP/SIGINT, a kill or crash | Left as is: it keeps running on the daemon, not archived. |
| Switching sessions (`/new`, `/open`, `/sessions`, `/resume`, a `/fork` switch) | Not an exit: the previous session keeps running. |

In every case a session that is still unused (no prompt, title, archive,
or fork yet) is never archived: the daemon deletes it once no TUI shows it
(the rule above), so quitting sends nothing for it and does not wait. The
graceful exit waits at most 2 s for the archive of a used session.

In a WebUI tab (`--web-tab`) `/to-background` is not offered (it is left
out of the command pane's suggestions, completion, and `/help`); typing it
there, or Ctrl+D on an empty input, shows
`Close the tab to leave this session running` and
does not quit. Closing the tab already does that.

To come back to a session, archived or not: `--resume [id]` at start, or
`/resume [id]` in a running TUI (the same picker; this is how a WebUI tab,
which cannot pass flags, resumes). Both unarchive the session and open it,
and say `Resumed <title>`. The terminal TUI and WebUI tabs of one database
share its daemon, so each resumes the other's sessions. The `/sessions`
picker shows archived sessions by default; Ctrl+A hides them (see
[Row actions](#row-actions)). Sending a prompt into an archived session
(for example one another client archived while it was open here) unarchives
it on the backend as well.

### Sidebar live updates

The sidebar's `Sessions` box (and an open `/sessions` picker, see
[Pickers](#pickers)) stays current without polling: every root session's
list-affecting change reaches every TUI over the global stream (unfiltered
or `interactionsOnly`), following the protocol guide's
[Session list push](protocol/README.md#session-list-push). At global-stream
open the TUI lists sessions (respecting the sidebar's own default, which
hides archived ones), then folds frames by `event.session`:

| Frame | Effect |
| --- | --- |
| `sessionStarted {agent, model, workdir}` | A session this TUI has not listed (another client's, a fork, a headless writer): the frame carries too little to build a row cheaply (no title yet), so the list is re-read, debounced with the same 120 ms/400 ms rule as the projection re-read. |
| `sessionUpdated {title\|agent\|model\|permissionMode}` | Patches the row in place (`state/store.ts` `patchSessionRow`); an id not listed yet re-lists instead. |
| `sessionUpdated {archived}` | An archived session leaves the list, except the open one, which stays with `· archived` after its agent (`applyArchived`); an unarchived one is marked back, or re-listed if it was missing. |
| `sessionUpdated {busy}` (live-only) | Patches `busy` on the row (`· running`), independent of any turn this client admitted. |
| `sessionDeleted {}` (live-only) | Drops the row. If it was the *open* session — deleted by another client, not by this TUI's own `/sessions` Ctrl+D (`deleteSession` marks its own deletes so this echo is not mistaken for one) — a status notice (`Session <id> was deleted elsewhere; opened a new session`) is shown and a fresh session opens, like `/new`: the TUI is never left pointed at a session with no log behind it. |

A `resync` on the global stream means these frames were lost: sessions (and
pending interactions, as before) are listed again. This complements
[Session titles](#session-titles) (title/agent/model on the *open*
session's own stream) and the archived-marking above: `patchSessionRow`
never touches `state.selected` — the open session's transcript and its
`permissionMode` notice stay the own stream's job — so a frame that reaches
both streams (a root session's own change, echoed on the global stream too)
is applied twice, harmlessly (each setter is an idempotent "set to this
value", not a counter).

Open `/model`/`/permissions` pickers and the Agents view already read live state
(`store.state`) each time they render. `/sessions` and `/resume` snapshot
their rows when they open (`sessionRows`/`resumeRows` over `store.state` or
a fresh `ListSessions`) and do not repaint while held open; re-opening them
(closing with Esc and pressing `/sessions` or `/resume` again) picks up
every change made meanwhile. The row is small enough, and reopening cheap
enough, that this was chosen over patching an open picker's rows in place.

### When the server goes away

The daemon can stop under a running TUI: `hya serve stop` or `restart`, a
signal, a crash, or a machine sleep that killed it. A stopping server ends
every event stream and answers `GET /v1/health` with `503 unavailable`; the
last frame of each stream says why (`serverStopping {reason}`, see
[Server shutdown](protocol/README.md#server-shutdown)). When a stream ends or
fails, the TUI probes the server twice, 500 ms apart; a server that still
answers was a blip, and the stream just reconnects (with the usual backoff).
A TUI that knows its database (started without `--server` or `--grpc`, or with `--db`)
then acts on the reason:

| Reason | What the TUI does | Internal transition text |
| --- | --- | --- |
| `stop` (`hya serve stop`) | Starts nothing. It is *stopped*: prompts and `!` commands are refused (`Not sent · the backend is stopped (hya serve stop) · /reconnect starts it again`), the Context pane shows `backend stopped` (error color), and slash commands such as `/reconnect`, `/exit`, and `/help` still work. While the streams keep retrying it only *looks* for a daemon (the discovery file plus a health probe): when another client starts one, it attaches. | `Backend stopped (hya serve stop) · /reconnect starts it again` |
| `signal` (SIGTERM/SIGINT/SIGHUP from anything but `hya serve stop`, for example Ctrl+C on a foreground `hya serve`), or an unknown reason | As `stop`. | `Backend stopped (signal) · /reconnect starts it again` |
| `restart` (`hya serve restart`) | Waits up to 60 s for the new daemon of the database and attaches to it; never starts one. Then it reloads its own code ([Hot update after `hya serve restart`](#hot-update-after-hya-serve-restart)). If none answers in time, it is stopped as after `stop`. | `Backend restarting (hya serve restart) · waiting for the new one…`, then `Server moved · now pid <pid>` and, from the reloaded TUI, `… · TUI reloaded (hya serve restart)` (or `Backend did not come back after hya serve restart · /reconnect starts it again`) |
| none (the stream ended without the frame: a crash, `kill -9`, a lost connection) | Runs the same find-or-start as at launch. | `Server stopped · reconnecting…`, then `Started a new server · pid <pid>` when it started the daemon, or `Server moved · now pid <pid>` when it found one |

`/reconnect` runs find-or-start at once, from any state, and says `Started a
new server · pid <pid>`, `Server moved · now pid <pid>`, or `Connected · pid
<pid>` (the current server is the live one); a failure says `Reconnect
failed: <reason> · /reconnect to try again`.

Whenever it moves to another server the TUI:

1. switches every later request to the new server's URL (the sidebar and
   `/status` show it),
2. resubscribes the session stream and the global ask stream,
3. reloads the catalogs, the pending asks, and the open session's transcript
   from the database.

Several TUIs that lose the server together end up on one new daemon: one
starts it (after a crash, or on `/reconnect`), the others find it. Example:

`hya serve stop` with a terminal TUI and two WebUI tabs open leaves all three
showing `Backend stopped`; `/reconnect` in the terminal starts the daemon and
the tabs move to it by themselves. A turn that was running on the old server
ends with it (the transcript shows how far it got; the server's shutdown
closes it as cancelled). Prompts queued in the TUI are dropped. If no server
can be found or started after a crash, the controller status state says `Server lost:
<reason> · retrying`, and the next stream retry tries again. A TUI whose
stream was down at the moment of a stop never gets the reason and treats the
stop as a crash. A TUI with a fixed `--server` and no `--db` never moves; it
keeps retrying that URL.
During a provider round, assistant deltas with `seq == 0` are live-only and
process-local. A restart may lose those in-flight deltas, and reconnect does
not retry a provider round or repeat its tool/file side effects. The durable
transcript and terminal round events are authoritative; after moving to a
successor the TUI re-reads messages and folds only durable replay plus new live
frames. A `resync` frame means the stream gap itself is not replayed: the TUI
re-reads the projection (or replays `ListEvents` from the last durable seq).

### Hot update after `hya serve restart`

`hya serve restart` is how a running backend takes new code
([cli.md](cli.md#self-proof-and-rollback)); the TUIs attached to it
take theirs in the same step. Once a TUI has attached to the new daemon
after a `restart`, it starts itself again from the TUI files on disk: the
source tree in a checkout, or the installed `lib/hya/tui` next to `hya`. New
TUI features therefore show up at once, in the terminal TUI and in every
WebUI tab, without quitting `hya`.

What carries over: the open session (the new TUI opens it with `--session
<id>`, a subagent's view included), the unsent composer text and cursor, and
every other flag of the first start (`--db`, `--dir`, `--hya`, `--web-tab`,
`--web-url`, …; `--server` becomes the new daemon's URL). `--continue` and
`--resume` are not repeated. Scroll position, open views and pickers, and
the prompt history of the old process are not kept. The session is left as
a signal leaves it: never archived. The reloaded TUI adds `TUI reloaded (hya
serve restart)` to its first status line.

Only a `restart` reloads the TUI. An attach after a crash, after `hya serve
stop` plus another client's start, or through `/reconnect` does not, and
neither does a remote backend (`/connect-remote`, `hya --connect`), a
`--grpc` start, or a fixed `--server` without `--db`.

How it works: the process a host starts (`bun <tui>/src/main.ts …`, run by
bare `hya`, by each WebUI tab of the web host, or by hand) is a small
supervisor (`src/supervisor.ts`). It runs the same entry again as the app
(`src/tui.ts`) on the same terminal, with stdin, stdout, and stderr
inherited, and forwards SIGINT, SIGTERM, and SIGHUP to it. To reload, the app
restores the terminal, writes `{"argv": [...], "draft": {"text", "cursor"}}`
to the file the supervisor named in `HYA_TUI_RELOAD_FILE` (mode 0600 in the
temporary directory), and exits with status **75**; the supervisor starts a
new app with those arguments and hands the draft over in `HYA_TUI_RELOAD`.
Any other exit (or 75 without a readable request, or any exit after a
forwarded signal) ends the supervisor with the same status (128 + the signal
number when the app died of a signal). The app removes `HYA_TUI_RELOAD_FILE`,
`HYA_TUI_SUPERVISOR` (the supervisor's pid), and `HYA_TUI_RELOAD` from its
environment at start, so a TUI started from its `!` shell is independent, and
it exits on its own when the supervisor is gone. The supervisor itself is
not reloaded: it is loaded once per host start and changes only with a new
`hya` start.

| Environment variable | Set by | Value |
| --- | --- | --- |
| `HYA_TUI_RELOAD_FILE` | supervisor, for the app | Path of the reload request file; its presence makes `src/main.ts` run as the app. |
| `HYA_TUI_SUPERVISOR` | supervisor, for the app | The supervisor's pid. |
| `HYA_TUI_RELOAD` | supervisor, for a reloaded app | `{"draft"?: {"text": string, "cursor": number}}`. |

### Remote backends (`/connect-remote`)

A backend that joined a secure relay (`hya serve --relay <url>`, see
[relay.md](relay.md#hosting-a-backend-on-a-relay)) prints a **relay link**,
`hya://…#<key>.<psk>`. `/connect-remote` moves a running TUI onto that
backend without restarting it; `/disconnect-remote` brings it back. Bare
`hya --connect` does the same for a whole `hya` start
([relay.md](relay.md#connecting-from-a-client)).

```text
/connect-remote hya://relay.example.com/eh7ddx5bksrgcytl7bkai36se4#…   # inline
/connect-remote --transport ws                                          # asks for the link, hidden
/disconnect-remote
```

**Connecting.** The TUI runs `<hya> bridge - --json --exit-with-stdin
[--transport T] [--relay-ca PEM]` (the same binary lookup as the daemon start:
`--hya`, `HYA_BIN`, `hya` on PATH; bare `hya`, `--connect` included, passes
its own executable as `--hya`), writes the link and a newline to the
child's stdin, and keeps that pipe open. The controller status state counts
`Connecting to the relay… Ns · <the bridge's latest line>` for up to 20 s.
When the bridge prints its readiness line
(`{"url","room","proxy","label","token"}`), the TUI:

1. closes the open session's stream (an unused one is ephemeral, so the
   server it leaves drops it; a used one keeps running there),
2. switches every request to the bridge's loopback URL, each carrying the
   bridge's per-bridge `token` as `x-hya-bridge-token` (the bridge answers
   `401 unauthenticated` to a connection without it, so another local
   process cannot use the remote through it), and shows the label
   (`remote: <relay>/<room>`) instead of that URL in the sidebar
   `Context` section and `/status` (`Server      <label> · via <url>`, `Backend
   remote · through this TUI's relay bridge …`),
3. behaves like a `--remote` start: no Project is ensured for `--dir`, no
   session is created, and the [Project view](#project-view) opens so you
   choose (or create) a Project on the remote, or press `t` for a temporary
   session,
4. never replaces the server by itself: a failing stream does not start or
   look for a local daemon ([When the server goes away](#when-the-server-goes-away)
   does not apply); the streams keep retrying the bridge, which answers
   `503 unavailable: remote backend is offline, or the relay link was rotated or is wrong …` while the remote is down (or the link was rotated), and
   the TUI picks up where it was when it comes back. The bridge's state
   changes (`hya bridge: remote backend online …`, `… offline`, `relay
   unreachable`) appear on the controller status state. `/reconnect` only resubscribes.

`Connected to remote: <relay>/<room> · choose a project, or t for a temporary
session` confirms it. A failure leaves the TUI where it was:
`Remote connection failed: <reason> · /connect-remote to try again`, where
the reason is the bridge's one-line error (for example `the remote backend
rejected the relay link … (rotated or wrong link); ask for a new one`,
`cannot reach the relay …`) or `no answer from the relay within 20 s`. A
second `/connect-remote` stops the running bridge first.

**When the bridge exits on its own** (it was killed, or failed), the status
line says `Remote bridge exited (<its last line>) · /connect-remote <link>
connects again · /disconnect-remote goes back to the local backend`, the
metadata state shows the connection as lost, and prompts are refused (`Not sent
· the relay bridge exited · /connect-remote <link> connects again`). Nothing
local is started.

**Disconnecting.** `/disconnect-remote` closes the remote session's stream
(an unused one is ephemeral, so the remote daemon drops it), closes the bridge's stdin (SIGTERM after 2 s if it is still running), clears
the label, and runs the local start again: the database's daemon (found, or
started with `hya serve start`) or the fixed local `--server`, the Project
of `--dir`, and a new session; `Back on the local backend · pid <pid>`. A
TUI started by bare `hya --connect` has no local backend (no `--db`, no local
`--server`): it says `No local backend to go back to: … · quit and run hya
for a local one` and stays on the remote. Going back to a local backend
drops the bridge token. Quitting the TUI closes the pipe, so the bridge
exits with it.

Bare `hya --connect` runs the bridge itself and passes its token to each TUI
in the environment variable `HYA_SERVER_TOKEN` (never argv); the TUI sends
it with every request to `--server` and removes `HYA_SERVER_TOKEN` and
`HYA_RELAY_LINK` from its environment at startup, so no child (an editor,
a shell, a `/connect-remote` bridge) inherits them.

**The link is a secret** (ADR-0025: whoever holds it controls the backend):

- It goes to the bridge's stdin only, never into argv (process listings).
- `/connect-remote` without a link opens a concealed entry in place of the
  composer: typed or pasted characters are shown as bullets (at most 32, then
  `…`) with a count (`145 characters`); Enter connects, Esc or Ctrl+C cancel.
  The text is held outside the store and the screen, like a provider key in
  the [Provider View](#provider-view). The composer itself has no masking:
  a link typed inline after `/connect-remote ` is visible while you type it,
  and cleared from the screen when you press Enter.
- The input history keeps `/connect-remote` without the link (flags stay),
  so Up never brings it back.
- An input that holds a relay link and is not `/connect-remote` (a prompt, a
  mistyped command that would run as a backend command) is refused and never
  sent: `Not sent · the input holds a relay link, which is a secret ·
  /connect-remote takes it`.
- Controller status states show relay links only in their redacted form (`hya://host/room#…`);
  the bridge itself only prints the redacted form.
- The bridge's stderr lines and the readiness line's `room`, `proxy`, and
  `label` can carry text from the remote side: terminal controls (escape
  sequences, C0/C1 control characters, DEL) are stripped before any of it
  reaches controller state or an explicit information view.
- Each WebUI tab is its own TUI process: `/connect-remote` in one tab moves
  only that tab. The WebUI shows exactly what the TUI draws.

Remote workspaces live on the backend machine, so in remote mode (`--remote`,
bare `hya --connect`, or after `/connect-remote`) paths name the backend's
files (see [Remote files](#remote-files)): `@file` suggestions, `@path` image
attachments, and a pasted path's `@path` conversion all resolve there; only a
pasted or dragged image that exists on this machine is read here.

To add a provider or set its API key, type `/key`: the full-screen
[Provider View](#provider-view) lists the providers, adds one through a short
pop-up (name, protocol, base URL, key), and fetches and tests its models.
Changes apply to the running backend at once; no restart is needed.

## Projects

A Project (ADR-0024) is a named list of root directories on the backend
machine; the first root is the primary root. Every non-temporary session
belongs to one, and the TUI always has at most one **active Project**: the
one new sessions go to and the directory scope (the `directory` field of
every scoped call: files, VCS, catalogs, bootstrap, agent models) follows.

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
  or start a temporary session` on the controller status state. Until a Project is
  chosen the client sends no directory scope at all (no `directory` field:
  `--dir` names nothing on the remote; the global catalogs work unscoped);
  choosing one sets the scope to its primary root. The same holds after
  `/connect-remote`; `/disconnect-remote` restores `--dir`.
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

### Project view

`/project` (and its alias `/projects`) opens the full-screen Projects panel
provided by `hya/basic-tui-components`. It keeps a highlighted Project,
busy/notice state, and create, rename, root-edit, and delete sub-flows.
Up/Down move the highlight; the active Project is marked and, in `--remote`
mode with none active yet, the view opens by itself (see "`--remote`" above)
and a new-session attempt without an active Project opens it too, instead of
only setting the controller status state.

| Key | Effect |
| --- | --- |
| Enter | Open/switch to the highlighted Project (`project.switch`), then close the view. |
| `n` | Create a Project: type a name, then add one root per step. Tab asks the host to complete the current root (`fs.complete`) and shows candidates; Enter on an empty root finishes; at least one root is required and the first root is primary. |
| `e` | Edit roots: Up/Down select, Shift+Up/Down reorder (first is primary), `a` adds a root with the same Tab completion, `d` removes it (refused when it is the only root), Enter saves, Esc cancels. |
| `r` | Rename the highlighted Project. |
| `d` | Ask to delete the highlighted Project; Enter confirms, Esc cancels. |
| `t` | Start a temporary session and close the view. |
| Esc | Close the view (cancels a sub-flow first, if one is open). |

**Errors.** A failed call shows as one line on the view's notice line,
`<what failed>: <code>: <message>` — the server's error code and message
(`HttpError.detail`), or `unavailable: <reason>` when the request got no
answer — never as a stack trace. A failed Enter or `t` reopens the view with
that line; opening the view while the backend cannot answer shows
`Refresh failed: …` over the last known list. A remote backend that went
offline behind the relay reads, for example, `Temporary session failed:
unavailable: remote backend is offline, or the relay link was rotated or is
wrong (…)`. A failed switch from the left Projects sidebar puts the same line
on the controller status state (`Switch failed: …`).

### Left Projects sidebar

A second, narrower sidebar on the left lists every Project live
(the `projects` contribution in `hya/basic-tui-components`): see
[Layout](#layout) for its visibility threshold, pane navigation, and
`/projects-sidebar`.

The Sessions, Todos, Projects sidebar, Context pane,
and this full-screen view are rendered by `hya/basic-tui-components`, not by
the frontend itself. While the catalog is loading or that bundle is starting,
surfaces show `Loading…`. Afterwards, without a
replacement, the TUI shows `<Pane> needs hya/basic-tui-components (<reason>)`,
where the reason identifies installation, catalog unavailability, or extension state.

## Commands and keys

| Input | Effect |
| --- | --- |
| Plain text + Enter | Admit a prompt in the current session; create one if needed. |
| Ctrl+J, Alt+Enter, Shift+Enter | Insert a newline instead of sending. Shift+Enter works in the WebUI and terminals that report the modifier; see [Composer](#composer). |
| Up / Down | On the input's first / last line: the previous / next submitted input. |
| `!<command>` + Enter | Run the command as a shell turn in the current session (see [Shell turns](#shell-turns)). |
| `@<text>` | Show matching file paths; Up/Down select, Tab or Enter inserts `@<path>`, Esc closes (see [File references](#file-references)). |
| `/` with an empty message, or Ctrl+X then `/` while drafting | Focus the separate command pane. The message draft stays in place (see [Command pane](#command-pane)). |
| `1` `2` `3`, Up/Down + Enter | With a permission prompt shown and an empty input: Allow once, Always allow, Deny. On a question prompt the digits pick its options (see [Permission and question prompts](#permission-and-question-prompts)). |
| Esc | In the command pane, return to the message composer; with a file list open, close it; else, with vim mode on and the input in insert mode, switch to normal mode (see [Vim mode](#vim-mode)); else, with a prompt shown and an empty input, deny the permission / reject the question; else, in a subagent's read-only view, return to the parent session; else cancel the running turn; else clear the input. |
| Ctrl+C | In the command pane, close it. In the composer, clear the input and show `Press Ctrl+C again to quit`; a second Ctrl+C within 2 s quits and archives the session (like `/exit`). |
| Ctrl+D | On an empty input: quit and leave the session running (like `/to-background`); in a WebUI tab it only shows `Close the tab to leave this session running`. With text it deletes the character under the cursor. |
| `/exit`, `/quit` | Quit and archive the session (an unused one is left for the daemon to delete). See [Quit and keep running, or archive](#quit-and-keep-running-or-archive). |
| `/to-background` | Quit at once and leave the session running on the daemon, not archived. Terminal only: not offered in a WebUI tab (close the tab instead). |
| `/resume [id]` | Unarchive and open that session; without an id, pick one of the active Project's top-level sessions (every session without an active Project), archived ones included and tagged `[archived]`, newest first. |
| `/new [agent] [model]`, `/new --temp [agent] [model]` | Create a session in the active Project (in `--dir` when it lies inside the Project, else in its primary root), using the first visible agent and its model by default; `--temp` creates a temporary one instead (no Project). Keyboard focus goes to the composer afterwards, even when the command pane was opened from the focused Projects sidebar. |
| `/sessions` | Open the sessions picker, scoped to the active Project (temporary sessions in their own group): a `New session` row, then saved and archived sessions (subagent sessions nested under their parent); Enter opens, F2 renames, Ctrl+D deletes with confirmation, Ctrl+A hides or shows archived sessions, F3 shows every Project's sessions instead (see [Pickers](#pickers)). |
| `/project`, `/projects` | Open the full-screen [Project view](#project-view): list, open/switch, create, edit roots, rename, delete, or start a temporary session. |
| `/projects-sidebar [on\|off]` | Show or hide the [left Projects sidebar](#left-projects-sidebar). Without an argument the command toggles what is visible now; Alt+arrows move keyboard focus (see [Layout](#layout)). |
| `/open <id or number>` | Switch sessions directly. Numbers are the ones the sidebar and `/sessions` show, counted over the sidebar's list of the active Project's sessions (plus temporary ones): top-level sessions count `1`, `2`, …; a subagent's session carries its parent's number plus its own place under it (`2.1`, `2.1.3`). Numbers use session creation order and remain stable when running state or updated time changes. A session the sidebar does not list (another Project's, shown by the picker's F3; an archived one, shown by its Ctrl+A) has no number; open it by id. In the command pane, a titled session's argument row shows as `title (id)` (for example `/open Fix login (hysec_1)`) and matches by its title as well as its id; choosing it inserts the id. `/resume` completes the same way. Opening a subagent's session shows it read-only (see [Subagents](#subagents)). |
| `/models`, `/model [provider/model]` | View catalog, or open the model picker; `/model <provider/model>` switches directly. Model choices are sent without a client-side effort cache. The choice is also remembered as the active agent's default, unless `config.yaml` pins that agent's model (`agents.<id>.model`): then it changes only the current session (see [Configuration — Remembered Agent Models](configuration.md#remembered-agent-models)). |
| `/effort [level]` | Pick or set the server-persisted thinking effort (`default`, `none`, or catalog variants); `/think` is an alias. |
| `/agent [name]` | Open the full-screen [Agents view](#agents-view): primary agents, subagents, and system agents, each agent's model and effort (Enter selects, `m` model, `t` effort). `/agent <name>` switches directly. With no session yet, the choice is remembered for the next one. |
| `/rename <title>` | Rename the current session (`UpdateSession`); see also the sessions picker's F2 (see [Session titles](#session-titles)). |
| `/permissions [mode]` | Open the permission mode picker, or with a mode id switch to it directly (see [Permission modes](#permission-modes)). |
| Shift+Tab in a list | Move the highlight up in command suggestions, file lists and pickers. It does not switch the permission mode. |
| `/key` | Open the full-screen [Provider View](#provider-view): list providers, add one, set or remove a key, fetch a provider's models, test a model, add a model or edit its metadata. No arguments. |
| `/diff` | Open the full-screen [Diff view](#diff-view): the working tree diff, split per file. |
| `/mcp` | Open the full-screen [MCP servers](#mcp-servers) view: server status, tools, connect/disconnect, login. |
| `/bundles` | Open the full-screen [Bundles](#bundles) view: every bundle of the scope with its components and TUI extension; install, uninstall, enable, disable, trust. |
| `/extensions [enable\|disable\|reload\|trust\|untrust <bundle id>\|sandbox <policy>]` | List the scope's [bundle TUI extensions](tui-extensions.md) (state, tier, sandboxing, permissions, contributions, log), or enable, disable, reload, trust, untrust one, or set the sandbox policy. |
| `/rules` | Open the full-screen [Saved Rules](#saved-rules) view: saved permission decisions, delete. |
| `/workflows`, `/workflow select <name>`, `/workflow run [name]` | View sources and selected state; select or start a Workflow in the selected session. |
| `/interactions` | View pending permissions and questions. |
| `/approve <id>`, `/deny <id>` | Respond to a permission request for this run only (`persist: false`); the keyboard fallback of the prompt, which shows the id. |
| `/answer <id> <text>` | Answer a question request. |
| `/cancel` or Esc | Cancel the running turn: the controller status state shows `Cancelling…`, then `Cancelled · Ready`. |
| `/refresh` | Reload sessions, messages, interactions, models, Workflows, and the command catalog (commands and skills). |
| `/reconnect` | Find the database's backend daemon or start it, now, and switch to it: after `hya serve stop` (see [When the server goes away](#when-the-server-goes-away)), or any time. Says `Connected · pid N` when the current server is the database's live one. With `--server` and no `--db`, or on a remote backend, it only resubscribes to that URL (never a local daemon). |
| `/connect-remote [link] [--transport auto\|grpc\|ws] [--relay-ca <pem>]` | Move this TUI to a remote backend through a relay link: starts a local `hya bridge` child and uses its loopback URL. Without a link a concealed `Relay link` entry asks for it. See [Remote backends](#remote-backends-connect-remote). |
| `/disconnect-remote` | Stop the relay bridge and go back to the local backend (the database's daemon, found or started), with the Project of `--dir` and a new session. |
| `/sidebar [on\|off]` | Show or hide the right sidebar (150 columns or more; below that it is always hidden). Without an argument it toggles what is visible now. Drag its left border with the mouse to resize it (29 columns at least). |
| `/layout …`, Alt+arrows | Split, assign, resize, focus, or close [tiled workspace panes](#tiled-workspace). |
| `/thinking [on\|off]` | Expand or collapse every reasoning (`Thinking`) block. |
| `/tools [on\|off]` | Expand or collapse every tool call card (see [Tool calls](#tool-calls)). |
| `/keybind [list \| show \| set \| unset \| reset]` | Browse shortcuts or save bindings to full commands; see [Keybinding settings](#keybinding-settings). |
| `/theme` | Pick the color theme: moving the highlight previews it, Enter keeps it and saves it to the preferences file, Esc restores the previous one (see [Themes](#themes)). |
| `/copy` | Copy the last assistant reply's text to the clipboard with OSC 52; the controller status state says `Copied N chars` (see [Copy](#copy)). |
| Mouse drag over text | Select it (theme selection color); on release it is copied with OSC 52 (see [Copy](#copy)). |
| Right-click a session or Project | Open a compact context menu at the pointer, in the sidebars or full Projects view. Esc closes it. |
| `/editor` | Edit the input in `$VISUAL` / `$EDITOR` (fallback `vi`); the edited text comes back into the input, unsent (see [External editor](#external-editor)). |
| `/vim [on\|off]` | Turn vim mode in the input on or off, saved in the preferences file; `-- INSERT --` / `-- NORMAL --` on the metadata state (see [Vim mode](#vim-mode)). |
| `/notifications [on\|off]` | Turn desktop notifications on or off, saved in the preferences file (see [Desktop notifications](#desktop-notifications)). |
| `/compact` | Compact the session's context now (`CompactSession`); the controller status state shows `Compacting…`, then `Compacted · <strategy>`. |
| `/summarize` | Summarize the session into a new message (`SummarizeSession`). |
| `/undo` | Revert the last prompt: it and every later message leave the transcript, the files its tools changed are restored, and the prompt goes back into an empty input. Again = one prompt further back (see [Undo, redo, and fork](#undo-redo-and-fork)). |
| `/redo` | Undo the pending `/undo` (messages and files come back); only until the next prompt, which makes the revert permanent. Use `/undo` and `/fork` for the other session operations. |
| `/fork` | Pick where to fork the session (the latest message, or before one of its prompts); Enter creates the fork, switches to it, and puts the picked prompt in the input. |
| `/todos` | Show the session's todo list (`GetSessionTodo`) in the main panel. |
| `/status` | Show the server URL, backend version, directory, session (and `Forked from <title>` for a fork), agent, model, permission mode, and the backend daemon (`daemon · pid <pid> · db <db> · started <N>m ago`, or `via --backend/--server` for a fixed URL); under bare `hya` also the WebUI address or why it is unavailable. Like `/models`, `/todos`, `/workflows`, and `/api`, it replaces the transcript in the main panel until a prompt or `!shell` command is sent, which brings the transcript back with its reply. |
| `/init`, `/review` | Server built-in commands from the backend command catalog, run as `CommandTurn`s. |
| `/<skill> [args]` | Run a discovered skill as a `CommandTurn` (see [Skill commands](#skill-commands)). |
| `/api` | List the HTTP operations from the generated operation catalog (`src/operations.json`, written with `docs/protocol/openapi.json` by `cargo run -p xtask -- gen-api`). |
| `/api METHOD /v1/path [JSON]` | Send an HTTP/JSON request and show its JSON response. It is sent as typed: a scoped rpc needs its `directory` (for example `/api GET /v1/fs/list?directory=/abs/dir`). |
| `/help`, `?` | Open the key and command help overlay (`?` only on an empty input; with text it types `?`). See [Key help](#key-help). |
| Tab | In the command pane, complete the highlighted command name or a supported argument; repeat to cycle argument matches. In a file list, insert the highlighted reference. |
| PgUp / PgDn | Scroll the transcript one page (the view height minus two rows). |
| Home / End with empty input | Jump to the top of the transcript / to the newest line, which the view then follows again. With text in the input they move the cursor. Ctrl+Home/End have no app default. |
| Mouse wheel | Scroll the transcript. |
| Click on a `Thinking` line | Expand or collapse that one reasoning block. |
| Click on a tool card | Expand or collapse that one card; on a `task` card, open the subagent's session read-only. |

The bottom instruction row is separate from the status message above the
input. Status updates and completion suggestions can change without erasing
the next-step instruction.

Other slash commands are forwarded to the backend as `CommandTurn`s, so
custom commands and skills from the server catalog (`ListCommands`, which
already includes skills tagged `source: "skill"`) remain usable in this
frontend. The command pane's suggestions and Tab completion also use that catalog.
Argument completion covers agents, sessions, models, Workflows, pending
interaction IDs, permission modes, and HTTP operations from the
generated OpenAPI catalog. Suggestions are refreshed with `/refresh` or
`/refresh`, and whenever the session or directory changes.

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

## Keybinding settings

`/keybind` lists active app, inherited editor, contextual and custom command bindings in
aligned **Shortcut**, **Action / command**, and **Scope** columns. Unassigned
actions are omitted from the list; `/keybind show <action>` can still inspect them.
Descriptions and full commands appear in the detail area beneath the rows.
`set` assigns a shortcut to a **full slash command**, including its arguments;
pressing the shortcut runs that text through the same command registry as the
command pane. This also supports backend and skill commands. Commands with
arguments need no extra quoting around the whole command.

### Usage

| Command | Result |
| --- | --- |
| `/keybind` or `/keybind list` | Open the filterable binding browser. |
| `/keybind list workspace` | Show workspace bindings. |
| `/keybind list conversation` | Show conversation bindings. |
| `/keybind list pane` | Show built-in focused-pane scrolling actions. |
| `/keybind show quit` or `/keybind show /exit` | Inspect a built-in action and its contextual key behavior. |
| `/keybind show F6` | Inspect the custom command assigned to F6. |
| `/keybind set F6 /layout focus left` | Save a workspace shortcut to the full layout command. |
| `/keybind set Alt+G /tools on` | Save a conversation shortcut that expands tool cards. |
| `/keybind set F7 --scope conversation /sidebar off` | Explicitly limit this sidebar command to conversation focus. |
| `/keybind unset F6` | Disable F6, removing its custom command and suppressing defaults. |
| `/keybind reset F6` | Remove the F6 override and restore its default. |
| `/keybind reset all` | Remove all overrides and restore defaults. |

Type an action, shortcut or command to filter the browser. Up/Down selects,
Enter opens details, and Esc closes the modal and returns to the previous pane.
Opening and closing settings preserves the message draft. `set`, `unset`, and `reset`
show a result modal; a rejected assignment or failed write shows **not saved**
with the reason. The dropdown completes operations, shortcut examples, scopes,
command names, and nested arguments using the target command's own completer.
The old `/keybindings` name has been replaced by `/keybind`.

For example:

```text
/keybind set F6 /layout focus left
# Esc closes the saved-binding modal. F6 now focuses the pane to the left.
/keybind set Alt+G /tools on
# Esc closes the modal. Alt+G expands tool cards in conversation focus.
/keybind show Alt+G
/keybind unset Alt+G
```

The override mechanism has three operations:

- `set <shortcut> <command...>` assigns the full command, replacing any app or
  editor default for that shortcut. Browser-reserved combinations, Ctrl+I/M/J/H,
  plain keys, named editing keys and Ctrl+Shift combinations are accepted.
- `unset <shortcut>` saves a disabled override. The physical key is consumed
  before app defaults, inherited editor behavior, modal/picker actions and Vim.
  Disabled keys disappear from the active list; `show <shortcut>` reports
  `disabled`. This applies to plain keys too: disabling `n` also suppresses typing
  it outside command input. `unset` can disable an existing default without first
  assigning a command.
- `reset <shortcut>` removes the override, restoring defaults. `reset all`
  restores all defaults. A failed save leaves runtime settings unchanged.

**Command input owns its administrative keys.** While the command pane is open,
its editing/completion/submit keys remain usable to repair settings. Modal and
full-screen views otherwise retain input ownership before custom command
execution; disabled overrides suppress their keys. An assigned conversation
shortcut is inactive outside Conversation, while workspace assignments can run
from any tiled pane.

`show` requires a target; `/keybind show` presents a visible `Keybind · error`
modal with `Usage: /keybind show <shortcut, action or command>`. It never chooses
an arbitrary suggestion on Enter. The list includes inherited editor and local
view bindings with their context in the description; `show Ctrl+W` inspects its
editor behavior, and `show Ctrl+C` inspects the app action or your override.

```text
/keybind set Ctrl+W /layout close
/keybind unset Ctrl+C
/keybind show Ctrl+C       # disabled
/keybind reset Ctrl+C      # restore exit handling
```

Shortcut syntax accepts Ctrl/Control, Alt/Meta/Option, Shift, Super/Cmd/Command,
printable single keys, named navigation/editing keys (Enter, Tab, Esc, Backspace,
Delete, Home/End, PgUp/PgDn), and F1–F12. Two-key custom chords remain unsupported.
There are no browser reservations. A browser or terminal can intercept a key
before it reaches the TUI; the parser accepts it without pretending that every
host delivers it. Traditional terminal aliases Ctrl+I → Tab, Ctrl+M → Enter,
Ctrl+J → line feed and Ctrl+H → Backspace are matched together. Set/unset replaces
an equivalent alias override; conflicting equivalent aliases in a preferences
file are rejected visibly. Ctrl+Shift works when the terminal reports Shift.

### Interfaces and routing

```text
/keybind [list [workspace|conversation|pane] | show <action, command or shortcut>]
/keybind set <shortcut> [--scope workspace|conversation] <command...>
/keybind unset <shortcut>
/keybind reset <shortcut|all>
```

`TuiPreferences.keybindings?: Record<string, CommandKeybinding | null>` stores
all overrides. `CommandKeybinding = { command: string; scope: "workspace" |
"conversation" }`; `null` disables that shortcut, and an absent key uses the
default. `isKeyDisabled(KeyLike): boolean` is checked before non-command input
handlers; `resolveCommandBinding(KeyLike)` returns only command assignments.
`inheritedBindingRows()` exposes the merged OpenTUI/composer editor map and
existing contextual help tables; the active list omits overridden defaults.
No HTTP/RPC contracts change.

Everything after the shortcut and optional scope flag is stored as the command
text; internal spaces, arguments and JSON bodies are preserved. The command
must be a single slash command on one line. Its invocation retains the existing
command parser and error behavior. `set` validates syntax, saves
preferences atomically, and then applies the assignment. A failed save leaves
active bindings unchanged. Settings operations are local; executing a bound
command may call the same RPCs as entering that command manually.

Scope defaults to the routing scope of a related built-in action when known
(for example `/layout` and `/help` are workspace actions, `/tools` is a
conversation action); otherwise it defaults to `conversation`. Use `--scope`
to choose explicitly. Workspace bindings run from any tiled pane. Conversation
bindings stay inactive while another pane owns focus. Modal views and command
input take precedence over custom shortcuts. A custom binding to `/exit` runs
that command immediately; it does not inherit Ctrl+C's two-press key guard.

Assignments are stored in the existing TUI preferences file
(`$HYA_TUI_CONFIG`, else `$XDG_CONFIG_HOME/hya/tui.json`, else
`~/.config/hya/tui.json`) and loaded on startup:

```json
{
  "keybindings": {
    "F6": { "command": "/layout focus left", "scope": "workspace" },
    "Alt+G": { "command": "/tools on", "scope": "conversation" },
    "Ctrl+C": null
  }
}
```

The preference field is
`keybindings?: Record<string, { command: string; scope: "workspace" | "conversation" } | null>`.
Shortcut labels are normalized (for example `option+g` becomes `Alt+G`). Invalid
or conflicting saved assignments are ignored as one group, with a startup
warning; other preferences still load. Unknown preference fields remain intact
when settings are saved.

`src/keys/custom.ts` exposes `parseShortcut`, `validateCustomKeybindings`,
`customKeybindings`, `setCustomKeybindings`, `isKeyDisabled`, `isKeyOverridden`,
`sameShortcut`, and `resolveCommandBinding`.
The resolver returns `{ command, scope } | undefined` for the current key;
Composer checks focus and submits the full command with source `command`.
`/help` includes custom assignments and `/keybind` includes them beside the
built-in catalog.

The keybinding picker opts into `PickerColumns { shortcut: string; label: string;
tag: string }` heading labels via `PickerSpec.columns?`, copied to
`PickerState.columns?` by `createPicker`. `PickerRow.shortcut?: string` holds
the shortcut separately from `label` and `detail`, and participates in filtering.
Column widths stay aligned while filtering; long cells clip with an ellipsis,
while the detail pane retains the full text. Other pickers omit `columns` and
keep their existing layout. No backend routes or preference fields change.

The built-in catalog remains `bindingSettings(): BindingSetting[]` and
`findBindingSetting(target: string): BindingSetting | undefined` in
`src/keys/catalog.ts`:

```ts
interface BindingSetting {
  id: KeyAction;
  scope: "workspace" | "conversation" | "pane";
  command?: string;
  keys: string[];
  description: string;
  context: string;
}
```

`CommandSpec.complete(position, context, registry?)` receives the owning
registry as its optional third argument, allowing `/keybind set` to reuse the
bound command's nested completion instead of maintaining another hint tree.
Related built-in commands can differ from contextual shortcuts: `/exit` exits
at once, while Ctrl+C clears/hints and requires a second press; `/interactions`
lists asks, while `/pending` opens the oldest ask in another session. `/help` also lists
editor, prompt, picker and view-specific keys outside this action catalog.

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
keys come from `src/commands/help.ts` next to those state machines.
Shift+Enter is available in the WebUI and terminals that report it separately
from Enter. Ctrl+J and Alt+Enter remain newline alternatives.

When the TUI cannot reach its backend, the main panel shows the same key
list as plain text instead.

## Essential default shortcuts

The default app shortcuts cover command/help access, focus, scrolling, cancellation
and exit. Optional session operations and view toggles use slash commands so they
leave more keys available for editing and personal bindings.

- `/` on empty input, or Ctrl+X then `/` while drafting: open commands.
- `?` on empty input: help. Alt+arrows: select a tiled pane.
- Tab: completion. PgUp/PgDn: scroll the focused pane. Home/End scroll the transcript with empty input.
- Esc: close/cancel/clear according to context. Ctrl+C twice within two seconds:
  exit and archive. Ctrl+D on empty input: exit and leave the session running.
- Enter submits; Ctrl+J, Shift+Enter and Alt+Enter insert newlines. Text editing
  and view-local picker/prompt controls remain available.

Ctrl+Home/End, Ctrl+R/B/P/O/G, F4, global Shift+Tab mode cycling and the editor/undo/redo/fork
Ctrl+X chords are unassigned. Use `/refresh`, `/sidebar`, `/projects-sidebar`,
`/thinking`, `/tools`, `/permissions`, `/editor`, `/undo`, `/redo`, `/fork` and
`/pending` instead. For example:

```text
/keybind set Ctrl+G /tools on
/keybind set Ctrl+R /refresh
/keybind set F4 /pending
```

**Interfaces.** `keyBindings: readonly KeyBinding[]` contains only active defaults.
`bindingSettings(): BindingSetting[]` also includes unassigned actions with
`keys: []`, omitted from `/keybind list` and shown as `unassigned` only in
explicit `/keybind show` inspection; command scope metadata remains available
for `/keybind set` inference. Saved custom bindings keep their existing contract.
`/pending` has no arguments and calls `AppActions.reviewPending(): Promise<void>`:
it opens/unarchives the oldest waiting request outside the current session tree
using existing `GetSession` and session resume/update RPCs. If none exists, its
status is `No pending request in another session`. No wire contract changes.

## Layout

Enclosing boxes identify selectable UI. Pending summaries, notifications,
warning docks, transcript tool/task blocks, and passive extension containers are
borderless. Headings, severity colors and text remain visible; a left gutter or
an output divider can group transcript content without enclosing it. Permission
choices still use the message editor's keys or mouse clicks, and `/pending`
opens another session's request. For example:

```text
┌─Projects───┐                                 ┌─Sessions─────┐
│ ▸ hya     │  ┃ your prompt                   │ ▸ 1. Review  │
│   app     │  Assistant response              │              │
│           │                                 └──────────────┘
│           │                                  Todos
│           │                                  ○ write tests
│           │
│           │  ◌ Working · Running bash        Context
│           │                                  Agent  build
│           │ ┌─────────────────────────────┐  Model  fake/…
│           │ │ Message                     │
│           │ └─────────────────────────────┘
└───────────┘
```

### Conversation without headings

Conversation starts directly with the transcript (or the explicitly selected
models, Workflows, API, or help view). It has no session/model/server header,
metadata bar, routine status row, or footer instructions. These rows are removed;
they are not relocated above or below the messages. This leaves conversation
space for messages and the focused input rather than a built-in monitoring area.

The running-turn indicator, pending requests, permission/question prompt,
yolo confirmation, and message composer follow the transcript when applicable.
Message content, Markdown headings, assistant/tool labels, and interactive
permission controls keep their usual rendering.

To inspect metadata deliberately, open `/status`, `/permissions`, or an assigned
Context or status pane. For example, press `/`, enter `status`, and press Enter;
Esc closes that view. Routine command completion and error status strings are
no longer automatically printed in Conversation. A user-chosen monitor can read
the existing `hya.v1` session, event, and catalog contracts independently.

**Interface:** `ConversationPane` omits the former session header, metadata bar, and
routine status component. This is a rendering change, with no new flag, saved preference,
plugin API, RPC route, or event type. Controller status state and the existing
explicit information views remain available; the conversation split keeps its
keyboard ownership and highlighted composer.

- **Sidebar.** Three sections on the right: a selectable Sessions box and
  borderless Todos and Context sections. `Sessions` (the list; `▸`
  marks the open one; a subagent's session is one `↳ N. <agent>` line nested
  under its parent, `· running` while it works, `· ◌ waiting` while a
  permission or question of that session waits for an answer. Clicking any
  session row opens that exact session (including subagents). Subsessions use
  one compact row each, with their full minted member handle (for example
  `hya-scout-skade`) rather than only the agent class — opening a session
  syncs its row to the fresh `GetSession` read, so a stale `running`
  from before it was opened does not linger, and its own stream's turn-end
  frame clears it live if the turn was already running when it was opened;
  `state/store.ts` `openSession()` / `setSessionBusy()`; every root
  session's row also stays current live from another client's changes —
  see [Sidebar live updates](#sidebar-live-updates)), `Todos` (the
  live todo list — see
  [Working indicator, metadata state, and todo panel](#working-indicator-metadata-state-and-todo-panel)),
  and `Context` (permission mode, session, agent, model, message count,
  context occupancy, tokens, directory, branch, server, frontend/backend
  versions as `<frontend>/<backend>`, and connection).
  These are independent panes in the editable layout tree. The right sidebar
  needs 150 columns and is always hidden below that width. At 150 columns or
  more, `/sidebar [on|off]` toggles its visibility. It is never
  narrower than 29 columns; drag its left border to resize it, and the saved
  split weight persists across launches. Hiding Context does not add metadata
  rows to Conversation. Its `Sessions`
  box (and the `/sessions` picker) is scoped to the active Project, with
  temporary sessions under their own `— Temporary —` heading (see
  [Projects](#projects) and [Pickers](#pickers)).
- **Left Projects sidebar.** Its panel is supplied by
  `hya/basic-tui-components`; Up/Down move the highlight and Enter switches
  the selected Project; Esc releases capture. A narrower titled pane on the
  left: one row per Project (`ListProjects`, live via `projectsUpdated`), with
  a separator between rows, the active one marked `▸`, a busy marker `●`
  while a session of it runs a turn, and its session count. Clicking a
  Project row switches to it just like clicking a session row opens that
  session. Right-clicking a Project opens Open and Delete actions;
  right-clicking a Sessions row opens Open, Archive (unless already archived),
  and Delete actions. Clicking an action executes it directly; Esc closes the
  menu. Rename and confirmation flows remain available in the Project view
  and `/sessions` picker. These use the same v1 Project and Session
  update/delete contracts as the Project view and session picker. It needs both sidebars and the chat column to fit, so it follows a threshold no
  lower than the right sidebar's (150 columns; an 80-column or even a
  149-column terminal keeps it hidden). `/projects-sidebar on` pins it open
  at any width; Alt+arrows or `/layout focus <pane-id>` selects it.
  Up/Down move the highlight, Enter switches (`switchProject`), and Esc returns
  focus to Conversation without closing it. `/projects-sidebar off` hides it.
- **Prompt.** A pending permission request or question of the open session
  or one of its subagent sessions is a borderless prompt (warning-colored heading)
  above the message input; see
  [Permission and question prompts](#permission-and-question-prompts).
- **Pending block.** While permission requests (`!`) or questions (`?`) of
  *other* sessions wait (sessions not in the open session's tree), a
  `Pending (N)` summary appears above the prompt with up to three of them
  (`! <title> · <n>. <session>` for a listed chat, or `saved session` when
  archived). Run **`/pending`** to reopen the oldest request's conversation; its
  normal prompt then shows numbered answer choices. They arrive live — see
  [Asks of other sessions](#asks-of-other-sessions). `/interactions` lists
  every detail. It disappears when nothing else is pending.
- **Keys and the browser.** Essential app actions use browser-safe keys.
  Optional actions use slash commands; `/keybind set` can assign your own shortcuts.
- **Focus.** Ordinary typing, editing, paste, and Enter belong exclusively
  to the focused pane. An auxiliary pane ignores unsupported keys without
  forwarding them to Conversation. Alt+arrows move focus between visible
  panes; `/` opens the global command overlay. Exactly one box highlights
  the keyboard owner. The renderer runs with `autoFocus: false`.

### Tiled workspace

The screen is one tree of nested rectangles. Every pane uses a common
registration contract, and declares whether it is **selectable** (can own
keyboard input) or **unselectable** (passive information). Layout membership
is independent of focus: passive panes still occupy, render, and resize their
rectangles. **A pane box means it is selectable.** Passive panes have no
bounding border or panel-colored frame; Todos, Context, and Status use plain
headings. Non-chat output inside the passive conversation viewer follows the
same rule. `PaneFrame` reads `PaneDefinition.selectable` rather than accepting
an independent border setting, so shared frames cannot draw passive borders. This separates message editing from viewing and provides the
foundation for additional built-in panes and a future pane plugin interface.

The default arrangement is Projects on the left; a conversation viewer,
agent activity line, and message editor in the middle; and Sessions, Todos,
and Context on the right. The viewer is passive and the editor is selectable.
They read the same session projection. No additional stream or durable state
model is created. The editor also holds the existing permission/question dock
and pending-request controls.

| Pane kind | Keyboard eligibility |
| --- | --- |
| `composer`, `projects`, `sessions`, `jobs`, `models`, `workflows`, `interactions`, `api`, `layout` | Selectable |
| `conversation`, `activity`, `todos`, `context`, `status` | Unselectable |

Alt+Left and Alt+Right visit the previous/next visible selectable pane in
visual reading order: top edge, then left edge, then numeric pane id as a tie
breaker. The order wraps into one cycle, guaranteeing reachability of every
visible selectable pane. `/layout focus previous|next` uses the same order.
Alt+Up and Alt+Down choose the nearest selectable rectangle wholly above or
below: prefer horizontal overlap, then vertical gap, then horizontal center
distance, vertical center distance and id. With no candidate they stay put.
Navigation reads mounted native bounds after minimum sizes, content sizing and
responsive hiding; hidden and zero-area panes are excluded. No extra default
shortcuts are installed.

Clicking a passive pane preserves keyboard ownership; mouse scrolling and text
selection remain available. One accent border belongs to the focused editor,
selectable side pane, or active overlay. A hidden, removed, zero-area, or newly
passive focus target falls back to a visible editor or another visible
selectable pane. Focus changes do not rebuild the tree. Pane instances mount
once per stable pane id and receive new bounds when moved, wrapped, resized or
reloaded; drafts, histories and transcript scroll survive changes of parent.

Ordinary keys and paste go exclusively to the focused selectable pane.
Unsupported input stops there rather than editing the message draft. Global
commands and modal/command overlays retain priority. The transcript's existing
PageUp/PageDown and empty-input Home/End shortcuts remain available from editor
focus, even though the transcript itself is passive. Projects retains its own
Up/Down/Enter selection and Esc return-to-editor behavior. The legacy Projects
focus accessor is derived from `paneLayout.active`; it is not a second owner.

For example, open commands using `/` (Ctrl+X then `/` while drafting):

```text
/layout focus pane-3              # Sessions owns input
/layout focus next                # next pane in visual reading order
/layout focus pane-1              # Message editor owns input
/layout split left jobs           # new selectable Jobs pane gets focus
/layout split up status           # add passive Status; focus stays on Jobs
/layout focus previous            # rotate through selectable panes only
/layout close todos               # remove a passive pane without selecting it
/layout close pane-5              # target an exact id (Context in the default)
/layout close                     # remove the selected auxiliary pane
/layout reset                     # restore the seven-pane default
```

`left` inserts the new pane to the left of the selected pane; `up` inserts
it above. Both divide the selected rectangle equally. A new
selectable pane gets focus; adding a passive pane preserves focus. The viewer
and editor are singletons and cannot be duplicated or closed. Assigning either
kind swaps it with the existing instance; if the target becomes passive,
keyboard focus follows the normal editor fallback. Use `/layout close <pane-name>` to remove a passive pane without selecting it.
Names are the lowercase job kinds, such as `todos`, `context`, or `activity`.
Named closing also works on panes hidden by responsive layout. A duplicate
name is refused with the matching ids; use `/layout close <pane-id>` to choose
one. Unknown targets and extra arguments produce errors without changing the
layout. Closing a different pane preserves focus; closing the selected pane
returns focus to the editor. The command dropdown offers currently present
closable ids and unambiguous names, updating after each layout edit.

| Command | Effect |
| --- | --- |
| `/layout` or `/layout show` | Show count and focused pane in status; return the viewer to chat. |
| `/layout split <up\|left> [job]` | Split the focused pane equally; default job `jobs`; maximum 32 panes. |
| `/layout assign <job>` | Assign the focused rectangle; viewer/editor assignment swaps singleton instances. |
| `/layout focus <left\|right\|up\|down\|next\|previous\|pane-id>` | Focus a visible selectable pane; passive/hidden/unknown ids are refused. |
| `/layout resize <+N\|-N>` | Transfer percentage points of parent weight between the selected child and its next sibling (previous at the end), within 10–90% of the pair; converts that parent’s content slots to weights. |
| `/layout close [pane-name\|node-id]` | Close the selected auxiliary pane or an explicit auxiliary pane/container, including hidden or passive content; preserve surviving focus. |
| `/layout reload` | Read and validate `paneLayout` from this TUI’s preferences file and apply it immediately, without writing the file or loading other settings. |
| `/layout tree` | Open or focus a selectable Layout pane for editing the saved tree. Reuse the first existing `layout` pane; otherwise add one beside the whole workspace. |
| `/layout insert <container-id\|root> <index> <job>` | Insert a new auxiliary pane at a zero-based child index (0 through child count); a selectable new pane gets focus. |
| `/layout move <node-id\|pane-name> <container-id\|root> <index>` | Move an existing pane/subtree, retaining ids and focus. Index refers to destination children after removing the source. Cycles are refused. |
| `/layout bubble <node-id\|pane-name\|root> <previous\|next>` | Swap a pane or subtree with its adjacent sibling in the same container. Preserve its size, ids, contents and focus. Root and edge moves do nothing. |
| `/layout wrap <node-id\|pane-name\|root> <row\|column> <job> [before\|after]` | Wrap a target, including the whole root, with a new auxiliary pane; default position `before`. |
| `/layout remove <node-id\|pane-name>` | Remove an auxiliary pane or subtree; refuse any subtree containing the viewer/editor. |
| `/layout reset` | Restore the default arrangement. |

Default ids are `pane-1` editor, `pane-2` Projects, `pane-3` Sessions,
`pane-4` Todos, `pane-5` Context, `pane-6` viewer, and `pane-7` activity.
Below 150 columns Projects is hidden unless `/projects-sidebar on` pins it;
Sessions, Todos, and Context are hidden below 150 columns. At wider sizes
`/sidebar off` hides those three kinds wherever they are placed. The saved
layout remains intact. Resizing and toggling preserve drafts and histories.
The default editor dock sizes to its visible content: three rows for an empty
input, more for multiline drafts, file completion, attachments, pending prompts,
and permission controls. Empty extension decoration containers are omitted and
reserve no rows. Decorators still render above and below the editor, and their
measured row counts contribute to its content size. The activity pane takes one
row while visible and zero rows while idle. The viewer receives all remaining
height, keeping the input
adjacent to the transcript rather than reserving an empty percentage of the
screen. This also applies after resizing the terminal. Existing generated
80%/20% viewer/activity/editor arrangements upgrade automatically; custom
split ratios remain weighted. No reset or new shortcut is needed. An explicit `/layout resize +10` converts that parent’s content slots to
weights. Splitting a weighted slot divides its weight equally; splitting a
content slot along its parent’s direction replaces it with two equal weighted
slots. An opposite-direction split wraps the slot and retains its outer sizing.

### Layout editor pane

The `layout` pane shows the entire saved tree and edits its nodes interactively.
It is a regular selectable pane, like Projects: add it, move it, resize it, or
close it using the same layout commands. It includes hidden and passive panes,
so Todos, Context and the activity row can be edited without receiving keyboard
focus. Each Layout pane keeps its own cursor, optional marked target, and form state.
Direct keys perform common edits without stepping through the action menu.

Open one with `/layout tree`. This focuses the first existing Layout pane or
adds a pane beside the whole workspace. To place one yourself, use
`/layout split left layout`, `/layout insert root 0 layout`, or
`/layout assign layout` on an auxiliary pane. Opening the pane and every
successful edit save the layout through the existing frontend preferences path.

Compact panes reserve at least two scrolling tree/menu rows at the normal
minimum height. Footer hints are limited to the remaining height and edit
previews occupy one row; the complete key reference remains available in
`/help` and the table below. For example, run `/layout split left layout`
at about 80 columns, then press End to reach Context and Enter to open its
actions. Long hints cannot cover the selected node or action menu. This changes
only rendering; layout commands, `paneLayout` fields, and key contracts below
are unchanged.

| In the Layout pane | Action |
| --- | --- |
| Up / Down | Move the cursor to the previous/next tree node or action; scroll to keep it visible. |
| Left / Right | Select the parent/first child in the tree. |
| Shift+Up / Shift+Down | Bubble the cursor node to the previous/next sibling. In a row this moves left/right; in a column it moves up/down. |
| Home / End | Select the first/last row. |
| Enter | Open the selected node's actions, choose a menu item, or save a weight. |
| Shift+Enter / Space | Mark or unmark the cursor pane/group (`◆`); marking another node replaces the mark. The cursor (`▸`) can move independently. In WebUI, Shift+Enter arrives as line feed; Ctrl+J / Linefeed are equivalent. |
| `i` | Insert immediately before the cursor node in its parent; show **Insert here** before choosing the new pane job. At the root, choose a child insertion position first. |
| `w`, then `r` / `c` | Wrap the marked node, otherwise the cursor node, in a row / column; choose the new pane job. Default placement is after the target (right / below); Tab toggles before / after. |
| Esc | Cancel a direct chooser or wrap prefix to the tree; cancel a menu form back to actions; return from actions to the tree; while browsing, clear the mark. Keep the pane open. |
| Backspace / Delete | While browsing, immediately remove the marked auxiliary pane, otherwise the cursor pane. Removing a group requires confirmation with **Cancel** selected initially. In the weight form these keys erase text only. |
| Click a tree row, then **Edit** | Select a node and open its actions. Click an action or choice to use it. |
| **Save**, **Choose**, **Back** | Mouse equivalents of form submission, choice, and cancellation. |

Alt+arrows still switch workspace panes; `/` still opens the global command
input. These local keys appear in `/help` and `/keybind` and respect disabled
keys. Unsupported typing in the tree never reaches the message draft.

Bubbling quickly reorders adjacent panes without choosing a destination. Select
`pane-4 todos` and press Shift+Down to swap it with Context, or use Enter →
**Bubble next**. `/layout bubble todos next` performs the same edit. The cursor
stays on the moved node, the Layout pane keeps keyboard focus, and any mark stays
unchanged; bubbling acts on the cursor even when another node is marked. A group
moves with all its descendants, including the conversation/editor. It never
crosses a parent boundary or wraps around. Sizes travel with their nodes: weight
values and column `content` policies are preserved. Successful swaps save through
the existing `paneLayout` preference and survive `/layout reload` and restart.

**Bubble interface:** `bubblePane(layout: PaneLayout, target: string, direction:
"previous" | "next"): PaneLayout` resolves the same unique pane names, node IDs
and `root` alias as other layout reducers. It exchanges two complete `PaneChild`
entries in one parent, keeping tree IDs and `active` unchanged. Root/edge operations
return the original layout; an unknown or ambiguous target raises the normal
layout resolution error. There are no new config fields or backend RPCs.

Direct insertion uses the cursor even when a different node is marked; wrapping
and removal use the mark when present. Successful direct insertion/wrapping moves
the tree cursor to the newly added pane and keeps keyboard focus in the Layout
pane. After removal the cursor moves to a surviving next sibling, previous
sibling, or parent (the root if normalization removes those containers). A
removed mark is cleared. The conversation, editor, and groups containing either
remain protected. Contextual hints show the next keys, including `r`/`c` after
`w`; unsupported keys while waiting for that suffix do nothing. Space works in
terminals that cannot distinguish Shift+Enter. Weight
entry accepts typing or paste without submitting pasted text; the first input
replaces the existing value. Errors appear inside the Layout pane.

Select a node and press Enter to see its applicable actions:

- **Insert before / Insert after** adds an auxiliary pane beside the selected
  node, in its parent container. **Add child** appends one to a selected container.
- **Change weight** sets that node's positive finite relative weight within its
  parent. In a column, enter `content` for content sizing. The root has no weight.
  Other sibling slots retain their sizes; normal tree normalization still applies.
- **Move** asks for a destination container, then a position before one of its
  remaining children or at the end. A node cannot move into itself or a descendant.
- **Change job** assigns another auxiliary kind to the selected auxiliary pane.
- **Wrap in row / Wrap in column** adds a chosen pane to the left of / above the
  selected node or subtree.
- **Remove** asks for an explicit choice, with **Cancel** initially selected.
  It removes the selected auxiliary node and its descendants. Nodes containing
  the conversation or message editor are protected, with a visible explanation.

For a direct edit, run `/layout tree`, move to `group-2 column`, and press
Shift+Enter (or Space) to mark the conversation group. Press `w`, then `r`, then
choose `jobs`: Jobs is added to the right of that group. Esc clears the mark;
Backspace or Delete can then remove the new Jobs pane. To insert at a different
position, move the cursor to that node and press `i`. Each chooser shows the
target/placement before it changes the layout, and Esc cancels it.

For the existing action menu, select `pane-5 context`, and press Enter.
Choose **Change weight**, type `2.5`, and press Enter. The tree now shows that
node's saved weight in its detail area. Select `group-2 column`, choose **Add
child**, then choose `jobs` to append a Jobs pane. Focus stays in the Layout
pane during both edits. `/layout close layout` closes it; if there are several,
use its exact pane id.

The tree uses the same v4 `PaneLayout` reducers as slash commands. Saved leaves
use `{type: "pane", id: "pane-N", kind: "layout"}`; no additional preference
keys, backend routes, or events are introduced. The cursor, mark, pending wrap
prefix and unfinished forms
are transient and local to each Layout pane. Surviving marks remain across cursor
navigation and pane switches; external removals clear stale marks and cancel
forms whose targets or containers vanished. Removing or reassigning the Layout
pane itself returns focus to
a surviving selectable pane. A failed save keeps the on-screen edit and reports
`Layout changed, not saved: …`. Layout reloads and external command edits update
the tree and repair selections whose nodes disappeared.

Local interfaces are `openLayoutPane(layout: PaneLayout): PaneLayout` and
`setPaneSize(layout: PaneLayout, target: string, size: PaneSize): PaneLayout`.
The weight setter accepts a node id, unique pane name, or `root` (which is
rejected because it has no parent), enforces a finite positive weight and finite
container total, and accepts `{mode: "content"}` only in columns. The editor
state machine in `state/layoutEditor.ts` exposes `layoutTreeRows`,
`createLayoutEditor`, `layoutEditorRows`, `layoutEditorKey`, `layoutEditorChoose`,
`layoutEditorPaste`, `layoutEditorBack`, `layoutEditorPreview`, `layoutEditorHint`,
and `reconcileLayoutEditor`. `LayoutEditorState` adds `marked?: string` to the
existing `selected: string` cursor, `stage`, `index`, and `error?: string` fields.
The pending wrap stage is `{type: "wrap", target: string}`; root insertion uses
`{type: "insert-position", destination: string}`. Direct job choosers and removal
confirmations carry the target/placement in their stage so navigation cannot
retarget a pending edit. The kind-stage additions are `direct?: boolean`,
`target?: string`, `before?: boolean`, `destination?: string`, and
`position?: number` (zero-based child insertion index); removal adds
`direct?: boolean` and `target?: string`. These fields are never saved in
`PaneLayout`. `layoutEditorPreview(state): string | undefined` and
`layoutEditorHint(state): string` expose the pending placement and applicable
keys to the renderer. The state machine's
outcomes are `{state: LayoutEditorState, layout?: PaneLayout}`; the component
applies an optional layout, retains editor focus when possible, and persists it.

### Editing layout with an agent

`/layout reload` applies a layout edited on disk without restarting the TUI.
This gives an agent a file interface for arranging panes, including passive
panes that cannot receive keyboard focus. It reads `paneLayout` from this
frontend's preferences file: `$HYA_TUI_CONFIG`, otherwise
`$XDG_CONFIG_HOME/hya/tui.json`, otherwise `~/.config/hya/tui.json`.
The file belongs to the machine running the TUI (the PTY host for WebUI).
An agent on a remote backend needs access to that file to edit this frontend's
layout.

For example:

1. Run `/layout reset` once to save a valid starter tree, if no layout has been saved.
2. Ask the agent: “Edit `~/.config/hya/tui.json`: remove the `context` leaf
   from its parent’s `children` list under `paneLayout.root`; collapse the parent
   only when it has one remaining child. Preserve the conversation and composer leaves, unique ids, and all
   other preferences.” Substitute your actual configured path.
3. Run `/layout reload`. The edited tree appears immediately. The agent can
   also edit a container’s `direction` and each child’s `size` using the contract below.

**Command contract:** `/layout reload` takes no arguments. It parses and
migrates the saved `PaneLayout` using the same validator as startup, normalizes
passive focus to the editor, and returns the viewer to chat. Draft text and the
current session are retained. It applies only `paneLayout`; theme, keys and
permissions are untouched. Missing/unreadable files, invalid JSON, missing or
invalid `paneLayout`, and extra arguments are errors; the current layout and
file remain unchanged. It never rewrites the file, including after migration.
Success sets status to `Layout reloaded from <path> · …`; errors use the usual
command-error reporting. There is no file watcher, new default binding, or
backend RPC. The internal reader is
`loadPaneLayout(path: string): PaneLayout` (throws on failure); the controller
exposes `AppActions.loadPaneLayout(): {layout: PaneLayout; path: string}`.

Jobs show busy sessions, current subagents, queued prompts and pending
requests from the existing TUI projection. Current-turn activity streams live;
other sessions follow catalog refreshes.

#### Pane interfaces and persistence

### Ordered layout containers and interfaces

Rows place children left-to-right; columns place children top-to-bottom. Each
container has an ordered child list, so inserting into a branch and wrapping
the whole layout use the same tree operations. `/layout split left|up` inserts
next to the selected leaf if its parent has the matching direction; otherwise
it wraps that leaf. A weighted slot is divided equally; other sibling weights
are retained. The old `horizontal`/`vertical` command arguments are replaced.

For example, from the default arrangement:

```text
/layout reset                          # root group-1; center group-2; right group-3
/layout insert group-3 1 jobs           # pane-8 between Sessions and Todos
/layout move pane-8 group-2 1           # move Jobs after the conversation viewer
/layout wrap root column status before # add passive Status above the entire layout
/layout remove pane-9                  # remove Status and collapse the wrapper
```

Only auxiliary pane kinds can be inserted or added by wrapping. Pane names are
case-sensitive; duplicate names require exact ids. Containers can be addressed
by `group-N`; `root` resolves the current root. Removing a container removes
its auxiliary descendants from the layout, without deleting backend sessions.
Moving the root into itself or a descendant is refused. Closing a selected
pane falls back to editor focus; moving preserves focus. Completion lists live
container ids, pane ids, valid insertion indexes and jobs. Empty containers
are removed and single-child containers collapse.

Equal-direction weighted containers flatten with multiplied relative weights,
so ordinary branches alternate rows and columns. A content-sized container can
flatten into content-sized children. A weighted container holding a mixture
of content and weighted children remains a group when flattening would change
its allocation. This preserves sizing constraints rather than silently
altering geometry. Pane ids remain stable across edits; a group id exists until
that container is collapsed or flattened. There is no backend layout RPC.

Bundled panels use the same tree and focus rules as built-ins. An extension
leaf has `kind: "extension"` and a required `panel: "bundle-id#panel-id"`;
non-extension leaves omit `panel`. For example:

```text
/extensions
/layout split left extension acme/git#git
/layout close extension
```

`/layout assign extension acme/git#git` changes a selected auxiliary leaf.
Panel-key completion reads the running extension catalog. The key must match
`^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}#[a-z0-9][a-z0-9._-]{0,63}$`.
A newly discovered `sidebar` contribution is placed once at the right of the
root as an ordinary selectable leaf, retaining current focus. It can be
moved, resized, or removed in the Layout editor. Closing it does not recreate
it during the same launch. The automatic addition remains in memory until a
layout edit saves it. `pane` contributions require explicit placement.
The direct editor's job chooser lists built-ins; use split/assign or the
preferences file to name a custom panel. Existing custom leaves remain
editable in the tree. Bundled replacements retain their host kind's eligibility:
Projects/Sessions are selectable; Todos/Context stay passive and borderless.
The conversation has no implicit extension header or Context line.

The preferences contract is:

```ts
interface PaneLayout { version: 4; root: PaneNode; active: string }
type PaneNode = PaneLeaf | PaneSplit
interface PaneLeaf { type: "pane"; id: string; kind: PaneKind; panel?: string }
interface PaneSplit {
  type: "split"
  id: string                   // group-N
  direction: "row" | "column"
  children: PaneChild[]
}
interface PaneChild { node: PaneNode; size: PaneSize }
type PaneSize = { mode: "weight"; value: number } | { mode: "content" }
```

Weights are positive finite relative numbers; they need not sum to one, but
the total in each container must be finite. Content sizing is valid only for
column children and takes their current minimum/content row count (zero for
idle activity). Weighted siblings divide remaining space, subject to pane
minimums. All-content containers leave unallocated space blank. Layout bounds
fit the viewport even when it is too small to satisfy minimums. Boundaries
between adjacent children in rows or columns can be dragged; their combined
weight is preserved, with a 10–90% clamp on the pair. Hidden siblings and
zero-height activity slots are skipped when choosing the visible pair; their
saved weights/sizing stay unchanged.

Trees require unique safe positive numeric `pane-N`/`group-N` ids, exactly one
conversation and composer, a known active pane id, at most 32 leaves, at most
31 containers, at least two children per container, and at most 31 nested
levels. Passive active ids normalize to editor focus. Invalid startup trees
are ignored; invalid explicit reloads fail without changing the current tree.
Version 1–3 binary layouts migrate automatically to v4 ordered containers,
retaining pane ids and weighted proportions. Old generated editor docks
migrate to content sizing. Version 1/2 keep the old conversation id on the
editor and allocate fresh viewer/activity ids. Saving any layout edit writes
v4; `/layout reload` itself does not rewrite the file.

Local reducer contracts in `src/state/panes.ts` (all return `PaneLayout`):

```ts
insertPane(layout, container: string, index: number, kind: PaneKind, panel?: string)
movePane(layout, target: string, container: string, index: number)
wrapPane(layout, target: string, direction: "row" | "column", kind: PaneKind, before = true, panel?: string)
closePane(layout, target = layout.active)
resizePane(layout, delta: number)
setContainerBoundary(layout, container: string, index: number, ratio: number, secondIndex = index + 1)
setPaneSize(layout, target: string, size: PaneSize)
openLayoutPane(layout)
```

`splitPane(layout, axis: "horizontal"|"vertical", kind = "jobs", before = false, panel?: string)`
remains the internal convenience wrapper; the command passes `before: true`.
`layoutRects(root, rect, minimum?)` returns bounds keyed by pane/container ids.
`movePaneFocus(layout, direction, measured?)` and
`rotatePaneFocus(layout, step = 1, measured?)` use an optional read-only map of
`{left, top, right, bottom}` bounds; running frontend calls always pass native
bounds via `AppActions.paneBounds()`. Completion adds
`layoutContainers?: {id: string; children: string[]; removable: boolean}[]`
to `CompletionContext`, alongside `panes?: {id: string; kind: string}[]`.

The built-in registry in `components/paneRegistry.tsx` exposes:

```ts
// PaneFrame props: kind: PaneKind; title: string; focused?: boolean;
// background?: ColorInput; children: JSX.Element. Borders follow kind eligibility.
interface PaneDefinition {
  title: string;
  selectable: boolean;
  minColumns: number;
  minRows: number;
}
interface PaneRenderProps {
  node: PaneLeaf;
  width: number;
  height: number;
  focused: boolean;
  scrollRef(element: ScrollBoxRenderable): void;
}
interface RegisteredPane extends PaneDefinition {
  render: Component<PaneRenderProps>;
  input?(context: PaneInputContext): PaneInputHandle;
}
interface PaneInputContext {
  controller: Pick<Controller, "projectsSidebarKey">;
  scroll: DiffScroller;
}
interface PaneInputHandle {
  onKey(event: KeyEvent): void;
  onPaste?(event: PasteEvent): void;
}
```

Mounted selectable panes register input handles in `UiHandles.paneInputs`,
keyed by stable pane id; passive panes register no input handle. Registry input
factories build side-pane handlers; the editor registers its stateful handler
on mount. The workspace
installs the single keyboard/paste listener. The current workspace router is
registered by the singleton editor and applies global/overlay actions before
dispatching local input to the active pane. Native textarea editing runs only
while the editor owns focus. Disposal removes only the component's own handles.
Registry metadata and renderers cover all built-in kinds; an external plugin
loader and passive-pane targeting UI are not introduced in this version.
`minRows` constrains horizontal splits, with extra editor space for prompts;
`minColumns` is metadata for future sizing policy. No RPC or backend wire
contract changes.

**Resizing with the mouse.** Drag the border between two side-by-side panes
with the left mouse button to move it: the right sidebar's left border, the
Projects sidebar's right border, or the border of any vertical `/layout
split`. Press on the border column (or the column just left of it) and drag;
the panes follow the pointer while you drag. On release the new share is
written into the saved layout (the split's `weight`), so the next TUI start
keeps it. A drag stays within the `0.1`–`0.9` weight range, and it cannot
make the right sidebar narrower than 29 columns. `/layout resize <+10|-10>`
changes the focused pane's share from the keyboard, and `/layout reset`
returns to the default widths. For example, on a 174-column terminal the
right sidebar starts at its 29-column minimum; dragging its left border 25
columns to the left makes it 54 columns wide, and a new TUI opens with the
same width.

The colors come from the theme in effect (see [Themes](#themes)). The
default `hya` theme:

| Name | Value | Used for |
| --- | --- | --- |
| `bg` | Terminal default | Screen and transcript background. |
| `panel` | Terminal default | Boxes, user message blocks, code blocks, the input. |
| `fg` | `#e8edf3` | Text. |
| `muted` | `#9caab9` | Controller status state, instructions, `Thinking` lines, model names, queued prompts. |
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
| `hya` | dark | The default: terminal background, light text and cyan accents (the palette in [Layout](#layout)). |
| `light` | light | Dark text (`#1f2933`) for terminals with a light background. |
| `contrast` | dark | White text and saturated accents on the terminal background. |
| `ember` | dark | Warm text and amber accents on the terminal background. |

All themes inherit the terminal background, including any transparency configured
in your terminal emulator. No TUI setting is needed: restart the TUI to apply it.
The WebUI uses xterm.js’s configured background. Text, borders and mouse-selection
highlights still use the selected theme. The internal `bg` and `panel` palette
fields are OpenTUI `ColorInput` values with ANSI default-background intent
(`RGBA.defaultBackground()`); they are not fixed RGB colors. Default-color fills
still clear the area under popups, so underlying content does not show through.

**Usage.** `/theme` (no arguments) opens the [picker](#pickers) with one
row per theme, `[dark]`/`[light]` tagged; `●` marks the theme in effect.
Moving the highlight (Up/Down, Tab/Shift+Tab, typing a filter) repaints the
whole screen in the highlighted theme at once — the transcript, Markdown,
highlighted code, tool cards, boxes, and the controller status state. Enter keeps it,
writes it to the preferences file, and shows `Theme → <label>`; Esc (or
Ctrl+C) closes the picker and restores the theme in effect when it opened,
writing nothing. If the file cannot be written, the theme still applies for
this run and the controller status state says `Theme → <label> · not saved: <reason>`.

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
  keybindings?: Record<string, { command: string; scope: "workspace" | "conversation" } | null> // command overrides; null disables a key
  theme?: string          // a built-in theme name: "hya" (default), "light", "contrast", "ember"
  vim?: boolean           // vim mode in the input (/vim); default false
  notifications?: boolean // desktop notifications (/notifications); default true
  permissionMode?: string // default for sessions this TUI creates: "manual" (default), "yolo", or a bundle mode id
  paneLayout?: PaneLayout // versioned full-workspace split tree; see Tiled workspace above
  extensionEnabled?: Record<string, boolean> // bundle TUI extensions by bundle id (/extensions enable|disable)
  extensionSandbox?: "required" | "best-effort" | "disabled" // OS sandbox of the extension host (/extensions sandbox)
  extensionTrusted?: Record<string, boolean> // bundle TUI extensions on the JIT tier (/extensions trust|untrust, /bundles t)
}
```

- The file is read at start, before the first frame; `/layout reload` can
  explicitly reread only its layout using stricter failure handling. A missing file
  means the defaults. An unreadable file, invalid JSON, or a JSON value that
  is not an object is ignored, and the controller status state says
  `Ignored unreadable TUI preferences <path>`; an unknown theme name says
  `Unknown theme <name> in <path>; using hya`. A key whose value has the
  wrong type is ignored.
- A change (`/theme`'s Enter, `/vim`, `/keybind set|unset|reset`, or a successful permission mode switch)
  merges the changed key into what is on disk —
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

## Working indicator, metadata state, and todo panel

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

**Metadata.** The former metadata state and session header are not rendered in
Conversation. Model, permission mode, directory, connection, and token/context
information belong to explicit information views or user-selected monitoring
panes. The existing Context pane can show usage when the backend reports it.

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

The sidebar's `Context` section shows both when known: `Context  42% ·
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
long list cannot push the `Context` section below the visible area. `/todos`
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

`/compact`'s controller status state reads the same way: `Compacting…`, then
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
reconnecting: <error>` in the controller status state while it retries (also reflected
in the metadata state's `reconnecting`); a version mismatch between this TUI and
the backend's bootstrap version appends `backend <version> ≠ tui <version>`
to the initial `Connected to hya …` status.
The packaged TUI and WebUI use the same version as the backend in this source
checkout. If that notice appears after replacing only the backend executable,
reinstall or run the TUI from the matching checkout and reconnect; for example,
run `HYA_BIN=target/debug/hya bun packages/hya-tui/src/main.ts --dir "$PWD"`
from the checkout used to build that `hya` binary.

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

A streamed ATX heading (`#` through `######` followed by a space) keeps its
accent color and bold style as its text arrives. A marker-only chunk such as
`### ` waits for heading text instead of flashing literal hashes. For
example, ask the model to reply with `### Summary` followed by a paragraph;
the TUI shows `Summary` in accent as it streams, with no key or setting to
enable. The input contract is unchanged: `partAppended {message, part,
textDelta}` on the session event stream extends an assistant `text` part,
`partReplaced {message, part, text}` may replace it, and `messageFinished`
finalizes the Markdown. A `###` line inside a fenced code block remains code.

**Reasoning.** Reasoning parts arrive as `reasoning` parts (durable deltas;
see the protocol guide). They are collapsed by default. `/thinking`
expands or collapses all of them (and forgets per-block choices); a click on
one `Thinking` line toggles just that block. The word count is the reasoning
text split on white space. Only provider routes that stream reasoning produce
these parts (for example `openai-response`; the `openai-compatible` decoder
ignores reasoning).

**Scrolling.** The transcript follows the newest line while you are at the
bottom. Scroll up (PgUp, the mouse wheel, Home with empty input) and it stays where you
left it; when more content arrives below, a `↓ New messages below · End
jumps` hint appears at the bottom right. End (with an empty input)
or scrolling back to the bottom clears the hint and resumes following.
Submitting a prompt jumps to the bottom. Opening a session starts at its
bottom. The transcript shows the newest 200 messages.

### Tool calls



Every tool call of an assistant message is a transparent, borderless block. The
header names the canonical tool and shows its state icon and duration. The
first content row is a display-ready argument block: builtin tools use their
semantic summary (for example `src/main.rs · lines 1-40`) rather than raw JSON;
generic namespaced/MCP tools use pretty-printed JSON. The raw argument JSON is
still retained for compatibility and expanded output is shown below a divider.

```text
read
✓
src/main.rs · lines 1-40
│ ────────────────
│ 1  fn main() {
```

- **State icon.** `○` pending, a spinner (`⠋⠙⠹…`) while running, `◌` while a
  permission request waits, `✓` done, and `✗` failed. Duration appears on the
  right once a call is done.
- **Expanding.** Cards are collapsed by default. `/tools` expands or
  collapses all of them (`/tools on`, `/tools off`; with no argument it toggles).
  A click toggles one card; the input keeps focus. `!command` shell turns start
  expanded. Tool output is clipped to the existing 12-line head/tail window.

The tool name and arguments are independent of the tool-specific summary, which
is still used in activity text and narrow views:

| Tool (canonical name) | Summary | Expanded output |
| --- | --- | --- |
| `bash` (hidden alias `shell`) | The command, then exit/timed-out status | Command output and exit status |
| `read` | Path and line range | File text with line numbers |
| `edit`, `write`, `apply_patch` | Path and change summary | Diff, new content, or patch rows |
| `grep`, `glob`, `find`, `ls`, `lsp` | Scope and count/operation | Matching output |
| `task` | Agent type and description | Linked subagent activity |
| anything else (MCP/plugin tools) | Compact JSON arguments | Tool output |

Arguments still stream before the call is complete; the card keeps updating from
the call's argument fragments. Cards appear before the projection is re-read (see
[Stream frames and the transcript](#stream-frames-and-the-transcript)).

### Subagents

A `task` call spawns a subagent in its own child session. Its card shows the
child's status and what it last did, and it always shows these lines:

```text
✓ task  hya-task · survey the repo                                  7ms
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
transcript, and the input's placeholder says so. Enter on a
prompt or `!command` keeps the text and shows
`Read-only: this is a subagent's session · Esc returns to the parent`;
the command pane still runs slash commands. Esc, when the command pane and file list are closed, opens the parent session
again (status `Back to the parent session`); the text you typed stays, and a
second Esc clears it. Opening another session this way resets the parent's
overlay and prompt queue like any session switch; the parent's turn keeps
running on the server, and its transcript is re-read on return.


### Subagent selector and viewer panes

`subagents` is a selectable layout pane for the open conversation's descendants.
`subagent-viewer` is a passive, read-only transcript pane. They let you monitor
helpers alongside the parent conversation without replacing its transcript,
changing its session, or sending the parent's draft to a child. Completed children
stay in the list; nested children are indented, and pending requests show `waiting`.
The existing parent permission/question prompt remains the place to answer asks.

Run `/subagents` to add missing selector and viewer panes beside the workspace and
focus the selector. Existing panes are reused. In the selector, Up/Down previews a
child in every following viewer; Enter pins it to the first following viewer (or
replaces the first pin if all are pinned); `n` adds a new pinned viewer. Unsupported
keys stay in the selector. Alt+arrows navigate selectable panes as usual. A viewer
has no border and does not take keyboard focus; use its mouse wheel to scroll.
Each viewer scrolls independently, even when two show the same child.

Commands (arguments are exact session and pane IDs, with dropdown completion):

| Command | Contract |
| --- | --- |
| `/subagents` | Add missing `subagents` and `subagent-viewer` panes, persist the layout, focus the selector. Requires an open session. |
| `/subagents select <session>` | Preview a descendant of the open session; keep the main session and composer unchanged. |
| `/subagents pin <viewer-pane> [session]` | Persist a pin on that viewer; omitted session uses the current preview or first child. |
| `/subagents follow <viewer-pane>` | Remove that viewer's pin and follow the selector again. |
| `/subagents view <session>` | Add a separate pinned viewer, preserving keyboard focus. |

For example, run `/subagents`, use Down to choose a helper, press Enter to pin it,
then choose another helper and press `n` to watch both. Use `/layout show` to find
pane IDs, `/subagents follow pane-9` to resume previewing there, and `/layout close
pane-9` to remove it. IDs depend on your layout. Both jobs also work through the
normal tree editor and `/layout split up|left <job>` commands; the convenience
command does not impose a fixed layout.

**Layout interface:** version 4 `PaneLeaf` accepts the additional jobs
`kind: "subagents" | "subagent-viewer"`. Only a `subagent-viewer` leaf may have
`session?: string`, a nonempty child-session ID. Absent `session` means following;
present means pinned. Pins are saved in `paneLayout` in TUI preferences and survive
`/layout reload` and restart. Preview selection is ephemeral and resets when the
main session changes. A pin outside the current conversation shows an unavailable
placeholder instead of another conversation's transcript. Removing the last
viewer for a child releases its watch; hidden viewers release theirs too.

**Backend interface:** these panes use the existing `hya.v1` session projection:
`GetSession` (`GET /v1/sessions/{session}`) for `SessionInfo`, `ListMessages` for
`MessageInfo[]`, `ListEvents` for replay, and `StreamSessionEvents` for live
`StreamFrame` events/resync. HTTP/SSE and direct gRPC share this implementation;
there are no new RPCs or durable frontend read models. Each server/root/child
combination has one reference-counted watch. Reconnect gap-fills from its durable
sequence and refreshes the authoritative projection; transient live parts use the
same overlay and Markdown/tool rendering as the main conversation. Viewer errors
appear within that viewer and do not replace the parent's connection status.
Live deltas are transient: joining a response after its part-start frame may
show that part only once its durable completed text is available. Keeping a
following viewer open before spawning a helper shows its response as it streams.

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

The controller status state above the input shows the turn state. While the
[working line](#working-indicator-metadata-state-and-todo-panel) shows a
running turn, the progress texts that repeat it (`Sending prompt…`,
`Running · <turn id>…`, `Running shell · …`, `Queued · N waiting`) are left
out of the controller status state, which stays empty until another message (a command
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
● hya-main · openai/gpt-5
✗ provider_error: http status 400: bad request
```

## Composer

The input at the bottom of the main column is a multi-line message editor
(OpenTUI's built-in `<textarea>`). It receives keyboard focus only when Conversation
is selected and no command input or modal owns the keyboard. Its placeholder is
`Message, !shell, or @file · / commands`.

**Writing.** Enter sends the whole input as a prompt or `!command` shell turn.
Commands are entered in the separate [command pane](#command-pane). A slash
inside an existing draft is ordinary text; a pasted line beginning with `/`
also stays a message. Ctrl+J, Shift+Enter, and Alt+Enter insert a newline
instead of sending. The WebUI translates Shift+Enter to the same LF sequence used by
Ctrl+J because xterm.js otherwise reports Shift+Enter as plain Enter. A
bracketed paste inserts its text, line breaks included, and never sends it.
In a native terminal, Shift+Enter requires kitty keyboard protocol or
modifyOtherKeys support; a terminal that sends the same CR for Enter and
Shift+Enter cannot distinguish them. Use Ctrl+J or Alt+Enter there.
For example, type `first line`, press Shift+Enter, type `second line`, then
press Enter to submit one prompt containing `first line\nsecond line`.
The box grows with its content up to 8 rows (wrapped lines count), then
scrolls. Newlines stay in the prompt text, so the transcript shows the lines
as typed. Editing keys: Left/Right, Up/Down between lines, Home/End to the
start/end of the current line, Ctrl+Left/Right or Alt+Left/Right by word
(Alt+Left/Right selects panes while the workspace is tiled),
Ctrl+A / Ctrl+E to the start/end of the logical line, Backspace, Delete,
Alt+Backspace deletes the previous word (Ctrl+W too, outside a browser, which
reserves it), Ctrl+U / Ctrl+K delete to the line start/end, Ctrl+- undo.

**History.** Every sent prompt or `!command` is kept for the life of the TUI
process, up to 200 entries; it is not saved to disk. Commands have their own
history in the command pane.
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
`Press Ctrl+C again to quit`; a second Ctrl+C within 2 s quits and archives
the session. Any other key in between disarms it. Ctrl+D on an empty input
quits and leaves the session running (in a WebUI tab it only shows a
notice); with text it deletes the character under the cursor. `/exit` and
`/quit` quit and archive; `/to-background` quits and leaves the session
running (see [Quit and keep running, or
archive](#quit-and-keep-running-or-archive)). Quitting destroys the
renderer, which restores the terminal, and exits with code 0.

Example:

```text
explain these two functions:          ← Ctrl+J
- parse_args                          ← Ctrl+J
- run                                 ← Enter sends all three lines
```

### Shell turns

An input that starts with `!` runs the rest of the line as a shell command in
the open session (a session is created first if none is open). The input box
shows the shell mode while you type: its title reads `! shell`; its accent border
continues to indicate keyboard focus, like an ordinary message. The command goes
through the prompt queue like a prompt, so it waits while a turn runs.

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
[Tool calls](#tool-calls)) that starts expanded. The user typed the command,
so it never asks for permission, in any permission mode:

```text
┃ !echo hello

● hya-main · openai/gpt-5
┌──────────────────────────────────────────────────────────────────┐
│ ✓ bash                                                       4ms │
│ {"command":"echo hello"}                                         │
│ ────────────────                                                 │
│ hello                                                            │
└──────────────────────────────────────────────────────────────────┘
```

The arguments row is the tool call's `inputJson` (this TUI's own shell turns
fill in `{"command": …}` before the part carries its input); the output is
the tool call's `outputJson`. Esc cancels a running shell command; the turn
then reads `Cancelled · Ready`.

### File references

Type `@` and at least one character (at the start of the input or after a
space) to see up to 8 files and directories under `--dir` whose relative
path contains the text. The list is a `Files` box above the input; the
selected row is marked `▸` in the accent color. Up/Down move the selection;
Tab or Enter replaces the `@text` token with `@<relative path>` and a space;
Esc closes the list until you edit the token again. The lookup runs 120 ms
after the last keystroke. Matching is case-sensitive (the server's glob), and
the best matches come first: file name starts with the text, then file name
contains it, then only the path does; shorter paths first. The separate
command input does not resolve file references.

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
to send while any row has an error — the controller status state names the file and the
reason, and nothing is sent — so a bad reference never reaches the server.

Once sent, the user message's transcript row shows each attachment under the
text: `↳ attachment · shot.png · image/png · 240 KB` (an `AttachmentPart`
listing never carries the bytes; size is shown once the server records it,
either in the initial response or right after, via a live `partsAdded` stream
frame appended to the message).

#### Remote files

In remote mode (`--remote`, bare `hya --connect`, or after `/connect-remote`)
the workspace is on the backend machine, and the rules above change where a
path is looked up:

- **`@path` names a backend file**, relative (to the open session's workdir,
  else the active Project's primary root) or absolute (inside one of the
  active Project's roots, else the session's workdir). The TUI fetches it
  with `ReadFile` (`GET /v1/fs/read`, `maxBytes` = the 10 MiB cap + 1, so an
  oversized file is refused without pulling more) and sends it exactly like a
  local attachment. A path outside that directory scope is refused by the
  server (`outside the project on the backend`); before a Project is chosen
  there is nothing to resolve against (`choose a project first`). The
  composer's preview fetches each file once while you type; Enter reads it
  again.
- **A pasted or dragged image that exists on this machine** (an absolute
  path) is your local file: it becomes an `@path ` mention and is read here
  and sent inline, never looked up on the backend.
- **Any other pasted image path** becomes a mention only when it exists on
  the backend (a one-byte `ReadFile`); otherwise the raw text is inserted.
- `@file` suggestions already come from the backend (`FindFiles`).

A file only in this machine's `--dir` is `file not found` in remote mode
unless pasted as above. Local mode is unchanged: everything is read here.

### Command pane

The `Commands` pane has a single-line input separate from the message
composer. It is a global overlay, independent of the tiled layout, so opening
it never resizes the conversation or side panes. Press `/` while the message
composer is empty, or from any other workspace pane, to focus it. While a
message draft is open in the conversation, press Ctrl+X then `/`; the draft
stays in the composer. Like the help overlay, Commands is centered near the
top of the whole terminal. Its first row is the command input bar; matching
suggestions drop down beneath it, followed by the keyboard hint. This keeps
the input in the same place while the recommendation list changes. For
example, press `/`, type `models`, and press
Enter to open the model catalog. To type `src/main.rs` in a message, keep
typing in the composer; its slash is literal after the first character.

The input starts with `/`. Up/Down selects a recommendation below the input;
Tab copies it into the input bar. Each suggestion row shows the command name, its
argument hint, its description (truncated to width), and its source in
brackets: `[local]` (this TUI's own
registry), `[command]` (a custom or built-in server command, `/init` and
`/review` among them), or `[skill]` (a discovered skill — see
[Skill commands](#skill-commands) below). The list is fuzzy-filtered as you
keep typing the name: an exact match ranks first, then a prefix match, then a
substring match, then any name whose letters appear in order (a subsequence
match); ties break alphabetically. Up/Down move the highlight through every
matching command, scrolling the visible rows as needed; they wrap only at the
first and last matching command. Shift+Tab moves it upward with the same
behavior. The dropdown displays up to 12 recommendations, 1.5 times the previous
eight-row limit. It reserves six terminal rows for its input, hint, borders,
and top offset: the visible choice count is `max(1, min(12, terminalRows - 6))`.
This fixed frontend limit adds no saved preference or server interface. For
example, press `/` and keep pressing Down past the initial twelve
rows to reach `/layout`, then press Tab to see its actions. Esc or Ctrl+C closes
the pane and keeps its command draft for reopening. Shift+Up/Down walks its own
last 200 submitted commands. The
message composer keeps a separate history and draft.

Backspace also closes the pane when it deletes the last character. For example,
press `/` then Backspace to return to the message composer; its draft stays in
place. When the pane was opened from Projects, focus returns there instead.
Backspace at the start of a nonempty command leaves the pane open. Reopening
an emptied pane starts a fresh `/` input. This uses the existing command-pane
key handler and focus restoration; no server operation or saved preference is
added.

After the command name, the same menu shows argument choices at every depth
that the command can complete. This makes subcommands and their next values
visible while typing. For example, `/layout ` lists actions including `split`
and `assign`; `/layout split ` lists `up` and `left`; and
`/layout split left ` lists pane jobs such as `jobs` and `todos`.
`conversation` is offered for `/layout assign`, but not for a split because a
split cannot create a second conversation pane. `/api ` similarly lists HTTP
methods, then `/api GET /v1/hea` suggests `/api GET /v1/health`. At most eight
rows are shown, and navigation reaches all matching argument choices with the
same scrolling and wrap behavior. Type more of the current argument to narrow
the list. Commands without an argument completer show their syntax hint in the command-name row
but do not invent argument values after the name.

Tab always completes the highlighted name and a trailing space, so you keep
typing its arguments. Enter's behavior depends on the highlighted command's
argument hint: with no hint, or one written `[in brackets]` (an optional
argument, for example `/new [agent] [model]` or `/sidebar [on|off]`), Enter
runs the command as is. Any other hint (`/open <id|number>`, `/workflow
select <name>`) names a required first argument, so Enter behaves
like Tab: it completes the name and waits for you to type the argument.
For an argument row, Tab or Enter inserts the highlighted full command line
and a trailing space, then shows the next available choices. An argument
selection does not run the command; press Enter again after the final value
when no choice is highlighted. Up/Down and Shift+Tab navigate argument rows
the same way as command-name rows. If the typed argument already exactly
matches its only completion, the menu closes so Enter runs the command.

The command pane builds all selectable rows through
`suggestCommandInput(input, entries, complete)`, which returns all matching
rows without a display limit. The renderer uses `pickerWindow(count, index, rows)`
to display at most eight rows, reduced on short terminals, while keeping the
selected row visible. The render order is input, suggestion rows, then hint;
the input stays at absolute row 3 and recommendations begin at row 4 while
the overlay is open. `App` owns one `CommandPane` for its full lifetime;
assigning, resizing, or selecting a workspace leaf does not replace its input
or history. Its absolute box starts at row 2, is centered across the terminal,
and is at most 96 columns wide (four columns of outside margin on narrower
terminals), with z-index 90 below the help/picker overlay at 100. `ui.command`
exposes `active(): boolean`, `open(): void`, `key(KeyEvent): boolean`, and
`paste(text: string): void`. Closing preserves the selected workspace pane
and command draft, restoring Projects keyboard focus when opened there. Each row
is a `CommandSuggestion` with `label: string`, `replacement: string`,
`kind: "command" | "argument"`, and `runOnEnter: boolean`. A local command's
`CommandSpec.complete(position, context)` supplies zero or more completions
for any argument depth; `position` contains `words`, `current`, and `head`
(the text before `current`). A completion (`Completion`) is either a
full-line replacement string, used as its own label, or
`{ replacement: string, label: string }` when the row shows other text than
it inserts: `/open` and `/resume` label a titled session `/open <title> (<id>)`
and insert `/open <id>`. `CompletionContext.sessions` is
`{ id: string, title?: string }[]`. Name rows use the merged
local/backend catalog; argument rows use these existing local completers.
No new server operation or payload is involved.

Local and backend (command or skill) names are merged and deduplicated by
name; a local name always wins a clash with a backend name (the registry
looks up local commands before falling back to the backend, so a local
command is what actually runs either way). The list refreshes with
`/refresh` and whenever the session or directory changes, the same as
Tab completion. The command pane uses the same registry and `GET /v1/commands`
catalog as before. A backend command or skill still creates a `CommandTurn`
through `POST /v1/sessions/{id}/turns` with
`{ "command": { "command": string, "arguments": string } }`; a local command
runs its existing TUI handler. View commands such as `/models` still switch
the main panel in the current layout.

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
  is copied and the controller status state says `Copied N chars`. A plain click selects
  nothing and copies nothing (clicks on `Thinking` lines and tool cards keep
  toggling them).
- **`/copy`** copies the text of the newest assistant reply that has text
  (its text blocks joined by a blank line; reasoning and tool calls are left
  out) and says `Copied N chars`; with no reply yet it says
  `Nothing to copy: no assistant reply yet`.

When the renderer knows the terminal refuses OSC 52 (its capability probe
says so), nothing is sent and the controller status state says
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

**Usage.** Run `/editor`. While a draft is nonempty, press Ctrl+X then `/`,
enter `editor`, and press Enter. The draft stays intact while the command
opens the editor. There is no default editor shortcut; for example,
`/keybind set F6 /editor` assigns one.

```sh
EDITOR="code -w" hya          # VS Code; -w waits for the tab to close
VISUAL=nvim hya-tui ...       # VISUAL wins over EDITOR
```

| Failure | Controller status state | Input |
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
(`controller.attachComposer`). `/editor` dispatches through
`AppActions.openEditor()` in `commands/native.ts`.

### Vim mode

Vim-style modal editing for the input, for people whose fingers expect it.
Off by default.

**Usage.** `/vim` toggles it (`/vim on`, `/vim off` set it); the choice is
saved as `vim` in the [preferences file](#preferences-file) and applies to
every later start. When on, the cursor is a bar in insert mode and a block in normal mode. The input starts in insert mode, where every key works as
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
(Ctrl+C, Ctrl+D, …), arrows, Tab, Shift+Tab, PgUp/PgDn keep their
usual meaning in both modes. The register is internal (not the system
clipboard; use [Copy](#copy) for that).

**Esc precedence** (vim on), first match wins:

1. The picker or the one-line yolo confirmation is open: it takes Esc.
2. The command pane or the file list is open: Esc closes it.
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
interactions as a prompt docked above the controller status state, so you can answer
without typing its id. The agent's turn waits until you answer.

### Permission prompt

```text
┌─Permission · 1 of 2──────────────────────────────────────────┐
│edit  src/main.rs · +1 -1                                     │
│asked by hya-main                                                │
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
│asked by hya-main                                                │
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
prompt. A slash command entered in the command pane runs separately.
Choosing `Other…` only points you at the input. `Reject` (or Esc) sends
`{question: {rejected: true}}`;
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
2. The command pane takes its own input and keys. An open `@file` list takes
   Up/Down, Tab, Shift+Tab (highlight up), Enter, and Esc.
3. The prompt, with an empty input: `1`–`9` choose that option at once;
   Up/Down move the highlight (`▸`, accent color); Enter chooses the
   highlighted option; Esc declines. With text in the input, a question takes
   Enter as its answer.
4. The composer: history, sending, Esc's other meanings (return from a
   subagent view, cancel the turn, clear the input), and the permission commands (switch
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
next listing). If the ask was already answered, the controller status state reads
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
`resync`; other listings happen only with a full refresh (start, `/refresh`).
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

- the [pending block](#layout) lists it as `! <title> · <n>. <session>`
  (`?` for a question). For an archived or otherwise unlisted chat it says
  `saved session`, keeping raw IDs out of the narrow box;
- the controller status state says `Permission needed in <n>. <session> · `/pending` to review`
  (`Question in …` for a question);
- while the terminal is unfocused, a [desktop
  notification](#desktop-notifications) says `Permission needed: <title> ·
  in <n>. <session>`.

**`/pending`** opens and unarchives the oldest waiting request's root session,
including one archived by `/exit`; the complete transcript and prompt appear
there. Press `1` to allow a permission once, `2` to save an allow rule, or
`3` to deny. Esc also denies; with text in the input, clear it before using
the numbered keys. `/sessions` can open any saved chat, and `/open <n>` or
`/open <id>` still works. `/approve <id>`, `/deny <id>`, and `/answer <id>
<text>` remain available for scripts or the `/interactions` view, which
shows full IDs. An ask answered elsewhere disappears at once. A session
created since the last listing is listed again when its first ask arrives.

Example: this TUI views session 1 while another tab's session 2 asks to run
a command:

```text
Pending (1)
! bash echo hi · 2. Fix the build
/pending review request · /sessions past chats · /interactions details
Permission needed in 2. Fix the build · `/pending` to review
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
metadata state's connection state, which the session stream owns.
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
for the semantics. The TUI switches it without a restart, shows it in `/status` and the
`/permissions` picker, and notes every switch in the transcript.

### Switching

Shift+Tab has no app-wide permission shortcut. It still navigates lists and
can skip the target in the local yolo confirmation.

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
line above the controller status state and waits:

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
choice is remembered and sent right after the next session is created (the first prompt, `/new`,
or a command that creates one), before the prompt is admitted. Opening an
existing session instead shows that session's own mode.

**Default for new sessions.** After a successful mode switch, the TUI saves
the selected mode as `permissionMode` in its [preferences file](#preferences-file).
New sessions created by this TUI use that mode, including after a TUI restart;
existing sessions retain the mode stored by the backend. For example, run
`/permissions yolo`, press Enter to confirm, then run `/new`: the new session
starts in `yolo`. Restart the TUI and create another session to use the same
default without another confirmation prompt. Run `/permissions manual` to make
later new sessions ask for permission again. A canceled or rejected switch
does not change the saved default. If a saved bundle mode is no longer
available, applying it to a new session reports a permission mode error and
leaves that session in `manual`.
The preference accepts `manual`, `yolo`, or a `<bundle-id>/<mode-id>` string;
the backend validates the mode when the TUI sends
`PATCH /v1/sessions/{id}` with `{ "permissionMode": "<mode>" }` after
creating a session. The TUI also saves a mode chosen before a session exists
once that mode is successfully applied to the first session.

### Display

`/permissions` marks the effective mode with `●`; `/status` shows it in the
`Mode` row, and the Context pane can be assigned when persistent monitoring is
wanted. Every switch from this TUI or another client adds one muted transcript
notice, `Permission mode → yolo` (a bundle mode uses `Permission mode →
<title> (<id>)`). A permission prompt's hint row also ends with the effective
mode (`… · perm_… · mode manual`). Conversation does not print a separate
mode or command-result status row.

### Bundle modes

A bundle declares modes with `permission_modes:` and answers them with a
`permission.approve` hook (see
[Agent bundle authoring — Permission modes](agent-bundle-authoring.md#permission-modes-permission_modes)).
Installed (for example `hya bundle install --project -y approver.hyabundle`,
or a source directory under `.hya/bundles/<dir>/` where `hya serve` runs),
its modes appear in the picker as `<title> [<bundle id>]` and in the
permission-mode picker after `yolo`. With one active, the approver decides first;
when it defers, the TUI shows the usual permission prompt. Worked example:
a bundle `e2e/approver` whose mode `echo-only` allows `echo …` commands —
`/permissions`, type `echo`, Enter: `/status` reads `Mode        Echo only`,
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
  further back. The controller status state summarizes the files:
  `Reverted · 2 files restored · 1 deleted` (`deleted`: the turn created
  the file), then every file that could not be restored with its reason,
  `skipped big.bin (too_large)` or `failed /etc/x (permission denied)`.
  Paths inside the session's directory are shown relative to it.
- **Commands with a draft.** Press Ctrl+X then `/` and enter `/undo`, `/redo`,
  or `/fork` in the command pane. The message draft stays intact; optional custom
  shortcuts can be assigned with `/keybind set`.
- **The input.** The reverted prompt goes into the input only when the input
  is empty, or still holds, untouched, the prompt a previous `/undo` or
  `/fork` put there (so `/undo` twice leaves the older prompt in it). Text
  you typed is never replaced; the controller status state then ends with
  `the input kept your text`.
- **While a revert is pending** the transcript ends with a line in the
  warning color:
  `↶ 2 messages reverted · /redo restores them · the next prompt makes it permanent`.
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
  (fork)` (the source id when the source is untitled). The sidebar's `Context` section and `/status` show where it came
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

## Thinking effort

Thinking effort controls how hard the model reasons on each request. The TUI
(and so the WebUI) lets you switch it at any moment — before a session
exists, between turns, or while a turn runs. Open `/status` to see
`Thinking    max (pref)`, including the layer that chose it (`pref`, `agent`,
`suffix`, `model default`, or `global default`).

`:default` means no request effort: the provider's own default applies.
`:none` is an explicit off switch. The effort is the server-resolved
`SessionInfo.effectiveEffort`, so the label shows exactly what the next
request sends.

**Usage.** `/effort` (or `/think`) opens the picker for the session's model;
`/effort <level>` sets it directly (`default`, `none`, or one of the model's
advertised variants such as `low`, `high`, `xhigh`, `max`):

```text
/model openai/gpt-6-astra
/effort max
/status             # Thinking    max (pref)
/effort default
/status             # Thinking    default
```

**Where the choice is saved.** The choice is remembered by the backend
(SQLite, shared by every client of the daemon), not in `tui.json`, so the
next start — and every later session on that model — comes back with it. It
is saved on the layer that decides the session's effort, so a switch always
takes effect:

1. A `#suffix` on the session model outranks every saved choice: the session
   switches to the bare model first.
2. If the session's Agent has its own effort (`effortSource` `EFFORT_SOURCE_AGENT`,
   from the Agents view `t`, `agents.<id>.reasoning`, or a bundle), the
   choice becomes the Agent's runtime effort (`PUT /v1/agent-efforts/{agent}`),
   which outranks its configured and authored ones.
3. Otherwise it becomes the model's preference
   (`PUT /v1/model-effort-preferences/{provider}/{model}`).

`/effort default` clears the model's preference, and the Agent's runtime
effort too when the Agent decides.

**Live updates.** A running turn resolves the effort again for every request
round, so a switch during a turn applies from its next request. Both
setters emit a live `catalogUpdated` frame; on it every TUI re-reads its open
session (`GET /v1/sessions/{id}`), so a switch made in one client (for
example the WebUI) shows in every other one (the terminal TUI) without a key
press.

**Interfaces.**

| Action | Call | Body | Reads |
| --- | --- | --- | --- |
| Model's choice | `PUT /v1/model-effort-preferences/{providerId}/{modelId}` | `{effort: string}` (`""` clears) | `ModelEffortPreference {providerId, modelId, effort, updatedAt}` |
| Agent's choice | `PUT /v1/agent-efforts/{agentId}` | `{effort: string, directory?: string}` (`""` clears) | `AgentEffort {agentId, effort}` |
| Drop a suffix | `PATCH /v1/sessions/{id}` | `{model: "provider/model"}` | `SessionInfo` |
| Label | `GET /v1/sessions/{id}` | — | `SessionInfo.effectiveEffort` (`""` = default), `SessionInfo.effortSource` (`EFFORT_SOURCE_SUFFIX`, `_AGENT`, `_PREFERENCE`, `_MODEL_DEFAULT`, `_GLOBAL_DEFAULT`, `_NONE`) |
| Live | global and session streams | — | `catalogUpdated {}` after either setter |

## Pickers

`/model`, `/effort`, and `/sessions` (with no argument) open the same
reusable modal picker `/permissions` uses (see [Permission modes — Switching](#switching)
for the shared filter/move/select keys). Rows are loaded from the catalog already
held by the TUI (`refresh()` at start and `/refresh`), so a picker opens
with no loading state.

- **`/model`** lists every model from `GET /v1/models`, `[tag]`ged with its
  provider id and, when the route advertises one, its context window (`128k ctx`);
  `●` marks the open session's model. Enter sends `UpdateSession {model}`; the
  model's saved effort preference (if any) applies on the server.
- **`/effort`** lists `default`, `none`, and the current model's advertised
  `reasoningVariants`; `●` marks the effort the session uses now. Enter saves
  the choice (see [Thinking effort](#thinking-effort) for which layer takes
  it) and shows `Thinking effort → <level>`. `none` disables the request
  effort explicitly.
- **`/agent`** opens the [Agents view](#agents-view) instead of a picker.
  `/agent <name>` switches directly (`UpdateSession {agent}`).
- **No session yet.** Before any session exists, a `/model`/`/effort`/`/agent`
  choice (picker or direct form) is remembered for the next `CreateSession`;
  the controller status state says it applies when the session is created.
- **`/sessions`** opens a picker with a `New session` row first, then every
  session as a tree (top-level sessions, subagent sessions nested under
  their parent and `[subagent]` tagged — see [Subagents](#subagents)),
  including archived chats by default, showing the agent, model, and a relative update time (`3m`, `2h`) in the
  detail column, and `● running` while busy. `●` marks the open session.
  Enter on the `New session` row runs `/new`; Enter on any other row opens
  it.

```text
┌─Sessions──────────────────────────────────────────────────────────────┐
│ Filter ▏  3 of 3                                                      │
│ ▸   New session          [new]       Create a session with the curr… │
│   ● Fix the flaky test              hya-main · fake/model · 3m       │
│       ↳ Explore the auth code [subagent]  hya-scout · fake/model · 1m│
│ F2 rename · Ctrl+D del · Ctrl+A hides archived · F3 all · Esc closes  │
└──────────────────────────────────────────────────────────────────────┘
```

### Row actions

The `/sessions` picker's highlighted row also takes keys the plain
filter never sees (independent of the message editor):

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
  session is never deleted without it. Deleting a session also removes every
  descendant subagent session and its session-scoped persisted state.
- **Ctrl+A** hides archived sessions, or shows them again (the default list
  reads `GET /v1/sessions?includeArchived=true`; the title says `Sessions ·
  archived included` and archived rows are tagged `[archived]`). Enter on an
  archived row resumes it like `/resume <id>`: it is
  unarchived, then opened. The sidebar never lists archived sessions.

`state/picker.ts`'s `PickerAction` (`{id, key, ctrl?, label, prompt: "value"
| "confirm" | "none", confirmText?}`; `"none"` commits at once, a toggle) and the `"rename"`/`"confirm"` picker modes are
a small, backward-compatible extension of the picker used by `/permissions`:
a picker with no `actions` behaves exactly as before. See
[Code layout — The picker](#code-layout) for the API.

### Session titles

The sidebar's `Sessions` box and the `/sessions` picker show the
session's `title` when the backend has set one (`/rename`, the picker's F2,
or the backend's own auto-generated title once it lands), falling back to
the raw id. A `sessionUpdated {title}` frame (see
[Stream frames and the transcript](#stream-frames-and-the-transcript))
updates both live, with no extra refresh — including a title set by
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
the right — same line colors as a tool card's diff (add/remove/hunk). The
file list windows around the open file (a `N more` marker above/below it
when the change touches more files than fit), so a large change never draws
past the list.

### Keys

Up/Down, PgUp/PgDn, Home/End, and the mouse wheel scroll the open file's
body. `n` / `p` (or `]` / `[`) move to the next / previous file, scrolling
the file list to keep it in view. `r` reloads the diff (after editing files
outside the TUI, for example). Esc closes the view; Ctrl+C closes it too and
keeps its quit meaning. The help overlay (`?`, group `diff`) lists the same
keys.

With no changes the body says `No changes`; outside a git repository (or
when the backend directory is not one) it says `Not a git repository` — the
same signal the metadata state's git branch uses (`GetVcsStatus`), since the
diff route itself does not distinguish the two.

### Diff view interfaces

| Action | Call | Reads |
| --- | --- | --- |
| Open, `r` reload | `GET /v1/vcs/diff?directory=<dir>` | `GetVcsDiffResponse.diff`: one unified-diff text, split client-side on `diff --git` headers into per-file rows (`raw`/`paths` are not sent, so the whole tree's diff is always read; the backend also accepts `paths` to restrict it) |

## MCP servers

`/mcp` opens a full-screen view of every configured MCP server: its
connection state, tool count, and (when failed) its error.

### Keys

Up/Down move the highlight over the server list; Enter opens the
highlighted server's tool list (`MCP › <name>`), each tool under the
server's own name (`tool_01` for the model-facing `mcp__many__tool_01`;
"No tools" while the server is not connected). On that detail screen,
Up/Down/PgUp/PgDn/Home/End move a highlight over the server's tools instead
— the tool list windows around it (a `N more` marker above/below when a
server has more tools than fit), so a server with many tools never draws
past the view. `c` connects the server now, `x` disconnects it; `r`
refreshes; `/` filters the server list by name or state. `a` starts a login
for a server that needs one (`authRequired`): the authorization URL is
copied to the clipboard (OSC 52, the same action `/copy` uses) and shown,
then a one-line pop-up takes the callback code — Enter completes the login,
Esc cancels the pop-up only (the server keeps needing a login). Esc/Left on
the detail screen backs out to the list; Esc on the list closes the view;
Ctrl+C closes it too and keeps its quit meaning. The help overlay (`?`,
group `mcp`) lists the same keys.

### MCP view interfaces

| Action | Call | Reads |
| --- | --- | --- |
| Open, `r` refresh | `GET /v1/mcp?directory=<dir>` | `McpServerStatus[]` (`name`, `state`, `tools` — `mcp__<server>__<tool>` names, set while `CONNECTED` — `error`, `authRequired`) |
| `c` connect | `POST /v1/mcp/{name}/connect` | `McpServerStatus` |
| `x` disconnect | `POST /v1/mcp/{name}/disconnect` | `McpServerStatus` |
| `a` start login | `POST /v1/mcp/{name}/auth` | `{authorizationUrl}` |
| Code pop-up Enter | `POST /v1/mcp/{name}/auth/complete` | `{code}` → `McpServerStatus` |

## Bundles

`/bundles` opens a full-screen view of every bundle of the current scope:
installed ones (user registry), the scope directory's project bundles
(`.hya/bundles`), and the ones shipped with hya (first-party). A row shows
the bundle id, version, scope, state (`active`, `shadowed` by a same-id
bundle of a higher scope, `disabled`, `unreadable`), its TUI extension
(`VM running`, `JIT running`, `VM blocked`, `off`, `—` without one), and what
it contributes (`2 agents · 1 skill · TUI`). A bundle can carry both backend
components (agents, skills, tools, MCP servers, workflows, APIs, hooks,
permission modes) and a TUI extension; the view manages both.

### Keys

Up/Down move the highlight; Enter opens the bundle's details (publisher,
digest, every component id, the TUI extension's permissions, tier, sandbox,
and state). `i` installs a `.hyabundle` package: type its path (a relative
path is resolved against the TUI's working directory; the backend reads the
file, so on a remote backend the path is on that machine), Enter, then choose
`u` user or `p` project (the scope directory's `.hya/bundles`) and Enter. `x`
uninstalls the highlighted bundle after a confirmation; first-party bundles
cannot be uninstalled (`e` disables them instead). `e` enables or disables the
bundle in every scope: a disabled bundle publishes nothing, TUI extension
included; for a bundle with a TUI extension it also sets the TUI's own
`extensionEnabled` switch, so a remote backend's extension is not left
blocked. `t` trusts or untrusts the bundle's TUI extension (the same as
`/extensions trust|untrust`: trusted extensions run on the JIT, without the VM
memory cap; see [Bundle-owned TUI extensions](tui-extensions.md) "Trust
tiers"). `r` reloads; `/` filters by id, scope, state, or contents. After
every change the list and the TUI extension catalog reload, so panels appear
and disappear at once. Esc cancels a running call, closes a pop-up, clears
the filter, backs out of the details, then closes the view. The help overlay
(`?`, group `Bundles`) lists the same keys.

### Bundles view interfaces

| Action | Call |
| --- | --- |
| Open, `r` reload | `GET /v1/bundles?directory=<dir>` (`ListBundles`) |
| `i` install | `POST /v1/bundles:install` `{directory, path, project}` |
| `x` uninstall | `POST /v1/bundles:uninstall` `{directory, bundleId, project}` |
| `e` enable/disable | `POST /v1/bundles:set-enabled` `{directory, bundleId, enabled}`, then `extensionEnabled` |
| `t` trust/untrust | `extensionTrusted` in the TUI preferences; the running extension restarts on its tier |

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

## Agents view

`/agent` opens a full-screen list of every catalog agent, grouped into three
sections, each under a titled divider rule:

- **Primary agents** (`mode: primary`): the agents a session runs on, such as
  `hya-main` and `hya-plan`.
- **Subagents** (`mode: subagent`): the agents the main agent starts with
  `task`, such as `hya-scout` and `hya-reviewer`.
- **System agents** (`hidden: true`): `compaction`, `summary`, and `title`,
  which the harness runs for context compaction, summaries, and session
  titles.

Each row shows the agent id, its effective `provider/model`, the tier that
resolved it (`session`, `configured`, `remembered`, `default`), and its own
default thinking effort (`EFFORT`: `high (set)` for a runtime choice,
`(config)` for `agents.<id>.reasoning`, `(bundle)` for the authored policy;
`default` means the model's own default applies; see
[Agent default effort](configuration.md#agent-default-effort)). `▸` is the
highlight, and `●` marks the agent the open session runs (or the one
remembered for the next session). With a session open, the rows are read
under that session, so its overrides show as `session`.

```text
┌─Agents───────────────────────────────────────────────────────────────────────┐
│ 21 agents · ● runs this session                                              │
│    AGENT                        EFFECTIVE MODEL    SOURCE      EFFORT        │
│ ── Primary agents ────────────────────────────────────────────────────────── │
│ ▸● build                        12th/gpt-6-sol     configured  high (config) │
│    plan                         12th/glm-5.3       default     default       │
│ ── Subagents ─────────────────────────────────────────────────────────────── │
│    explore                      12th/glm-5.3       default     default       │
│ ── System agents ─────────────────────────────────────────────────────────── │
│    compaction                   12th/glm-5.3       default     default       │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Keys

| Key | Action |
| --- | --- |
| Up / Down | Move the highlight over agents in display order; divider rules are skipped. |
| Enter | Select the highlighted primary agent: with a session open it sends `UpdateSession {agent}`, closes the view, and reports `Agent → <id>`; with no session it is remembered for the next `CreateSession`. On a subagent or system agent it shows why instead (`explore is a subagent; only primary agents run a session`). |
| `m` | Open the shared model picker (the one `/model` uses) for the agent's default model. For an agent whose model is not pinned, the choice is remembered in the backend database (`SetAgentModel`). For a pinned agent (`configured`: `agents.<id>.model` in `config.yaml`, a bundle's `config.yml`, or an authored bundle policy), the choice is written to the owning config file and applies at once, and the notice names the file (`build → 12th/gpt-6-sol · saved to ~/.config/hya/config.yaml`). |
| `t` | Open the effort picker (the rows `/effort` shows for the agent's effective model). A level saves it as the agent's runtime default, and `default` clears it. The agent's next request, and every later spawn of it, uses the new level. |
| `c` | Clear a remembered model. On a pinned agent it names the config file instead; use `m` to change the pin. |
| `r` / `/` | Refresh the list / filter the rows by id, section title, or model. Enter keeps the filter, Esc clears it. |
| Esc | Cancel a running call, clear the filter, then close the view. Ctrl+C closes it too and keeps its quit meaning. |

The help overlay (`?`, group `agents`) lists the same keys.

### Agents view interfaces

| Action | Call | Body | Reads |
| --- | --- | --- | --- |
| Open, `r` refresh | `GET /v1/agent-models?directory=<dir>[&session=<id>]` | — | `AgentModelState[]` (`agentId`, `mode`, `hidden`, `configured`, `settable`, `preference`, `preferenceAvailable`, `effective`, `source`, `configuration`, `configurationPath`, `effort`, `effortSource`) |
| Enter | `PATCH /v1/sessions/{id}` | `{agent}` | `SessionInfo` |
| `m` → picker Enter (not pinned) | `PUT /v1/agent-models/{agentId}` | `{directory, preference: {providerId, modelId}}` | `AgentModelState` |
| `m` → picker Enter (pinned) | `PUT /v1/agent-models/{agentId}/configuration` | `{directory, model: {providerId, modelId}}` | `AgentModelState` (`configurationPath` names the file written) |
| `c` clear | `PUT /v1/agent-models/{agentId}` | `{directory}` (no `preference`) | `AgentModelState` |
| `t` → picker Enter | `PUT /v1/agent-efforts/{agentId}` | `{directory, effort}` (`""` for `default`) | `AgentEffort` (`agentId`, `effort`) |

## Interface definitions

The frontend uses the existing HTTP/JSON+SSE transport. Every scoped call
names the directory scope (the absolute `--dir` path, or the active Project's
directory) in its `directory` field: a query parameter on GETs, a body field
otherwise. No request sends the removed `x-hya-directory` header (the server
refuses it). JSON uses protojson lower camel case,
string encoded 64-bit values, and the error envelope documented in the
[protocol guide](protocol/README.md). These are the first-class calls:

| Method and route | Request | Response read by the TUI |
| --- | --- | --- |
| `GET /v1/bootstrap?directory=<dir>` | No body | `Bootstrap` (`location`, `agents`, `models`, `interactions`) |
| `GET /v1/sessions` | No body | `ListSessionsResponse.sessions: SessionInfo[]` (every session of the directory, subagent sessions included; `parent` nests them in the sidebar and the `/sessions` picker, `busy` marks `· running`, `timeUpdated` feeds the picker's relative time). Re-read with each child-session round (see [Subagents](#subagents)). |
| `POST /v1/sessions` | `{agent: string, model: string, workdir: string}` | `CreateSessionResponse.session: SessionInfo` |
| `GET /v1/sessions/{id}` | No body | `SessionInfo` (including `permissionMode`, read by `/status`; `parent`, which makes the view read-only; `members: MemberInfo[]`, the subagent rows the task cards link to; `usage: TokenUsage`, the metadata state's token total, re-read after `tokensRecorded`). For a child session: `busy` and `agent` for its task card. |
| `GET /v1/sessions?includeArchived=true&projectId=<id>` | No body | `ListSessionsResponse.sessions: SessionInfo[]`, including archived chats (`archived`, `archivedAt`, `ephemeral`, `busy`, `timeUpdated`, `parent`, `projectId`). A plain local launch uses it to choose the latest durable root session of the active Project, preferring a tree with a waiting interaction. The `/sessions` and `/resume` pickers also include archived sessions by default. The `projectId` filter is optional for the pickers. |
| `PATCH /v1/sessions/{id}` | `{archived: bool}` | `SessionInfo`: a graceful exit archives the open session's root (`true`); plain relaunch, `--resume`, `/resume`, `/pending` review, and opening an archived `/sessions` row unarchive (`false`). |
| `PATCH /v1/sessions/{id}` | `{title?: string, model?: string, agent?: string, permissionMode?: string}` (`UpdateSession`; `/model`, `/agent`, `/rename`, the `/sessions` picker's F2, and a permission mode switch each send one field; `permissionMode` is `manual`, `yolo`, or `<bundle-id>/<mode-id>`) | `SessionInfo`; after a switch its `permissionMode` is the mode shown. An unknown or unavailable mode fails with `invalid_argument`. |
| `DELETE /v1/sessions/{id}` | No body (`DeleteSession`; the `/sessions` picker's Ctrl+D, confirmed first) | Empty response; deletes the requested session and every descendant subagent session, while unrelated sessions remain. The TUI re-reads the session list and, if the deleted session was open, opens the next top-level one. |
| `GET /v1/agents?directory=<dir>` | No body (`ListAgents`; read with the catalogs) | `ListAgentsResponse.agents: AgentSummary[]` (`name`, `model`, `description`, `hidden`); completes `/agent <name>`. |
| `GET /v1/permission-modes?directory=<dir>` | No body (`ListPermissionModes`; read with the catalogs and by `/permissions`; a `404` from an older backend counts as an empty list) | `ListPermissionModesResponse.modes: [{id, title, description, source}]` — built-ins first; `source` is `builtin` or the bundle id. Feeds the picker rows, and bundle mode titles. |
| `GET /v1/sessions/{id}/messages` | No body | `ListMessagesResponse.messages: MessageInfo[]` (`roundUsage` and `model` of the newest assistant message give the metadata state's `ctx N%`); tool cards read `parts[].toolCall` (`ToolCallPart {callId, tool, state, inputJson, outputJson, durationMs, errorCode, errorMessage}`). For a child session: its latest activity. `parts[].attachment` is an `AttachmentPart {name, mime?, path?, size?}` (never the bytes) — see [Attachments](#attachments). |
| `POST /v1/sessions/{id}/compact` | `{}` (`CompactSession`) | `CompactSessionResponse {compactedUntilSeq, strategy}` for `/compact` |
| `POST /v1/sessions/{id}/summarize` | No body (`SummarizeSession`) | `SummarizeSessionResponse {summaryMessage}` for `/summarize` |
| `POST /v1/sessions/{id}/revert` | `{}` (`/undo`) or `{undo: true}` (`/redo`) (`RevertSession`) | `RevertSessionResponse {session, files}`; `SessionInfo.revert` drives the pending-revert line (see [Undo, redo, and fork](#undo-redo-and-fork)) |
| `POST /v1/sessions/{id}/fork` | `{}` or `{messageId}` (`ForkSession`, `/fork`) | `ForkSessionResponse {session, promptText}`; `SessionInfo.forkedFrom` is shown in the sidebar and `/status` |
| `GET /v1/sessions/{id}/todo` | No body (`GetSessionTodo`) | `TodoList.items: TodoItem[]` for `/todos` and to seed the sidebar's `Todos` box when a session opens; `todoUpdated` frames keep it current. |
| `GET /v1/vcs?directory=<--dir>` | No body (`GetVcsStatus`) | `VcsStatus.branch` for the metadata state's git branch; read when a session opens and after a turn ends. Never errors on a non-repository directory (`branch` comes back empty, so the segment is omitted). |
| `POST /v1/sessions/{id}/turns` | `{prompt: {text: string, attachments?: PromptAttachment[]}}`; `PromptAttachment {name, mime?, data, path?}`, `data` standard base64 of the file bytes — see [Attachments](#attachments) | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{command: {command: string, arguments: string}}` for other slash commands | `CreateTurnResponse.turn: TurnInfo` |
| `POST /v1/sessions/{id}/turns` | `{shell: {command: string, agent: string, model?: {providerId: string, modelId: string}}}` for `!command` (the session's agent and model) | `CreateTurnResponse.turn: TurnInfo` once the command has finished; `id` is the shell turn's assistant message. |
| `POST /v1/sessions/{id}/turns/{turn}/cancel` | `{}` | `TurnInfo`. Esc and `/cancel` send the admitted turn id (the user message id). The server cancels whatever runs in the session, so a shell turn whose id is not known yet is sent as `current`. |
| `GET /v1/fs/find?pattern=**/*<text>*&limit=50&directory=<dir>` | No body (`FindFiles`) | `FindFilesResponse.paths: string[]` (relative paths) for `@file` suggestions. |
| `GET /v1/sessions/{id}` | No body | `SessionInfo.lastSeq` when a session is opened (the stream's first `sinceSeq`). |
| `GET /v1/sessions/{id}/events/stream?sinceSeq=N&includeDescendants=true` | SSE | `StreamFrame` with `event` or `resync`; `N` is the last applied durable seq. `includeDescendants=true` adds the ask frames of every subagent session below (see [Subagent asks](#subagent-asks)). |
| `GET /v1/events/stream?sinceSeq=18446744073709551615` | SSE | `StreamFrame`s of every session, live-only (no durable event passes the watermark); the TUI reads only ask/resolve frames (see [Asks of other sessions](#asks-of-other-sessions)). |
| `GET /v1/sessions/{id}/events?sinceSeq=N&limit=500` | No body | `ListEventsResponse.events` / `nextSeq`, paged, to fill the gap after each stream (re)connect and `resync`. |
| `GET /v1/interactions` | No body (every type, every session; read at start, on a full refresh, after every stream (re)subscribe and `resync`, and after a permission mode switch — never polled) | `ListInteractionsResponse.interactions: Interaction[]`, oldest first. The TUI reads `id`, `session` (the asking session, a subagent's child session included), `type` (`INTERACTION_TYPE_PERMISSION` / `_QUESTION`), `title`, `detail` (a question's header), `options` (a question's option labels), and a permission's `payload`: `action`, `resource`, `always` (what Always allow covers), `callId` (marks the waiting tool card, `◌ <tool>  awaiting approval`), `tool` and `input` (the prompt's details). A listed question has no options or header; the TUI keeps those from its live `questionRequested` frame, else reads them from the waiting `ask_user` call in the transcript. |
| `POST /v1/interactions/{id}/respond` | Prompt: `{permission: {allowed: boolean, persist: boolean}}`, `{question: {answer: string}}`, or `{question: {rejected: true}}`. `/approve`, `/deny`: `persist: false`. | `RespondInteractionResponse.applied` (`false`: already resolved elsewhere) |
| `GET /v1/models` | No body | `ListModelsResponse.models: ModelSummary[]` (`id`, `providerId`, `modelId`, `displayName`, `contextLimit`, `outputLimit`, `reasoning`, `reasoningVariants`, `reasoningDefault`, `source`, `imageInput`); the `/model` picker tags rows by `providerId`, and `/effort` uses the advertised variants; `contextLimit` (a uint64 string, `0`/absent = unknown) is the metadata state's `ctx N%` denominator; the [Provider View](#provider-view) lists a provider's rows with their `source`; `imageInput: false` refuses attachments locally before a turn is sent (see [Attachments](#attachments); absent means unknown and is allowed). |
| `GET /v1/providers` | No body | `ListProvidersResponse.providers: ProviderSummary[]` (`id`, `kind`, `baseUrl`, `keySource`, `auth`, `modelCount`): the Provider View's list. |
| `GET /v1/commands` | No body | `ListCommandsResponse.commands: CommandSummary[]` (includes skills, tagged `source: "skill"`) for command-pane suggestions and completion. |
| `PUT /v1/providers/{id}`, `POST …/refresh`, `PUT …/models`, `DELETE …/models?modelId=`, `POST …/test` | See [Provider View interfaces](#provider-view-interfaces) | `ProviderUpdate` / `TestProviderModelResponse` |
| `PUT /v1/auth/{provider_id}` | `{apiKey: string}` (Provider View `k`) | `{status, provider?, discovery?}`; the key value is sent only to the backend. |
| `DELETE /v1/auth/{provider_id}` | No body (Provider View `x`) | `{provider?}` |
| `GET /v1/workflows` | No body | `ListWorkflowsResponse.workflows: WorkflowSummary[]` |
| `GET /v1/sessions/{id}/workflow` | No body | `WorkflowState` |
| `POST /v1/sessions/{id}/workflow` | `{select: {name: string}}` or `{run: {name: string}}` | `SubmitWorkflowCommandResponse` |

Conversation has no surrounding heading, metadata, status, or footer rows.
`ConversationPane` renders the transcript, active turn/prompt controls, and
composer. `AppState.status: string` remains internal controller state and is not
printed there. For example, `/layout show` no longer inserts a layout status
line into the messages. Press `?` or `/help` for keys, or `/status` to explicitly
open metadata. These use the existing overlays rather than an always-visible
conversation banner. No server operation or configuration field is added.

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
| `errorReported {message, code, errorMessage}` | durable | Stored as the message's error. Shown in the transcript and, at turn end, in the controller status state. |
| `messageFinished {message, finish, cause}` | durable | The turn ends at the first assistant `messageFinished` after the turn's user message whose `finish` is not `FINISH_REASON_TOOL_CALLS`. Then the projection is re-read. |
| `permissionRequested {interaction}`, `questionRequested {interaction}` | live | The ask is added to the pending list at once (a prompt appears); its options and header are remembered by id. With `includeDescendants=true` a subagent's asks arrive here too (`event.session` = the child): they change only the pending list, never the open session's transcript. Other frames of another session are ignored. |
| `interactionResolved {request}` | live | The ask is removed at once (its prompt closes); also for a subagent's ask. |
| `sessionUpdated {permissionMode}` | durable (root session) | The tree's mode changed (this TUI's switch echoed, or another client's): the open session's `permissionMode` is updated, and a `Permission mode → …` notice is added unless the transcript already announced that mode. |
| `sessionUpdated {title, agent, model}` | durable | Patches the session's row (and, if it is the open one, its explicit views and sidebar) at once — a `/rename`/`/model`/`/agent` from another client, or the backend's auto-generated title (see [Session titles](#session-titles)) — instead of waiting for the next catalog refresh. |
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
names-only list. The generic `/api` command sends the supplied path and JSON
unchanged to the named `/v1` route (it adds no `directory` scope); its full request and response schemas are in the
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
| `src/prefs.ts` | The TUI preferences file ([Themes — Preferences file](#preferences-file)): `preferencesPath()` (`HYA_TUI_CONFIG`, XDG, home), `loadPreferences()` (never throws; `warning` for an unusable file), `savePreferences()` (merge + atomic rename), `TuiPreferences`. Thinking effort is not stored here; it is a server-side model preference. |
| `src/launch.ts` | One-command launch: `resolveHyaBinary()` (`--hya`, `HYA_BIN`, `PATH`), `parseReadyLine()`, `defaultDatabase()`, `startBackend()` (spawn `hya serve`, drain its output, wait for readiness, `stop()` with SIGTERM then SIGKILL), `initialSessionId()` (`--continue` / `--session`), `BackendError`. |
| `src/client.ts` | Typed v1 HTTP/JSON+SSE client (`HyaClient` with `streamSession` and `streamGlobal`, `SseDecoder`, `parseApiCommand`); `ModelSummary` carries reasoning variants/defaults for `/effort`; an optional relay bridge token sent as `x-hya-bridge-token`. |
| `src/state/store.ts` | `createAppStore()`: the single store. It holds the server projection (sessions, messages, interactions, models, agents, providers, workflows, backend commands, todos, stream cursor, the open session's subagent members, what was last read about each child session), the published streaming overlay, the prompt queue, the turn state (`running`, `turnId`), and UI state (view, status, the open Provider View's state, sidebar mode, terminal columns, the reasoning switch and per-part toggles, the tool-card switch and per-card toggles, the highlighted prompt option (`promptSelection`, by ask id), whether the input holds text (`draft`), the jump-to-bottom tick, the `/status` text, the backend version from bootstrap, the `/name args` display text of command turns by user message id). Each field is a Solid signal, and only the store's mutation methods change it. |
| `src/state/overlay.ts` | `TranscriptOverlay`: the pure fold of stream frames by message and part id (seq filter, live/durable handover, `resync` handling, turn-end lookup). `mergeTranscript()` merges it over the projection. |
| `src/state/messages.ts` | The transcript view model: `transcriptViews()` (projection + overlay + waiting queued prompts), `messageView()` (role, agent/model, typed blocks, finish notice; cached per message object), `finishNotice()`, `reasoningLabel()`, `reasoningExpanded()`, `toolExpanded()`; transcript notices spliced in by `withDividers()`, including the dividers derived from compaction summaries in the history. |
| `src/state/tools.ts` | The tool-card view model: `toolCard()` (status, per-tool summary, body lines with tones, duration, error, task info), `toolStatus()`, `formatDuration()`, `clipLines()`, `diffLines()`, `partialField()`. |
| `src/state/modes.ts` | Permission modes: `modeCycle()` (local confirmation order), `nextMode()`, `requestMode()` and `confirmKey()` (the yolo confirmation state machine), `modeDisplay()` (metadata state text and tone), `modeNotice()`, `modeRows()` (picker rows), `effectiveMode()`, `isShiftTab()`. |
| `src/state/picker.ts` | The reusable modal picker's pure state (API below): `createPicker()`, `pickerMatches()`, `pickerRows()`, `pickerHighlighted()`, `pickerKey()`, `pickerWindow()`, and the `PickerRow` / `PickerAction` / `PickerSpec` / `ActivePicker` types; `"rename"`/`"confirm"` row-action modes (F2/Ctrl+D on `/sessions`, [Pickers — Row actions](#row-actions)). |
| `src/state/providers.ts` | The [Provider View](#provider-view)'s pure state: `initialProviderView()`, `providerViewKey()` (screens, filter, busy), the pop-up forms (`addProviderForm()`, `setKeyForm()`, `addModelForm()`, `editModelForm()`, `formKey()`, `formPaste()`, `withSecretLength()`), validation (`validateProviderId()`, `validateBaseUrl()`), row text (`providerLine()`, `modelLine()`, `providerDetailHeader()`, `tokenCount()`, `discoveryNotice()`, `testResultText()`), `providerKeyRows` (footer hint and help), and `defaultModelRef()`. |
| `src/app/providers.ts` | `createProviderController()`: the Provider View's calls (one at a time, Esc aborts), the `SecretEntry` behind key fields, the catalog re-read after every write, and the `/model` prompt after adding a provider while the next turn would run on `hya/offline`. |
| `src/state/bundles.ts` | The [Bundles](#bundles) view's pure state: `bundleRows()` (backend bundles merged with the extension states), `bundlesViewKey()` (screens, filter, install and uninstall pop-ups, busy), row and detail text (`bundleLine()`, `bundleDetailLines()`), and `bundleKeyRows` (footer hint and help). |
| `src/app/bundles.ts` | `createBundlesController()`: the Bundles view's calls (one at a time, Esc aborts), the `extensionEnabled`/`extensionTrusted` preferences, and the list and extension-catalog reload after every change. |
| `src/components/BundlesView.tsx` | The full-screen Bundles view. |
| `src/state/catalog.ts` | `/model`/`/effort`/`/sessions` picker row builders: `modelRows()`, `effortRows()`, `sessionRows()` (the `New session` row + `sessionTree()`), `relativeTime()`. |
| `src/state/agentsView.ts`, `src/app/agentsView.ts`, `src/components/AgentsView.tsx` | The [Agents view](#agents-view) (`/agent`): pure state and keys (`agentsViewLines()` sections, `agentsViewKey()`), its calls and pickers (`createAgentsViewController()`), and its rendering. |
| `src/app/modes.ts` | `createModeSwitcher()`: `cycle()` (internal mode navigation), `request(mode)`, `key()` (the confirmation's keys), `applyPending()` (a mode chosen before any session or saved as the TUI default, sent after `CreateSession`); sends `UpdateSession {permissionMode}`, saves the successfully selected default, re-lists interactions, reports in the controller status state. |
| `src/state/prompts.ts` | Permission and question prompts: `promptQueue()` (asks of the open session's tree), `treeSessionIds()`, `promptView()` (headline, asker, details from `toolCard()`, options), `currentPrompt()`, `promptKey()` (option keys), `respondBody()`, `mergeInteractions()` (listing + live frames + answered ids), `waitingKind()`, `askFrameRoute()` (the session stream) and `globalAskRoute()` (the global stream). |
| `src/app/prompts.ts` | `answerPrompt()`: send a choice's `RespondInteraction`, hide the ask, report the outcome in the controller status state. |
| `src/state/members.ts` | Subagents: `foldMember()`, `taskLink()` (card → member and child session), `childStatus()`, `childActivity()`, `childSessionIds()`. |
| `src/state/layout.ts` | Sidebar visibility modes and width breakpoints, plus `parseSwitch()` for `on`/`off` arguments. |
| `src/state/panes.ts`, `src/components/PaneWorkspace.tsx`, `src/components/ConversationPane.tsx` | Versioned ordered row/column containers, legacy migration, tree operations, rendered-bound navigation and stable flat pane instances. |
| `src/state/projectsSidebar.ts` | The left Projects sidebar's pure state: `projectSidebarRows()` (name, busy, session count, active), `projectsSidebarKey()` (Up/Down/Enter/Esc while it has focus). |
| `src/state/scroll.ts` | `ScrollFollow` (the "new messages below" hint), `atBottom()`, `pageStep()`. |
| `src/state/format.ts` | Pure text for the header, sidebar (session list with `sessionTree()` nesting, context box), pending lines, the metadata state (`statusBarSegments()`, `contextUsage()`, `sessionTokens()`, `formatTokens()`), the compaction divider (`compactionText()`), and the non-chat views. |
| `src/app/controller.ts` | `createController()`: refreshes, the session SSE loop (subscribe, `ListEvents` gap-fill, `resync`), the global SSE loop for other sessions' asks (`onGlobalFrame`, backoff), batched overlay flushes, the debounced projection re-read (`app/debounce.ts`), child-session rounds for subagent cards, `returnToParent()`, session creation, prompt submission (refused in a subagent's read-only view), command dispatch, the Provider View (`providerKey`, `providerPaste`, `closeProviders`; app/providers.ts), and `savePreferences` (the `preferencesPath` option; `actions.savePreferences(patch)` for commands). It writes results into the store. |
| `src/app/turns.ts` | `createTurnRunner()`: the client-side prompt queue, `409 session_busy` retry, and turn-end detection and status text. |
| `src/app/revert.ts`, `src/state/revert.ts` | [Undo, redo, and fork](#undo-redo-and-fork): `createRevertController()` (`undo()`, `redo()`, `fork()`, the input prefill rule); `revertSummary()`, `revertIndicator()`, `forkRows()`, `forkSourceText()`, `sessionRow()` (a fresh session row over the open one, dropping a `revert` it no longer has). |
| `src/app/App.tsx`, `src/app/run.tsx`, `src/app/context.ts` | Root split-tree mount and overlays, startup (the started backend, the preferences file and saved theme, then the renderer) and the single `shutdown()` every exit path runs (restore the terminal, stop the backend, exit), and the `AppContext` (store, controller, server URL, and `ui` handles such as the transcript's scroll actions) that components read with `useApp()`. |
| `src/components/` | `ConversationPane` (transcript and input, with no heading/status rows), `MainPanel` (transcript or view panel), `Transcript` (scrollbox, follow/hint), `MessageView` (`MessageItem`, user/assistant messages, blocks, reasoning, tool cards and `task` subagent cards, `KeyedFor`), `Spinner` (the shared spinner clock), `Markdown` (the `<markdown>` wrapper, `SyntaxStyle`, code-block boxes), `Panel`, `PendingBlock` (other sessions' asks), `PromptDock` (the permission / question prompt), `ModeConfirm` (the one-line yolo confirmation), `Picker` (the modal picker), `ProviderView` (the full-screen Provider View and its pop-up forms), `Sidebar` (right: Sessions/Todos/Context), `ProjectsSidebar` (left: every Project, live), `ProjectView` (the full-screen [Project view](#project-view)), `Composer` (the message `<textarea>`, history, shell mode, file list, global key routing), `CommandPane` (separate `<input>`, suggestions, completion, command history), `selection.ts` (`paintSelection`, the theme's mouse-selection color; [Copy](#copy)), `Footer`. |
| `src/composer/` | Pure composer logic: `history.ts` (`InputHistory`), `quit.ts` (`createQuitGuard`, the Ctrl+C double press), `escape.ts` (`escapeAction`), `shell.ts` (`shellCommand`, `isShellInput`), `mention.ts` (`mentionAt`, `insertMention`, `findPattern`, `rankPaths`), `vim.ts` (`vimKey`, the [vim mode](#vim-mode) state machine), `editor.ts` (`editText`, `editorCommand`, `splitCommand`; [External editor](#external-editor)), `clipboard.ts` (`copyNotice`; [Copy](#copy)). |
| `src/commands/` | The slash-command registry (`registry.ts`), the built-in commands (`native.ts`), the key and command help (`help.ts`: `helpRows()`, `helpPickerRows()`, `composerKeyLabel()`, `keyHelpText()`, generated from the binding tables), and the command pane's merge/fuzzy-filter/argument-hint logic (`menu.ts`: `mergeCommandEntries`, `filterCommands`, `requiresArgument`). |
| `src/keys/bindings.ts` | The global key binding table (`keyBindings`, essential defaults only) and the textarea overrides (`composerKeyBindings`: Enter submits; Ctrl+J, Shift+Enter, Alt+Enter insert a newline; Home/End). |
| `src/completion.ts`, `src/api.ts`, `src/theme.ts` | Tab completion and `SecretEntry` (the Provider View's key fields), the `/api` operation catalog (reads `src/operations.json`, generated by `gen-api` so the package ships without the repository's docs; `test/api-catalog.test.ts` checks it matches `docs/protocol/openapi.json` and that no source file imports from outside the package), and the themes: the reactive palette (`colors`, `toolColors`, `diffColors`, `syntaxColors`), `themes`, `themeName()`, `currentTheme()`, `setTheme()`, and `syntaxStylesFor()`, the Markdown/tree-sitter scope styles ([Themes](#themes)). |

The Solid transform has two parts. `bunfig.toml` preloads
`@opentui/solid/preload` for `bun test` and for `bun src/...` run inside the
package. `tsconfig.json` sets `"jsx": "preserve"` and
`"jsxImportSource": "@opentui/solid"`. Bun reads `bunfig.toml` only from the
directory it runs in, so `src/main.ts` imports the preload itself. It then
loads `.tsx` modules and `solid-js` with a dynamic `import()`. Keep static
imports in `main.ts` free of Solid code. Without the preload, Bun resolves
`solid-js` to its non-reactive server build.

**The picker.** `components/Picker.tsx` is a reusable modal list for
choosing one value (`/permissions`, `/model`, `/effort`, and `/sessions` all
use it). Open one from a command handler with `actions.openPicker(spec)`
(or `controller.openPicker`):

```ts
interface PickerColumns { shortcut: string; label: string; tag: string }

interface PickerRow {
  id: string          // value handed to onSelect (a mode id, model id, session id, …)
  shortcut?: string                           // optional, searchable shortcut cell
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
  columns?: PickerColumns  // opt-in shortcut / label / tag headings
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

The name becomes Tab-completable and appears in the command pane and the
help overlay automatically (source `[local]`; it wins a name clash with a
backend command). An `argumentHint` written `[in brackets]` is optional (the command
pane's Enter runs it as is); anything else is treated as required (Enter
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
(`bun run typecheck && bun test ./test && bunx playwright test e2e/<spec>.ts`
for the specs covering your change; CI runs the whole suite; see
[tui-web.md](tui-web.md)). `e2e/hya-tui.spec.ts` and
`e2e/hya-tui-commands.spec.ts` cover the layout, colors, commands, key
entry, narrow widths, and Ctrl+C. `e2e/hya-tui-layout.spec.ts` covers the
main column and sidebar at the default viewport and at about 80 columns
(`/sidebar`), the prompt dock, and the pending block of another session's ask. `e2e/hya-tui-messages.spec.ts`
covers user and assistant styling, Markdown and code highlighting, reasoning
(`/thinking`, click), error, length, and cancel notices, and
scrolling (PgUp/PgDn, End, the wheel, the new-messages hint).
`e2e/hya-tui-streaming.spec.ts` uses the fake model to cover streaming text,
heading previews without marker or color flashes, queued prompts, and the
turn controller status state (`Ready`, provider errors).
`e2e/hya-tui-commands-menu.spec.ts` covers the `/` command pane (open,
fuzzy filter, sources, Up/Down, Tab, Esc, Enter's argument-hint rule), skill
commands (a fixture `SKILL.md` under `.hya/skills/<name>/`), `/compact`,
`/rename`, and `/status`. `e2e/hya-tui-tools.spec.ts` covers tool cards (read, bash, edit/write diff
colors, a failed call, the running spinner, `/tools`, a click) and a
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
`e2e/hya-tui-permission-modes.spec.ts` covers permission modes: command
through xterm.js, the yolo confirmation (Esc, Enter, no second ask), the
metadata state colors, the transcript notice, a bash call under `yolo` without a
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
pending block with its session, then `/pending` opens its numbered prompt (default
and about 80 columns); `e2e/hya-tui-notifications.spec.ts` checks that ask's
single desktop notification. `e2e/hya-tui-bundles.spec.ts` covers the
[Bundles](#bundles) view against a real backend: installing a package (its
sidebar panel appears), disabling and enabling it (the panel goes and comes
back), trusting it (JIT tier), uninstalling it, and the first-party refusal.

### Session pane mouse navigation

The `Sessions` pane is mouse-aware: clicking either line of a session row opens that session. Clicking a subagent row opens its top-level parent session, so the pane always switches the main session tab rather than entering a read-only child. Top-level session groups are separated by horizontal divider lines; these are visual separators and are not clickable.

## Precompiled startup

Release frontends ship precompiled JSX in `dist/app.js` and its sibling chunks.
This avoids loading Babel and transforming the entire UI on every launch. The
HTTP path imports the gRPC implementation only when `--grpc` selects it.

For a source checkout, run `bun install --frozen-lockfile` and `bun run build`
in `packages/hya-tui`, then start `bun src/main.ts` as usual. Rebuild after source
edits; remove `dist/` to return to live source execution. A build writes hashed
chunks first and replaces `dist/app.js` last. Source files, protobuf definitions,
and the adjacent TUI SDK remain required; the bundle is not a standalone binary.
The release workflow and `release-rehearsal` both run this build.

`HYA_STARTUP_TRACE_FILE=<absolute path>` appends JSONL startup diagnostics to a
file rather than the terminal. Each row has `hya_startup: true`, `mark: string`,
`wall_ms: number` (Unix milliseconds), `pid: number`, and optional `detail: string`.
Backend marks use integer milliseconds; frontend marks preserve fractions.
`frontend_launch` starts at the native launch handler, before daemon discovery;
`tui_tree_mounted` records the mounted UI, `tui_controller_ready` records the
completion of the initial controller load, and `tui_extensions_loaded` records
a successful catalog load with every listed extension running. A controller
error appears in the ready mark's detail and must not count as a successful
performance sample. Marks do not assert that a browser has painted the frame.

For repeatable browser verification, build a release backend and all tool-family
libraries, stage all first-party bundles with `xtask stage-first-party-bundles`,
and run the dedicated spec from `packages/hya-tui-web`:

```sh
HYA_BIN=/absolute/package/bin/hya \
  HYA_STARTUP_BUDGET_COLD_MS=100 HYA_STARTUP_BUDGET_WARM_MS=50 \
  bunx playwright test e2e/hya-startup.spec.ts --workers=1
```

The spec launches bare `hya` on a real PTY through the browser, measures a cold
daemon, a subsequent connection to that same daemon, and a stopped daemon
restarted with its existing native cache (`cold_cached`). It verifies commands and
attaches phase timings and screenshots. The two optional positive-number
budget variables enforce milliseconds from the test wrapper’s `frontend_spawn`
(immediately before spawning the executable) to controller
readiness. With neither variable set it verifies correctness without asserting
hardware-dependent timing. Record whether the package/native cache and database
were fresh; do not confuse repeated daemon starts with first-install latency.
