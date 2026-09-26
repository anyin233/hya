# One server per database: a lock and a discovery file

Bare `hya` (ADR-0020) and the TUI's one-command launch each started their own
server on the default database, `$XDG_STATE_HOME/hya/sessions.db`. SQLite in
WAL mode with a 5 s busy timeout allowed that, so two servers wrote the same
event log. Neither knew about the other. Live events (streamed turns, asks,
renames) from one server never reached the other's clients, and each ran its
own crash recovery and resident supervisors over the same rows.

## Decision

A database file has at most one server, and every other frontend uses that
server.

- **Lock.** `hya serve --db <file>` and bare `hya`'s in-process server take
  an exclusive advisory lock (`flock`, non-blocking) on `<db>.lock` before
  they open the store. They hold it for the whole life of the process, and
  the OS releases it on exit, including a crash or SIGKILL. The lock file
  holds the owner's pid for error messages. It is never deleted. `<db>` is
  the database path with its directory canonicalized. In-memory stores (`""`,
  `:memory:`) and SQLite URIs are not locked.
- **Discovery file.** Once its listener is bound, the owner writes
  `<db>.server.json` atomically (temporary file, then rename):
  `{"url": "http://127.0.0.1:<port>", "pid": <u32>, "version": "<hya
  version>", "startedAt": <unix ms>}`. An unspecified bind address is
  published as loopback. A clean shutdown removes the file after the drain
  and before the lock is released. The file is trusted only while the lock is
  held: whoever takes the lock deletes any discovery file it finds, because
  that file belongs to a crashed owner.
- **`hya serve` on a held database** fails before it composes anything. It
  exits with status **75** (`EX_TEMPFAIL`) and one stderr line that names the
  running server's pid and URL from the discovery file, or the lock holder's
  pid if the holder has not published yet.
- **Bare `hya` on a held database** attaches. If the discovery file answers
  `GET /v1/health` with `ok: true`, `hya` starts no server and points the
  WebUI host and the terminal TUI at that URL. The TUI gets
  `--attached-pid <pid>`. Quitting stops only the web host and the TUI. If
  the holder has not published a healthy server within 20 s, bare `hya`
  exits 1 with a message before it touches the terminal. During those 20 s it
  retries the lock every 250 ms, in case the holder is starting up or
  shutting down.
- **TUI self-launch** (no `--server`) does the same from Bun, which cannot test
  a `flock`. The TUI reads `<db>.server.json` and attaches only if the pid is
  alive and the health probe answers. Otherwise it starts `hya serve`. If
  that exits 75 (another launcher won the race, or the holder is still
  starting), the TUI waits up to 20 s for the holder's discovery file and
  attaches, or reports a clear error. `/status` says `started by this TUI` or
  `attached to a running server`. A TUI never stops a server it attached to.

## Why a lock plus a file, not a socket or a registry

The lock is the only part that must be correct under crashes. `flock` on a
local file is released by the kernel, so a dead owner can never block the
database. The discovery file is only a hint, checked against the lock (Rust)
or against a live pid plus a health probe (Bun). A stale file therefore costs
one failed probe, never a wrong attach. Keeping both files next to the
database ties discovery to the store rather than to a per-user registry:
`--db` keeps its meaning, and two databases never interfere.

## Consequences

- One process owns a database's live state. Every frontend on it, whether a
  TUI, a WebUI tab, or bare `hya`, sees the same sessions and live events.
- An attached frontend depends on the owner. When the owner exits (for
  example the first TUI quits and stops the `hya serve` it started),
  attached TUIs lose their server and say so. They do not take over. Restart
  them to start a new owner. *Superseded by
  [ADR-0023](0023-persistent-backend-daemon.md):* the server is a daemon no
  client owns or stops, and a TUI that loses it finds or starts the next one.
- Flags that shape the server (`--model`, `--yolo`, `--pure`, config and
  environment) come from the owner. Bare `hya` notes in its log that its own
  flags do not apply when it attaches.
- The health probe does not prove that the URL serves this database. A pid
  reused by an unrelated `hya` server on the same port is theoretically
  possible and accepted.
- `hya exec`, `hya sessions`, `tail-session`, and `workflow` open the store
  without the lock, as before. Read-only commands are safe. `exec --db` on a
  served database remains a second writer.
- Advisory locks on network filesystems may not work. Keep the database on a
  local disk.
- Amends ADR-0020: bare `hya` starts its in-process server only when no
  other server owns the database.
