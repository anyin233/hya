# Render the TUI in a browser for tests and the WebUI

The Bun/OpenTUI frontend (`packages/hya-tui`) needs visual and interaction
tests. Driving it through tmux gives only a scraped character grid. That grid
has no colors or glyph widths, and it depends on timing. We also want a WebUI
without building a second frontend.

## Decision

A separate package, `packages/hya-tui-web`, runs the TUI on a real PTY
(Bun 1.4 `Bun.spawn({ terminal })`) and streams it over a WebSocket to xterm.js
in the browser. Playwright tests drive Chromium and assert on the xterm buffer:
text, per-cell style, and size. They also attach screenshots. The same host
serves the WebUI, one TUI process per browser connection.

- The host runs a fixed command. The browser can never choose argv.
- WebSocket frames reuse the `hya.v1` `PtyClientFrame`/`PtyServerFrame`
  protojson shapes. A later client could attach to `/v1/pty` unchanged.
- The host lives outside `hya serve`. Rendering stays out of the backend, as
  AGENTS.md requires.

## Considered options

- **Host through `/v1/pty` in `hya-server`.** Rejected for now. Its sessions
  use piped stdio, not a PTY; they ignore resize; and they accept no argv. So
  OpenTUI (raw mode, `isatty`, SIGWINCH) cannot run there. Fixing that would
  put terminal hosting in the backend.
- **A DOM renderer inside OpenTUI.** Rejected. It would test a different
  renderer than the one users run in a terminal.
- **tmux capture.** Rejected. It loses styles and wide-glyph layout, and its
  timing is flaky.

## Consequences

- TUI tests exercise the real OpenTUI escape output through a real terminal
  emulator.
- The WebUI shows exactly what the TUI shows. Browser-only affordances need
  their own later decision.
- The host spawns processes for any same-origin client, so it binds loopback
  by default and rejects cross-origin WebSocket upgrades.
