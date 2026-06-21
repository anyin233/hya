# Design — yaca TUI 1:1 opencode parity port

Merged from two diverse parallel planners (oracle = conservative/architecture-first;
deep = aggressive/implementation-first), reconciled by the main agent. Companion docs:
[prd.md](./prd.md) · [research/opencode-tui-inventory.md](./research/opencode-tui-inventory.md) · execution in [implement.md](./implement.md).

## 0. Reference pin

- opencode TUI reference: `/tmp/opencode-src/packages/tui` @ commit
  **`5606d2bab9744e5ec7992fffe424569fadf37ae7`** (`5606d2b`, 2026-06-21), TS on `@opentui/solid`.
- `/tmp` is ephemeral. **W0 re-pins** with an explicit target dir (run from anywhere; NOT
  inside the yaca repo — every git op uses `-C` on the clone):
  ```sh
  rm -rf /tmp/opencode-src
  git clone --filter=blob:none --no-checkout https://github.com/sst/opencode /tmp/opencode-src
  git -C /tmp/opencode-src sparse-checkout init --cone
  git -C /tmp/opencode-src sparse-checkout set packages/tui
  git -C /tmp/opencode-src checkout 5606d2bab9744e5ec7992fffe424569fadf37ae7
  git -C /tmp/opencode-src rev-parse HEAD   # must print 5606d2b...
  ```
  (No `--depth 1`: a shallow default-branch clone may not contain the pinned SHA.) Vendoring a
  snapshot into the repo is a W0 opt-in (size cost) — default is SHA + re-clone.

## 1. Key decisions (conflicts resolved)

1. **Embedded-default + transport trait seam.** The default `yaca` TUI keeps the
   in-process `Arc<SessionEngine>` + event bus (it is already the moral equivalent of
   opencode's normalized SSE store). All engine access goes through a `TuiBackend`
   trait so an `HttpBackend` (over `yaca-client` SSE) can be a later drop-in.
   *Resolution:* oracle's stance over deep's "build+test both transports per feature" —
   doubling every feature across two transports buys zero user-visible parity; opencode's
   client/server split is incidental to its separate-TS-core design, not a UX feature.
2. **TEA architecture, pure view.** `yaca-tui` holds `AppModel` + `update(model,msg)->Vec<Effect>`
   + `view(frame,&model)`, **zero I/O**. `yaca-cli` owns terminal, `crossterm::EventStream`,
   a 16ms redraw tick, the transport impl, and the effect runner. Purity is what makes the
   `insta` snapshot harness possible.
3. **`Projection` (proto, from event log) vs `UiStore` (tui, everything else).** The chat
   itself stays the canonical event-sourced projection; sessions list, model/agent/mode
   catalogs, palette state, history/stash/frecency, theme id, keymap, toasts, modal stack,
   prompt parts, permission UI, usage display live in `UiStore` (hydrated via transport
   calls or local JSONL).
4. **Backend: extend, phased.** Adopt deep's comprehensive route/DTO/event table as the
   *target surface*, but build only what each wave needs (oracle's wave discipline). Some
   opencode features map to yaca concepts that **don't exist yet** (workspace "warp"/move,
   MCP, console-org, provider OAuth, team/goal/loop live status) — flagged as the highest
   backend-build risk and pushed late / to explicit out-of-scope review.
5. **Testing: `insta` snapshots** (over a hand-rolled golden harness) + real-terminal tmux QA.
6. **TDD per wave**: RED (failing parity test) → GREEN (smallest impl) → SURFACE (tmux).

## 2. Crate responsibilities

| Crate | Responsibility (target) |
|---|---|
| `yaca-cli` | Terminal lifecycle (raw mode, alt screen, mouse, bracketed paste, panic cleanup), `crossterm::EventStream`, 16ms redraw tick, transport construction, effect runner, CLI flags. **Only place with I/O side-effects.** |
| `yaca-tui` | `AppModel`, `update`, `view`, dialog stack, prompt editor, render models, theme tokens, keymap registry, widgets, `insta` snapshot helpers. **Pure.** |
| `yaca-proto` | All shared wire types: events, DTOs, catalogs, config, projection. No view types leak in. |
| `yaca-core` | Session lifecycle, runtime model/agent/mode switch, cancellation/abort, permission/question plumbing, catalog providers, usage emission. |
| `yaca-store` | Durable session metadata queries, event replay, projection cache, token/cost ledger, UI-config persistence. |
| `yaca-server` | Additive HTTP routes + session/global SSE (with `since_seq` backfill). Needed only for `HttpBackend`. |
| `yaca-client` | Typed methods for new routes + streaming event decode (`eventsource-stream`). |

## 3. `yaca-tui` module tree

```
crates/yaca-tui/src/
  lib.rs            app.rs            backend.rs        model.rs   msg.rs
  update.rs         effects.rs        local.rs          test_support.rs
  sync/             { store.rs, hydrate.rs, selectors.rs }     # SyncStore (Projection + extras)
  theme/            { tokens.rs, builtin.rs (opencode_dark), loader.rs }
  keymap/           { key.rs, registry.rs, commands.rs (Command enum), leader.rs, which_key.rs }
  layout/           { home.rs, session.rs, sidebar.rs, status.rs, footer.rs, scroll.rs }
  prompt/           { editor.rs (ropey), extmark.rs, completion.rs, history.rs, stash.rs,
                      frecency.rs, paste.rs, shell.rs }
  dialog/           { stack.rs, primitive.rs, select.rs, prompt.rs, confirm.rs, alert.rs,
                      help.rs, export.rs, palette.rs, model.rs, provider.rs, session.rs,
                      move_session.rs, workspace.rs, status.rs, mcp.rs, agent.rs, skill.rs,
                      tag.rs, theme.rs, variant.rs, retry.rs, stash.rs, timeline.rs,
                      fork.rs, message.rs, subagent.rs }
  render/           { message.rs, markdown.rs, syntax.rs, ansi.rs, attachment.rs,
                      diagnostics.rs, tool/ (bash, write, edit, read, glob, grep, webfetch,
                      websearch, todowrite, question, task, skill, generic), diff.rs }
  overlay/          { which_key.rs, toast.rs, permission.rs, question.rs, diff_viewer.rs }
  widgets/          { scrollbox.rs, spinner.rs, chip.rs }
  logo.rs           # yaca-branded block-glyph (opencode shadow technique)
  tests/snapshots/  # insta
```

`yaca-cli/src/tui.rs` slims to: terminal init/teardown, `EventStream`, redraw tick, Msg
fan-in, effect runner; **no view state**.

## 4. Key types

```rust
struct AppModel { route: Route, sync: SyncStore, prompt: PromptEditor,
                  dialogs: DialogStack, theme: Theme, toasts: ToastQueue,
                  viewport: ViewportState, interrupt: InterruptState,
                  catalogs: CatalogSnapshot, keymap: KeymapRegistry,
                  commands: CommandRegistry, ui_config: UiConfig, dirty: bool }
enum Route { Home, Session { session: SessionId } }
enum Msg { Event(Envelope), Ask(PermissionAsk), Term(crossterm::Event),
           EffectDone(EffectResult), Key(Chord), LeaderTimeout, Tick, Resync, ... }
enum Effect { Transport(TransportCall), Spawn(BoxFuture), RequestRedraw, SaveLocal(LocalWrite) }

struct SyncStore { sessions: IndexMap<SessionId,SessionRecord>,
                   messages: IndexMap<MessageId,MessageRecord>,
                   parts: IndexMap<PartId,PartRecord>, last_seq: Option<EventSeq>,
                   extras: SessionExtras /* pending_permission, last_usage, aborted */ }
struct PromptEditor { text: Rope, cursor: Cursor, selection: Option<Selection>,
                      parts: Vec<EditorPart>, extmarks: ExtmarkSet, mode: PromptMode,
                      history: PromptHistory, stash: PromptStash, completions: CompletionState }
enum PromptMode { Normal, Shell }
enum EditorPart { Text{range}, FileMention{path,range:Option<LineRange>},
                  AgentMention{agent}, McpResourceMention{server,uri},
                  Attachment{id,kind,label}, LargePaste{id,line_count,char_count} }
struct DialogStack { stack: Vec<DialogKind> }   // render top; sizes 60/88/116
struct Theme { tokens: ThemeTokens }            // no hex literal outside theme/
struct KeymapRegistry { leader: Chord /*Ctrl-X*/, leader_timeout_ms: u64 /*2000*/,
                        bindings: IndexMap<KeyScope, Vec<KeyBinding>> }
enum Command { AppExit, PaletteShow, PromptSubmit, SessionNew, SessionList, ModelList,
               AgentCycle, ThemeSwitch, SidebarToggle, /* one per opencode CommandMap row */ }
```

## 5. Transport seam

```rust
#[async_trait] trait TuiBackend {
  type EventStream: Stream<Item=Result<BackendEvent,BackendError>> + Send + Unpin + 'static;
  async fn create_session(&self, CreateSessionRequest) -> Result<CreateSessionResponse>;
  async fn list_sessions(&self, ListSessionsQuery) -> Result<ListSessionsResponse>;   // NEW
  async fn projection(&self, SessionId) -> Result<SessionProjection>;
  async fn prompt(&self, SessionId, PromptRequest) -> Result<PromptResponse>;
  async fn abort(&self, SessionId, AbortRequest) -> Result<AbortResponse>;            // NEW
  async fn decide_permission(&self, SessionId, AskId, PermissionDecisionRequest) -> ..;// NEW
  async fn catalog(&self) -> Result<CatalogSnapshot>;        // models/agents/modes/cmds NEW
  async fn find_files(&self, FindFilesQuery) -> Result<FindFilesResponse>;            // NEW
  async fn ui_config(&self) / patch_ui_config(&self, UiConfigPatch) -> ..;            // NEW
  fn stream(&self, since: Option<EventSeq>) -> Result<Self::EventStream>;
}
enum BackendEvent { Envelope(Envelope), CatalogChanged(..), PermissionRequested(..),
                    QuestionRequested(..), UsageChanged(..), ResyncRequired{last_seq}, Error(..) }
```
- `EmbeddedBackend { engine: Arc<SessionEngine>, agent }` — the only impl wired into default `yaca`.
- `HttpBackend { client: yaca_client::Client }` — deferred (W6+), behind `--server <url>`; requires the `/stream?since_seq=` backfill fix before it ships.

## 6. Data flow (Solid reactive → ratatui immediate-mode)

| opencode (Solid) | yaca (ratatui TEA) |
|---|---|
| SSE → `context/event.ts` | `TuiBackend::stream()` → `Msg::Event` |
| 16ms batch into `context/sync.tsx` | `tokio::time::interval(16ms)` redraw tick; Msgs fold into `AppModel`; redraw only if `dirty` |
| Solid signals re-render affected nodes | one `view(frame,&model)` per dirty tick; ratatui's buffer diff minimizes writes |
| modal stack | `model.dialogs.stack` (top rendered last) |
| key dispatch + leader | `keymap::LeaderMachine`; cli forwards `Msg::Key`; resolves to `Command`; `update` routes |
| `LocalStore` | `yaca-tui::local` JSONL in `$XDG_STATE_HOME/yaca/tui/` |

Event loop (`yaca-cli::tui::run`): `tokio::select!` over {transport events, permission asks,
crossterm input, effect results, 16ms tick, leader timeout} → drain Msgs → `update` →
spawn Effects → set dirty. **Lag/backfill:** embedded `bus.recv()=Lagged` → re-read
projection + `Msg::Resync`; SSE `/stream?since_seq=N` must backfill before live (W6 blocker).

## 7. Theme, keymap, rendering, editor, dialogs

- **Theme**: `ThemeTokens` mirrors opencode token names 1:1 (so JSON themes load directly).
  Default = inline `opencode_dark` (hex table in inventory §2). hex→`Color::Rgb`; truecolor
  detected via `COLORTERM`, else nearest-256 via `ansi_colours`. **No hex outside `theme/`.**
- **Keymap**: port `config/keybind.ts` defaults; leader=`Ctrl-X`/2000ms `LeaderMachine`;
  editor keys scoped (shadow globals when editor focused); which-key overlay reads bindings-with-prefix.
- **Rendering**: ordered assistant parts (text=markdown, reasoning=collapsible, tool=specialized);
  markdown via `pulldown-cmark` (streaming-safe); syntax via `syntect` themed by `syntax*` tokens;
  diff via `similar`, split if width>120, full-screen `DiffViewer` overlay with file tree/hunk nav.
- **Editor**: `ropey` buffer; `parts[]`+extmarks (visible text ≠ payload); modes normal/shell;
  `@`/`/` completion (`nucleo-matcher`); paste rules (CR→LF, large→`[Pasted ~N lines]`, file→attachment);
  history/stash/frecency JSONL.
- **Dialogs**: `DialogStack` primitive (sizes 60/88/116, Esc/Ctrl-C close, scrim); `select` is the
  base for most; full catalog in inventory §7.

## 8. Backend extension surface (phased target)

Every new route ships: proto request+response DTO, any new `Event` variant, `yaca-core`
method, `yaca-server` route, `yaca-client` method, `EmbeddedBackend` method, tests. Target
table (build per-wave):

| Capability | Method+path | Wave |
|---|---|---|
| list sessions | `GET /sessions` | W2/W3 |
| load projection | `GET /sessions/:id/projection` | W2 |
| global stream + backfill | `GET /stream?since_seq=` | W2 (embedded), W6 (http) |
| abort | `POST /sessions/:id/abort` (+`Event::Aborted`) | W5 |
| permission decision | `POST /sessions/:id/permissions/:ask` (+`PermissionRequested/Resolved`) | W5 |
| models / providers | `GET /models` `GET /providers` | W3 |
| switch model/agent/mode | `PATCH /sessions/:id/{model,agent,mode}` (+changed events) | W3/W5 |
| find files | `GET /workspace/files` | W2 |
| usage | `GET /sessions/:id/usage` (+`UsageRecorded`, emit at provider boundary) | W6 |
| rename/delete/fork/compact | `PATCH/DELETE /sessions/:id`, `POST .../fork|compact` | W5 |
| ui config | `GET/PATCH /config/ui` | W6 |
| catalogs (commands/tools/skills/agents/modes) | `GET /commands /tools /skills /agents /modes` | W3/W6 |
| **mcp / workspace-warp / console-org / provider-oauth / team/goal/loop** | various | **W6 / out-of-scope review** (yaca concepts may not exist) |

Also: ensure `yaca-proto::projection` fully projects reasoning + tool lifecycle (today some
`*End`/`Step*` events are no-ops) so collapsible-reasoning + tool states render.

## 9. New dependencies

Runtime: `pulldown-cmark` (markdown), `syntect` (syntax), `similar` (diff), `nucleo-matcher`
(fuzzy), `ropey` (editor buffer), `unicode-width` + `unicode-segmentation` + `unicode-linebreak`
(CJK/grapheme/wrap), `strip-ansi-escapes`, `arboard` (clipboard), `ignore`+`walkdir` (`@` files),
`base64`+`mime_guess`+`infer` (attachments), `directories` (state paths), `indexmap`, `lru`
(render cache), `ansi_colours` (256 fallback). Dev: `insta` (snapshots), `assert_cmd`+`assert_fs`+`predicates`
(CLI smoke), `tokio-test`, `pretty_assertions`. Deferred: `oauth2`, `open`, `ratatui-image`.

## 10. Testing strategy

- **Unit/contract**: `insta` snapshots over `TestBackend` buffers (serialize char+fg+bg+mods),
  driven by deterministic `Vec<Msg>` through pure `update`. Per-route + per-state goldens.
- **Backend**: embedded-adapter tests; `yaca-server/tests` for routes incl. SSE backfill round-trip.
- **Real-terminal QA (gate)**: tmux launch of `yaca`, scripted keystrokes, `capture-pane`, side-by-side
  vs opencode; via the **visual-qa** skill for each interactive surface.
- **Workspace gate** (every wave): `cargo fmt --all --check` · `cargo clippy --workspace --all-targets -D warnings` · `cargo test --workspace`. Headless modes (`exec/serve/tail-session/-p`) must stay green.

## 11. Risks + rollback

| Risk | Mitigation / validate-by |
|---|---|
| Streaming markdown ugliness (partial fenced code/table) | W0 spike: 3 partial-input fixtures must look right mid-stream |
| TEA 16ms redraw perf under 1000-msg stream | W0 spike: synthetic burst, measure redraws/s; batch harder if <30fps |
| Truecolor vs 256 fidelity | W0/W1: render on `COLORTERM=truecolor` and `TERM=xterm-256color` |
| CJK/wide-char column math (current code uses `chars().count()`) | W0 spike: mixed CJK/emoji golden; `unicode-width` everywhere |
| engine model fixed in AgentSpec | W3: must add `engine.switch_model` before model picker ships |
| `/stream` no backfill | blocker only when `HttpBackend` lit (W6); fix before exposing |
| opencode features w/o yaca backend concept (warp/mcp/oauth/team) | late wave or explicit out-of-scope; do not block core parity |

### Rollback / abort / backout (concrete)

- **Isolation**: each wave runs on branch `tui/w<N>` cut from `main`, tagged `tui-w<N>-start`
  at HEAD before the first edit. `main` always holds the last green wave.
- **Abort triggers (stop the wave, do not push):** (a) 3 consecutive failed GREEN attempts on
  one RED test → STOP + Oracle per Phase 2C; (b) workspace gate (`fmt`/`clippy`/`test`) cannot
  be made green; (c) any headless mode (`exec`/`serve`/`tail-session`/`-p`) regresses;
  (d) `visual-qa` verdict = bad on the wave's surfaces; (e) W7 perf below the §10 threshold.
- **Revert (non-destructive, no shared-history rewrite):** abandon the wave branch —
  `git switch main` (work stays on `tui/w<N>` for diagnosis); or selectively undo with
  `git restore --source tui-w<N>-start -- <paths>`. Never `reset --hard` shared `main`.
- **Cleanup on abort:** kill QA tmux sessions (`tmux kill-session -t yaca-w<N>`), remove temp
  DBs/dirs created by tests (`$XDG_STATE_HOME/yaca/tui-test-*`), drop any scratch ports.
- **W6 persistence backout (special):** sqlite schema migrations are **additive + reversible**
  (each `up` has a `down`); the durable store ships **behind a flag**, default stays
  `--in-memory` until W6 acceptance passes; backout = flag off + `migrate down` to the prior
  schema version. No data-loss path on the default (in-memory) build.
- Each wave is independently shippable; reverting wave N never touches waves < N.

## 12. Out of scope (this port)

Custom text-selection layer (lean on terminal-native), inline image previews (opencode uses
chips), non-TUI API changes beyond parity needs, reworking yaca's event-sourcing/provider core
beyond TUI-driving needs, opencode features with no yaca backend concept unless a wave explicitly adds them.
