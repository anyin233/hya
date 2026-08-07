# Wave 4 - Reconciliation pass

You are finishing the documentation-coverage work for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

All content batches have LANDED. Fifteen prose batches and thirteen crates of Rust
API docs are committed. Your job is the cross-cutting layer that could only be
written once everything else existed.

## Your files

You own exactly these eight, and no others:

- `docs/README.md`
- `README.md`
- `AGENTS.md`
- `DESIGN.md`
- `docs/project-structure.md`
- `docs/compat-parity.md`
- `docs/opencode-feature-inventory.md`
- `docs/hya-pi-compat-comparison.md`

## Task 1 - Wire in the nine new documents (highest priority)

These files were CREATED during this effort and are currently unreachable from
`docs/README.md`. Add every one to the **Docs Map** table with a one-line purpose,
and add the user-facing ones to the right **Reading Path**:

| New file | Belongs in |
| --- | --- |
| `docs/tui-keybindings.md` | Docs Map + "If you want to run hya" reading path |
| `docs/tui-reference.md` | Docs Map + "If you want to run hya" reading path |
| `docs/skills.md` | Docs Map + "If you want to run hya" reading path |
| `docs/plugin-protocol.md` | Docs Map + codebase reading path |
| `docs/compat-plugins.md` | Docs Map |
| `docs/architecture/admission-and-governor.md` | Docs Map + codebase reading path |
| `packages/hya-tui-ts/README.md` | Source Entrypoints or Docs Map |
| `packages/hya-tui-ts/scripts/README.md` | linked from the package README entry |
| `packages/hya-tui-ts/test/README.md` | linked from the testing entry |

**`README.md:111` and `docs/getting-started.md:171` promise a keybinding reference
that did not exist until now.** Repoint `README.md:111` at
`docs/tui-keybindings.md`. Do NOT edit `docs/getting-started.md` - it is not yours;
just note it in your report.

## Task 2 - Correct the stale and contradicted claims below

## Non-negotiable rules

1. Verify every claim against the source reference before writing it. If the
   source contradicts the entry, the SOURCE WINS.
2. Stale entries are CORRECTED or DELETED, never merely supplemented.
3. Edit only your eight files.
4. Do not run `git commit`.
5. Every link you add must resolve. Check the target exists before linking it.

## Work list

### `docs/README.md`
**STALE 1.** Claims: 'Use one root `bundle.hya.md` with both v1 markers' — presented as the single source layout, and all four shipped examples use the markdown form.

- Reality: crates/hya-bundle/src/prepare.rs:474 accepts EXACTLY ONE of bundle.yaml (directory form) or bundle.hya.md. bundle.yaml is the form the shipped built-in bundles use (bundles/builtin/hya-core-agents/bundle.yaml, bundles/builtin/hya-development/bundle.yaml) and is never mentioned in any in-scope doc.
- Action: correct or delete.

### `README.md`
**STALE 1.** Claims: '[docs/cli.md](docs/cli.md) | `hya` commands, flags, and the TUI slash-command reference.'

- Reality: docs/cli.md contains no slash command at all. Every /-command in packages/hya-tui-ts (23 TUI commands) and crates/hya-server/src/compat/command_catalog.rs (7 backend commands) is absent from it.
- Action: correct or delete.
**STALE 2.** Claims: docs/configuration.md contains 'the complete `HYA_*` environment-variable reference'.

- Reality: The docs/configuration.md table lists 5 HYA_* variables. The code reads at least 17: HYA_MODEL, HYA_DB, HYA_COMPACTION_THRESHOLD, HYA_COMPACTION_KEEP_RECENT, HYA_COMPAT_ADAPTER_DIR, HYA_FRONTEND_BIN, HYA_BACKEND_BIN, HYA_TUI_TS_DIR, HYA_STARTUP_TRACE, HYA_SUBAGENT_MAX_DEPTH, HYA_SUBAGENT_MAX_CONCURRENCY, HYA_SUBAGENT_BUDGET, HYA_SUBAGENT_TURN_BUDGET, HYA_SUBAGENT_MESSAGE_BUDGET, HYA_EVENT_BUS_CAPACITY, HYA_DEFER_SIDEPLANES, HYA_VERSION, HYA_CHANNEL, HYA_ROUTE, HYA_FAST_BOOT, HYA_E2E_BACKEND_BIN.
- Action: correct or delete.
**STALE 3.** Claims: '# same commands work on hya-ts:' followed by `hya oauth login --provider codex --type openai-codex` and `hya oauth status`.

- Reality: The comment says hya-ts but both example lines invoke `hya`, so the block duplicates lines 71-73 and demonstrates nothing. The real hya-ts surface (crates/hya-ts/src/lib.rs:106) is reached as `hya-ts oauth login …`.
- Action: correct or delete.
**STALE 4.** Claims: docs/cli.md is "the TUI slash-command reference" / "For the full command and TUI slash-command reference, see the CLI Reference (cli.md)."

- Reality: docs/cli.md contains no slash commands — grep for 'slash' in that file returns nothing. The ~23 built-in slash names derived in packages/hya-tui-ts/src/upstream/keymap.tsx:260 (/sessions, /new, /models, /agents, /mcps, /variants, /status, /themes, /help, /exit, /rename, /timeline, /fork, /compact, /undo, /redo, /timestamps, /thinking, /copy, /export, /editor, /skills, /diff plus aliases) are documented nowhere in the repository.
- Action: correct or delete.

### `AGENTS.md`
**STALE 1.** Claims: '`crates/xtask` | Dev-tooling entry point. Currently a small scaffold for future workspace maintenance commands.'

- Reality: Same as above — xtask ships four implemented commands that other docs already depend on.
- Action: correct or delete.
**STALE 2.** Claims: `hya-plugin-example` is a 'Minimal fixture/example plugin binary' / 'Minimal plugin binary used as a concrete fixture/example for host and transport behavior.'

- Reality: crates/hya-plugin-example/src/main.rs is `fn main() {}`. Its own module doc says 'Phase 0 ships a no-op stub; Phase 7 makes it speak the plugin protocol'. It implements none of the protocol and has 0.0% coverage per docs/testing/coverage-baseline.md:40.
- Action: correct or delete.
**STALE 3.** Claims: Both tables present themselves as the complete crate map of the workspace.

- Reality: `hya-native` (the in-process embedding transport, crates/hya-native/src/transport.rs) and `hya-updater` (the self-update TCB, crates/hya-updater/) are both missing from both tables. hya-native appears only as a mermaid edge at docs/project-structure.md:254.
- Action: correct or delete.

### `DESIGN.md`
**STALE 1.** Claims: A single dark palette with every Light column cell marked "N/A"; a Status Line whose structure is "product label, session label, running state, optional YOLO/think/goal state"; a Prompt Composer that "grows from 1 to 6 visible rows" in a `row-input` region of 6-11 rows; and the rule "Terminal rendering is immediate; do not add animation artifacts."

- Reality: The TypeScript TUI ships 33 light/dark-aware themes plus a generated `system` theme, with `theme.switch_mode` and `theme.mode.lock` commands that flip or pin light/dark (theme/index.ts:130, config/keybind.ts:75). There is no status line — agent, model, variant and usage live in the prompt footer meta line (component/prompt/index.tsx:1378), and there is no YOLO/think/goal indicator. The prompt textarea is capped by the `prompt.max_height` config key (default one third of terminal height, minimum 6), not a fixed 6-11 row region. Animations are pervasive and deliberate: fade-in transitions, block spinners, and an `app.toggle.animations` command that swaps the spinner for a static `[⋯]` (config/keybind.ts:51).
- Action: correct or delete.

### `docs/project-structure.md`
**1. BundleCatalog resolution API and BundleRegistryRecord persisted fields (items 115,124)** - `undocumented`

- Source: `crates/hya-bundle/src/catalog.rs:43; crates/hya-store/src/bundle_registry.rs:22`
- Evidence: docs/project-structure.md:47 describes hya-bundle as 'AgentBundle prepare/validate/catalog types and package fixtures'. No doc lists the catalog resolution entrypoints or what the SQLite registry actually stores.
- Write: For hya-bundle: name the catalog entrypoints — from_prepared / from_verified_catalogs / with_verified_catalogs build an immutable catalog indexing agents by stable id AND by bundle:<id>/agent/<local_id>, and resources by (ExportKind, stable id) plus bundle-local name and alias; the public reads are resolve_agent, resolve_resource, bundle_resources, resolve_spawn, and spawnable_agents. For hya-store: list the BundleRegistryRecord columns — bundle_id, version, publisher, a 32-byte source_digest, prepared_digest, prepared_bytes, and installed_at — all tracked under a monotonically increasing registry generation that drives the catalog reload described in docs/cli.md.
**2. hya-native embedding API (items 177,178): HyaNativeTransport / HyaNativeClient in-process transport and spawn_event_bridge SSE bridge** - `undocumented`

- Source: `crates/hya-native/src/transport.rs:20; crates/hya-native/src/events.rs:23`
- Evidence: The crate `hya-native` is absent from the Crate Responsibilities table in docs/project-structure.md:33-48 and from the crate table in AGENTS.md:70-88. Its ONLY appearance in any in-scope doc is inside a mermaid edge, docs/project-structure.md:254 'hya-app -- hya-native'. Nothing describes what it is or how to embed hya in-process.
- Write: Add the crate and describe its purpose: hya-native drives the in-process hya axum Router via tower `oneshot` instead of HTTP — no TCP, no reqwest — injecting the directory header on every request; it is the Rust analogue of the compat adapter's in-process app.fetch and is the supported way to embed hya in another Rust process. Also document spawn_event_bridge: it subscribes to the in-process GET /global/event SSE stream, decodes each frame into hya_sdk::GlobalEvent, forwards it to an mpsc sender, TOLERATES undecodable frames (skipping them rather than failing), re-subscribes after a 50 ms backoff on stream loss, and stops when the receiver is dropped.
**3. hya-plugin-example is described as a working example plugin but is an empty stub (item 179)** - `contradicted`

- Source: `crates/hya-plugin-example/src/main.rs:7`
- Evidence: docs/project-structure.md:41 says 'Minimal fixture/example plugin binary' and AGENTS.md:85 says 'Minimal plugin binary used as a concrete fixture/example for host and transport behavior'. The actual file is `fn main() {}` — its own rustdoc admits 'Phase 0 ships a no-op stub; Phase 7 makes it speak the plugin protocol'. It does not implement the plugin protocol at all, so anyone following the docs to it as a reference implementation finds nothing. docs/testing/coverage-baseline.md:40 corroborates: 0.0% coverage, 1 line.
- Write: Correct both rows to say the crate is currently a placeholder stub (`fn main() {}`) that does NOT speak the plugin protocol, and is reserved for a future deterministic native-plugin QA fixture (planned: a message.user.before marker, a chat.params temperature override, a tool.execute.before veto sentinel, and event logging to stderr). Point readers wanting a real reference implementation at the new docs/plugin-protocol.md worked example instead.
**4. hya-sdk and hya-native crates missing from the repository/crate maps** - `undocumented`

- Source: `crates/hya-sdk/src/lib.rs:6`
- Evidence: docs/project-structure.md:33-51 lists 17 crates and includes neither hya-sdk nor hya-native nor hya-updater, though all three exist under crates/. AGENTS.md:68-90 has the same omission. hya-sdk is referenced in passing at docs/architecture/tui.md:26 and docs/adr/0001:16 with no page describing it, even though it owns the Client trait, the MessageStore, the TeamProjection mirror, and the V2Event reducer.
- Write: Add rows to the Crate Responsibilities table: hya-sdk (crates/hya-sdk/src/lib.rs) — typed Client trait over an HTTP or in-process-stdio Transport, the DIRECTORY_HEADER wire constant `x-opencode-directory`, ServerHandle process supervision, the live MessageStore, the frontend TeamProjection mirror, and the session.next.* V2Event reducer; hya-native (crates/hya-native) — the in-process native bridge target; hya-updater (crates/hya-updater) — the independent self-update TCB, cross-linked to docs/self-update.md. Mirror the same rows into AGENTS.md's component table. Also add a `hya-sdk` module map (client.rs, native.rs, server.rs, events.rs, store.rs, team.rs, reducer.rs, types.rs, pending.rs, error.rs) mirroring the existing hya-proto/hya-core module tables.
**5. hya-store module map (admission.rs, mailbox.rs, resident_claim.rs, sync.rs, permission.rs, bundle_registry.rs) and migrations 0002-0008** - `stale`

- Source: `crates/hya-store/src/lib.rs:41`
- Evidence: docs/project-structure.md:141-145 lists only three files for hya-store: src/lib.rs, src/error.rs, migrations/0001_init.sql. Six further source modules (admission.rs 1487 lines, mailbox.rs 508, bundle_registry.rs 423, resident_claim.rs 279, sync.rs 83, permission.rs 69) and seven further migrations (0002-0008 plus bundle_migrations/) exist on disk and are invisible to the map.
- Write: Replace the three-row hya-store file table with a complete module map: lib.rs (connections, append/replay/read_projection, list/delete sessions, token ledger, decode_session_key), admission.rs (durable spawn admission journal), mailbox.rs (event-sourced mail writes, resident recovery, stop/failure finalization), resident_claim.rs (actor claim fencing primitives), sync.rs (compat sync history/replay), permission.rs (saved permissions), bundle_registry.rs (separate registry DB), error.rs. List all migrations 0001-0008 with one line each, plus bundle_migrations/0001_init.sql as a separate migration set for a separate database file.
**6. hya-core module map (mailbox.rs, resident.rs, orchestrator.rs, runtime_registry.rs, sidecar.rs, prompt.rs, title.rs) plus removed team.rs** - `stale`

- Source: `crates/hya-core/src/resident.rs:1319`
- Evidence: docs/project-structure.md:163-179 lists hya-core modules and includes a row for `team.rs` — that file DOES NOT EXIST (ls crates/hya-core/src shows no team.rs, and grep for TeamControlPlane in crates/hya-core/src returns nothing; docs/adr/0001:12 records that it was deleted). Meanwhile resident.rs (1957 lines), orchestrator.rs (479), runtime_registry.rs (4670), mailbox.rs, sidecar.rs, prompt.rs and title.rs are all absent from the table.
- Write: Delete the `team.rs` row (the file and TeamControlPlane were removed per ADR-0001) and add rows for: mailbox.rs (mailbox service loop draining MailboxRequest), engine/mailbox.rs (team-root mail delivery, roster/channel queries, MAIN_HANDLE), resident.rs (ResidentSupervisor, TeamState, per-team lock and quiescence), orchestrator.rs (SubagentLimits, SubagentGovernor, stream permits, per-team budgets), runtime_registry.rs (RuntimeRegistry, TurnBinding, ConfigGeneration publication), sidecar.rs (SidecarLifecycle contract), prompt.rs, title.rs. Apply the same team.rs correction at docs/architecture/runtime.md:255-256.
**STALE 1.** Claims: "Tool output is capped at 16 KiB for large text fields."

- Reality: Same as above — the global cap is 5000 characters (crates/hya-tool/src/output_cap.rs:11-29); 16 KiB is shell-specific.
- Action: correct or delete.
**STALE 2.** Claims: "Builtins currently include:" followed by a Tool/Permission-action/Behavior table.

- Reality: Same omission — no `list_agents` and no mailbox tools (`send`, `roster`, `channels`, `join`, `leave`). The table also assigns `find` the `Glob` action, which is right, but assigns non-Action values ("Web planes", "Interaction plane", "Plan tool", "None") in the permission-action column, mixing runtime planes with the `Action` enum; the real actions are `webfetch`/`websearch` for the web tools and `Tool` for question/ask_user/plan_exit/invalid.
- Action: correct or delete.
**STALE 3.** Claims: '[`../crates/xtask`](../crates/xtask) | Developer tooling crate. Currently a scaffold.'

- Reality: crates/xtask/src/main.rs:12 dispatches four working tasks — sync-compat, migrate, startup-bench, matrix-check — backed by three modules (sync_compat, startup_bench, matrix_check). docs/configuration.md:548-600 already documents sync-compat recipes and docs/testing/agent-matrix.md:163 relies on matrix-check.
- Action: correct or delete.
**STALE 4.** Claims: `hya-plugin-example` is a 'Minimal fixture/example plugin binary' / 'Minimal plugin binary used as a concrete fixture/example for host and transport behavior.'

- Reality: crates/hya-plugin-example/src/main.rs is `fn main() {}`. Its own module doc says 'Phase 0 ships a no-op stub; Phase 7 makes it speak the plugin protocol'. It implements none of the protocol and has 0.0% coverage per docs/testing/coverage-baseline.md:40.
- Action: correct or delete.
**STALE 5.** Claims: Both tables present themselves as the complete crate map of the workspace.

- Reality: `hya-native` (the in-process embedding transport, crates/hya-native/src/transport.rs) and `hya-updater` (the self-update TCB, crates/hya-updater/) are both missing from both tables. hya-native appears only as a mermaid edge at docs/project-structure.md:254.
- Action: correct or delete.
**STALE 6.** Claims: "`SessionEngine` is the central write path. It appends every event through the store and immediately publishes the same envelope on the `EventBus`."

- Reality: Same contradiction as runtime.md:22-23 — publish_live bypasses the store entirely, and publish_envelope (engine.rs:565) dispatches to global hooks and activation/sidecar hooks BEFORE the bus, so the bus is not the only consumer.
- Action: correct or delete.
**STALE 7.** Claims: "The reducer ignores duplicate or older envelopes by comparing `Envelope.seq` to `Projection.last_seq`, which makes replay and SSE reconnect logic use the same state transition rules."

- Reality: Same omission — the seq==0 live-only branch is checked first and bypasses the last_seq comparison entirely.
- Action: correct or delete.
**STALE 8.** Claims: Crate module table row: "`team.rs` | Team lifecycle state machine, mailbox, and task board primitives." (with a link to ../crates/hya-core/src/team.rs)

- Reality: Dead link — the file was removed with TeamControlPlane per ADR-0001. The live equivalents are crates/hya-core/src/engine/mailbox.rs (delivery, roster, channels) and crates/hya-core/src/resident.rs (team lifecycle state).
- Action: correct or delete.
**STALE 9.** Claims: hya-store's "Important files" are src/lib.rs, src/error.rs and migrations/0001_init.sql, and "The migration also creates tables for sessions, messages, parts, teams, mail, tasks, and goals."

- Reality: Six further source modules exist (admission.rs 1487 lines, mailbox.rs 508, bundle_registry.rs 423, resident_claim.rs 279, sync.rs 83, permission.rs 69) and seven further migrations plus a second migration set. The singular "the migration" reads as if 0001 were the whole schema.
- Action: correct or delete.

### `docs/compat-parity.md`
**STALE 1.** Claims: 'Ratatui app has compat-dark theme, session picker, permission/question overlays, slash commands, model switching, and render tests.'

- Reality: There is no Ratatui TUI in the repository. It was removed per docs/adr/0005-drop-legacy-tui-surface.md and docs/adr/0010-remove-retained-rust-tui.md, and docs/architecture/tui.md:59-63 states packages/hya-tui-ts is the sole interactive terminal UI implementation. This parity row describes a deleted surface.
- Action: correct or delete.
**STALE 2.** Claims: 'Missing or incomplete Compat command palette, theme picker/bundled theme library, model variant picker, skill picker error UI ... and full keymap/leader UX.'

- Reality: All four named items exist in packages/hya-tui-ts: the command palette is `command.palette.show` bound to ctrl+p (component/command-palette.tsx:26, config/keybind.ts:56); the theme picker is /themes / theme.switch (app.tsx:682); the model variant picker is /variants / variant.list (app.tsx:649); the skill picker is /skills / prompt.skills (component/prompt/index.tsx:504). The keybind registry carries ~200 named overridable bindings with a ctrl+x leader (config/keybind.ts:44).
- Action: correct or delete.
**STALE 3.** Claims: Skills: '`.hya/skills` and `~/.config/hya/skills` discovery ... are present.'

- Reality: crates/hya-tool/src/skill_catalog.rs:46 scans ELEVEN directories in a fixed first-name-wins order, including ~/.claude/skills, ~/.config/opencode/skills, ~/.config/opencode/skill, ./.opencode/skills, ./.opencode/skill, ./.agents/skills, ~/.codex/skills, and ~/.agents/skills. The two-directory claim understates discovery and hides the precedence rule.
- Action: correct or delete.
**STALE 4.** Claims: "TUI base | Partial | Ratatui app has compat-dark theme, session picker, permission/question overlays, slash commands, model switching, and render tests."

- Reality: There is no Ratatui app. ADR-0010 removed the retained Rust TUI and crates/hya/tests/no_rust_tui.rs asserts crates/hya-tui, crates/hya-tui-lib and crates/hya-parity do not exist. The sole frontend is packages/hya-tui-ts (SolidJS/OpenTUI) and the default theme is `hya`, not `compat-dark`.
- Action: correct or delete.
**STALE 5.** Claims: "TUI full feature parity | Missing or incomplete Compat command palette, theme picker/bundled theme library, model variant picker, skill picker error UI, rich markdown/diff/code rendering, usage/cost display wiring, prompt stash, and full keymap/leader UX."

- Reality: All of these ship in packages/hya-tui-ts: the Commands palette (component/command-palette.tsx:78), a theme picker over 33 bundled themes plus a generated `system` theme (theme/index.ts:130), the variant dialog (component/dialog-variant.tsx:34), the skill dialog (component/dialog-skill.tsx:53), split/unified syntax-highlighted diff rendering (routes/session/index.tsx:2847), sidebar Context token/percent/USD wiring (feature-plugins/sidebar/context.tsx:13), the Stash dialog plus prompt.stash/.pop/.list (component/dialog-stash.tsx:57), and a full leader-chord keymap (config/keybind.ts:41).
- Action: correct or delete.
**STALE 6.** Claims: Of the `/tui/*` control routes: "Missing real TUI main-loop integration and event-bus delivery parity."

- Reality: packages/hya-tui-ts/src/upstream/app.tsx:855-876 consumes `tui.command.execute` (dispatches a keymap command), `tui.toast.show` (raises a toast) and `tui.session.select` (navigates to a session), each gated on the event workspace matching the current one. Main-loop integration exists for at least these three.
- Action: correct or delete.

### `docs/opencode-feature-inventory.md`
**STALE 1.** Claims: TUI config and attention: "partial: themes/keymaps/status surfaces exist; full dedicated config and attention behavior need scope decision."

- Reality: The dedicated TUI config schema is implemented and validated (config/index.tsx:18-60: theme, keybinds, leader_timeout, attention with volume/sound_pack/per-name sounds, prompt.max_height/max_width, scroll_speed, scroll_acceleration, diff_style, mouse), and the attention service with desktop notifications, focus gating and a six-slot builtin sound pack ships (attention.ts:119, feature-plugins/system/notifications.ts:29). No scope decision is outstanding.
- Action: correct or delete.
**STALE 2.** Claims: TUI workflow: "richer file refs, prompt UX, theme/model pickers, undo/redo UI need coverage."

- Reality: `@` completion over files/agents/reference-aliases/MCP resources ships (component/prompt/autocomplete.tsx:60), as do the theme picker (component/dialog-theme-list.tsx:25), the model picker with favorites and recents (component/dialog-model.tsx:146), and the undo/redo UI with a revert banner and Confirm Redo dialog (routes/session/index.tsx:1544, config/keybind.ts:141).
- Action: correct or delete.

### `docs/hya-pi-compat-comparison.md`
**STALE 1.** Claims: Plugins are 'discovered from `<workdir>/.hya/plugins/**/plugin.toml`' — the `**` glob implies recursive discovery at any depth.

- Reality: crates/hya-app/src/plugins.rs:8 plugins_dir() resolves cwd/.hya/plugins and scan_manifests reads plugin.toml from each IMMEDIATE subdirectory only. A plugin.toml nested deeper is never found.
- Action: correct or delete.
**STALE 2.** Claims: "`TeamControlPlane` models lifecycle, mailbox, and task-board state; `WorktreeManager` can allocate owned git worktrees under `.hya/worktrees`."

- Reality: TeamControlPlane no longer exists (deleted per ADR-0001). Mailbox state is now event-sourced through MailSent/ChannelJoined/ChannelLeft folded into hya_proto::TeamProjection. Only the WorktreeManager half of the sentence is still accurate.
- Action: correct or delete.
**STALE 3.** Claims: "Important constraints make this intentionally controlled rather than unbounded: subagents cannot recursively spawn more subagents through `TaskTool`, and background execution is constrained."

- Reality: Nested subagent spawning is supported and explicitly bounded rather than forbidden: SubagentLimits.max_depth = 5 (crates/hya-core/src/orchestrator.rs:39), the turn loop selects reserved vs general stream permits BY DEPTH for depth-0 vs depth>0 turns (crates/hya-core/src/engine/turn.rs:654), and AdmissionMemberIdentity is propagated through a tokio task_local specifically so a NESTED spawn can be attributed to its admitted parent member (crates/hya-core/src/engine.rs:109).
- Action: correct or delete.

## When you are done

Report: files written, how many entries resolved, any source contradictions found,
anything you could not confirm, and any link you could not resolve.
