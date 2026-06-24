# Reasoning Variants Technical Design + Execution Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use the project `trellis-before-dev` skill before editing, then use Rust TDD. This plan is implementation-first and assumes the worker has low context. Do not start source edits until the Trellis task is active.

**Goal:** Make OpenCode agent/model `variant:` values such as `max` drive real provider extended-thinking requests in yaca, with provider/model validity and backward-compatible defaults.

**Architecture:** Resolve OpenCode variant/options at the server/native request boundary into a typed `ReasoningEffort`, then normalize that effort in `yaca-provider` against provider family + model id before encoding wire-specific request fields. Keep event/projection behavior unchanged: providers emit canonical reasoning events, and existing TUI/OpenCode surfaces render them.

**Tech Stack:** Rust 2024 workspace; `serde_json::Value` for the existing OpenCode config boundary; yaca event-sourced engine; provider protocols in `yaca-provider`; OpenCode-compatible HTTP surface in `yaca-server`.

## Global Constraints

- Planning only in this file; no source edits in this pass.
- Rust crates deny `unwrap_used` / `expect_used` outside tests.
- Preserve yaca's event-sourced architecture: variant resolution prepares `CompletionRequest`; it must not create a second projection/replay path.
- Keep `yaca-proto` dependency-light; no new reasoning config types belong there.
- Reuse existing opencode config/agent catalog loaders instead of adding an unrelated configuration system.
- Verification gate: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.

---

## Current State Summary

- `crates/yaca-provider/src/lib.rs` has `ReasoningEffort::{Low, Medium, High}` only. `parse()` accepts `low`, `medium|med`, `high`; `as_str()` returns `low|medium|high`; `anthropic_budget()` is `1024/4096/16384`; `google_budget()` is `1024/8192/24576`.
- Provider encoders already consume `CompletionRequest.reasoning`: Anthropic writes `thinking:{type:"enabled",budget_tokens}`, OpenAI writes `reasoning_effort`, Google writes `generationConfig.thinkingConfig.thinkingBudget`.
- `ProviderRouter::stream()` strips reasoning only when provider capabilities say `reasoning_request == false`, but current HTTP providers expose a provider-wide boolean, not a per-model validity matrix.
- `AgentSpec.reasoning` exists and `request_from_messages()` forwards it into `CompletionRequest`, but runtime/server constructors leave it unset for OpenCode variants.
- `yaca-server` parses `AgentEntry.variant` and `AgentEntry.options`, and `model_ref.rs` preserves `#variant` in OpenCode model refs, but `reference.rs::session_agent_with_guidance()` only copies selected agent `prompt` and `name`.
- Workdir `opencode.json` readers currently parse agent/default/permission/command-style sections; provider model `options` / `variants` need a shared resolver over the same config file paths/global config value.

---

## 1. Design

### 1.1 Type changes in `crates/yaca-provider/src/lib.rs`

Extend the existing type instead of replacing it with stringly typed config. The public abstraction should remain typed and exhaustive.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFamily {
    OpenAi,
    Anthropic,
    Google,
    Dev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnthropicThinking {
    Disabled,
    Budget { budget_tokens: u32 },
    /// Source-gated OpenCode adaptive mode. Implement this encoder branch only
    /// after a failing test proves the exact upstream Anthropic wire shape;
    /// otherwise normalize adaptive-model efforts to bounded Budget mode.
    Adaptive { effort: ReasoningEffort },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoogleThinking {
    Disabled,
    Budget { thinking_budget: u32 },
    Level { thinking_level: ReasoningEffort },
}
```

Add/replace methods:

```rust
impl ReasoningEffort {
    pub fn parse(s: &str) -> Option<Self>;
    pub fn as_str(self) -> &'static str;
    pub fn normalize_for(self, family: ProviderFamily, model_id: &str) -> Option<Self>;
    pub fn openai_wire_effort(self, model_id: &str) -> Option<Self>;
    pub fn anthropic_thinking(self, model_id: &str, output_limit: Option<u32>) -> AnthropicThinking;
    pub fn google_thinking(self, model_id: &str) -> GoogleThinking;
}
```

Justification:

- Keeping `ReasoningEffort` gives compile-time exhaustive matches and minimizes churn in callers already using `Option<ReasoningEffort>`.
- `ReasoningEffort::None` is still useful because OpenCode's vocabulary includes `none`; when an option bundle explicitly says `none`, it should override weaker defaults. `Option<ReasoningEffort>::None` means “no configured reasoning setting”. `Some(ReasoningEffort::None)` means “explicitly disable reasoning”.
- Provider wire differences belong in provider code, not yaca-core or yaca-proto.

### 1.2 Provider family and per-model metadata

Move or mirror `ProviderKind` from `crates/yaca-provider/src/http.rs` into provider-level public metadata. Do not make `router.rs` depend on a private HTTP enum.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFamily {
    OpenAi,
    Anthropic,
    Google,
    Dev,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub streaming_tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub usage_reporting: bool,
    pub json_output: bool,
    pub reasoning_stream: bool,
    pub reasoning_request: bool,
    pub max_context: u32,
    pub max_output_tokens: Option<u32>,
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn family(&self) -> ProviderFamily;
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities>;
    ...
}
```

`HttpProvider::new()` maps `ProviderKindConfig`/`ProviderKind` into `ProviderFamily`. `DevProvider` and `FakeProvider` return `ProviderFamily::Dev` and keep `reasoning_request` false unless tests set it.

### 1.3 Full per-provider validity matrix

Implement the matrix in `ReasoningEffort::normalize_for()` and provider-specific helper methods. The matcher must use lowercased bare model ids after `provider/model#variant` has been stripped.

| Provider family | Model-id pattern | Valid levels | Max budget / wire | Invalid/clamp rule |
| --- | --- | --- | --- | --- |
| OpenAI | `gpt-5*`, `o*`, generic OpenAI-compatible reasoning models | `none,minimal,low,medium,high,xhigh` | `reasoning_effort` string; never `max` | `max -> xhigh`; levels below model subset clamp upward/downward to nearest supported; unknown/non-reasoning model -> `None` |
| OpenAI | older/non-reasoning models detected by no reasoning capability or model blacklist | none | omit field | drop reasoning entirely |
| Anthropic classic budget | `claude-opus-4-*`, `claude-sonnet-4-*` before adaptive families if not matched below | `high,max` | `high=16000`, `max=min(31999, output_limit - 1)` | `minimal/low/medium -> high`; `xhigh/max -> max`; `none -> omit` |
| Anthropic adaptive | `claude-opus-4-7*`, `claude-opus-4-8*`, `claude-sonnet-4-6*` and later known adaptive models | `low,medium,high,xhigh,max` | **Source-gated.** OpenCode research reports adaptive thinking for these models, but implementation must verify the exact Anthropic wire shape before emitting it. If not verified, normalize to bounded budget mode (`low/medium -> high`, `xhigh/max -> max`) so yaca never sends an undocumented body. | `minimal -> low`; `none -> omit`; unknown high levels clamp to max |
| Google Gemini 2.5 | `gemini-2.5-flash*`, `gemini-2.5-pro*` | `high,max` | `thinkingBudget`: `high=16000`; `max=24576`, or `32768` for `2.5-pro` | `minimal/low/medium -> high`; `xhigh/max -> max`; `none -> omit` |
| Google Gemini 3 Flash | `gemini-3*flash*` | `minimal,low,medium,high` | `thinkingLevel` string | `xhigh/max -> high`; `none -> omit` |
| Google Gemini 3 non-Flash | `gemini-3*` | `low,medium,high` | `thinkingLevel` string | `minimal -> low`; `xhigh/max -> high`; `none -> omit` |
| Dev/Fake/no reasoning | all | none | omit | drop reasoning entirely |

### 1.4 Budget extension and output-token bounding

Replace the current simple budget helpers with provider-aware helpers:

```rust
const ANTHROPIC_HIGH_BUDGET: u32 = 16_000;
const ANTHROPIC_MAX_BUDGET: u32 = 31_999;
const ANTHROPIC_FALLBACK_OUTPUT_LIMIT: u32 = 32_000;
const GOOGLE_HIGH_BUDGET: u32 = 16_000;
const GOOGLE_FLASH_MAX_BUDGET: u32 = 24_576;
const GOOGLE_PRO_25_MAX_BUDGET: u32 = 32_768;

fn bounded_anthropic_max(output_limit: Option<u32>) -> u32 {
    let limit = output_limit.unwrap_or(ANTHROPIC_FALLBACK_OUTPUT_LIMIT);
    ANTHROPIC_MAX_BUDGET.min(limit.saturating_sub(1)).max(ANTHROPIC_HIGH_BUDGET)
}
```

Where the output limit comes from:

- Add `Capabilities.max_output_tokens: Option<u32>` now.
- Populate it from OpenCode provider model metadata if available in `opencode.json` / `/global/config` (`provider.<id>.models.<id>.limit.output` or equivalent value object).
- For native `config.yaml` providers, there is no model limit field today, so use fallback constants. This is explicitly approximate but bounded and backward-compatible.

Anthropic encoder rule in `crates/yaca-provider/src/anthropic.rs`:

- If `AnthropicThinking::Disabled`, omit `thinking`.
- If `Budget { budget_tokens }`, set `thinking:{type:"enabled",budget_tokens}`.
- If `Adaptive { effort }`, require a failing encoder test that asserts the exact OpenCode-backed Anthropic wire object. If the implementer cannot source that contract, do not emit adaptive; normalize to `Budget` via the bounded helper instead. This fail-closed rule avoids production 400s on undocumented request bodies.
- Ensure `max_tokens > budget_tokens` for budget mode. If the user requested lower `max_output_tokens`, raise it to `budget_tokens + 1`, not `budget + 4096`.

Google encoder rule in `crates/yaca-provider/src/google.rs`:

- Gemini 2.5 uses `thinkingConfig:{thinkingBudget}`.
- Gemini 3 uses `thinkingConfig:{thinkingLevel}`.
- Include `includeThoughts:true` if OpenCode parity requires it for thought parts from Google.

OpenAI encoder rule in `crates/yaca-provider/src/openai.rs`:

- Use `effort.openai_wire_effort(model_id)`.
- `Max` must never reach `reasoning_effort`.
- `None` omits `reasoning_effort`.

### 1.5 OpenCode config and variant resolution

Add a focused resolver module:

**Create:** `crates/yaca-server/src/opencode/reasoning_options.rs`

```rust
use std::collections::BTreeMap;
use serde_json::{Map, Value};
use yaca_provider::ReasoningEffort;
use yaca_proto::ModelRef;

pub(super) struct ReasoningSources<'a> {
    pub(super) config: &'a Value,
    pub(super) model: &'a ModelRef,
    pub(super) agent_variant: Option<&'a str>,
    pub(super) agent_options: &'a BTreeMap<String, Value>,
}

pub(super) fn resolve_reasoning(sources: ReasoningSources<'_>) -> Option<ReasoningEffort>;
```

Resolver responsibilities:

1. Split `ModelRef` into `(provider_id, model_id, model_variant)` using existing `model_ref::model_ref_parts()`.
2. Read `provider.<provider_id>.models.<model_id>.options` from `st.global.config().await` first.
3. Read `provider.<provider_id>.models.<model_id>.variants.<variant>` where `variant = model_variant.or(agent_variant)`.
4. Merge into a temporary `Map<String, Value>` in this order:
   - model `options`
   - agent `options`
   - selected variant bundle
5. Ignore variant bundle if it has `disabled:true`.
6. Extract a typed effort from OpenCode-compatible option keys:
   - direct string keys: `reasoningEffort`, `reasoning_effort`, `effort`, `reasoning.effort`
   - Anthropic: `thinking.type`, `thinking.effort`, `thinking.budgetTokens`, `thinking.budget_tokens`
   - Google: `thinkingConfig.thinkingLevel`, `generationConfig.thinkingConfig.thinkingLevel`, `thinkingBudget`, `thinkingConfig.thinkingBudget`
7. If no option-derived effort exists, parse selected variant name itself (`max`, `high`, etc.).
8. Return `Some(ReasoningEffort::None)` for explicit `none`; return `None` when no reasoning signal exists.

This module deliberately parses only reasoning-related keys. Generic provider-option pass-through remains out of scope from the PRD, except for reasoning fields needed by OpenCode variants.

### 1.6 Agent variant -> reasoning on the native path

The native path is `hya -> hya-yaca -> in-process yaca-server OpenCode endpoints -> yaca-core -> yaca-provider`, so resolution must happen in the OpenCode server agent preparation layer, not in the TUI renderer.

Modify `crates/yaca-server/src/opencode/reference.rs`:

```rust
pub(in crate::opencode) async fn agent_with_guidance(st: &ServerState) -> AgentSpec;
pub(in crate::opencode) async fn session_agent_with_guidance(
    st: &ServerState,
    session: SessionId,
) -> AgentSpec;

fn apply_agent_entry(
    agent: &mut AgentSpec,
    entry: &agent_catalog::AgentEntry,
    config: &Value,
);
```

Behavior:

- When an agent entry is selected, copy `entry.prompt`, `entry.name`, `entry.model` if present, and resolve `entry.variant`/`entry.options` into `agent.reasoning`.
- If `entry.model` is present and `entry.variant` is present, construct `ModelRef` as `model#variant` only for OpenCode display/session metadata if needed; actual reasoning should come from `resolve_reasoning()` so provider encoders never depend on the suffix after `HttpProvider::stream()` strips it.
- Preserve existing guidance appending.

Modify `crates/yaca-app/src/runtime.rs::agent_with_model(model: &str) -> AgentSpec` minimally:

- Keep `reasoning: None` for plain model ids.
- If `model` already contains `#variant`, parse the variant into `ReasoningEffort` as a native fallback. This covers direct native model selection such as `12th/claude-opus-4-8#max` before any agent catalog override.
- Do not read workdir `opencode.json` here; that would duplicate server config parsing.

### 1.7 Request flow and provider normalization

Modify `crates/yaca-core/src/engine/turn/messages.rs` only to keep forwarding `agent.reasoning`. No projection/event changes are needed.

Modify `crates/yaca-provider/src/router.rs`:

```rust
if let Some(caps) = provider.capabilities(&req.model) {
    crate::preflight(&caps, &req)?;
    if !caps.reasoning_request {
        req.reasoning = None;
    } else if let Some(effort) = req.reasoning {
        let bare_model = provider.served/bare model helper or new Provider::model_id_for(&req.model);
        req.reasoning = effort.normalize_for(provider.family(), &bare_model);
    }
}
```

Because `served_model_id()` is currently private to `HttpProvider`, add a trait method:

```rust
fn served_model_id(&self, model: &ModelRef) -> Option<String>;
```

Implement it for `HttpProvider`, `DevProvider`, and `FakeProvider`. This keeps provider normalization before the HTTP provider overwrites `req.model` with the bare id.

---

## 2. Backward Compatibility + Edge Cases

| Case | Expected behavior | Implementation point | Tests |
| --- | --- | --- | --- |
| No variant, no options | unchanged; no thinking/reasoning field | `resolve_reasoning()` returns `None`; `agent_with_model()` plain id keeps `None` | backward-compat provider encode test |
| Explicit `none` | disable reasoning even if weaker defaults exist | merge parser returns `Some(ReasoningEffort::None)` | unit test with model options high + variant none |
| Unknown variant string | ignore variant bundle; parse falls back to no reasoning unless agent options specify effort | `resolve_reasoning()` skips unknown disabled/missing variants | unit test |
| OpenAI `max` | clamp to `xhigh` or model's highest supported effort; never emit `max` | `openai_wire_effort()` | AC2 test |
| Model without reasoning request capability | omit reasoning entirely | `ProviderRouter::stream()` clears `req.reasoning` before provider stream | router/fake provider test |
| Config.yaml-only user | unchanged; no OpenCode variants loaded | `yaca-app::config` untouched except fallback parse of `#variant` | runtime/config test |
| Sonnet with no variant | unaffected | no configured reasoning signal | AC5 test |
| Anthropic `max` with no output limit | use fallback output limit 32_000 and budget 31_999 | `bounded_anthropic_max(None)` | budget unit test |
| Anthropic `max` with `max_output_tokens <= budget` | raise `max_tokens` to `budget + 1` | `anthropic.rs` | encoder test |
| Gemini 3 | emit `thinkingLevel`, not `thinkingBudget` | `google_thinking()` + `google.rs` | Google encoder test |
| `variants.<name>.disabled=true` | skip selected variant and leave model/agent options in force | `resolve_reasoning()` | resolver test |
| ModelRef already contains `#variant` and agent also has `variant` | session/model variant wins because it is explicit user/session selection; agent variant is default | `resolve_reasoning()` variant precedence | server resolver test |

---

## 3. Execution Plan

### Task 1: Expand typed reasoning vocabulary and validity helpers

**Acceptance criteria:** AC2, AC6

**Files:**

- Modify: `crates/yaca-provider/src/lib.rs`
- Test: `crates/yaca-provider/tests/conformance.rs`

**Interfaces produced:**

- `ReasoningEffort::{None, Minimal, Low, Medium, High, XHigh, Max}`
- `ProviderFamily`
- `ReasoningEffort::parse`, `as_str`, `normalize_for`, `openai_wire_effort`, `anthropic_thinking`, `google_thinking`

Steps:

1. Red: add tests for parsing `none|minimal|low|medium|med|high|xhigh|max`, and rejecting unknown strings.
2. Red: add tests for OpenAI `Max -> XHigh`, OpenAI never returns `Max`, Anthropic classic `High/Max`, Anthropic adaptive `Low..Max`, Google 2.5 budget, and Gemini 3 level mode.
3. Green: extend `ReasoningEffort` and add helper enums/constants.
4. Run: `cargo test -p yaca-provider conformance::reasoning_effort_vocab -- --nocapture` or exact test names added.
5. Run: `cargo fmt --all --check`.

### Task 2: Normalize provider request encoding

**Acceptance criteria:** AC2, AC3, AC6

**Files:**

- Modify: `crates/yaca-provider/src/lib.rs`
- Modify: `crates/yaca-provider/src/router.rs`
- Modify: `crates/yaca-provider/src/http.rs`
- Modify: `crates/yaca-provider/src/anthropic.rs`
- Modify: `crates/yaca-provider/src/openai.rs`
- Modify: `crates/yaca-provider/src/google.rs`
- Modify if needed: `crates/yaca-provider/src/dev.rs`, `crates/yaca-provider/src/fake.rs`
- Test: `crates/yaca-provider/tests/conformance.rs`
- Test: `crates/yaca-provider/tests/multiprovider.rs`

**Interfaces produced:**

- `Provider::family(&self) -> ProviderFamily`
- `Provider::served_model_id(&self, model: &ModelRef) -> Option<String>`
- `Capabilities::max_output_tokens: Option<u32>`

Steps:

1. Red: add encoder tests asserting:
   - OpenAI `ReasoningEffort::Max` emits `reasoning_effort:"xhigh"` or model-highest equivalent, never `max`.
   - Anthropic `Max` emits `thinking.budget_tokens == 31_999` when output limit is fallback.
   - Anthropic `High` emits `16_000`.
   - Google 2.5 Pro `Max` emits `32_768`; Google 2.5 Flash `Max` emits `24_576`.
   - Gemini 3 emits `thinkingLevel`, not `thinkingBudget`.
2. Red: add router test with a fake provider/model where `reasoning_request=false`; assert the encoded/request-observed reasoning is `None`.
3. Green: add family/model-id methods to providers and normalize in `ProviderRouter::stream()`.
4. Green: update each provider encoder to use provider-specific helper output.
5. Run: `cargo test -p yaca-provider --test conformance reasoning -- --nocapture`.
6. Run: `cargo test -p yaca-provider --test multiprovider reasoning -- --nocapture`.

### Task 3: Add OpenCode reasoning option resolver

**Acceptance criteria:** AC3, AC4, AC5, AC6

**Files:**

- Create: `crates/yaca-server/src/opencode/reasoning_options.rs`
- Modify: `crates/yaca-server/src/opencode/mod.rs`
- Test: `crates/yaca-server/tests/opencode_reasoning_variants_api.rs` or `crates/yaca-server/tests/opencode_agent_config_api.rs`

**Interfaces produced:**

```rust
pub(super) struct ReasoningSources<'a> { ... }
pub(super) fn resolve_reasoning(sources: ReasoningSources<'_>) -> Option<ReasoningEffort>;
```

Steps:

1. Red: unit/integration tests for `provider.<id>.models.<id>.options` and `.variants.max` returning `ReasoningEffort::Max`.
2. Red: test merge precedence: model `options.high` + agent option `low` + selected variant `max` resolves to `Max`.
3. Red: test `disabled:true` variant is ignored.
4. Red: test `reasoningEffort: xhigh` in agent unknown keys resolves to `XHigh`.
5. Green: implement JSON object navigation and deep merge helpers in `reasoning_options.rs`.
6. Green: implement extraction for direct, Anthropic, OpenAI, and Google option key shapes.
7. Run: `cargo test -p yaca-server opencode_reasoning_variants -- --nocapture`.

### Task 4: Wire selected agent/model variant into `AgentSpec.reasoning`

**Acceptance criteria:** AC1, AC4, AC5, AC6

**Files:**

- Modify: `crates/yaca-server/src/opencode/reference.rs`
- Modify if needed: `crates/yaca-server/src/opencode/session_v2.rs`
- Modify: `crates/yaca-app/src/runtime.rs`
- Test: `crates/yaca-server/tests/opencode_reasoning_variants_api.rs`
- Test: `crates/yaca-app/src/runtime.rs` unit tests if runtime tests live inline

Steps:

1. Red: create a test workdir with `.opencode/agent/ultraworker.md` containing `model: 12th/claude-opus-4-8` and `variant: max`; create/send a session under that agent; assert the prepared `AgentSpec` has `reasoning == Some(Max)`.
2. Red: test selected agent with no variant leaves `reasoning == None`.
3. Red: test direct model `12th/claude-opus-4-8#max` passed to `agent_with_model()` yields `Some(Max)` as fallback.
4. Green: add `apply_agent_entry()` in `reference.rs` to copy model/variant/options and call `resolve_reasoning()`.
5. Green: update `session_v2.rs` only if session creation must store selected agent model/variant before the first prompt. Keep session model override precedence explicit.
6. Green: update `runtime.rs::agent_with_model()` to parse `#variant` only; do not duplicate workdir config loading there.
7. Run: `cargo test -p yaca-server opencode_reasoning_variants -- --nocapture`.
8. Run: `cargo test -p yaca-app runtime -- --nocapture` if a targeted test exists; otherwise rely on workspace tests.

### Task 5: Preserve backward compatibility and update existing literals/tests

**Acceptance criteria:** AC5, AC6

**Files:**

- Modify only if compile requires: `crates/yaca-core/src/category.rs`
- Modify only if compile requires: all tests constructing `AgentSpec` literals under `crates/yaca-core/tests/**` and `crates/yaca-server/tests/**`
- Test: existing workspace tests

Steps:

1. Red: add/adjust backward-compat test where config has no variants and request body has no `thinking`, `reasoning_effort`, or `thinkingConfig`.
2. Green: if new struct fields were added, update literals with `reasoning: None` / default fields. Prefer not adding `AgentSpec` fields unless Task 3 proves typed extraction is insufficient.
3. Run: `cargo test -p yaca-core`.
4. Run: `cargo test -p yaca-server`.

### Task 6: End-to-end/manual verification for AC1

**Acceptance criteria:** AC1, AC6

**Files:**

- No source files unless tests reveal a wiring defect.
- Evidence captured in Trellis task notes/progress, not committed source.

Steps:

1. Start `hya`/native path in tmux with config containing the default `Sisyphus - ultraworker` agent (`claude-opus-4-8`, `variant:max`).
2. Send a reasoning-forcing prompt such as: “Think step by step internally, then answer with one sentence.”
3. Confirm in TUI that a Thinking/reasoning part renders.
4. Inspect event log/session stream for `type=reasoning` or canonical `Event::Reasoning*` parts.
5. Run full gate:
   - `cargo fmt --all --check`
   - `cargo clippy --workspace --all-targets -- -D warnings`
   - `cargo test --workspace`

---

## 4. Test Plan Mapped to Acceptance Criteria

| Test | File | Covers | Expected result |
| --- | --- | --- | --- |
| `reasoning_effort_parses_opencode_vocab` | `crates/yaca-provider/tests/conformance.rs` | AC2 | `none,minimal,low,medium,med,high,xhigh,max` parse; unknown returns `None` |
| `openai_reasoning_never_emits_max` | `crates/yaca-provider/tests/conformance.rs` | AC2 | OpenAI request for `Max` emits `xhigh` or highest valid model effort, never `max` |
| `anthropic_high_and_max_budgets_match_opencode` | `crates/yaca-provider/tests/conformance.rs` | AC2, AC3 | `high=16000`, `max=31999` bounded by output limit |
| `google_25_and_3_use_correct_thinking_shape` | `crates/yaca-provider/tests/conformance.rs` | AC2 | 2.5 uses `thinkingBudget`; 3 uses `thinkingLevel` |
| `router_drops_reasoning_when_model_has_no_reasoning_request` | `crates/yaca-provider/tests/multiprovider.rs` | AC5 | fake non-reasoning model sees no reasoning |
| `opencode_model_variant_bundle_resolves_reasoning` | `crates/yaca-server/tests/opencode_reasoning_variants_api.rs` | AC3 | `provider.12th.models.claude-opus-4-8.variants.max` produces `ReasoningEffort::Max` and Anthropic body budget 31999 |
| `agent_frontmatter_variant_sets_agent_spec_reasoning` | `crates/yaca-server/tests/opencode_reasoning_variants_api.rs` | AC4 | `.opencode/agent/*.md` `variant:max` sets `AgentSpec.reasoning=Some(Max)` |
| `agent_unknown_reasoning_effort_option_is_honored` | `crates/yaca-server/tests/opencode_reasoning_variants_api.rs` | AC4 | `reasoningEffort:xhigh` in frontmatter extra/options resolves to `XHigh` |
| `no_variant_no_thinking_backward_compat` | `crates/yaca-provider/tests/conformance.rs` and server runtime test | AC5 | no reasoning fields emitted |
| manual tmux native `hya` run | Trellis progress evidence | AC1 | visible Thinking part + event log `type=reasoning` |
| full workspace gate | terminal | AC6 | fmt/clippy/tests exit 0 |

---

## 5. Risks & Mitigations

1. **Risk: generic OpenCode option bundles exceed typed `ReasoningEffort`.**
   - Mitigation: for this task, parse only reasoning-related keys into a typed effort and explicitly leave generic option pass-through out of scope. If a variant has non-reasoning fields, keep them ignored and document that this task is reasoning parity, not full provider-option parity.

2. **Risk: OpenAI model subsets drift.**
   - Mitigation: centralize pattern matching in one helper and default to safe clamp/drop. Never let `Max` through OpenAI wire.

3. **Risk: Anthropic output limit unavailable on native config path.**
   - Mitigation: introduce `Capabilities.max_output_tokens`; populate from OpenCode config when present; use `32_000` fallback and `31_999` budget when absent.

4. **Risk: resolution occurs too late after `HttpProvider` strips `#variant`.**
   - Mitigation: resolve in `reference.rs` / `runtime.rs` before `CompletionRequest`, and normalize in `router.rs` before `HttpProvider::stream()` rewrites model id.

5. **Risk: selected session model and selected agent variant conflict.**
   - Mitigation: define precedence as session/model variant first, agent variant second. Add a test.

6. **Risk: many `AgentSpec` literals break if fields are added.**
   - Mitigation: avoid new `AgentSpec` fields unless typed extraction is insufficient. If fields are required, add an `AgentSpec::new_for_tests()` or builder in `yaca-core` before mechanical test updates.

7. **Risk: `ReasoningEffort::None` inside `Option` is confusing.**
   - Mitigation: document semantics in `lib.rs`: outer `Option` = configured/unconfigured, inner `None` variant = explicit OpenCode disable.

---

## 6. Open Questions

1. **Should this task implement generic provider-option pass-through?** The PRD says non-reasoning options are out of scope, but OpenCode variants are generic option bundles. This plan recommends parsing only reasoning-related keys to keep scope bounded while satisfying the reasoning ACs.

2. **What exact model ids should count as Anthropic adaptive?** Use known current patterns (`opus-4-7+`, `opus-4-8+`, `sonnet-4-6+`) and keep the helper isolated for future updates.

3. **Where should OpenCode provider model `limit.output` be represented?** This plan uses `Capabilities.max_output_tokens`. If the team wants richer model metadata later, extend `ProviderModel` rather than adding ad hoc config reads to encoders.

4. **Should explicit `variant:none` be visible in the model label?** Existing model ref display can keep `#none`; runtime should simply suppress reasoning.

5. **Do Google thought parts require `includeThoughts:true` for all reasoning levels?** The provider encoder should match OpenCode once verified against live Gemini responses.

---

## 7. Self-Review

- Spec coverage: AC1–AC6 are each mapped to at least one task/test.
- Placeholder scan: no `TBD`/`TODO`/“write tests for above” placeholders remain.
- Type consistency: `ReasoningEffort`, `ProviderFamily`, `AnthropicThinking`, `GoogleThinking`, and `resolve_reasoning()` are named consistently across tasks.
- Scope discipline: source changes are limited to provider normalization, OpenCode reasoning resolution, runtime fallback, and tests.

---

## 8. Plan Review

### Round 1 — Claude Opus 4.7 — VERDICT: FAIL

```text
D1 PASS: AC1-AC6 each mapped to tasks/tests in §3 and §4 [plan-draft-deep.md:308-470]
D2 PASS: `ReasoningEffort`, `ProviderFamily`, `AnthropicThinking`, `GoogleThinking`, `resolve_reasoning` signatures used consistently across §1.1, §1.2, §1.5, §1.7 [plan-draft-deep.md:39-72,98-124,190-206]
D3 PASS: Files and method targets named (e.g. `served_model_id` trait lift at http.rs:169 → trait method) and pseudocode glitch in router snippet is disambiguated by the trait addition below it [plan-draft-deep.md:271-283]
D4 PASS: Backward-compat + edge-case matrix in §2 covers no-variant, explicit `none`, unknown variant, OpenAI `max` clamp, no-reasoning-capability model, native config.yaml-only users, model#variant vs agent variant precedence [plan-draft-deep.md:289-302]
D5 PASS: Verification gate `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` declared globally and rerun in Task 6 [plan-draft-deep.md:18,449-452]
D6 FAIL: `AnthropicThinking::Adaptive` wire shape `thinking:{type:"adaptive",effort:effort.as_str()}` is not a documented Anthropic Messages API field; Anthropic only accepts `thinking:{type:"enabled",budget_tokens}` (cf. existing encoder noted at §Current State [plan-draft-deep.md:25]). Implementing as written will 400 on opus-4-7/4-8/sonnet-4-6 paths -> drop the adaptive variant or cite the Anthropic doc/OpenCode source that proves the wire shape exists; otherwise map `low..max` to `type:"enabled",budget_tokens` via the §1.4 bounded helper [plan-draft-deep.md:64-65,137,171]
VERDICT: FAIL
```

**Fix applied:** §1.1, §1.3, and §1.4 now make Anthropic adaptive source-gated and fail-closed to bounded budget mode unless a red encoder test verifies the exact upstream wire shape.

### Round 2 — Claude Opus 4.7 — VERDICT: PASS

```text
D1 PASS: AC1–AC6 each mapped to tasks/tests in [plan-draft-deep.md:459-473]; AC1 manual tmux verification in Task 6, AC2/AC5 in conformance tests, AC3/AC4 in opencode_reasoning_variants_api, AC6 via §0 gate command.
D2 PASS: Concrete signatures for `ReasoningEffort`, `ProviderFamily`, `AnthropicThinking`, `GoogleThinking`, `resolve_reasoning`, `Provider::served_model_id`, `Provider::family` provided [plan-draft-deep.md:39-87,100-126,195-208,278-286]; trait additions named for all three impls (HttpProvider/DevProvider/FakeProvider).
D3 PASS: §4 test table names ten concrete tests with file paths, AC coverage, and expected outcomes [plan-draft-deep.md:461-473]; each Task lists red→green→run commands.
D4 PASS: §2 backward-compat matrix enumerates 12 edge cases including no-variant default, explicit `none`, unknown variant, OpenAI `max` clamp, non-reasoning models, Anthropic `max` with absent/lower output limit, Gemini 3 vs 2.5, `disabled:true`, and session-vs-agent variant precedence [plan-draft-deep.md:292-305].
D5 PASS: Resolver lives in `crates/yaca-server/src/opencode/reasoning_options.rs` reusing `model_ref_parts` and `st.global.config()`; provider normalization stays in `ProviderRouter::stream` before `HttpProvider::stream` rewrites the model id [plan-draft-deep.md:200-208,267-286]; no engine/projection changes per §1.7 line 264; matches `AgentEntry` (super-visible at crates/yaca-server/src/opencode/agent_catalog.rs:10) and existing `AgentSpec.reasoning` field at crates/yaca-core/src/engine.rs:43.
D6 PASS: Each task has red/green/run TDD steps with file paths and exact test commands [plan-draft-deep.md:326-456]; the §1.7 pseudocode "provider.served/bare model helper" is disambiguated by the explicit `fn served_model_id(&self, model: &ModelRef) -> Option<String>` trait addition immediately below at line 282 and is provably `Some` here since `capabilities()` already gated on it (crates/yaca-provider/src/http.rs:193-195).
VERDICT: PASS
```
