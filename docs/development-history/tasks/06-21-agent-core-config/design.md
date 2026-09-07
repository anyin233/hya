# Design — Config-driven AgentCore

Merged from 3 parallel planners (oracle/conservative, ultrabrain/failure-modes,
deep/authoring-UX). Decisions D1–D7 in [prd.md](./prd.md); omo evidence in
[research/omo-injection-mechanism.md](./research/omo-injection-mechanism.md).

## Guiding principles

- Keep yaca's architecture (event-sourced engine, permission plane, goal/loop/team). Add config-driven
  agents as a new `yaca-core::agent` module; do not rewrite the engine.
- Engine stays UI-agnostic. Config loading happens in the CLI/host layer; the engine consumes a fully
  resolved `AgentCore`.
- TDD + quality gate every step: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`
  (`unwrap_used`/`expect_used` denied in libs), `cargo test --workspace`.
- Smallest correct change; ≤250 pure LOC per file (split modules); no panics in library code.

## Corrected confirmed fact (all 3 planners flagged)

`ToolRegistry::builtins()` ([tool.rs:68-86](../../../crates/yaca-tool/src/tool.rs)) registers **10** tools (review Round 1
corrected 8→9→10): `read, write, edit, ls, glob, find, grep, shell, ask_user` (`AskUserTool`), **and `task`**
(`TaskTool`, [tool.rs:547](../../../crates/yaca-tool/src/tool.rs)) — the **subagent-spawn tool**, lead-only
(`ctx.parent_session.is_some()` ⇒ rejected, [tool.rs:599](../../../crates/yaca-tool/src/tool.rs)); its JSON schema hardcodes
`subagent_type ∈ {quick,deep,ultrabrain,writing}` ([tool.rs:581](../../../crates/yaca-tool/src/tool.rs)) — see §7 for why this
makes `task` central to the category-unification blast radius. `allowed_tools` default `All` includes `ask_user`+`task`;
read-only example agents exclude `write/edit/shell/ask_user/task`. Also: `AgentSpec` currently has 5 fields incl.
`reasoning: Option<ReasoningEffort>` (concurrent work); `ToolCtx` now also carries `spawner: SpawnerPlane` +
`parent_session: Option<SessionId>` ([tool.rs:32-39](../../../crates/yaca-tool/src/tool.rs)). Re-verify the live structs at impl time.

## Merge — conflict resolutions (explicit)

| # | Conflict | Resolution | Rationale |
|---|---|---|---|
| C1 | rename: alias (A) vs hard rename (B/C) | Canonical **`AgentCore`**; keep a **transitional** `pub type AgentSpec = AgentCore;` during migration commits, **delete in final cleanup** | Honors "one class called AgentCore" end-state (B/C) while de-risking the LIVE worktree during migration (A) |
| C2 | struct home: engine.rs (A) vs `agent.rs` (B) vs `agent/` dir (C) | New **`agent/` module dir** (`core.rs`, `config.rs`, `loader.rs`, `injection.rs`, `builtin.rs`, `skills.rs`, `mod.rs`) | 250-LOC ceiling; separation of concerns |
| C3 | `ToolAllowlist` in yaca-core (A/B) vs yaca-tool (C) | **yaca-tool** (next to `ToolRegistry`), with `schemas_for(&allowlist)` + `require_allowed(&allowlist,name)` | Registry owns schemas+execution; yaca-core already depends on yaca-tool |
| C4 | keyword: literal substrings (A) vs `regex` (B/C) | **`regex`** crate; keyword compiled at load, fail-fast on invalid | D2/omo `ulw` IS a regex `(?i)(ultrawork|ulw)`; regex is a standard Rust dep |
| C5 | unknown skill: warn (A) vs hard error (B/C) | **Hard error** with known-skill list | Fail fast at config boundary; catch typos |
| C6 | skills default | `skills: all` keyword (builtin `build` uses it = back-compat); omitted ⇒ none; `Named([..])` curated | Preserves today's "all skills to build" without re-creating everyone-sees-everything by default |
| C7 | MemberSpec: carry `AgentCore` (A/B) vs `AgentName`+catalog (C) | **`MemberSpec { agent: AgentName }`** + `catalog` into `run_team`/`run_member`; unknown-name ⇒ `MemberStatus::Failed` | Literal realization of D7 "spawn by agent name"; preserves supervisor isolation |
| C8 | injection plug + dedup (Round-1 D1/D4) | **Coherent split by trigger**: `always`+`session-start` directives are composed into `AgentCore.system_prompt` at **LOAD** (static for a given agent+model). `keyword` directives are evaluated at **run_turn** off the projection and emitted as a logged `<yaca-injection name="X">` System message via `inject_system_message`; `once` dedup = marker absent in projection. | One policy per trigger (no transient-vs-logged mix); no engine mutable state; no `admit_user_prompt` change. Post-compact recovery DEFERRED ⇒ a keyword directive summarized away by compaction is NOT re-injected (documented limit, §6/§9). |
| C9 | `request_from_messages` return type | Stays **infallible**; only filters tools + appends `always` injection text | Injection bodies pre-loaded; fallible emits happen via `inject_system_message` in run_turn |
| C10 | `schemas()` nondeterministic order | `schemas_for()` filters **and sorts** | Deterministic advertise order; cheap win |
| C11 | tool count (Round-1 D3) | **10** (incl. `ask_user` + `task`) | Round-1 review corrected 8→9→10; `task` is the lead-only subagent-spawn tool |

## Module layout

```
crates/yaca-core/src/agent/
  mod.rs         # re-exports: AgentCore, AgentCatalog, AgentConfigError, ResolvedSkill, SkillSelection
  core.rs        # AgentCore struct
  config.rs      # AgentFrontmatter, InjectionFrontmatter (serde), frontmatter splitter
  loader.rs      # AgentCatalog::load: discovery, precedence, extends, validation, no-config default
  injection.rs   # InjectionRule, InjectionTrigger, Selector, match/select + variant resolution
  skills.rs      # SkillCatalog + SkillSelection (moved from yaca-cli; CLI imports back)
  builtin.rs     # include_str! builtin agent presets + ulw injection
  builtin_agents/{build,quick,deep,ultrabrain,writing}.md
  builtin_injections/ulw.md
crates/yaca-tool/src/tool.rs   # + ToolAllowlist, schemas_for, require_allowed
```

## 1. `AgentCore` + `ToolAllowlist`

```rust
// crates/yaca-tool/src/tool.rs (or allowlist.rs)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolAllowlist {
    All,                                  // default — every builtin tool (incl. ask_user)
    Only(std::collections::BTreeSet<String>),
}
impl ToolAllowlist {
    #[must_use] pub fn permits(&self, tool: &str) -> bool {
        match self { Self::All => true, Self::Only(s) => s.contains(tool) }
    }
}
impl ToolRegistry {
    #[must_use] pub fn schemas_for(&self, a: &ToolAllowlist) -> Vec<ToolSchema> { /* filter + sort by name */ }
    pub fn require_allowed(&self, a: &ToolAllowlist, name: &str) -> Result<Arc<dyn Tool>, ToolError> {
        if !a.permits(name) { return Err(ToolError::Other(format!("tool '{name}' not allowed for this agent"))); }
        self.get(name).ok_or_else(|| ToolError::Other(format!("unknown tool: {name}")))
    }
}
```

```rust
// crates/yaca-core/src/agent/core.rs
#[derive(Clone, Debug)]
pub struct AgentCore {
    pub name: AgentName,
    pub model: ModelRef,
    pub system_prompt: String,            // fully composed: extends-chain + env + context + skills + always-injections
    pub workdir: PathBuf,
    pub reasoning: Option<ReasoningEffort>,
    pub allowed_tools: ToolAllowlist,     // NEW — default All
    pub skills: Vec<ResolvedSkill>,       // NEW — resolved, validated
    pub injections: Vec<InjectionRule>,   // NEW — directives whose `agents` selector matched this agent
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSkill { pub name: String, pub description: String, pub path: PathBuf }
```

Field names match today's `AgentSpec` to minimize churn; `system_prompt` kept (not `prompt`) so engine
read-sites are untouched. `injections` holds the agent-matched rules; model/trigger/dedup are evaluated at
run time (§6).

## 2. Config schema + examples

**Parser: reuse `serde_norway`** (workspace dep, used at [config.rs:96](../../../crates/yaca-cli/src/config.rs)). A small splitter extracts the `---`-fenced frontmatter; `serde_norway::from_str` parses it; the body is the prompt. The scalar-only `parse_skill` ([skills.rs:10](../../../crates/yaca-cli/src/skills.rs)) is insufficient (needs lists/maps) and stays untouched. `#[serde(deny_unknown_fields)]` on frontmatter structs catches typos.

**Agent file** `.yaca/agents/<name>.md` (frontmatter + body=prompt):
```yaml
---
name: build              # optional; defaults to file stem; must match stem if present
model: claude-sonnet-4-6 # optional; required somewhere in the extends chain
description: ...          # optional
extends: build           # optional; single parent
reasoning: medium        # optional: low|medium|high
allowed_tools: all       # optional; "all" | [read, ls, ...]; omitted ⇒ All
skills: all              # optional; "all" | [name, ...]; omitted ⇒ none
injections: [ulw]        # optional; opt-in allowlist; if empty, rule `agents:` selector decides
---
You are yaca, a coding agent.
```

**Injection file** `.yaca/injections/<name>.md`:
```yaml
---
name: ulw
agents: [build]                 # glob/list; "*"=all; omitted ⇒ ["*"]
models: "*"                     # glob/list of id/family; omitted ⇒ all
trigger: keyword                # always | session-start | keyword
keyword: "(?i)(ultrawork|ulw)"  # required iff trigger=keyword; compiled regex
once: true                      # dedup per session (marker scan)
priority: 100                   # i32; ascending => later in concatenation (stronger recency)
variants:                       # optional per-model-family override → sibling file
  gpt: ulw.gpt.md
  gemini: ulw.gemini.md
---
<ultrawork-mode>
...default body...
</ultrawork-mode>
```

**Builtin presets** (embedded via `include_str!`, preserve `CategoryRegistry` model strings + appends):
`build` (persona, `allowed_tools: all`, `skills: all`), `quick`→`extends: build, model: tier-cheap` body "Be fast and minimal.", `deep`→`tier-strong` "Think deeply and thoroughly.", `ultrabrain`→`tier-max` "Hardest reasoning; leave no stone unturned.", `writing`→`tier-writer` "Write clear, well-structured prose." Plus a read-only example `oracle`/`reader` (`allowed_tools: [read, ls, glob, find, grep]`) and the `ulw` injection.

## 3. Loader (`AgentCatalog`)

```rust
pub struct AgentCatalog { agents: BTreeMap<String, AgentCore> }
impl AgentCatalog {
    pub fn load(opts: AgentLoadOptions) -> Result<Self, AgentConfigError>;
    pub fn get(&self, name: &str) -> Option<&AgentCore>;
    pub fn require(&self, name: &str) -> Result<&AgentCore, AgentConfigError>;
}
// AgentLoadOptions { workdir, default_model, primary_agent (default "build"),
//   prompt_env: PromptEnv, context_files: Vec<(String,String)>, skills: SkillCatalog,
//   injections: InjectionLibrary }
```

- **Discovery**: builtins (`include_str!`) → global `~/.config/yaca/agents/*.md` (honor `$XDG_CONFIG_HOME`) → project `<workdir>/.yaca/agents/*.md`. Same for `injections/`. Keyed by frontmatter `name` (must match file stem).
- **Precedence** project > global > builtin: whole-file replace per name (NO field-level cross-layer merge — keeps errors localizable). Field merge happens ONLY along `extends`.
- **`extends`**: resolve after precedence. Compose parent→child: `system_prompt` = parent body + "\n\n" + child body; `allowed_tools`/`skills`/`injections` = UNION; `model`/`reasoning`/`description` = child wins. Cycle detection (`HashSet` of seen) + depth cap (8) → `AgentConfigError::InheritanceCycle`/`UnknownParent`. Single-parent only.
- **Validation (fail-fast, no panic)**: unknown tool in `allowed_tools` (checked vs `ToolRegistry` names), unknown skill, unknown injection, missing model in chain, empty prompt, invalid keyword regex, filename/`name` mismatch, malformed/unterminated frontmatter. All → `AgentConfigError` (thiserror) with path + context.
- **No-config default**: zero agent files ⇒ builtin `build` resolves with `default_model` and today's persona — byte-identical behavior to current `agent_with_model`. Broken project file ⇒ surfaced error; CLI may fall back to a hardcoded `build` (mirrors offline-provider philosophy) so a typo never locks the user out.

## 4. Allowed-tools enforcement (both points)

- **Advertise** — `request_from_messages` ([engine.rs:474](../../../crates/yaca-core/src/engine.rs)): `tools: tools.schemas_for(&agent.allowed_tools)` (was `tools.schemas()`).
- **Execute** — dispatch loop ([engine.rs:325-358](../../../crates/yaca-core/src/engine.rs)): `self.tools.require_allowed(&agent.allowed_tools, &tc.name)` (was `self.tools.get(&tc.name)`); disallowed ⇒ `Event::ToolError` "tool not allowed", model self-corrects next round. Unknown-tool error preserved.
- Single source of truth (`permits`) at both points ⇒ advertise/execute can't disagree (proptest). Empty `Only({})` rejected at load. Default `All`.

## 5. Skills wiring (per-agent)

Move `SkillCatalog`/`SkillSelection` into `yaca-core::agent::skills` (CLI imports back). `SkillSelection::{None, All, Named(Vec<String>)}`. Loader resolves each agent's selection against the discovered catalog (unknown name ⇒ hard error). `build_system_prompt` gains an optional related-skills section appended after project context, listing ONLY that agent's skills as "available on demand" (names+descriptions, not bodies). Empty ⇒ no section. Builtin `build` = `skills: all` (back-compat); siblings curate. This replaces the global `skills_section` injection at [main.rs:166-169](../../../crates/yaca-cli/src/main.rs).

## 6. Injection-rule engine

```rust
pub enum InjectionTrigger { Always, SessionStart, Keyword }
pub enum Selector { All, Patterns(Vec<String>) }   // glob via yaca_tool::permission::glob_match (re-export)
pub struct InjectionRule {
    pub name: String, pub agents: Selector, pub models: Selector,
    pub trigger: InjectionTrigger, pub keyword: Option<regex::Regex>,
    pub once: bool, pub priority: i32,
    pub default_body: String, pub variants: BTreeMap<String,String>, // family → body (pre-loaded)
}
```

- **Two-phase by trigger (coherent persistence policy — Round-1 D4):**
  - **LOAD phase (static):** `always` + `session-start` directives matching the agent's `agents` selector are composed into `AgentCore.system_prompt` at load (after env/context/skills), ordered by ascending `priority`, per-model variant chosen for the agent's bound model. They are part of the system prompt every turn — no markers, no per-turn logic. (`always` and `session-start` are equivalent under a static system prompt in this task; the distinction is reserved for a future dynamic/event phase.)
  - **RUN phase (dynamic):** only `keyword` directives are evaluated in `run_turn`, off the projection it already reads: filter the agent's keyword rules by `models` (active `agent.model`), test each compiled regex against the latest user-message text; on a match, if its `<yaca-injection name="X">` marker is absent from the projection's System messages, emit the per-model-variant body wrapped in that marker via `inject_system_message` ([engine.rs:113](../../../crates/yaca-core/src/engine.rs)) BEFORE building the request. `once: true` ⇒ marker dedup prevents re-fire.
- **Why logged markers (not transient `request.system`):** a logged System message is a real event ⇒ deterministic across `replay()` and re-derivable dedup with NO extra engine field. (omo-faithful: omo dedups by scanning the transcript for `<ultrawork-mode>`.)
- **Compaction interaction (Round-1 D1):** `compact_with` ([compaction.rs:55](../../../crates/yaca-core/src/compaction.rs)) reshapes ONLY the per-request message Vec, never the store — so the marker always remains in `read_projection` ⇒ dedup never double-fires. BUT a keyword directive whose turn is later summarized away is NOT re-shown to the model (post-compact recovery is OUT OF SCOPE per D5). This is a **documented limitation, not a guarantee** — no "survives compaction" claim is made.
- **Ordering**: ascending `priority`, tiebreak by name (deterministic).
- **Per-model variants**: `model_family(model)` (substring map: gpt/gemini/claude/default) → `variants.get(family)` else `default_body`.
- **`ulw`**: `{agents:[build,…], models:"*", trigger:keyword, keyword:"(?i)(ultrawork|ulw)", once:true}` — user types "ulw …" ⇒ body injected once as a logged `<yaca-injection name="ulw">` System message before the turn.
- **Plug point**: a `prepare_keyword_injections(session, agent, &projection)` helper at the top of `run_turn` before `request_from_messages`. **No change to `admit_user_prompt` or its callers.** `request_from_messages` is unchanged re: injections (always/session-start already baked into `system_prompt`).

## 7. Category → preset unification + spawn-by-name

- **Delete** `crates/yaca-core/src/category.rs` (`CategoryRegistry`, `CategoryEntry`, `ResolvedCategory`, `build_member_agent`); remove `pub mod category` + re-exports ([lib.rs:18](../../../crates/yaca-core/src/lib.rs)). `inject_skills` logic folds into the loader's skills section.
- **Presets**: quick/deep/ultrabrain/writing become builtin agent files `extends: build` with model overrides + body = old `prompt_append`. **Names unchanged**, so `TaskTool`'s `subagent_type` enum `{quick,deep,ultrabrain,writing}` ([tool.rs:581](../../../crates/yaca-tool/src/tool.rs)) stays valid as **agent names** (semantics shift category→agent; enum + description kept this task).
- **Runtime spawn path = the real "spawn by name" (Round-1 D6).** Flow: model calls `task` → `TaskTool.execute` builds `Vec<SpawnMember{description,prompt,subagent_type}>` → `ctx.spawner.spawn(..)` ([tool.rs:636](../../../crates/yaca-tool/src/tool.rs)) → `SpawnRequest` over mpsc → the **supervisor loop [yaca-cli/src/main.rs:200-230](../../../crates/yaca-cli/src/main.rs)** drains it and calls `run_team` (the ONLY `run_team` caller). **ACTUAL current behavior** (`spawn_team_supervisor`, [main.rs:199-238](../../../crates/yaca-cli/src/main.rs)): the supervisor **ignores `subagent_type`** and clones a single `base: AgentSpec` into every `MemberSpec` (using `m.prompt` as the directive) — so `task subagent_type:deep` currently runs on the **base build model**. Category routing is **NOT wired into the live spawn path**; `CategoryRegistry`/`build_member_agent` are exercised ONLY by `tests/category_routing.rs`, never at runtime. **Migrate the supervisor** to hold an `Arc<AgentCatalog>` and map each `SpawnMember.subagent_type` → `MemberSpec { agent: AgentName::new(subagent_type) }`, passing the catalog into `run_team` (resolution + unknown⇒`Failed` happen in `run_member`). This is a behavior **improvement**: spawned members finally get their preset's model. `SpawnerPlane` / `ToolCtx.spawner` / `engine.with_spawner` ([engine.rs:42,79,343](../../../crates/yaca-core/src/engine.rs)) are UNCHANGED.
- **`run_team`/`run_member`** ([subagent.rs:76,53](../../../crates/yaca-core/src/subagent.rs)): `run_team(engine, lead, catalog: Arc<AgentCatalog>, specs, cancel)`; `run_member` resolves `catalog.require(member_name)?` → `AgentCore` (workdir from lead). Resolution failure ⇒ `MemberEvidence{status: Failed, summary}`.
- **Rewrite** `tests/category_routing.rs` → `agent_preset_routing.rs`, **preserving** the contract: 4 presets → 4 distinct model calls `{tier-cheap,tier-strong,tier-max,tier-writer}`; plus `preset_extends_build_prompt`, `preset_includes_referenced_skill`, and `unknown_subagent_type_member_fails`.

## 8. `AgentSpec → AgentCore` migration (blast radius)

Rename consumers that hold/read an agent VALUE (mechanical `AgentSpec`→`AgentCore`, no catalog needed): [engine.rs](../../../crates/yaca-core/src/engine.rs) (`run_turn`, `projection_to_messages`, `request_from_messages`), [completion.rs](../../../crates/yaca-core/src/completion.rs) (`run_goal`, `LeadTurnExecutor.agent`), [loop_mode.rs](../../../crates/yaca-core/src/loop_mode.rs) (`run_loop`, `WorkerSessionExecutor.agent`), [yaca-server/src/lib.rs](../../../crates/yaca-server/src/lib.rs) (`AppState.agent: Arc<AgentCore>`), [yaca-cli/src/tui.rs](../../../crates/yaca-cli/src/tui.rs) (`run`, `spawn_turn`, `/model` re-resolve). Sites with API changes: [subagent.rs](../../../crates/yaca-core/src/subagent.rs) (`MemberSpec.agent: AgentName`; `run_member`/`run_team` take `catalog`), [yaca-cli/src/main.rs](../../../crates/yaca-cli/src/main.rs) (`agent_with_model`→`AgentCatalog`; **supervisor loop :200-230** holds the catalog + resolves `subagent_type`). **`run_team` is called ONLY by the supervisor** ([main.rs:217](../../../crates/yaca-cli/src/main.rs)) — completion/loop_mode use `run_turn` on a single agent ⇒ rename only. Tests: turn_loop, subagent, goal_loop, category_routing, server/api construct `AgentSpec` literals → add new fields / switch type. Transitional `pub type AgentSpec = AgentCore;` keeps concurrent worktree code compiling; **removed in final cleanup** (`rg AgentSpec crates/` ⇒ zero). `cargo build --workspace` catches every miss.

## 9. Risks / edge cases

- **Live worktree churn** on engine.rs/lib.rs/main.rs → land additive Wave 1 first; transitional alias; small atomic commits.
- **`schemas()` nondeterminism** → `schemas_for` sorts; tests set-compare.
- **`/model` slash command** mutates `agent.model` ([tui.rs](../../../crates/yaca-cli/src/tui.rs)) → must re-resolve model-dependent injections; covered by test.
- **Loader perf** (per-turn) → `AgentCatalog` built once at startup; `run_turn` only does the cheap injection scan.
- **`extends` cycle / depth bomb** → cycle set + depth cap 8.
- **`once` + compaction (Round-1 D1)** → dedup marker lives in the event log (compaction reshapes only the per-request Vec, [compaction.rs:55](../../../crates/yaca-core/src/compaction.rs)) ⇒ never double-fires. A keyword directive summarized away is NOT re-injected (post-compact recovery deferred, D5) — a documented limit, NOT a guarantee. Test asserts no double-fire while the marker is in the projection (no "survives compaction" claim).
- **`ask_user` in `All`** → kept for back-compat; read-only agents exclude it.

## 10. Out of scope (explicit)

Char budgets/truncation; dynamic rules; post-compact recovery; field-level cross-layer merge; multi-parent `extends`; executable injection hooks; hot-reload/watch; `yaca agents` CLI; JSON-schema/versioning; tool groups (`readonly`/`write`); merging permission-plane with allowlist; HTTP per-session agent routing; broadening `TaskTool`'s `subagent_type` enum beyond the migrated presets; porting omo's full roster. (Several deferred to later pi-parity waves.)

## Plan Review

### Round 1 — codex `gpt-5` (cross-family) — VERDICT: FAIL
D1 FAIL: injection scope self-contradiction (defer post-compact recovery vs once-dedup "survives compaction") → **fixed**: coherent two-phase policy; dedup marker in event log never double-fires; compacted-away directive NOT re-injected (documented limit, no survival claim). [§6, §9, prd D5]
D2 FAIL: Wave 7 step 27 non-atomic → **fixed**: split into per-file atomic sub-steps with gates. [implement Wave 7]
D3 FAIL: 10 builtins incl `TaskTool`, not 9 → **fixed**: corrected count + read-only exclusions + allowlist tests. [§ corrected-fact, C11]
D4 FAIL: once(logged) vs always(transient) incoherent with "all triggers emit markers" → **fixed**: always/session-start baked into `system_prompt` at load; only keyword emits a marker. [§6, C8]
D5 FAIL: AC3/AC5 lack real-surface QA → **fixed**: added live `yaca exec` skills QA + live `task`-tool spawn-by-name QA. [implement Wave 9]
D6 FAIL: spawn-by-name omitted `TaskTool`/supervisor → **fixed**: supervisor (main.rs:200-230) resolution + `subagent_type`→catalog enumerated. [§7, §8]

### Round 2 — codex `gpt-5` (cross-family) — VERDICT: FAIL (1 of 6)
D1, D2, D4, D5, D6 **PASS**. D3 **FAIL**: design misstated current supervisor behavior — `spawn_team_supervisor` ([main.rs:199-238](../../../crates/yaca-cli/src/main.rs)) actually **ignores `subagent_type`** and clones `base`; it does NOT use `CategoryRegistry`/`build_member_agent` (test-only). → **fixed**: §7/§8 + implement 27c now state real behavior; migration reframed as *wiring* the currently-ignored `subagent_type` → catalog resolution (a behavior improvement, not a replacement).

### Round 3 — codex `gpt-5` (cross-family) — VERDICT: PASS
All D1–D6 **PASS**. D3 confirmed against source (`spawn_team_supervisor` ignores `subagent_type` + clones base, [main.rs:199](../../../crates/yaca-cli/src/main.rs); `TaskTool` [tool.rs:547,581](../../../crates/yaca-tool/src/tool.rs); `CategoryRegistry`/`build_member_agent` test-only). **Cross-model gate cleared — plan is execution-ready, pending user `task.py start`.**

## Implementation deviations (recorded post-build)

1. **`extends` allowed_tools/skills (improvement over §3).** A child's explicit list **overrides** an
   inherited `All`; list+list **unions**. Plain "union" would make `All ∪ [read,ls] = All`, breaking the
   read-only `reader` preset. Covered by `loader_resolve.rs` tests.
2. **Tool count = 10** (incl `task`); default `All` includes it.
3. **`AgentSpec` fully removed** — `AgentCore` is the single canonical name (transitional alias deleted;
   `rg AgentSpec crates/` = 0).
4. **`AgentLoadOptions.injections`** is overwritten by on-disk discovery inside `AgentCatalog::load`
   (the field matters only for the `builtins_only` test path). Minor, functional.
5. Per-agent skill section composed in the loader (not by changing `build_system_prompt`'s signature),
   keeping existing prompt tests stable.
