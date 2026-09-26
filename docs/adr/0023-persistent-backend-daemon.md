# A persistent backend daemon that outlives its clients

> **Superseded in part by [ADR-0024](0024-project-model-and-client-chosen-workspace.md):**
> the daemon's working directory is no longer the starter's (see
> Consequences).

ADR-0022 made one server own a database and let other frontends attach to
it. The server still belonged to whichever frontend started it: bare `hya`
ran it in-process, and a TUI started without `--server` stopped the
`hya serve` it had started when it quit. Quitting that first frontend took
the server away from every other TUI and WebUI tab on the database; they
could only report the loss and be restarted. Every start of `hya` also paid
for composing a server (config, catalogs, MCP, plugins).

## Decision

The backend is a daemon. It runs on its own, is shared by every client of the
database, and outlives all of them. Clients never stop it.

- **Discover or start.** A client (bare `hya`, a TUI without `--server`, a
  TUI that loses its server) looks for the database's server through
  `<db>.server.json` and a health probe (ADR-0022). When none answers it
  starts `hya serve --bind 127.0.0.1:0 --db <db>` **detached**: a new session
  (`setsid`), stdin `/dev/null`, stdout and stderr appended to
  `<db>.server.log`. The database lock (ADR-0022) arbitrates concurrent
  starts: a daemon that loses exits 75, and its starter waits for the
  winner's discovery file; while the lock is held by a server that is still
  starting or shutting down, the starter waits and starts again once the
  lock is free. The one implementation is `hya serve start`; bare `hya`
  calls it in-process and the Bun TUI runs `hya serve start --json`.
- **Manual control.** `hya serve start|status|stop|restart` (default
  database: the durable one). `stop` sends SIGTERM to the lock holder and
  waits until the lock is released; `--force` sends SIGKILL after a timeout.
  Plain `hya serve` still serves in the foreground.
- **Shutdown ends streams.** When a server begins to shut down it ends every
  live event stream (SSE and gRPC) and answers `GET /v1/health` with `503
  unavailable`. Open client streams would otherwise hold the graceful
  shutdown open forever, and clients learn at once that the server is going
  away.
- **Reconnect.** A TUI that knows its database (`--db`, or the default
  without `--server`) treats a stream that ends or fails, followed by two
  failed health probes 500 ms apart, as a lost server. It runs
  discover-or-start again, switches its base URL, resubscribes its streams,
  and reloads the catalogs and the open session from the durable log. The
  status line says `Started a new server · pid N` or `Server moved · now pid
  N`. Bare `hya` passes `--server <url> --db <db> --hya <hya>` to the
  terminal TUI and to the WebUI host's tab command, so every tab behaves the
  same, and a new tab whose fixed URL no longer answers falls back to the
  database's daemon. A TUI with a fixed `--server` and no `--db` never moves.
- **Bare `hya`** runs no server. It discovers or starts the daemon before it
  touches the terminal, runs the WebUI host and the terminal TUI against it,
  and on quit stops only those. `hya --backend <url>` names a server instead
  (no discovery, no start; an unreachable URL is an error).
- **Sessions follow the client, not the server.** A TUI started without
  `--session`/`--continue` creates a session on connect. ~~A session a client
  created and never used is deleted when that client leaves it (another
  session opened, or the client exits), after a server-side re-check that it
  is still empty.~~ Superseded by the amendment "The daemon drops unused
  sessions" below: the daemon deletes it. This replaces the old lazy
  creation, which existed so a quick look did not leave empty sessions
  behind.

## Why

- **Nobody owns the shared state.** With several frontends per database (a
  terminal, WebUI tabs, a second terminal), any owner rule makes quitting one
  of them a disruption for the others. A daemon has no owner to quit.
- **Start is instant after the first.** The next `hya` or TUI attaches to a
  warm server instead of composing one.
- **One start path.** Detaching, the log file, and the race handling live in
  Rust (`hya serve start`); Bun only runs it and parses one JSON line, so the
  TUI and bare `hya` cannot diverge.
- **Reconnecting beats owner handover.** A handover protocol (confirm quits,
  count clients, elect a new owner) was considered and dropped: the lock
  already elects exactly one starter, and the durable log already holds
  everything a client needs to resume.

## Consequences

- A daemon keeps running (and holding its database and port) until `hya
  serve stop`, a reboot, or a crash. `hya serve status` shows it.
- Flags that shape a server (`--model`, `--yolo`, `--pure`) apply to the
  daemon a command starts, and then to every client until it stops. A
  `--yolo` daemon auto-approves tools for every later client of that
  database; start it deliberately.
- A daemon of an older hya keeps serving after an upgrade. Clients show a
  version mismatch notice with `hya serve restart`.
- ~~`hya serve stop` with TUIs open is effectively a restart: they start the
  next daemon.~~ Superseded by the amendment below: a stopped daemon stays
  stopped.
- Turns that run on a server when it stops end with it (closed as cancelled
  by its shutdown drain, or left for crash recovery after a SIGKILL); a
  client's queued prompts are dropped on reconnect.
- ~~The empty-session delete is a read then a delete, two requests: a prompt
  another client sends into that empty session in between is lost with it.
  Only the creating client deletes, only sessions it never used.~~
  Superseded by the amendment "The daemon drops unused sessions" below.
- Headless commands on a daemon's database (`hya --db <db> exec`, `workflow
  use|run|state`) run through the daemon instead of opening the database a
  second time, so their sessions show up in every client. A command that holds
  the lock itself (no daemon running) makes a daemon started meanwhile wait
  (ADR-0022, docs/cli.md "Database lock and the backend daemon").
- ~~The daemon's working directory is the starter's. Requests carry their own
  directory (`x-hya-directory`), so this only matters for requests without
  one.~~ *Superseded in part by ADR-0024:* the daemon has no working directory
  that any request depends on; a request without a directory scope is
  refused or answered from the global view.
- Supersedes ADR-0020's in-process server and ADR-0022's "an attached
  frontend depends on the owner" consequence; the lock and the discovery
  file of ADR-0022 are unchanged.

## Amendment (2026-09-26): a manual stop stays stopped

With the reconnect rule above, `hya serve stop` under open TUIs only
restarted the daemon: the TUIs saw their server go and started the next one.
The backend must be stoppable by hand, so the server now says why it goes
away, and clients start a server by themselves only after an unexpected loss.

- **Reason frame.** When a server shuts down, the last frame of every live
  stream (SSE and gRPC, global and session, and the only frame of a stream
  opened while it shuts down) is the live-only `serverStopping {reason}`
  (`StreamEvent` payload 26), sent before the stream ends. `reason` is
  `stop`, `restart`, or `signal`.
- **How the daemon learns the reason.** `hya serve stop` and `restart` write
  `<db>.server.stop` (`{"pid": <holder>, "reason": "stop"|"restart"}`,
  atomically) before they send SIGTERM. On its termination signal the server
  reads the file, uses it only when `pid` is its own, and deletes it; without
  one the reason is `signal`. Whoever takes the database lock deletes a stale
  file. A v1 admin route (`POST /v1/shutdown`) was considered and rejected:
  it would add a remotely reachable way to stop the server (any local
  process, any web page that can reach loopback), change the contract every
  frontend and the gRPC surface implement, and still need the pid-directed
  signal path for older daemons. The file sits next to the lock (same
  owner, mode, and trust as the lock and discovery files), costs nothing when
  unused, and degrades to `signal` (treated like `stop`) if it cannot be
  written or an old daemon ignores it.
- **Client rules.** `stop`, `signal`, or an unknown reason: start nothing.
  The TUI shows `Backend stopped (hya serve stop) · /reconnect starts it
  again`, refuses prompts, and only *looks* for a server (the discovery file
  plus health, never a start); when another client starts one, it attaches.
  `restart`: wait up to 60 s for the next daemon of the database and attach
  (`Server moved · now pid N`); never start one; after 60 s behave as after
  `stop`. No reason (the stream ended without the frame: crash, `kill -9`,
  lost network): find or start, as before. `/reconnect` finds or starts the
  daemon at once from any state.

Consequences: `hya serve stop` stops for good until a user acts
(`/reconnect`, or a new `hya`/TUI). A client that misses the frame (its
stream was down, backing off, at that moment) treats the stop as a crash and
starts the next daemon; clients older than this amendment ignore the frame
and do the same. A SIGKILL (`stop --force` after the timeout) comes after the
SIGTERM, so the frame was already sent.

## Amendment (2026-09-26): how a client leaves its session

With the daemon, a session outlives the client that shows it. How the client
exits now says what should happen to the session, so sessions a user is done
with do not pile up in the list and sessions left to work keep working.

- **Graceful exit archives.** Ctrl+C twice, `/exit`, or `/quit` in a TUI
  archives the open session at once (`PATCH {archived:true}`; the root when a
  subagent's read-only view is open). Archiving is only a flag
  (docs/protocol/README.md "Archived sessions"): a running turn finishes on
  the daemon.
- **Background exit keeps running.** Ctrl+D on an empty input or
  `/to-background` quits the terminal TUI at once and leaves the session as
  is, running on the daemon.
- **Abnormal exit keeps running.** A signal (the WebUI host's SIGHUP when a
  tab closes, SIGTERM, SIGINT), a kill, or a crash never archives.
- **Switching is not an exit.** Opening another session leaves the previous
  one running.
- **Empty stays deleted.** In every case an empty session the client created
  and never used is deleted instead (the rule above; since the amendment
  "The daemon drops unused sessions" the daemon deletes it, and it is never
  archived).
- **Resume unarchives.** `--resume [id]` (TUI and bare `hya`), `/resume
  [id]`, and opening an archived row of the `/sessions` picker (Ctrl+A shows
  them) unarchive the session and open it. `--continue` picks the newest
  session that is not archived. A WebUI tab cannot pass flags, so `/resume`
  is how tabs and the terminal resume each other's sessions.
- **WebUI tabs.** Closing the tab is a tab's background exit, so a tab's TUI
  does not offer `/to-background`, and its Ctrl+D only shows `Close the tab
  to leave this session running`. The TUI learns it runs in a tab from a
  flag, `--web-tab`, that bare `hya` puts in its web host's tab command; the
  host stays generic (it runs its fixed command), and a host started by hand
  passes the flag in its command. A generic host environment variable was
  considered and rejected: it would put a frontend-specific contract into the
  host, which knows nothing about hya.

Consequences: an archived session is out of the sidebar and `--continue`
until something resumes it or a prompt reaches it (a prompt unarchives on
the backend). ~~A TUI killed while its session is still empty leaves nothing
behind only if its exit handler runs; a SIGKILL leaves the empty session.~~
Since the next amendment a killed TUI leaves nothing behind either.

## Amendment (2026-09-26): the daemon drops unused sessions

The client deleted its empty session itself on the way out: a re-read, a
message listing, and a delete, raced against a 2 s exit budget. Under load
the process exited first and killed the delete (about one exit in seven in
the launch spec), a SIGKILLed TUI never ran it, and a client that had opened
another client's empty session lost it when the creator left. Deciding
"nobody needs this session any more" needs to know who else shows it, and
only the daemon knows that.

- **Ephemeral on create.** A client that opens a session before the user
  asked for one (the TUI on connect, and `/new`) creates it with
  `CreateSessionRequest.ephemeral`. The server records
  `SessionEphemeralSet {ephemeral: true}`; the projection folds
  `SessionProjection.ephemeral` (reducer version 9). The session's first
  message (a prompt, command, or shell turn from any client), a title, an
  archive, or a fork taken from it (`SessionEphemeralSet {ephemeral:
  false}` on the source) clear the mark for good, so replay decides it and
  logs from before the event are never ephemeral. `SessionInfo.ephemeral`
  shows it.
- **Watching is a session stream.** A client watches a session while it
  has a `StreamSessionEvents` stream open on it (SSE or gRPC); the stream
  layer counts them per session. The global stream does not count. A TUI
  always streams its open session, so an open session is never dropped
  under its viewer, whichever client created it.
- **Drop after a grace.** 5 s after the last stream of an unused ephemeral
  session closes, the daemon re-checks it and deletes it, then publishes the
  live `sessionDeleted` frame. The grace lets a reconnecting client (a
  server move, a stream retry) or a reopening one keep it. A session nobody
  ever watched is checked 30 s after creation; at start the daemon sweeps
  leftovers (a crash, a kill, a stop) with a check 30 s after it starts,
  long enough for clients waiting on a `restart` to resubscribe.
- **No lost prompt.** The re-check and the delete hold the session's
  admission slot (the run registry every prompt, command, shell turn, and
  Workflow run reserves), and the delete removes the log only if it did not
  grow since the re-check. A prompt either lands first and keeps the
  session, or is refused (`session_busy` during the delete,
  `session_not_found` after it).
- **Clients never delete.** The TUI no longer deletes on leave or exit; an
  exit never waits for a delete. A graceful exit archives only a used
  session: an unused one it created costs no request, and one another
  client created is archived only when the server says it is not
  ephemeral (archiving would keep it).

A client-side delete with a longer exit budget was considered and rejected:
it cannot see other viewers, cannot survive SIGKILL, and a longer budget
only makes the race rarer. Keeping empty sessions and hiding them from lists
was rejected too: they would still pile up in the database and in `--resume`.

Consequences: an empty session disappears from other clients' lists about
5 s after its last viewer leaves, not at once. A write that is not a turn
(a rename, a model switch) keeps the session when it lands before the
delete (the log grew) and fails with `session_not_found` when it arrives
after; one whose existence check passed just before the delete and whose
append lands just after can still leave a one-event log behind, the same
window any `DeleteSession` has against a concurrent write. Old clients that never
send the flag keep their own client-side delete; an old daemon ignores the
flag, so a new TUI on it leaves its empty sessions behind until the daemon
is upgraded (`hya serve restart`).
