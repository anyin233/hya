# Implement — Config-driven AgentCore

Execution plan for [design.md](./design.md). Every behavior step is TDD: failing test first
(RED, captured), smallest code to green (GREEN), then the gate. Waves are mostly sequential;
within a wave, tests are independent.

## Validation commands (every wave)
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Per-step fast loop: `cargo test -p <crate> <module_or_test>`.

## Wave 0 — Baseline + deps  [no behavior change]
1. [ ] Capture baseline: `cargo test --workspace` green; `yaca exec "hello"` (offline) transcript saved to `.omo/evidence/baseline.txt` (or `/tmp`). Re-confirm live `AgentSpec` fields (`reasoning`?).
2. [ ] Add `regex = "1"` to workspace `Cargo.toml` deps; add `serde_norway = { workspace = true }` + `regex = { workspace = true }` to `crates/yaca-core/Cargo.toml`. Re-export `yaca_tool::permission::glob_match` from `yaca-tool/src/lib.rs`.
3. [ ] Validate: `cargo build --workspace`. **Rollback**: revert Cargo edits. Risky: `Cargo.lock`.

## Wave 1 — `ToolAllowlist` in yaca-tool  [AC2 foundation]
4. [ ] RED `crates/yaca-tool/tests/tool.rs`: `permits` cases (All admits any incl `ask_user`+`task`; Only excludes unlisted; Only admits listed). `schemas_for` filters+sorts — assert all **10** builtins (read/write/edit/ls/glob/find/grep/shell/ask_user/task); read-only `Only({read,ls,glob,find,grep})` advertises 5; `require_allowed` rejects disallowed, errors unknown.
5. [ ] GREEN: add `ToolAllowlist` enum + `ToolRegistry::schemas_for` + `require_allowed` ([tool.rs](../../../crates/yaca-tool/src/tool.rs)). Export from `yaca-tool/src/lib.rs`.
6. [ ] Gate. **Rollback**: revert yaca-tool. Risky: `tool.rs`, `lib.rs`.

## Wave 2 — `agent/` module: `AgentCore` + frontmatter parser  [AC1 scaffold]
7. [ ] RED `agent/config.rs` tests: parse valid frontmatter+body; reject missing/unterminated `---`; reject unknown field (`deny_unknown_fields`); reject empty body; lists (`allowed_tools`, `skills`) + untagged `all`-string parse.
8. [ ] GREEN: `agent/core.rs` (`AgentCore`, `ResolvedSkill`), `agent/config.rs` (`AgentFrontmatter`, `InjectionFrontmatter`, splitter via `serde_norway`), `agent/mod.rs` re-exports. Update `lib.rs` (`pub mod agent; pub use agent::{…}`).
9. [ ] Gate `cargo test -p yaca-core agent::config`. **Rollback**: rm `agent/`, revert `lib.rs`. Risky: `lib.rs`.

## Wave 3 — `AgentCatalog` loader  [AC1]
10. [ ] RED `agent/loader.rs`: unknown agent errors; project shadows global shadows builtin; no-config builtin `build` resolves with default model + today's persona; filename/`name` mismatch errors; unknown tool/skill/injection name errors.
11. [ ] RED extends: 2-node cycle, self-cycle, depth>8, missing parent, no-model-in-chain, union of allowed_tools+skills, child overrides model.
12. [ ] GREEN: `agent/builtin.rs` (`include_str!` 5 presets + reader + ulw); `AgentCatalog::load` (discovery, precedence, extends compose, validation, no-panic `AgentConfigError` via thiserror). Loader also composes matching `always`+`session-start` injection bodies into each agent's `system_prompt` via `compose_static(agent,model)` (Wave 5).
12b. [ ] RED+GREEN: an agent matched by an `always` injection has that body in `system_prompt`; a sibling not matched does not (loader-level test).
13. [ ] Gate `cargo test -p yaca-core agent::loader`. **Rollback**: revert loader+builtin. Risky: builtin `build.md` must reproduce current persona byte-for-byte.

## Wave 4 — Per-agent skills  [AC3]
14. [ ] RED `agent/skills.rs` + loader: `SkillSelection::{None,All,Named}`; agent with `skills:[foo]` → prompt has foo, not bar; sibling `skills:[]` → no `## …skills` section; unknown skill → friendly error; `skills: all` → all discovered.
15. [ ] GREEN: move `SkillCatalog`/discovery into `agent/skills.rs` (CLI imports back); add related-skills section to `build_system_prompt` (extra `skills_section: Option<String>` param) ([prompt.rs:8](../../../crates/yaca-core/src/prompt.rs)).
16. [ ] Gate `cargo test -p yaca-core agent::skills` + `cargo build -p yaca-cli`. Risky: cross-crate move (audit `use crate::skills` in yaca-cli).

## Wave 5 — Injection engine (unit)  [AC4 unit]
17. [ ] RED `agent/injection.rs`: parse always|session-start|keyword; default agents/models `["*"]`; invalid keyword regex errors; variant resolve + missing-variant fallback; agent∧model selector match; priority asc + name tiebreak; model-family mapping. `compose_static(agent,model)` → always+session-start bodies joined by priority (for the loader). `keyword_matches(rule,text)` regex test.
18. [ ] GREEN: `InjectionRule`/`Selector`/`Trigger`, `InjectionLibrary::load`, `compose_static`, `keyword_matches`, `model_family`, marker `<yaca-injection name="X">` build/scan helpers.
19. [ ] Gate `cargo test -p yaca-core agent::injection`.

## Wave 6 — Engine integration  [AC2 + AC4 integration]
20. [ ] GREEN (mechanical): rename `AgentSpec`→`AgentCore` across engine/completion/loop_mode/subagent/server/tui/tests; add transitional `pub type AgentSpec = AgentCore;` in `lib.rs`. `cargo build --workspace` until clean.
21. [ ] RED `crates/yaca-core/tests/turn_loop.rs` (use `FakeProvider::scripted_turns` [fake.rs](../../../crates/yaca-provider/src/fake.rs) + `RecordingProvider`): agent `[read,ls]` advertises only read,ls (not write/edit/shell/ask_user); forced disallowed `write` call → `Event::ToolError "not allowed"`; advertise/execute agree (proptest).
22. [ ] GREEN: `request_from_messages` → `schemas_for(&agent.allowed_tools)` ([engine.rs:474](../../../crates/yaca-core/src/engine.rs)); dispatch → `require_allowed` ([engine.rs:325-358](../../../crates/yaca-core/src/engine.rs)).
23. [ ] RED injection integration (AC4): (a) keyword `ulw` → on a user prompt matching `(?i)(ultrawork|ulw)`, `run_turn` emits a logged `<yaca-injection name="ulw">` System message; non-matching prompt → none; sibling agent not selected → none. (b) `once:true` → fires once across two matching turns (marker dedup). (c) compaction-correctness: after `compact_with` summarizes the turn, the marker remains in `read_projection` ⇒ NO double-fire — assert no re-injection; do NOT assert the directive is re-shown (post-compact recovery out of scope, D5). [always/session-start coverage is in step 12b.]
24. [ ] GREEN: `prepare_keyword_injections(session, &agent, &projection)` at the top of `run_turn` (keyword rules only; emit markers via `inject_system_message`). always/session-start are already baked into `system_prompt` at load — `request_from_messages` is unchanged re: injections.
25. [ ] Gate `cargo test --workspace`. **Rollback**: this is the riskiest wave — commit Wave 0–5 first; revert per-file. Risky: `engine.rs`.

## Wave 7 — Category → preset unification + spawn-by-name  [AC5]
26. [ ] RED rewrite `tests/category_routing.rs` → `tests/agent_preset_routing.rs`: `presets_resolve_to_4_distinct_models {tier-cheap,tier-strong,tier-max,tier-writer}`; `preset_extends_build_prompt`; `preset_includes_referenced_skill`; `four_named_members_drive_four_distinct_model_calls`; `unknown_subagent_type_member_fails`.
27a. [ ] GREEN: author builtin preset files (quick/deep/ultrabrain/writing `extends: build` + tier models) so model + extends assertions pass. Gate: `cargo test -p yaca-core --test agent_preset_routing presets_resolve preset_extends`.
27b. [ ] GREEN: `MemberSpec → { id, agent: AgentName, directive }` ([subagent.rs:32](../../../crates/yaca-core/src/subagent.rs)); `run_member(engine, lead, catalog, spec, cancel)` resolves `catalog.require(spec.agent)` → AgentCore (unknown ⇒ `MemberStatus::Failed`); `run_team(engine, lead, catalog, specs, cancel)`. `cargo build -p yaca-core`.
27c. [ ] GREEN: `spawn_team_supervisor` ([main.rs:199-238](../../../crates/yaca-cli/src/main.rs)) currently IGNORES `subagent_type` and clones `base` into every member — change it to hold `Arc<AgentCatalog>` and map each `SpawnMember.subagent_type` → `MemberSpec{ agent: AgentName::new(subagent_type) }`, passing the catalog into `run_team` (NEW behavior: members get their preset model; unknown ⇒ `Failed` in `run_member`). `cargo build -p yaca-cli`.
27d. [ ] GREEN: delete `crates/yaca-core/src/category.rs`; remove `pub mod category` + re-exports (`CategoryEntry/CategoryRegistry/ResolvedCategory/build_member_agent/inject_skills`, [lib.rs:18](../../../crates/yaca-core/src/lib.rs)). `cargo build --workspace` (catches stragglers).
27e. [ ] Verify `TaskTool` `subagent_type` enum `{quick,deep,ultrabrain,writing}` ([tool.rs:581](../../../crates/yaca-tool/src/tool.rs)) still resolves as agent names (no change unless a preset is renamed).
28. [ ] Gate `cargo test -p yaca-core --test agent_preset_routing` + `cargo test --workspace`.

## Wave 8 — CLI / server / TUI wiring  [AC1 live]
29. [ ] GREEN: replace `agent_with_model(model)` ([main.rs:156](../../../crates/yaca-cli/src/main.rs)) with `AgentCatalog::load(opts)?.require("build")?.clone()` + model override last (preserve `--model`/`YACA_MODEL`); build catalog once; pass into `build_session_engine`/team sites.
30. [ ] GREEN: `AppState.agent: Arc<AgentCore>` ([yaca-server/src/lib.rs](../../../crates/yaca-server/src/lib.rs)); TUI `/model` re-resolves model-dependent injections ([tui.rs](../../../crates/yaca-cli/src/tui.rs)).
31. [ ] GREEN cleanup: remove transitional `pub type AgentSpec = AgentCore;`; `rg "AgentSpec" crates/` returns zero. `cargo build --workspace`.

## Wave 9 — Quality gate + manual QA  [AC5 + live]
32. [ ] Full gate: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
33. [ ] Manual QA (tmux/Bash, capture evidence to `.omo/evidence/`):
    - **AC1**: `yaca exec "hello"` no-config → builtin persona transcript == baseline. Drop `.yaca/agents/build.md` with a distinct persona → `yaca exec --json "hi"` shows it; remove file → baseline restored.
    - **AC2**: define `.yaca/agents/reader.md` (`allowed_tools:[read,ls,glob,find,grep]`); `yaca exec` with reader → advertised tools exclude write/edit/shell/ask_user/task (assert via `--json`/provider trace); forced disallowed call rejected (Wave 6 test is the execute-side real surface).
    - **AC3** (Round-1 D5): `.yaca/agents/reader.md` with `skills:[<one discovered skill>]` → `yaca exec --json` shows that skill in reader's related-skills section; `build` (`skills: all`) shows the full set; an agent with `skills:[]` shows none.
    - **AC4**: `yaca exec "ulw summarize"` → transcript contains `<yaca-injection name="ulw">`/`<ultrawork-mode>`; `yaca exec "summarize"` → absent.
    - **AC5** (Round-1 D5): real spawn surface — drive the lead to call `task` with `subagent_type:"deep"` (a `FakeProvider` scripted `task` call in an integration test, OR a real-model `yaca exec` that triggers it) → team evidence returns, spawned member ran on `tier-strong`; `subagent_type:"nope"` → that member reported failed, lead unaffected. Capture the evidence envelope.
34. [ ] Cleanup QA artifacts: remove temp `.yaca/agents/*`, tmux sessions, temp files.

## AC → wave map
| AC | Waves | Evidence |
|---|---|---|
| AC1 loader + no-config fallback | 3, 8, 9 | unit + live `--json` transcript |
| AC2 allowlist advertise+execute | 1, 6 | unit + FakeProvider trace |
| AC3 per-agent skills | 4, 7, 9 | unit + live `--json` transcript |
| AC4 injection per-(agent,model) | 5, 6, 9 | unit + live transcript |
| AC5 gate + preserved behavior + spawn-by-name | 7, 9 | rewritten test + gate + live spawn evidence |

## Risky files / rollback points
`engine.rs` (central; Wave 6), `lib.rs` (re-exports), `main.rs`/`tui.rs`/`server lib.rs` (Wave 8), `tool.rs` (Wave 1), `tests/category_routing.rs` (Wave 7), workspace `Cargo.toml` (Wave 0). Each wave ends green ⇒ individually revertable. Land Wave 0–1 (additive) fast to minimize live-worktree conflicts; commit before Wave 6.

## Sub-agent manifests (curate before `task.py start` if delegating execution)
- `implement.jsonl`: spec refs (this file + design.md + prd.md) + per-wave file targets.
- `check.jsonl`: AC1–AC5 + quality-gate as the verification manifest.
