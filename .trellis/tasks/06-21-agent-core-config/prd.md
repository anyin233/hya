# Config-driven AgentCore — unified, file-loaded agent spec

Child of `06-20-hya-pi-parity`. Overlaps Wave 2 (system-prompt builder) and Wave 4
(SKILL.md skills), and subsumes/feeds the hardcoded category system. This is an
architecture optimization of hya's multi-agent design, not a `pi` feature port.

## Goal

Replace hya's hardcoded, in-code agent construction with a **single shared agent
type, `AgentCore`**, whose specification is **loaded from a config file**. One type
for every agent (primary `build` agent, team/subagent members, goal/loop runners),
each defined declaratively by:

1. **prompt** — base persona/system prompt
2. **name** — agent identifier
3. **allowed tools** — per-agent tool allowlist
4. **related skills** — names resolved against the pre-provided (discovered) skill list
5. **prompt-injection script** — extra directive injected **only** for that agent (like omo's `ulw`)

## User value

Today hya has exactly one hardcoded agent (`"build"`) plus four hardcoded categories.
Changing an agent's persona, tools, skills, or model requires editing Rust and
recompiling. After this, **agents are data**: define `build`, `plan`, `oracle`, … in a
config file; choose tools/skills/model/injected directives per agent without touching
code. This makes hya a real multi-agent platform and gives the team/subagent/category
systems a single substrate to resolve against.

## Confirmed facts (from code inspection)

- **`AgentSpec`** (`crates/hya-core/src/engine.rs:26`, `#[derive(Clone)]`) has **5 fields**:
  `name: AgentName`, `model: ModelRef`, `system_prompt: String`, `workdir: PathBuf`,
  `reasoning: Option<ReasoningEffort>` (last added by concurrent work).
  Re-exported from `hya-core` lib (`lib.rs:26`).
- **One real agent** is constructed: `agent_with_model()` (`crates/hya-cli/src/main.rs:156`)
  → name `"build"`, base persona literal `"You are hya, a coding agent."`, `system_prompt`
  assembled by `build_system_prompt(base, env, context_files)`.
- **`build_system_prompt`** (`crates/hya-core/src/prompt.rs:8`) is a pure fn: base + env
  preamble (cwd/platform/date) + project context files. (Wave 2 already done.)
- **Context/skills wiring** (`main.rs`): `discover_context_files()` walks up for `AGENTS.md`
  (`:123`); `skills::discover_skills(skill_dirs())` + `skills::skills_section()` appended as a
  context entry (`:166-169`); `skill_dirs()` = `[".hya/skills", "~/.config/hya/skills"]` (`:148`).
- **Categories** (`crates/hya-core/src/category.rs`): `CategoryRegistry::builtins()` hardcodes
  `quick/deep/ultrabrain/writing` → `CategoryEntry { model, fallback, prompt_append, token_budget }`.
  `build_member_agent(base, resolved, skills)` clones the base `AgentSpec`, appends
  `resolved.prompt_append`, then `inject_skills()` appends a flat `## Skills` text list.
- **Tools** (`crates/hya-tool/src/tool.rs`): `ToolRegistry::builtins()` = `read, write, edit,
  ls, glob, find, grep, shell, ask_user, task` (**10**; `task` = lead-only subagent-spawn tool — Round-1 review corrected 8→10). `request_from_messages` (`engine.rs:463`) sends
  `tools.schemas()` — **all tools to every agent**. **No per-agent allowlist exists.**
  `Tool` trait = `name()`, `schema()`, `execute(ctx, input)`.
- **Config today** (`crates/hya-cli/src/config.rs`): parses only opencode's `opencode.json`
  → providers/models (`ResolvedConfig { router, default_model }`). **No agent config.**
  `serde`/`serde_json` already deps.
- **Blast radius** (AgentSpec construct/consume): `engine.rs` (`projection_to_messages`,
  `request_from_messages`, `run_turn`), `subagent.rs` (`MemberSpec.agent`, `run_member`),
  `completion.rs` (`run_goal`), `loop_mode.rs` (`run_loop`), `category.rs` (`build_member_agent`),
  `hya-server/src/lib.rs` (`AppState.agent`), `hya-cli/src/tui.rs` (`run`, `spawn_turn`),
  `hya-cli/src/main.rs` (`agent_with_model`). ⚠️ `AgentSpec` has **no covering tests**;
  `build_member_agent` is tested in `crates/hya-core/tests/category_routing.rs`.

## Conceptual model (proposed)

- Extend/rename `AgentSpec` → **`AgentCore`** (the runtime, engine-facing agent value), gaining
  structured fields: `name`, `model`, base `prompt`, `allowed_tools` (allowlist), `skills` (names
  resolved against the discovered skill list), `injection` (per-agent prompt-injection directive).
- Add an agent **config schema** + **loader** in `hya-core` (`AgentConfig` → `AgentCore`). A
  **built-in default** config is embedded so a no-config install runs unchanged (preserving
  today's graceful default, mirroring the offline-provider fallback philosophy).
- `request_from_messages` + `run_turn`'s tool dispatch consult `AgentCore.allowed_tools` to
  **(a) advertise only allowed schemas** to the model and **(b) reject disallowed tool calls** at
  execution.
- `CategoryRegistry` + `build_member_agent` resolve over the new config (categories become agent
  presets, or an orthogonal model/prompt overlay) — exact unification is **Q3**.

## Requirements (draft)

- **R1.** A single shared agent type `AgentCore` represents every agent (primary + team members +
  goal/loop), carrying name, model, prompt, allowed_tools, skills, injection.
- **R2.** Agent specs load from a config file (format/location → Q1), with a built-in default so a
  no-config run is unchanged.
- **R3.** Per-agent allowed-tools: an agent only sees and can execute its allowlisted tools;
  disallowed calls are rejected (not merely hidden from advertisement).
- **R4.** Per-agent related skills: configured skill names resolve against the pre-provided
  discovered skill list and inject into that agent's prompt; unknown skill handling → Q (warn/err).
- **R5.** Per-agent prompt-injection script (ulw-style): injected into the system prompt only for
  the agent(s) it targets (semantics → Q2).
- **R6.** Regression-free: build agent, categories, team/subagent, goal/loop, server, TUI all keep
  working. Quality gate green (`fmt --check`, `clippy -D warnings`, `cargo test --workspace`);
  new behavior TDD-covered.

## Acceptance criteria (draft — refine per design)

- [ ] **AC1.** A config file defining agent `build` (prompt/name/model/tools/skills/injection)
      loads and a turn reflects it; removing the file → built-in default still runs.
      (evidence: unit test + live `hya exec` transcript)
- [ ] **AC2.** An agent with `allowed_tools = [read, ls]` is **not** offered write/edit/shell
      schemas **and** a forced disallowed call is rejected. (RED→GREEN unit test + live)
- [ ] **AC3.** An agent with `skills = [X]` has X's section in its system prompt; a sibling without
      it does not. (unit test)
- [ ] **AC4.** An agent with an injection script has that text in its prompt; a sibling without it
      does not. (unit test)
- [ ] **AC5.** `cargo fmt --check` + `clippy -D warnings` + `cargo test --workspace` green; existing
      category/subagent/prompt tests still pass.

## Resolved decisions

- **D1 (Q1) — Config format + location.** Per-agent **Markdown + YAML frontmatter** under
  `.hya/agents/<name>.md` (project) + `~/.config/hya/agents/<name>.md` (global), merged over an
  **embedded built-in default**; precedence **project > global > built-in**. Markdown **body = the
  `prompt`**; frontmatter carries `name`, `model`/`category`, `allowed_tools`, `skills`, `injection`.
  Mirrors the existing `SKILL.md` + `skill_dirs()` convention. Design note: confirm a frontmatter
  parser — reuse `hya-cli/src/skills.rs` parsing if it already handles frontmatter, else add a YAML dep.

- **D2 (Q2 + user steer) — Injection = omo-style per-(agent, model) RULE engine.** Verified against
  omo (see `research/omo-injection-mechanism.md`). An **injection directive** is a named text block
  whose **applicability is a rule** matched on the active **agent** ∧ **model**, plus a **trigger**.
  hya mirrors omo's model, adapted to hya's in-process engine (no hook IPC):
  - Library: `.hya/injections/<name>.md` (project) + `~/.config/hya/injections/<name>.md` (global).
    Frontmatter selectors: `agents` (glob/list, `*`=all), `models` (glob/list of id/family, optional=all),
    `trigger` (`always` | `session-start` | `keyword`), `keyword` (regex, when `trigger=keyword`),
    `once` (per-session dedup), `priority` (ordering). Body = injected text.
  - Engine: at prompt-build (and per user prompt for `keyword` triggers) select every directive whose
    `agents` ∧ `models` match `(name, model)` and whose `trigger` fires; inject by `priority`; dedup by
    `once`. `ulw` = `{ trigger: keyword, keyword: (?i)(ultrawork|ulw), once: true, agents: [lead] }`.
  - Per-model variants supported (`name.<family>.md` or frontmatter `variants`), mirroring omo's
    `ultrawork/{default,gpt,gemini}`.
- **D3 (Q4) — `allowed_tools` default = all builtin tools** when unspecified (back-compat + least
  surprise). Enforcement is **both**: advertise-filtered schemas AND execution-time rejection of
  disallowed calls (stricter than omo, which keeps tool policy as prose in the agent prompt).
- **D4 (Q5) — Roster scope.** Loader + all 5 fields end-to-end for the primary `build` agent and team
  members; seed 1–2 example agents (`build` + one read-only role) + the `ulw` injection. Porting omo's
  full roster (plan/librarian/metis/momus/…) is data, not code → out of scope.
- **D5 — Injection-engine scope for THIS task.** per-(agent, model) matching + `always`/`session-start`/
  `keyword` triggers + `once` dedup + `priority` ordering + per-model variants. **Defer** omo's heavier
  machinery: char budgets/truncation, dynamic rules, post-compact recovery (hya compaction is its own wave).
- **D6 — Agent schema validated vs omo.** omo `*.toml` (`name`, `description`, `model`,
  `model_reasoning_effort`, `developer_instructions` body) maps 1:1 to hya markdown+frontmatter
  (frontmatter `name`/`description`/`model`(or `category`)/`allowed_tools`/`skills`/`injections?`,
  body = prompt). hya adds **structured `allowed_tools` + `skills`** (omo keeps them as prose).
- **D7 (Q3) — FULLY UNIFY: categories become agent presets.** `CategoryRegistry::builtins()` /
  `CategoryEntry` / `ResolvedCategory` removed; the 4 categories (`quick/deep/ultrabrain/writing`)
  migrate to **agent presets** in `.hya/agents/` built-in defaults. Team/subagent members spawn by
  **agent name** resolved from config (not category-overlay-on-base). To migrate without duplicating
  the shared `build` persona, agent files support optional **`extends: <agent>`** inheritance (parent
  prompt prepended; `allowed_tools`/`skills` unioned; child overrides `model`/scalars) — the unified
  replacement for the old overlay. Impacted: `category.rs` (replaced by agent config), `subagent.rs`
  (`MemberSpec` resolves agent by name), `tests/category_routing.rs` (rewritten as agent-preset
  routing), `main.rs` (`agent_with_model`→loader), + `AgentSpec`→`AgentCore` rename across
  engine/completion/loop_mode/server/tui.

## Open questions — ALL RESOLVED (brainstorm complete)

- **Q1 → D1** (per-agent markdown+frontmatter) · **Q2 → D2** (omo-style (agent,model) injection rules)
  · **Q3 → D7** (fully unify; categories→agent presets) · **Q4 → D3** (allowed_tools default=all, enforced)
  · **Q5 → D4** (loader + 5 fields + seed 1–2 agents).

## Out of scope

- Porting omo's/pi's full agent roster or extension runtime.
- Dynamic/executable injection hooks (Q2 alternative) — deferred unless chosen.
- Provider/auth, session-tree, slash-command work (other pi-parity waves).
