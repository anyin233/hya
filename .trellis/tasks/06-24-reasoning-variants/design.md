# Design: Per-model reasoning variants with OpenCode parity

Merged from two parallel planners (oracle = conservative/architecture-first;
deep = thorough/failure-modes). Divergences and their resolutions are logged in
§9. Requirements/ACs live in `prd.md`; OpenCode schema + citations in
`research/opencode-reasoning-schema.md`.

## 1. Architecture (one seam)

Resolve an agent/model `variant` (+ opencode.json model `options`/`variants`)
into a typed `ReasoningEffort` at the OpenCode server agent-prep boundary, store
it on `AgentSpec.reasoning`, and let the EXISTING pipeline carry it to the wire.
Provider encoders own per-provider validity (so an invalid level can never reach
the wire). No event/projection/TUI changes — providers already emit reasoning
events and the TUI already renders `Part::Reasoning`.

```
opencode.json{,c} on DISK (config_paths(workdir) [+ ~/.config/opencode], jsonc::from_str::<Value>)
        │  NOTE: st.global.config() is runtime-PATCH-only and starts EMPTY (global.rs:23) — it is
        │  NOT a disk load. The provider variant bundles are read from disk per turn.
        ▼  read per prompt turn: cfg = load_opencode_config(location::workdir(st))  // {} if none
session_agent_with_guidance(st, session)           ◄═══ THE SEAM (reference.rs:19)
   • existing: overlay session agent name + custom prompt
   • NEW: entry = agent_catalog::list(..).find(active agent)
          model  = projection.session.model ⟶ else base agent.model
          agent.reasoning = resolve_reasoning(entry.variant, entry.options, model, &cfg)
   • the agent-variant-NAME path needs NO cfg (AC1/AC4): "max" parses directly;
     cfg only feeds the provider.models.{options,variants} bundle path (AC3).
        │  &AgentSpec{ reasoning: Some(effort), .. }
        ▼
engine/turn/messages.rs:58  request_from_messages → CompletionRequest.reasoning   (unchanged)
        ▼
ProviderRouter::stream → strips reasoning iff !caps.reasoning_request             (unchanged)
        ▼
Protocol::encode (anthropic/openai/google)
   anthropic: req.reasoning.and_then(|e| e.anthropic_budget()) → thinking{enabled,budget_tokens}
   openai:    req.reasoning.and_then(|e| e.openai_label(model_id)) → reasoning_effort  (Max⟶"xhigh")
   google:    req.reasoning.and_then(|e| e.google_budget(model))→ thinkingConfig.thinkingBudget
```

VERIFIED: all prompt turns converge on `session_agent_with_guidance`
(`session_prompt.rs:113,155`, `session_prompt_legacy.rs:60`,
`session_legacy.rs:217,329`); `agent_with_guidance` covers the no-session case
(`session_legacy.rs:401`). The native `hya` bridge talks to these in-process
opencode endpoints, so this one seam covers the native path.

## 2. Type design (`crates/hya-provider/src/lib.rs`)

Keep `ReasoningEffort` a typed enum (NOT a stringly/option-bag), extended to the
full OpenCode vocabulary. Per-provider validity + clamping live in methods on the
enum, so encoders cannot bypass them.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReasoningEffort {
    Off,      // OpenCode "none" — explicit disable. Named `Off` (not `None`) to
              // avoid Option<ReasoningEffort>::None shadowing/confusion.
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub fn parse(s: &str) -> Option<Self>;        // accepts none|off, minimal, low, medium|med, high, xhigh, max
    pub fn as_str(self) -> &'static str;          // canonical OpenCode strings; Off → "none"

    /// OpenAI `reasoning_effort` label, or None to omit. `model_id` enables
    /// per-model narrowing (consistent with google_budget). Max CLAMPS to
    /// "xhigh" (OpenAI rejects "max"); Off → None. All configured OpenAI models
    /// (gpt-5.5/5.4) accept "xhigh"; fine-grained per-model subset narrowing
    /// (xhigh-by-release-date, research §2) is a bounded follow-up.
    pub fn openai_label(self, model_id: &str) -> Option<&'static str>;

    /// Anthropic thinking.budget_tokens, or None to omit (Off/Minimal → None).
    /// High = 16_000, Max = 31_999 (constant; see §6).
    pub fn anthropic_budget(self) -> Option<u32>;

    /// Google 2.5 thinkingBudget, or None to omit. `model_id` picks the Max cap
    /// (2.5-pro = 32_768 else 24_576). Gemini-3 thinkingLevel is a documented
    /// follow-up (§8) — no Google model is configured to test it.
    pub fn google_budget(self, model_id: &str) -> Option<u32>;
}
```

Decision (resolves PRD DQ1): typed enum + per-provider methods. Rejected the
richer "variant = arbitrary provider-option bundle on AgentSpec/CompletionRequest"
model — PRD excludes non-reasoning option passthrough and it bloats the contract.
`AgentSpec.reasoning: Option<ReasoningEffort>` and `CompletionRequest.reasoning`
are UNCHANGED in shape; only the enum grows.

Option semantics: outer `Option::None` = "no reasoning configured" (default
behavior unchanged); `Some(ReasoningEffort::Off)` = "explicitly disabled" — both
omit the wire field, so they are observationally identical at the provider but
distinct for config precedence.

## 3. Per-provider validity matrix

| Level   | OpenAI `reasoning_effort` | Anthropic `thinking.budget_tokens` | Google 2.5 `thinkingBudget` |
|---------|---------------------------|------------------------------------|------------------------------|
| Off     | omit                      | omit                               | omit                         |
| Minimal | "minimal"                 | omit                               | omit                         |
| Low     | "low"                     | 1024                               | omit                         |
| Medium  | "medium"                  | 4096                               | omit                         |
| High    | "high"                    | 16000                              | 16000                        |
| XHigh   | "xhigh"                   | 24000                              | 20000                        |
| **Max** | **"xhigh"** (clamp)       | **31999**                          | 24576 (32768 for `*2.5*pro*`)|

The "OpenAI never emits max" rule is enforced INSIDE `openai_label()` (returns
"xhigh" for Max) and asserted by a unit test, so a future encoder edit cannot
silently regress it.

## 4. Reasoning resolver (new: `crates/hya-server/src/opencode/reasoning_options.rs`)

```rust
/// Read + deep-merge opencode.json{,c} from DISK into one Value (workdir paths
/// per config_paths() then ~/.config/opencode, via jsonc::from_str::<Value>;
/// workdir wins). Returns {} when no file exists. This — NOT st.global.config()
/// (runtime-PATCH-only, empty) — is the source for provider model bundles.
pub(in crate::opencode) fn load_opencode_config(workdir: &Path) -> serde_json::Value;

pub(in crate::opencode) fn resolve_reasoning(
    agent_variant: Option<&str>,
    agent_options: &BTreeMap<String, serde_json::Value>,
    model: &ModelRef,                 // active model (may carry #variant)
    config: &serde_json::Value,       // merged disk opencode.json ({} if none)
) -> Option<hya_provider::ReasoningEffort>;
```

Algorithm (mirrors OpenCode precedence):
1. Fast exit (backward compat / AC5): `agent_variant.is_none() && agent_options
   has no reasoning key && model has no #variant` → return `None`.
2. Split model via `model_ref::model_ref_parts` → `(provider_id?, model_id,
   model_variant?)`. Selected variant = `model_variant.or(agent_variant)`.
3. Locate the model node: `config["provider"][provider_id]["models"][model_id]`.
   If `provider_id` is absent (bare native id), scan `provider.*.models` for
   `model_id`; first match wins (BTreeMap → deterministic). (Steps 1-2 and 5a need
   no `config`; disk `config` is consulted only here onward for the bundle path —
   so AC1/AC4 work even with NO opencode.json on disk.)
4. Build a merged option map (deep-merge, reuse `agent_catalog::merge_json_value`,
   extracted to `opencode/json_merge.rs`): base `models.<id>.options` ← agent
   `options` ← selected `variants.<name>` (skip if `disabled:true`).
5. Project merged map → effort, first match wins:
   a. selected variant name parsed as a level (`max`,`high`,…);
   b. direct keys `reasoningEffort` / `reasoning_effort` / `reasoning.effort` / `effort`;
   c. Anthropic `thinking.budgetTokens|budget_tokens` → budget heuristic
      (≥30000→Max, ≥20000→XHigh, ≥15000→High, ≥3500→Medium, ≥512→Low);
   d. Google `thinkingConfig.thinkingBudget` / `thinkingLevel` similarly.
6. Explicit "none"/"off" → `Some(Off)`; nothing found → `None`.

This module is the ONLY place that knows opencode.json shape. `hya-provider`
knows nothing about variants; `hya-core`/`hya-proto` know nothing about
opencode.json.

## 5. Wiring (`crates/hya-server/src/opencode/reference.rs`)

Extend `session_agent_with_guidance` (and `agent_with_guidance` for the
no-session default agent) with a shared PURE helper
`apply_agent_entry(agent: &mut AgentSpec, entry: &AgentEntry, active_model: &ModelRef, config: &Value)`
(no `ServerState` → unit-testable in-module): after the existing name/prompt
overlay, compute the active model, load `config = load_opencode_config(location::workdir(st))`
(DISK read — NOT `global.config()`), look up the agent entry (already fetched in
the existing block — widen its scope), call `resolve_reasoning`, and set
`agent.reasoning = effort` when `Some`. Precedence
when both a session `model#variant` and an agent `variant` exist: the
session/model variant wins (explicit user selection > agent default).

`crates/hya-app/src/runtime.rs::agent_with_model` stays `reasoning: None` for a
plain id — UNCHANGED. The native direct-selection `model#variant` fallback is
OUT OF SCOPE (see §8): the user's agents use frontmatter `variant:`, which flows
through the seam above, so no AC needs it.

## 6. Max-budget bounding (resolves PRD DQ3)

`Capabilities` has no per-model output limit. Use a CONSTANT max budget (31999
Anthropic; 24576/32768 Google) matching OpenCode, NOT a new `Capabilities`
field. Rationale: adding `max_output_tokens` to the `Provider` trait + all impls
is scope the ACs don't need; `anthropic.rs` already bumps `max_tokens` to
`budget + 4096` when the requested cap is below the budget, which is the only
practical bound. (Reconciles oracle vs deep — see §9.) PRD R3/AC3 are worded to
match this constant (31999 covers all configured ≥32k-output models). Follow-up:
wire a real per-model output limit into provider metadata if a <32k-output model
appears.

## 7. Provider encoder changes (minimal)

Each encoder switches to the Option-returning method; no new branches beyond
Google's existing `thinkingConfig`:
- `anthropic.rs:42-47`: `if let Some(b) = req.reasoning.and_then(|e| e.anthropic_budget()) { ensure max_tokens > b; body["thinking"] = {type:"enabled", budget_tokens:b} }`.
- `openai.rs:59-60`: `if let Some(l) = req.reasoning.and_then(|e| e.openai_label(model_id)) { body["reasoning_effort"] = l }` (model_id from `req.model`).
- `google.rs:180-183`: `if let Some(b) = req.reasoning.and_then(|e| e.google_budget(model_id)) { thinkingConfig.thinkingBudget = b }`.

ADAPTIVE THINKING — FAIL CLOSED (critical, from deep's plan-review + oracle Risk#5):
do NOT emit `thinking:{type:"adaptive",effort}` — it is not a documented Anthropic
Messages wire field and would 400. `claude-opus-4-8` is in OpenCode's "adaptive"
family, but the documented Anthropic extended-thinking wire is
`type:"enabled",budget_tokens`, which an Anthropic-compatible gateway (12th.day)
accepts. We emit ONLY `enabled+budget_tokens` and confirm reasoning renders via
the AC1 manual run. If (and only if) AC1 fails with the enabled shape, escalate
to a source-backed adaptive branch — do not implement adaptive speculatively.

## 8. What NOT to build

Raw provider-option passthrough on AgentSpec/CompletionRequest; `Provider::family`
/ `served_model_id` trait methods; `Capabilities.max_output_tokens`; router-level
`normalize_for`; migrating `config.yaml` to JSON; the native `model#variant`
direct-selection fallback in `agent_with_model` (the seam covers the agent-config
case — follow-up only if direct native model selection is needed); a new runtime
plane; TUI render changes; Anthropic adaptive wire; Gemini-3 `thinkingLevel` (documented stub only —
no Google model configured); serde derives on `ReasoningEffort`; a second
resolution site inside `request_from_messages` (the double-resolution bug class
that caused this task).

## 9. Reconciliation log (oracle ↔ deep)

| Topic | oracle | deep | Resolution |
|---|---|---|---|
| Where validity lives | enum methods, encoders call them; no router change | `normalize_for` in router + `Provider::family()`/`served_model_id()` trait methods | **oracle.** Encoders already know their family and have `req.model`; trait/router churn buys nothing the ACs need. Smaller surface. |
| Max budget bound | constant 31999 | `Capabilities.max_output_tokens` + `bounded_anthropic_max` | **oracle (constant).** No Capabilities/trait changes; existing `max_tokens` bump suffices. Real limit = follow-up. |
| Explicit "none" level | `None_` variant | `None` variant + Some(None) vs None | **merged:** include the level, name it `Off` to avoid Option shadowing; document semantics. |
| Adaptive thinking | defer entirely | source-gated, fail-closed to budget | **merged:** emit only enabled+budget; verify AC1 empirically; no speculative adaptive. |
| runtime.rs `#variant` | not needed (seam covers it) | add native fallback | **merged:** seam is primary; runtime.rs fallback optional/trivial-only. |
| Resolver module name | `agent_reasoning.rs` | `reasoning_options.rs` | `reasoning_options.rs`. |

## 10. Backward compatibility & edge cases

| Case | Expected | Where |
|---|---|---|
| No variant/options | unchanged; no thinking/effort field | resolver fast-exit; `agent_with_model` keeps None |
| Explicit `none`/`off` | omit wire field | resolver → `Some(Off)`; encoders omit |
| Unknown variant string | ignore; no reasoning unless options specify | resolver step 5 |
| OpenAI + Max | "xhigh", never "max" | `openai_label` |
| Model w/o `reasoning_request` cap | reasoning stripped | `router.rs:55-56` (unchanged) |
| config.yaml-only user (no opencode.json) | unchanged | resolver returns None when config has no provider node |
| sonnet, no variant | unaffected | no signal → None |
| session `model#variant` vs agent `variant` | session wins | resolver step 2 precedence |
| `variants.<n>.disabled=true` | skip that bundle | resolver step 4 |

## 11. Risks / verify-first

1. **AC1 hinges on opus-4-8 honoring `enabled+budget_tokens` on the 12th.day
   gateway.** Verify empirically early (manual tmux). If it ignores thinking,
   investigate gateway behavior before any adaptive work.
2. **`conformance.rs:366`** asserts `max_tokens > 16384` for High; with High=16000
   the `+4096` bump (→20096) keeps it passing — confirm the existing bump logic
   and test still hold after the budget change.
3. **`reasoning_request` capability** for the 12th-anth provider: `HttpProvider::new`
   sets it `true` (`http.rs:117`), so reasoning is not stripped — confirm.
4. **Model→provider lookup** for bare native ids: scan `provider.*.models`;
   confirm deterministic and correct for `claude-opus-4-8`.
5. **`merge_json_value` extraction** from `agent_catalog.rs` to a shared
   `json_merge.rs` (pub(in crate::opencode)) without changing its semantics.
6. **CONFIG SOURCE (verified):** `st.global.config()` is runtime-PATCH-only and
   starts EMPTY (`global.rs:23,31`) — provider variant bundles MUST be read from
   DISK (`config_paths(workdir)` [+ `~/.config/opencode`], `jsonc::from_str::<Value>`),
   not from `global.config()`. The agent-variant-NAME path (AC1/AC4) needs no disk
   config; only the bundle path (AC3) does.

## 12. Plan Review (cross-model gate)

### Round 1 — oracle (claude-opus-4-7) — VERDICT: FAIL
> Cross-family constraint NOT satisfied: the oracle subagent ran on a
> Claude-family model (planner is also Claude); the subagent could not self-switch
> to its GPT fallback. Re-run cross-family via codex/gpt-5.5 in Round 2.

D1 PASS · D2 **FAIL** · D3 PASS · D4 PASS · D5 PASS · D6 PASS.
- D2 FAIL: `implement.md` Task 2 preserved `conformance.rs:366` (`max_tokens > 16384`)
  but omitted the sibling assertions that move with the parity budgets:
  `conformance.rs:365` (`budget_tokens == 16384`) and `:372` (`thinkingBudget == 24576`).
- D3 independently confirmed all cited symbols live, incl. native `hya` flowing
  through the opencode endpoints via in-proc axum (`serve.rs:81-103`) → the seam.
- FIX APPLIED: `implement.md` Task 2 now explicitly updates `conformance.rs:365`
  16384→16000 and `:372` 24576→16000 (verified these are the only budget literals).

### Round 2 — codex gpt-5 (cross-family ✓) — VERDICT: FAIL
D1 FAIL · D2 FAIL · D3 FAIL · D4 PASS · D5 FAIL · D6 PASS.
- D1: PRD R3/AC3 said "bounded by output limit" but design uses constant 31999 → PRD wording aligned to the constant (dynamic bound = follow-up).
- D2: Task 4 had two test paths + an optional runtime fallback → picked one concrete path (assert on `session_agent_with_guidance` output); runtime fallback moved to §8 out-of-scope.
- D3: `openai_label` made model-aware (signature + note); corrected the budget-literal sweep (enum source `lib.rs:105,114` rewritten in Task 1 + test assertions `conformance.rs:365,372`).
- D5: added an early AC1 gateway smoke (`implement.md` Task 2.5) before building the resolver.

### Round 3 — codex gpt-5 (cross-family ✓) — VERDICT: FAIL
D1 FAIL (prd.md:46 Scope still said "bounded by output limit") · D2 FAIL (`session_agent_with_guidance` is `pub(in crate::opencode)`, unreachable from external `tests/`) · D3 FAIL (diagram + encoder §7 still called `openai_label()` without model_id) · D4 PASS · D5 PASS · D6 PASS.
- D1: prd.md:46 reworded to the constant.
- D2: design §5 gives `apply_agent_entry` a pure ServerState-free signature; implement Task 4 is now an INTERNAL unit test on it.
- D3: design diagram (§1) + encoder §7 now pass `model_id`.

### Round 4 — codex gpt-5 (cross-family ✓) — VERDICT: FAIL
D1 PASS · D2 PASS · D3 **FAIL** · D4 PASS · D5 **FAIL** · D6 PASS.
- D3 (MAJOR, both same-family planners missed it): design sourced provider bundles from `st.global.config()`, which is runtime-PATCH-only and starts EMPTY (`global.rs:23`) — NOT a disk load. FIX: resolver now reads opencode.json from DISK via `load_opencode_config(workdir)` (`config_paths` + `jsonc`); §1/§4/§5 + §11#6 updated. AC1/AC4 (variant-name path) need no disk config.
- D5: tests used in-memory Values and AC1 could pass via the variant-name parse → didn't prove the disk bundle is actually read. FIX: added a file-backed integration test (`implement.md` Task 3) writing a real opencode.json whose variant name is NOT a level keyword so the BUNDLE must supply the effort, asserting it reaches the request body.

### Round 5 — codex gpt-5 (cross-family ✓) — VERDICT: FAIL
D1 PASS · D2 PASS · D3 FAIL · D4 PASS · D5 PASS · D6 PASS.
- D3: `config_paths` is private to `agent_sources.rs`; module root is `opencode.rs` (no `opencode/mod.rs`). FIX: implement Task 3 now makes `config_paths` `pub(super)` and declares modules in `opencode.rs`.

### Round 6 — codex gpt-5.5 (cross-family ✓) — VERDICT: PASS
D1–D6 all PASS. Plan cleared the cross-family gate; execution may begin after user go-ahead + `task.py start`.
