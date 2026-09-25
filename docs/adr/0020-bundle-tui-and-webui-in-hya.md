# Bundle the TUI and the WebUI into the `hya` command

ADR-0019 made `packages/hya-tui` the interactive TUI but left it out of the
`hya` binary and the release archive. To use it you needed a source checkout,
Bun, and a second command. ADR-0018 made `packages/hya-tui-web` the WebUI host,
which you also started by hand. Users want one command: `hya` should open the
TUI and serve the WebUI.

## Decision

Bare `hya` on a terminal (stdin and stdout are TTYs) starts both frontends.
The `hya` binary orchestrates processes; it does not render.

- **Server in-process.** `hya` composes the v1 server exactly like
  `hya serve` (same `hya-app` composition, global `--db`/`--model`/`--yolo`/
  `--pure`) and binds it to `127.0.0.1:0`. An empty `--db` means the durable
  default database, as for `hya sessions`. It prints no readiness line.
- **Bun children.** `hya` runs the web host (`bun <tui-web>/src/main.ts --host
  127.0.0.1 --port <port> --cwd <cwd> -- bun <tui>/src/main.ts --server <url>
  --dir <cwd>`) on `--port` (default 3250, `0` = a free port), in its own
  process group, and waits for its `hya-tui-web listening on <url>` line. It
  then runs the terminal TUI on the terminal with `--server <url> --dir <cwd>`
  and `--web-url <url>`, or `--web-error <reason>` if the host failed. The TUI
  shows the WebUI address or a `WebUI unavailable` warning. A missing WebUI
  never blocks the TUI.
- **Terminal ownership.** While the TUI runs, `hya`'s stdin is `/dev/null` and
  its stdout and stderr go to `$XDG_STATE_HOME/hya/hya.log`. Server notices and
  the server's own children therefore never draw over, or read from, the
  terminal.
- **Lifecycle.** When the TUI exits, `hya` stops the web host (SIGTERM, then
  SIGKILL). The host ends every tab's TUI (SIGHUP, then SIGKILL to the tab's
  process group) before it exits. The server then drains and shuts down, and
  `hya` exits with the TUI's status. SIGINT, SIGTERM, and SIGHUP to `hya` stop
  the TUI first and then do the same cleanup.
- **Packaging.** The release archive and `install.sh` ship both packages with
  production dependencies as `lib/hya/tui` and `lib/hya/tui-web`. Each package
  must stay self-contained, with no imports from outside it. `hya` finds each
  package through `HYA_TUI_DIR` / `HYA_TUI_WEB_DIR`, then next to the binary
  (`<prefix>/lib/hya/…`), then in the source checkout it was built from (the
  same order as the Bun plugin adapter). Bun is required: `$BUN`, else `bun` on
  `PATH`. Without Bun or the packages, bare `hya` exits 1 with a clear message
  before it touches the terminal. Without a terminal it prints the guidance
  banner, as before.

## Why rendering still stays out of the Rust backend

The TUI remains the one OpenTUI program that ADR-0019 adopted. The WebUI
remains that program on a PTY behind the generic host from ADR-0018. `hya` only
spawns and supervises them, and the host still runs only the fixed command it
is given. Both frontends reach the server over the public `hya.v1` contract on
loopback, like any other client, so the in-process server needs no special
path for them. Rendering in Rust (a ratatui TUI, or a PTY host inside
`hya serve`) would bring back the second frontend that ADR-0005, ADR-0010, and
ADR-0019 removed.

## Consequences

- Bun is a runtime requirement of bare `hya`. Every other subcommand works
  without it.
- The release archive grows by the two packages and their production
  `node_modules`. The TUI's `/api` catalog is generated into the package
  (`src/operations.json`, by `gen-api`) instead of importing
  `docs/protocol/openapi.json`.
- `hya --port` is a bare-only flag. `hya serve --port` keeps its own meaning,
  and `--port` together with a subcommand or `-p` is an error.
- Scripts that run `hya` with no arguments and no terminal still get the
  banner and exit 0.
- A `hya` killed with SIGKILL cannot clean up. The web host then outlives it
  until it is stopped by hand.
- ADR-0019's "not bundled into the `hya` binary or the release archive" is
  superseded: the TUI is not linked into the binary, but it ships beside it and
  `hya` runs it. ADR-0018's host-outside-`hya serve` decision stands: the
  host is a separate Bun process.
