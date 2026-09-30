# 0.43.32

## Features

- The message input no longer has a status line above it or a contextual footer below it. Command results and errors now appear directly beneath the conversation header; help and command overlays retain their keyboard instructions.

- The TUI's right sidebar (Sessions, Todos, Context) is always hidden below 110 columns; a pin no longer forces it open, and `/sessions` no longer opens it.
- The header line and the status bar are merged into one top status line that carries the same fields as the sidebar's `Context` box (mode, session, agent, model, occupancy, tokens, directory, branch, todos, server, WebUI, connection). The top status line and the `Context` box are mutually exclusive: exactly one is on screen, decided by width, Ctrl+B / `/sidebar`, and the pane layout.
- The `Context` box gains `Vim`, `Mode`, `Branch`, `Todos`, and `Backend` rows and shows the model with its effort.
