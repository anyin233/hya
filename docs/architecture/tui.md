# TUI

The current terminal UI is split between:

- [`crates/hya`](../../crates/hya): the user-facing frontend binary. It parses
  frontend flags, bootstraps first-run config, creates a pending SDK client, and
  connects to a backend without blocking first paint.
- [`crates/hya-tui`](../../crates/hya-tui): the app-specific terminal runtime.
  It owns raw mode, alternate-screen setup, crossterm `EventStream`, keymaps,
  prompt state, routing, panes, screens, widgets, themes, and rendering.
- [`crates/hya-tui-lib`](../../crates/hya-tui-lib): pure, reusable terminal UI
  primitives: geometry, color, flex layout, overlay/layer validation,
  component descriptors, and ratatui draw adapters.

`hya-backend` no longer owns a TUI controller or renderer. Bare `hya-backend`
still preserves interactive startup by running an HTTP/SSE backend on an
ephemeral loopback port and spawning the current `hya` frontend with `--server`
and, when requested, `--resume`.

## Frontend startup

`crates/hya/src/main.rs` enters the terminal with `Tui::enter()`, starts the
input task, creates a `PendingClient`, and calls `hya_tui::app::run_tui`. The
backend connection happens in `spawn_connect` so the UI can render a connecting
state immediately.

Connection modes live in `crates/hya/src/transport.rs`:

| Mode | Behavior |
| --- | --- |
| default `hya` | Starts `hya_app::HyaRuntime` in-process and talks through the native transport. |
| `hya --http` | Spawns `hya-backend serve` and talks over HTTP/SSE. |
| `hya --server <url>` | Attaches to an already-running hya/Compat-compatible server. |
| `hya --compat` | Uses the bundled Compat bridge instead of the hya runtime. |

Startup `--resume <session>` validates the session with `session_get` after the
client becomes available. A valid session emits `AppEvent::LoadSession`; an
invalid or missing session leaves the current route alone and shows a toast.

## Terminal runtime

`crates/hya-tui/src/tui.rs` owns terminal side effects:

- raw mode enable/disable
- alternate-screen enter/leave
- mouse capture and bracketed paste
- panic hook restoration
- crossterm key, mouse, paste, resize, and focus events
- `TestBackend` construction for headless render tests

`crates/hya-tui/src/app/runtime.rs` owns the async app loop over `AppEvent`:

- draw scheduling, spinner ticks, leader-key timeout, and toast timeout
- backend readiness and queued prompt release
- session/home navigation and startup resume loading
- permission/question modal routing
- slash-command dispatch and command palette actions
- model/agent/session/status/theme/export/copy flows
- read-only auxiliary panes for observing subagent sessions

## State and rendering

[`AppState`](../../crates/hya-tui/src/state/mod.rs) holds shared UI state:

- `MessageStore` folded from backend/SSE events
- sync status
- project metadata
- current route

The runtime owns transient interaction state that is not part of the domain
store: prompt text, history index, dialogs, permission/question views, pane
focus, toasts, animation state, and queued submissions.

Rendering code stays under `crates/hya-tui/src/render`, `screens`, `widgets`,
`theme`, and `prompt`. App-neutral primitives belong in `hya-tui-lib`; anything
that references hya app state, keymaps, providers, sessions, prompts, async
tasks, or terminal events stays in `hya-tui`.

## Panes

The TUI is single-submit by design. The main pane is permanent and owns the
input bar. Auxiliary panes are read-only observers of other sessions; focusing
an aux pane changes scroll/close/cycle behavior but never redirects submitted
prompts away from the main route.

## Slash commands

Client-side built-ins are handled before prompt submission:

| Command | Behavior |
| --- | --- |
| `/model`, `/models` | Open model selection. |
| `/think` | Open reasoning-variant selection for the active model. |
| `/resume`, `/sessions` | Open resumable session selection. |
| `/new`, `/clear` | Start a new session. |
| `/compact` | Request session compaction. |
| `/agent`, `/agents` | Open agent selection. |
| `/tools`, `/mcp` | Open hya status/tool/MCP views. |
| `/export` | Export the current transcript. |
| `/quit`, `/exit`, `/q` | Exit the TUI. |
| `/help`, `/?` | Show help. |

Prompt-macro commands and unknown slash commands fall through to the backend
command catalog or normal prompt handling.

## Test boundary

`crates/hya-tui` tests drive the real runtime with `Tui::from_test_backend` via
`AppHarness`, so render and input assertions stay headless while still covering
the production `Runtime -> Tui -> ratatui buffer` path. `hya` frontend tests cover
argument parsing, first-run bootstrap, and startup-resume validation. Backend
tests cover CLI validation and `hya` launch arguments.
