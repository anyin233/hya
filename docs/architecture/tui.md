# TUI

The terminal UI is split between:

- [`hya-backend/src/tui.rs`](../../crates/hya-backend/src/tui.rs): terminal I/O and
  async event loop.
- [`hya-legacy-tui`](../../crates/hya-legacy-tui): pure ratatui state, layout, theme,
  view-model conversion, and widgets.

## CLI Event Loop

The CLI TUI owns side effects:

- terminal raw mode
- alternate screen enter/leave
- panic hook that restores the terminal before printing panic output
- keyboard input
- mouse wheel scroll events
- spawning assistant turns
- subscribing to the engine event bus
- receiving permission ask requests
- mirroring interactive history to per-session JSON/JSONL bundles

On startup it creates a session, primes the projection, enters the alternate
screen, and draws the initial UI. Each submitted prompt runs in a spawned task:

1. admit the user prompt
2. run one assistant turn
3. inject a system message if prompt admission or turn execution fails
4. notify the UI loop that the turn completed

## Renderer Crate

`hya-legacy-tui` is intentionally free of terminal I/O. Its public entrypoint is
[`draw`](../../crates/hya-legacy-tui/src/lib.rs), which composes four internal
modules:

| Module | Responsibility |
| --- | --- |
| [`layout.rs`](../../crates/hya-legacy-tui/src/layout.rs) | Computes status, timeline, optional sidebar, prompt, and footer rectangles. |
| [`theme.rs`](../../crates/hya-legacy-tui/src/theme.rs) | Defines the dark color palette and base style. |
| [`view_model.rs`](../../crates/hya-legacy-tui/src/view_model.rs) | Converts `Projection` messages into timeline items. |
| [`widgets.rs`](../../crates/hya-legacy-tui/src/widgets.rs) | Renders the status bar, timeline, sidebar, prompt, footer, permission panel, and cursor. |

## Renderer State

[`AppState`](../../crates/hya-legacy-tui/src/lib.rs) holds:

- current `Projection`
- optional goal, loop, team, and permission views
- optional generic dialog view for commands such as model selection, help, and resume
- input buffer
- running flag
- scrollback
- model label
- session label

`AppState::apply` folds an incoming `Envelope` into the projection. The renderer
does not know where the envelope came from.

## Layout

The view is a responsive terminal layout:

1. one-line status bar
2. scrollable timeline
3. input box
4. one-line footer

When the timeline region is at least 110 columns wide, the renderer reserves a
right sidebar for context. Otherwise the timeline uses the full width.

If a permission request is active, a modal-like permission panel is drawn near
the bottom. Otherwise a generic centered dialog may render command lists,
model choices, or resumable sessions.

## Input

General keys:

| Key | Behavior |
| --- | --- |
| `Enter` | Submit input if no turn is running and input is not blank. |
| `Backspace` | Delete one character. |
| `PageUp` / `PageDown` | Scroll five lines. |
| `Home` / `End` | Jump to oldest or newest transcript content. |
| mouse wheel | Scroll transcript. |
| `Up` / `Down` | Navigate prompt history; scroll one line when no prompt history exists. |
| `Tab` while input starts with `/` | Complete slash command prefixes; open command choices when ambiguous. |
| `F2` | Open the model selector. |
| `Ctrl-P` | Open the command/help dialog. |
| `Ctrl-N` | Start a new session. |
| `Ctrl-R` | Open the resume dialog. |
| `Ctrl-C` | Close dialog, clear input, interrupt a running turn, or exit only when idle. |

Slash commands:

Slash command completion is handled by the controller before prompt
submission. `/mo` + `Tab` completes to `/model `; `/` + `Tab` opens the
command list, where `Enter` or `Tab` accepts the selected command.

| Command | Behavior |
| --- | --- |
| `/model`, `/models` | Open model selection. |
| `/resume`, `/sessions` | Open resumable session selection. |
| `/new` | Start a new session. |
| `/compact` | Inject a compacted transcript summary and prune earlier provider context. |
| `/init` | Create a starter `AGENTS.md` without overwriting an existing one. |
| `/agent`, `/agents` | Open the built-in agent profile selector. |
| `/tools`, `/mcp` | Open builtin tool and MCP status. |
| `/export` | Export the transcript as Markdown. |
| `/quit`, `/exit` | Exit the TUI. |
| `/help`, `/?` | Show commands and shortcuts. |

Leading built-in agent mentions, such as `@plan` or `@explore`, are routed
before file-reference expansion. Other `@path` mentions are expanded into
bounded file or directory context blocks.

Generic list dialogs use `Up`/`Down`, `Tab`/`Shift-Tab`, `Home`/`End`,
`PageUp`/`PageDown`, `Enter`, and `Esc`.

Permission panel keys:

| Key | Behavior |
| --- | --- |
| `Left` / `Right` / `Tab` | Move selected decision. |
| `Enter` | Confirm selected decision. |
| `Esc` / `Ctrl-C` | Deny. |
| text input | Add optional rejection feedback. |

## Interactive History

The TUI does not use one central database as the canonical conversation
history. It mirrors each interactive session into its own bundle:

```text
<history-root>/
  index.json
  sessions/
    <session-uuid>/
      meta.json
      events.jsonl
```

`HYA_HISTORY_DIR` overrides the root. Otherwise hya uses `~/.hya/history`.
`index.json` is only a rebuildable listing cache; `meta.json` and
`events.jsonl` are the source of truth. A malformed session bundle is skipped
while other sessions remain listable and resumable.

## Rendering Tool Calls

Tool parts render as compact lines with:

- tool name
- truncated input
- status
- elapsed time for completed calls
- error summary for failed calls

Reasoning parts become a compact `Thinking` timeline marker when they contain
non-empty text.

## Test Boundary

Because `hya-legacy-tui` is pure rendering, tests in
[`../../crates/hya-legacy-tui/tests`](../../crates/hya-legacy-tui/tests) can render states
into buffers without opening a real terminal. The CLI event loop remains covered
separately by controller, history, and dummy harness tests in `hya-backend`.

The dummy harness drives the TUI controller with key events and a local provider
that records requested models while returning a fixed `dummy response`. This
keeps model switching, slash command, and prompt/response tests off the network.
