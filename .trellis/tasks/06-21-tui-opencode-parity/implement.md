# Implement — yaca TUI 1:1 opencode parity (execution plan)

Execution plan for [design.md](./design.md). Each wave = one Trellis **child task**
(independently planned/implemented/checked/archived). Dependencies are explicit, not
tree-implied. Every wave is TDD (RED→GREEN→SURFACE), ends green on the workspace gate,
and is rollback-tagged.

> **RECONCILIATION (read first):** an in-flight `w1-pi-parity` worktree already supplies the
> functional substrate (embedded engine+bus, SQLite persistence, providers incl. Google,
> auth/OAuth, basic permission overlay + session picker, ls/find). See
> [research/existing-work-reconciliation.md](./research/existing-work-reconciliation.md).
> Therefore **W0 is a REFACTOR of the existing `tui::run` loop** (wrap engine in `TuiBackend`,
> add `Msg`/`update`/`Effect` + theme + `insta`), NOT greenfield; W5 permission is wiring +
> the `AllowAlways` bug-fix; W6's net-new shrinks to usage wire-up, `/stream` backfill,
> list-models/abort/title/decision routes, theme/keybind config. The coordination decision
> (which branch/worktree) must be settled before W0 `task.py start`.

## Global rules

- **Workspace gate (every wave, before "done")**: `cargo fmt --all --check` ·
  `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`.
  Headless modes (`yaca exec`, `serve`, `tail-session`, `-p`) MUST stay green.
- **TDD**: write the failing parity test first (named), watch RED for the right reason,
  GREEN with the smallest change, then SURFACE (tmux real-terminal capture).
- **Rollback**: `git tag tui-w<N>-start` before each wave.
- **Reviewer gate**: after W0 design lock and before W1 execution, and again at W4 and W7,
  run the **plan-review**/oracle gate; binding.
- **Backend rule**: a backend-dependent feature is "done" only when it works end-to-end
  against the real `EmbeddedBackend` (not stubbed), proven in tmux.
- **Atomic-decomposition rule**: no wave begins coding until its child-task `implement.md`
  decomposes it into ordered steps each completable in **1–3 tool calls**, naming the exact
  file(s), the API/signature added, and the first RED test. W0 below is fully decomposed as
  the template every child task follows.

## Wave overview

| Wave | Child task | Goal | Hard deps |
|---|---|---|---|
| W0 | `tui-w0-foundation` | reference pin, baseline, transport+TEA skeleton, theme, insta harness, de-risk spikes, RED parity tests | — |
| W1 | `tui-w1-appearance` | home+session shell, layout, scrollbox, status bar, splash, theme-correct render | W0 |
| W2 | `tui-w2-editor` | prompt editor (parts/extmarks), `@`/`/` completion, history/stash/frecency; backend: find-files, list-sessions, projection-load | W0 |
| W3 | `tui-w3-dialogs` | dialog stack, palette, model picker, session switcher, theme picker, which-key, full keymap; backend: list/switch models+sessions | W1,W2 |
| W4 | `tui-w4-rich-render` | specialized tool renderers, split/unified diff viewer, markdown+syntax | W1 |
| W5 | `tui-w5-flows` | permission+question flows, abort/interrupt, sidebar, lifecycle (new/rename/delete/compact/fork/timeline) | W3 |
| W6a | `tui-w6a-backend-core` | session persistence (flagged), token/cost end-to-end, `/stream` backfill, usage in status/sidebar | W3,W5 |
| W6b | `tui-w6b-backend-dialogs` | remaining dialogs (agent/variant/mcp/status/stash/skill/tag/provider), theme loader + 33 themes, keybind config, optional `HttpBackend` | W6a |
| W7 | `tui-w7-fidelity` | parity-checklist QA, keymap audit, perf, polish, docs | all |

Sequencing note: W2 and W4 can run parallel to W1's tail once W0 lands; W3 needs W1+W2;
W5 needs W3; W6a needs W3+W5, W6b needs W6a; W7 last.

---

## W0 — Foundation (DERISK FIRST)

**Deliverables**
- Re-pin reference (SHA `5606d2b`); record baseline gate output; decide vendor-snapshot opt-in.
- `TuiBackend` trait + `EmbeddedBackend` impl; move current TUI behavior onto it (zero behavior change).
- `yaca-tui` TEA skeleton: `AppModel`/`Msg`/`update`/`view`/`Effect` + cli 16ms redraw tick + effect runner; existing chat re-implemented through it.
- `theme/` module + `Theme::opencode_dark()` (full hex table); `COLORTERM` truecolor detect + 256 fallback.
- `insta` snapshot harness + `test_support` (build AppModel, render TestBackend, snapshot).
- Spikes: streaming-markdown (`pulldown-cmark`, 3 partial fixtures), CJK/wide-char (`unicode-width`), 1000-event redraw perf, mouse + bracketed paste enabled in cli.

**Atomic steps (ordered — the decomposition template every wave child task copies)**
1. `crates/yaca-tui/src/theme/tokens.rs`: add `struct ThemeTokens` + `enum Token` (names mirror opencode). RED: `parity_theme.rs::token_count_matches`.
2. `crates/yaca-tui/src/theme/builtin.rs`: add `fn opencode_dark() -> ThemeTokens` (hex table, inventory §2). RED: `parity_theme.rs::default_theme_tokens_match_opencode_dark`.
3. `crates/yaca-tui/src/theme/mod.rs`: `Color` mapping hex→`Color::Rgb`; `COLORTERM` truecolor detect + `ansi_colours` 256 fallback. RED: `theme::tests::truecolor_and_256_fallback`.
4. `crates/yaca-tui/src/msg.rs` + `model.rs`: define `Msg`, `AppModel` (port current `AppState` fields). Compile-only.
5. `crates/yaca-tui/src/update.rs` + `effects.rs`: `update(&mut AppModel, Msg) -> Vec<Effect>`; port current key/scroll/submit handling. RED: `update::tests::enter_submits_nonempty`.
6. `crates/yaca-tui/src/backend.rs`: `trait TuiBackend` + `struct EmbeddedBackend`. Compile-only.
7. `crates/yaca-cli/src/tui.rs`: rewrite loop to `select!{events,asks,term,effects,16ms tick,leader}` driving `update`/`view` via `EmbeddedBackend`. SURFACE: tmux smoke == pre-W0.
8. `crates/yaca-tui/src/test_support.rs`: `assert_snapshot(model,w,h)` over `insta`+`TestBackend`. RED: the three parity tests below.
9. `crates/yaca-tui/src/logo.rs`: yaca block-glyph + shadow. RED: `parity_home.rs::home_80x24_shows_yaca_logo_and_primary_fab283`.
10. Spikes (own test files): `markdown::tests::partial_fenced_table_link` (3 fixtures), `parity_cjk.rs::mixed_cjk_emoji_width_stable`, `perf::tests::burst_1000_events_redraw` (assert p95 frame < 33ms).

**RED tests (write first)**
- `yaca-tui/tests/parity_home.rs::home_80x24_shows_yaca_logo_and_primary_fab283`
- `yaca-tui/tests/parity_theme.rs::default_theme_tokens_match_opencode_dark`
- `yaca-tui/tests/parity_cjk.rs::mixed_cjk_emoji_width_stable`
- `yaca-tui/tests/perf.rs::burst_1000_events_redraw_p95_under_33ms`

**Acceptance**: skeleton renders current behavior identically; theme tokens exact; CJK golden stable; markdown spike clean mid-stream; perf ≥30fps under burst. Workspace gate green.
**Verify (tmux)**: `tmux new -s yaca-w0 'cargo run -p yaca-cli'`; smoke a turn; `capture-pane` vs pre-W0.
**Rollback**: `git tag tui-w0-start`.

## W1 — Appearance shell

**Deliverables**: `Route::Home` (yaca block-glyph logo + shadow technique), `Route::Session`
4-region layout (status 1 · sidebar 42 when width>120 · body · prompt-area min1/max(6,h/3)),
`widgets::scrollbox` sticky-bottom + page/half/line/home/end, `status_bar` (agent·model·mode·session·spinner·tokens-slot),
upgraded text/tool/reasoning render (ordered parts, collapsible reasoning), `spinner` (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`@80ms).
**Backend**: none (sidebar/usage placeholders).
**RED**: `parity_layout.rs::session_160x48_uses_42col_sidebar`; `parity_home.rs::home_centers_logo_and_prompt`.
**Acceptance**: tmux side-by-side vs opencode matches color/layout for splash, status, body region; goldens for home + empty session. **Verify**: visual-qa skill (tmux capture diff). **Rollback**: tag.

## W2 — Editor parity

**Deliverables**: `prompt::editor` (`ropey`, multiline wrap, min1/max(6,h/3)); modes normal+shell
(`!`@col0); keys (Enter submit; Shift/Ctrl/Alt+Enter,Ctrl-J newline; readline; Ctrl-C clear; Ctrl-V paste);
Esc 2-stage interrupt; parts/extmarks; large-paste placeholder; `completion` popup `@`-files/`/`-cmds (`nucleo-matcher`);
`local` history(50)/stash(50 LIFO)/frecency(1000) JSONL in `$XDG_STATE_HOME/yaca/tui/`.
**Backend**: `find_files(query,limit)` (`ignore`/`walkdir`) + `EmbeddedBackend::find_files`; `list_sessions` + `projection` load (foundation for W3).
**RED**: `parity_editor.rs::keymap_matrix` (every editor key); `parity_complete.rs::at_lists_real_files`; `parity_paste.rs::large_paste_roundtrips`.
**Acceptance**: full editor keymap works; `@` lists real files; paste roundtrips on submit; history persists across launches. **Verify**: tmux keymap script + completion-popup golden. **Rollback**: tag.

## W3 — Dialog system + core dialogs

**Deliverables**: `dialog` primitive (60/88/116, Esc/Ctrl-C, scrim); `select`/`prompt`/`confirm`/`alert`/`help`;
`palette` (over `Command` enum, fuzzy); `model` picker; `session` switcher (favorites/recents/pin in local);
`theme` picker (opencode + light to start); `which_key` overlay; full keymap port (leader `Ctrl-X`).
**Backend**: `engine.list_sessions/list_models/switch_model` + proto `SessionMeta`/`ModelEntry` + `EmbeddedBackend` wiring.
**RED**: `parity_palette.rs::ctrl_p_opens_palette`; `parity_model.rs::leader_m_switches_model_for_next_turn`; `parity_session.rs::leader_l_swaps_active_session`.
**Acceptance**: `Ctrl-P` palette; `<leader>m` model switch actually changes next turn's model; `<leader>l` swaps session + replays projection; `<leader>t` live theme switch; which-key after held leader. **Verify**: scripted tmux journey over keymap §4 (minus diff/permission/question). **Rollback**: tag.

## W4 — Rich rendering

**Deliverables**: specialized tool renderers (`render/tool/`: bash, write, edit/apply_patch, read, glob,
grep, webfetch, websearch, todowrite, question, task, skill, generic) w/ hide-completed + generic-output toggles;
inline diff (`similar`, split if width>120) + full-screen `DiffViewer` overlay (file tree `b`, hunk `]`/`[`,
file `n`/`p`, wrap, single-patch `s`, `?`); markdown tables + conceal (`<leader>h`) + syntect syntax via `syntax*` tokens.
**Backend**: none (ensure reasoning/tool lifecycle fully projected — verify W0 audit holds).
**RED**: `parity_tools.rs::transcript_fixture_snapshot` (FakeProvider-generated JSONL exercising every tool); `parity_diff.rs::split_unified_threshold`.
**Acceptance**: curated transcript fixture renders snapshot-stable; diff viewer opens from tool row; CJK in diffs correct. **Verify**: tmux compare vs opencode rendering the same transcript (offline `FakeProvider`). **Rollback**: tag.

## W5 — Flows + sidebar + lifecycle

**Deliverables**: `overlay::permission` (inline state machine permission>question>subagent>prompt; allow/allow-always/reject+feedback; fullscreen for diff asks); `overlay::question` (if engine surfaces it, else stub+document); `sidebar` (title/workspace/share, auto>120, overlay narrow, hidden for child); `subagent_footer`; Esc→`abort`; lifecycle: new/rename(Ctrl-R)/delete(Ctrl-D+confirm)/compact(`<leader>c`)/fork/timeline(`<leader>g`).
**Backend**: `engine.abort` (+`Event::Aborted`, honor mid-stream/mid-shell); `rename_session`; `compact_session`; `fork_session(from_seq)`; permission round-trip mirrored to bus (keep mpsc for embedded; add event-mirror for future http).
**RED**: `parity_permission.rs::bash_ask_allow_runs`; `parity_abort.rs::double_esc_aborts_emits_aborted`.
**Acceptance**: permission-gated bash shows inline prompt, allow-once streams output; double-Esc<5s aborts + bus emits `Aborted`; rename updates status bar live. **Verify**: scripted tmux per flow + TestBackend "long shell, abort" test. **Rollback**: tag.

## W6a — Backend core (persistence, usage, backfill)

**Deliverables**: session persistence behind `--store=sqlite|memory` (default `memory` until acceptance; durable path `$XDG_STATE_HOME/yaca/sessions.db`, additive+reversible migrations); token/cost via `Event::UsageRecorded` emitted at provider boundary → `UiStore.usage` → status bar + sidebar + subagent footer; **`/stream?since_seq=N` backfill** (replay seq>N then live).
**Backend**: `GET /sessions/:id/usage`, `GET /sessions/:id/projection`, `/stream` backfill; store: durable session metadata + usage ledger read.
**RED**: `yaca-server/tests/parity_backfill.rs::reconnect_replays_missing_seqs_then_live`; `yaca-tui/tests/parity_usage.rs::tokens_and_cost_shown_after_turn`; `yaca-store/tests/migrate.rs::up_down_roundtrip`.
**Acceptance**: kill stream mid-turn → reconnect → zero missing/duplicated seqs; usage shows real numbers after a turn; `migrate down` restores prior schema with no data loss on default build. **Verify**: `cargo test -p yaca-server parity_backfill` + tmux usage check. **Rollback**: branch `tui/w6a`, tag.

## W6b — Remaining dialogs + theme/keybind config + optional HttpBackend

**Deliverables**: remaining dialogs each with a named snapshot test + the catalog/route it needs:
`dialog/agent.rs` (`GET /agents`), `variant.rs` (`GET /variants`), `mcp.rs` (`GET /mcp`),
`status.rs` (`GET /status`), `stash.rs` (local), `skill.rs` (`GET /skills`), `tag.rs` (local),
`provider.rs` (multi-step, `GET /providers` + `POST /providers/:id/connect`); theme loader
(`~/.config/opencode/themes/*.json` + `./.opencode/themes/*.json` + 33 built-ins) + persist;
keybind config read/write (`GET/PATCH /config/ui`); optional `HttpBackend` + `yaca tui --server <url>` (default unchanged).
**RED (one per dialog)**: `yaca-tui/tests/parity_dialog_agent.rs::agent_dialog_snapshot`, `..parity_dialog_variant.rs::variant_dialog_snapshot`, `..parity_dialog_mcp.rs::mcp_dialog_snapshot`, `..parity_dialog_status.rs::status_dialog_snapshot`, `..parity_dialog_stash.rs::stash_dialog_snapshot`, `..parity_dialog_skill.rs::skill_dialog_snapshot`, `..parity_dialog_tag.rs::tag_dialog_snapshot`, `..parity_dialog_provider.rs::provider_wizard_steps_snapshot`; `..parity_theme_loader.rs::loads_custom_theme_json`; `..parity_keybind_config.rs::override_rebinds_command`.
**Acceptance**: every dialog in inventory §7 has a yaca equivalent passing its snapshot; theme switch survives restart; a custom keybind override takes effect; `--server` mode renders byte-identical to embedded on the home + session goldens.
**Verify**: `cargo insta test` (all dialog snapshots) + tmux open each dialog. **Rollback**: branch `tui/w6b`, tag.
**Risk flag (go/no-go, surfaced to user at W6b planning)**: warp/move-workspace, MCP, console-org, provider-OAuth, team/goal/loop live status may have **no yaca backend concept**. Decision per feature: build a minimal backend, or mark out-of-scope in `docs/parity.md` with justification. Does NOT block W6a or earlier waves.

## W7 — Fidelity QA + polish

**Deliverables**: committed parity checklist `docs/parity.md` (one row per inventory item: surface, opencode ref, yaca status, deviation+justification); keymap audit test `yaca-tui/tests/parity_keymap_audit.rs::every_commandmap_row_has_binding_and_handler` (table from `config/keybind.ts` CommandMap); perf harness `crates/yaca-tui/benches/stream.rs` (criterion) over fixture `crates/yaca-tui/tests/fixtures/transcript_1000msgs.jsonl`; toast polish (top-right, `min(60,w-6)`, 2x1 pad, theme bg, auto-dismiss after 4s); docs (`README` TUI section + `docs/keymap.md` + `docs/parity.md`).
**Acceptance (numeric)**: parity checklist 100% rows resolved (each deviation justified); **p95 frame render < 33ms (≈30fps) and steady-state CPU < 25% of one core** on the 1000-msg streaming fixture at 120×40; all `insta` goldens stable (`cargo insta test` clean); headless modes (`exec/serve/tail-session/-p`) byte-unchanged vs `main`.
**Verify**: QA script `scripts/qa/parity-walkthrough.sh` (tmux: launches `yaca`, drives each surface via `send-keys`, `capture-pane` per dialog, exits 0 only if every capture is non-empty + matches checklist); `cargo bench -p yaca-tui` attached to PR. **Rollback**: branch `tui/w7`, tag.

---

## Child-task creation (after user approves this plan)

```
task.py create "TUI W0 foundation"      --slug tui-w0-foundation  --parent .trellis/tasks/06-21-tui-opencode-parity
task.py create "TUI W1 appearance"      --slug tui-w1-appearance  --parent ...
... (W2..W7 similarly)
```
Each child gets its own `prd.md` (acceptance) + (for the heavier ones) `design.md`/`implement.md`,
and `implement.jsonl`/`check.jsonl` manifests pointing at this design + the inventory + the relevant
opencode reference files, so sub-agents get scoped context.

## Validation commands (copy/paste)

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo insta test --review            # snapshot review
tmux new -s yaca-qa 'cargo run -p yaca-cli'   # real-terminal QA
```

## Effort

Large: ~16–18 engineer-days across W0–W7; each wave independently shippable + rollback-tagged.
Recommend executing W0 in isolation, then re-evaluating wave sizing before W1.

## Plan Review (cross-model gate)

- **Round 0 — oracle (claude-opus-4-7, same-family)** — best-effort PASS, but flagged the
  cross-family gate as NOT bound (GPT fallback unreachable in that sandbox). Caught a stale
  PRD LOC ref (252→327, fixed).
- **Round 1 — codex → gpt-5.5 xhigh (cross-family, binding)** — VERDICT: **FAIL**.
  D2 (W0/W6 not atomic), D3 (re-pin command ran git in wrong repo), D4 (rollback lacked abort
  criteria/backout), D5 (W6/W7 verification unquantified). All fixed.
- **Round 2 — codex → gpt-5.5 xhigh (cross-family, binding)** — VERDICT: **PASS** (D1–D6 all PASS).

Gate satisfied. Execution may begin after user approval of artifacts (Trellis 1.4).
