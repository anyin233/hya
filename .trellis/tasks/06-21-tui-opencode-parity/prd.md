# TUI opencode 1:1 parity port

## Goal

Rebuild yaca's terminal UI so that it is a 1:1 match with **opencode's** TUI in both
**feature set** and **appearance**, implemented in Rust on the existing
`ratatui` + `crossterm` stack. The current `yaca-tui` is a minimal single-pane chat
view; the target is the full opencode experience (chat with markdown/diff rendering,
multiline editor with completions, themed status bar, sidebar, splash, and the full
dialog family) running on top of yaca's event-sourced engine.

### User value

A yaca user gets the same polished, discoverable, keyboard-driven experience they
already know from opencode — same layout, same theme, same dialogs, same keymap —
without learning a new tool, while yaca keeps its own branding.

## Locked decisions (confirmed with user)

1. **Scope**: Full 1:1 port, delivered in waves under this Trellis parent task with
   independently verifiable child tasks.
2. **Backend**: Extend yaca's backend (client/server/core) as needed so each parity
   feature actually works (not stubbed) — e.g. model list, session list, file
   listing for completions, permission round-trip, mode/agent switching.
3. **Appearance & branding**: Replicate opencode's **default theme** (palette,
   borders, layout, splash/logo structure) but keep **"yaca"** as the product
   name/logo. opencode layout + opencode colors + yaca name.

## Confirmed facts — yaca current state (verified, explore bg_9e89d395)

- **Stack**: `ratatui 0.28` + `crossterm 0.28` (workspace deps, [Cargo.toml](file:///chivier-disk/yanweiye/Projects/yaca/Cargo.toml)).
- **View layer** [yaca-tui/src/lib.rs](file:///chivier-disk/yanweiye/Projects/yaca/crates/yaca-tui/src/lib.rs): pure renderer over `AppState` (~327 lines; was 252, grown by concurrent work). Renders a 1-line
  status bar, a bordered "conversation" pane (scrollable, wrapped), and a 3-line
  input box. Has *unwired* placeholders for `team` overlay and `pending_permission`
  panel. Renders `Text` and `Tool` parts; **deliberately hides `Reasoning`**. Colors
  are hardcoded locally — no theme system, no config consumption.
- **Driver** [yaca-cli/src/tui.rs](file:///chivier-disk/yanweiye/Projects/yaca/crates/yaca-cli/src/tui.rs): owns terminal I/O. Enables raw mode + alternate screen
  (no mouse capture). Event loop is a `tokio::select!` over (1) in-process engine
  **event bus** subscription, (2) turn-done signal, (3) crossterm input stream.
  Embedded, **not** client/server based — the TUI talks directly to an in-process
  `SessionEngine`.
- **Keymap today**: Ctrl-C / Ctrl-D / Esc = quit; Enter = submit (if idle & non-empty);
  Backspace; PageUp/PageDown = scroll 5; Up/Down = scroll 1; printable chars append.
  No mouse, no command palette, no multi-session nav, no model picker, no permission
  handling, no team ingestion.
- **Modes**: `yaca` (TUI), `yaca exec` (headless single turn), `yaca serve`
  (axum HTTP+SSE), `yaca tail-session` (replay JSONL), `yaca -p` (headless goal mode).
- **Projection contract** [yaca-proto/src/projection.rs](file:///chivier-disk/yanweiye/Projects/yaca/crates/yaca-proto/src/projection.rs): event-sourced `Projection` → `SessionProjection`
  → `MessageProjection` → `PartProjection {Text, Reasoning, Tool}`. Stable enough to
  drive a richer view; some richer state (team, permission asks, model list) lives
  outside `Projection` and must come from core/server side channels.
- **Permission plane**: read/glob/grep auto-allowed; mutating tools remain in **Ask**
  mode. A permission ask/allow/deny path exists in-process (`PermissionPlane` →
  `asks_rx`, decisions `AllowOnce | AllowAlways | Reject{feedback}`); a half-merged
  TUI wiring of it was in flight during exploration.
- **Build baseline**: `cargo check -p yaca-cli` is **green** as of this writing (an
  earlier transient breakage from concurrent in-progress work was resolved). Full
  `cargo build --workspace` to be re-confirmed at execution start (wave 0).

## opencode reference — corrected (verified from real source)

The reference is **opencode's current TUI**, sparse-cloned to `/tmp/opencode-src/packages/tui`.
It is **NOT** the old Go/bubbletea TUI — it is **TypeScript on `@opentui/solid`**
(OpenTUI + Solid.js reactive terminal rendering), ~**27k LOC**. Deps that matter:
`@opentui/core` + `@opentui/solid` (framework), `solid-js`, `diff`, `fuzzysort`
(completions/palette), `effect`, `clipboardy`, `strip-ansi`. We port its **appearance,
features, and keymap** into ratatui (immediate-mode) — not its reactive framework.

Reference file map (high-value):
- `app.tsx` (root) · `routes/home.tsx` · `routes/session/index.tsx` (chat, 2648) ·
  `routes/session/{footer,sidebar,subagent-footer}.tsx`
- `component/prompt/{index,autocomplete,move,workspace}.tsx` · `prompt/{history,stash,frecency,display}`
- `theme/index.ts` (1089) + `theme/assets/*` · `component/logo.tsx` (885) · `keymap.tsx` + `config/keybind.ts`
- `feature-plugins/system/{diff-viewer,which-key,notifications,plugins}.tsx`
- dialogs: `ui/dialog*.tsx` + `component/dialog-*.tsx` (model, session-list, move-session,
  workspace-*, status, retry-action, mcp, stash, console-org) + `command-palette.tsx`
- flows: `routes/session/{permission,question,dialog-message,dialog-fork-from-timeline}.tsx`
- server bridge: `context/{sync,data,local,sdk,runtime,theme}.tsx`

Detailed inventory (components, dialogs, editor, theme hex, logo art, full keymap) is
being extracted by 4 parallel agents → feeds `design.md`.

## yaca backend EXISTS / MISSING matrix (verified — explore bg_9e89d395)

> **UPDATED (reconciliation):** substantial backend substrate already exists in the
> `w1-pi-parity` worktree (SQLite persistence, session list/resume, multi-provider config +
> **Google** + **auth/OAuth**, `ls`/`find`, reasoning/tool-lifecycle events, `since_seq` on
> `/events`). The authoritative current state is
> [research/existing-work-reconciliation.md](./research/existing-work-reconciliation.md). The
> table below is the from-`main` baseline; many "MISSING" rows are EXISTING/PARTIAL on that branch.

Today (on `main`) the only remote API is 4 routes: `POST /sessions`, `POST /sessions/:id/prompt`,
`GET /sessions/:id/events`, `GET /sessions/:id/stream`. The default TUI is **embedded**
(in-process `SessionEngine` + event bus), not client/server, and uses an in-memory store
(sessions not persisted by default).

| Capability | Status | Note |
|---|---|---|
| create session | EXISTS | engine + `POST /sessions` |
| send prompt | EXISTS | `admit_user_prompt` + `run_turn` |
| stream events (bus + SSE) | EXISTS | no client SSE wrapper; `/stream` has no backfill |
| replay / read by known id | EXISTS | no pagination |
| in-process cancel token | PARTIAL | not checked mid-stream/mid-shell; no abort API |
| goal mode | EXISTS (core+CLI) | no server/client/TUI control; no progress events |
| loop mode | EXISTS (core only) | no CLI/server/client/TUI |
| tool registry / schemas | EXISTS (in-proc) | no server/client surface |
| glob/file listing | EXISTS (as tool) | no completion helper API |
| opencode config read | EXISTS (read-only) | providers/models only; no write |
| permission ask/allow/deny | PARTIAL | in-proc contract; no server path |
| **list sessions** | **MISSING** | needed for session switcher |
| **list models / providers** | **MISSING** | needed for model picker |
| **switch model at runtime** | **MISSING** | model fixed in AgentSpec |
| **list/switch agents & modes** | **MISSING** | needed for agent/mode switcher |
| **abort/cancel endpoint** | **MISSING** | needed for esc-to-cancel |
| **team status subscribe** | **MISSING** | snapshot exists; no events/API |
| **token/cost reporting** | **MISSING** | types+ledger exist; not wired end-to-end |
| **theme/keybind config** | **MISSING** | needed for theme picker |
| **session title/rename** | **MISSING** | event shape exists; no surface |
| **session persistence (default)** | **MISSING** | TUI uses in-memory store |

→ Per decision #2, every **MISSING** row that an opencode parity feature depends on must
be built in `yaca-core`/`yaca-server`/`yaca-client` (and likely new `Event`/DTO variants).

## Requirements (draft — refine after evidence)

- R1. Visual parity with opencode's default theme: layout regions, palette, borders,
  splash/logo structure (yaca-branded), markdown + code + diff styling.
- R2. Feature parity: chat (markdown/diff/syntax/tool/reasoning rendering), multiline
  editor with `@` file + `/` command completions, status bar, sidebar, toasts, and the
  full dialog family (model picker, session switcher, theme picker, command palette,
  file picker, permission prompt, help/keybinds, agent/mode switcher).
- R3. Keymap parity with opencode's keybindings + command palette command set.
- R4. Backend extensions for every feature whose data yaca does not yet expose.
- R5. No regressions to headless modes (`exec`, `serve`, `tail-session`, `-p`).
- R6. Quality gate stays green: `cargo fmt --check`, `clippy -D warnings`, `cargo test`.

## Acceptance criteria (draft — make testable per child)

- [ ] Side-by-side, yaca's TUI matches opencode's default-theme appearance for: splash,
      chat transcript, editor, status bar, and each ported dialog (visual QA evidence).
- [ ] Every opencode keybinding + command-palette command has a working yaca equivalent
      (mapped 1:1, documented in a parity checklist; deviations explicitly justified).
- [ ] Each backend-dependent feature works end-to-end against the real engine (not
      stubbed): model picker lists real models; session switcher loads real sessions;
      permission prompt completes a real allow/deny round-trip; file completion lists
      real files.
- [ ] Workspace quality gate green; headless modes unchanged.
- [ ] Manual QA performed in a real terminal (tmux) for every interactive feature.

## Out of scope (draft)

- Non-TUI surfaces (HTTP API shape changes beyond what parity features require).
- Features opencode's TUI does not have.
- Reworking yaca's event-sourcing / provider architecture beyond TUI-driving needs.

## Open questions (to resolve in planning, one at a time)

- Exact wave boundaries + which deliverables become child tasks.
- Whether to keep the embedded engine model or move the TUI onto the client/server
  surface for parity features (depends on backend matrix).
- Theme fidelity bar: how close to opencode's exact hex palette vs. nearest ratatui
  256/truecolor representation.
