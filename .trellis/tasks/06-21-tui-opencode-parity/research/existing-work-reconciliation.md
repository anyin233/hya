# Reconciliation with existing `w1-pi-parity` work

Two **complementary** efforts exist; this doc reconciles them so the opencode-TUI plan
builds on, not duplicates, the in-flight work.

| Effort | Reference | Focus | Where |
|---|---|---|---|
| `06-20-hya-pi-parity` (existing) | **pi** (`earendil-works/pi`) | **functional** "hya can code": permission fix, ls/find, context, slash cmds, compaction, providers+auth, session tree, RPC | worktree `w1-pi-parity`, branch `feat/hya-w1-agent-can-code`, **uncommitted WIP** (ahead of `main`) |
| `06-21-tui-opencode-parity` (this) | **opencode** TUI | **appearance + UX**: theme/logo/layout, editor parts-model, dialog system, rich markdown/diff render, keymap+leader | `main` |

pi-parity = the functional **backend/substrate**; opencode-parity = the **TUI front-end**. They layer.

## Existing TUI architecture (worktree, verified)

Embedded `SessionEngine` + in-process `EventBus`; `tui::run(engine, agent, model, asks, session)`
`select!`s over bus / turn-done / permission asks / key events. `hya-render-tui` is still **one pure
renderer** (`AppState` direct-mutation in `handle_key` — **no** TEA `Msg`/`update`/`Effect`, **no**
`TuiBackend` trait, **no** theme module). `serve`/`rpc`/`hya-client` exist but the TUI does **not**
use them. → matches our embedded direction; **diverges** from our TEA/trait/theme design (that's the W0 refactor).

## DONE / PARTIAL / MISSING vs our waves

- **W0 foundation — PARTIAL**: renderer/loop split ✅, embedded engine+bus+projection ✅, render tests ✅. MISSING: TEA, `TuiBackend` trait, theme module, logo, `insta` harness, 16ms tick, spikes. → **W0 becomes a REFACTOR of the existing loop, not greenfield.**
- **W1 appearance — PARTIAL**: 3-region layout + scrollback + status(model/session/goal/loop) exist. MISSING: theme tokens (colors hardcoded), logo/splash, opencode 42-col sidebar (only a `team` overlay today), status agent/mode/tokens/spinner, sticky-bottom + nav.
- **W2 editor — MISSING mostly**: input is a single `String`, Enter submits immediately. MISSING: multiline, newline, paste, shell mode, history/stash/frecency, live `@`/`/` completion popup (slash exists only as post-submit parse + templates in `commands.rs`).
- **W3 dialogs — PARTIAL**: `SessionPicker` overlay (basic, `/sessions`) + `/model` switch exist. MISSING: command **palette**, model **picker** dialog, theme picker, which-key, leader system.
- **W4 rich render — MISSING mostly**: tool calls render as one compact line; text is plain (no markdown), reasoning **projected but dropped** in render. MISSING: markdown, syntax, diff viewer, specialized tool renderers, visible reasoning.
- **W5 flows — PARTIAL**: permission plane + overlay (allow-once/allow-always/deny) ✅ wired; new/resume/list-switch sessions ✅; auto-compaction (`compaction.rs`) ✅. MISSING: question flow, real per-session abort/interrupt (Esc just quits), rename/fork/timeline UI. **BUG**: `AllowAlways` not re-consulted on later `assert()`.
- **W6 backend — PARTIAL**: SQLite persistence + `list_sessions` + resume ✅ (but TUI defaults to in-memory: `--db ""`); multi-provider config/router + **Google** + **auth/OAuth** (`auth.rs`) ✅; skills discovery ✅; `TokenUsage`/`CostBreakdown` + token ledger types ✅. MISSING: usage/cost **wire-up** (provider caps `usage_reporting:false`, never surfaced); list-models/agents API; abort/title/permission-decision/question HTTP routes; `/stream?since_seq` **backfill** (only `/events` has `since_seq`); theme/keybind config; opencode-styled dialogs over these.
- **W7 fidelity — MISSING**: plain render tests only; no `insta`, no parity checklist/docs, no benches, no tmux QA script.

## Re-scoped plan deltas (apply to design.md / implement.md)

1. **W0 = refactor, not greenfield**: wrap the existing `tui::run` engine access in `TuiBackend`/`EmbeddedBackend`; introduce `Msg`/`update`/`Effect` + 16ms tick around the existing `select!`; modularize `hya-render-tui`; add theme module + `insta` harness. Keep engine/bus/store/provider untouched.
2. **Backend matrix downgrades** (no longer pure-MISSING): persistence, session list/resume, providers incl. Google, auth/OAuth, RPC/serve, `since_seq` on `/events`, ls/find, reasoning/tool-lifecycle events → **EXISTING/PARTIAL**. Net-new backend shrinks to: usage/cost wire-up, `/stream` backfill, list-models/agents + abort/title/permission-decision/question routes, theme/keybind config.
3. **W5 permission = wiring + bug-fix** (overlay exists): fix `AllowAlways` persistence; add abort/interrupt; question flow; rename/fork/timeline.
4. **W2/W4 remain the largest net-new** (editor parts-model + completion; markdown/diff/syntax/specialized renderers + visible reasoning) — opencode's signature UX the pi-work doesn't touch.

## Coordination decision (needs user)

Where does the opencode-TUI work happen, given the substrate is uncommitted in the `w1-pi-parity` worktree?
- **(A)** In the `w1-pi-parity` worktree/branch — build directly on the WIP substrate (fastest reuse; intermixes with pi-parity WIP).
- **(B)** Commit/merge pi-parity to `main` first, then build opencode-TUI on `main` (clean base; needs the WIP landed first).
- **(C)** Fold opencode-TUI as added waves of the existing `06-20-hya-pi-parity` effort (one unified parity program).
