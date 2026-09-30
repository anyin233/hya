# 0.43.29

## Fixes

- Command suggestions now scroll through every matching command and nested argument when using Up/Down or Shift+Tab. The pane still shows at most eight rows, and selection wraps only at the ends of the full list. Commands below the initial visible rows, including `/layout`, are reachable without narrowing the search.

## Features

- **The TUI can connect straight to a gRPC listener.** `bun packages/hya-tui/src/main.ts --grpc HOST:PORT --dir PATH` talks `hya.v1` over gRPC (h2c) instead of HTTP/JSON+SSE. It does not start a local daemon. Sessions, turns, event streams, and the `/api` view all use gRPC. `--grpc` cannot be combined with `--server` or `--db`. The package ships its own copy of the `hya.v1` protobuf definitions so installed releases stay self-contained.
- **The TUI workspace is an editable tiled layout.** The default Projects | Conversation | Sessions/Todos/Context screen is one split tree. `/layout split|assign|focus|resize|close|reset` edits it, and Alt+arrows move focus. The tree is saved in the TUI preferences as `paneLayout: {version: 2, root, active}`, and saved version-1 center-only trees are migrated. Focus changes keep each pane mounted and keep its scroll position.
- **Commands have their own pane.** `/` on an empty message, or Ctrl+X then `/` while drafting, opens a separate `Commands` input with its own history. A slash inside a message stays literal. The pane suggests arguments at every depth; for example, `/layout split ` lists `horizontal` and `vertical`, then pane jobs.
- **The TUI remembers its permission mode.** A successful mode switch is saved as `permissionMode` in `tui.json` and applied to new sessions, including after a restart. Existing sessions keep the mode the backend stored for them.
- **Returning to the TUI reopens the last chat.** A plain local launch reopens the active Project's latest saved conversation, including one archived by `/exit`. A chat with a pending permission or question takes priority. `/sessions` now includes archived chats by default; Ctrl+A hides them. F4 opens the oldest waiting request and shows its numbered answer choices.

## Fixes

- **DeepSeek tool turns continue after a turn with no reasoning.** For `deepseek-*` model ids, the OpenAI Chat encoder now adds `reasoning_content` (`""` when there is none) to every assistant message, even before the transcript contains chat-native reasoning. Previously, a tool turn whose reply streamed no reasoning got HTTP 400 on the follow-up request.
- **Streamed Markdown headings keep their style.** A heading chunk that has only its marker (`### `) waits for the title text. Before, the literal hashes flashed and the heading lost its accent color while it streamed.
- **Consecutive daemon restarts complete from one agent turn.** The old server now holds its turn gate before it acknowledges `queued`, so a tool result cannot start another model round ahead of the handoff checkpoint. A second `hya serve restart` waits for the previous handoff to reach `ready`.

The previous 0.43.28 notes are archived in `docs/changes/CHANGELOG_0.43.28.md`.
