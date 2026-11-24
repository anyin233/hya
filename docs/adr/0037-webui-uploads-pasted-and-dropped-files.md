# ADR-0037: The WebUI uploads pasted and dropped files

Date: 2026-11-23
Status: Accepted

## Context

The WebUI (ADR-0018, ADR-0020) shows the TUI running on a PTY on the host
machine. A user who reaches it over an SSH port forward is not on that
machine: a screenshot in the browser's clipboard, or a file on their own disk,
cannot reach the TUI. The page only forwards xterm.js text input, so pasting
an image does nothing, and dropping a file onto the tab opens it in the
browser. A native terminal pastes a dragged file as its path, which the TUI
already understands (docs/tui.md "Paste and attachments"), but over SSH that
path does not exist on the host.

ADR-0018 keeps the host generic: it runs one fixed command and renders its
terminal, and a browser-only feature needs its own decision.

## Decision

- The page sends a pasted or dropped file to the host as a new
  `PtyClientFrame.upload { id, name, mime, data }` on the same WebSocket
  (p2 PQ4: an upload belongs to the tab that made it, so no extra route,
  token, or connection identity is needed).
- The host stores it in a directory of its own for that connection,
  `$TMPDIR/hya-tui-web-<pid>/<connection>/` (0700), under a sanitized name,
  and answers `PtyServerFrame.uploaded { id, path }` or
  `upload_failed { id, reason }`. Limits: 20 MiB per file, 32 files per
  connection; the directory is removed when the connection closes and the
  root when the host stops.
- The page then pastes the stored paths into the terminal, single-quoted, as a
  native terminal pastes dragged files. The host never interprets a file or
  tells the program about it; the program sees an ordinary paste.
- `hya serve` PTY sessions (`/v1/pty/{id}/connect`, gRPC `StreamPty`) answer
  every upload with `upload_failed`: they are shells on the backend, not the
  WebUI host.
- The same-origin check stays the only guard, as for every other frame.

## Consequences

- An SSH user can paste a screenshot or drop files into the WebUI; the TUI
  turns them into image and file chips without knowing they were uploaded.
- The frame contract changes in `proto/hya/v1/pty.proto`,
  `packages/hya-tui-web/src/frames.ts`, and docs/tui-web.md together.
- The host writes files to the local disk for as long as a tab is open;
  anyone who can open a same-origin tab could already run the command.
- A program other than the hya TUI gets the paths as text, which is what a
  terminal emulator gives it for a drag and drop.
