# Progress — reasoning-variants

## Task 0 — assumptions (verified)
- Seam `session_agent_with_guidance` on native hya prompt path: session_prompt.rs:113,155 (+ legacy). ✓
- `st.global.config()` is EMPTY/runtime-PATCH-only (global.rs:23,31) → resolver reads opencode.json from DISK (config_paths + jsonc). ✓
- `HttpProvider::new` sets `reasoning_request: true` (http.rs:117) → reasoning not stripped for 12th-anth. ✓
- Anthropic wire: POST {base}/messages, AuthStyle::Anthropic (x-api-key + anthropic-version 2023-06-01) (http.rs:86-93,133-143). ✓

## Task 2.5 — EARLY AC1 gateway smoke — PASS (2026-06-24)
Direct Anthropic Messages call to 12th.day with yaca's exact wire format:
`POST https://api.12th.day/v1/messages`, headers `x-api-key` + `anthropic-version: 2023-06-01`,
body `thinking:{type:"enabled",budget_tokens:16000}`, model `claude-opus-4-8`.

**Result:** response `content` block types = `['thinking', 'text']`, `error: None`.

=> claude-opus-4-8 HONORS `enabled+budget_tokens` (returns a thinking block). NO
adaptive thinking needed. Implementation proceeds.

## Implementation (Tasks 1-6) — DONE + verified
- Task 1+2 (yaca-provider): `ReasoningEffort` = Off/Minimal/Low/Medium/High/XHigh/Max;
  `openai_label`(Max→xhigh, never max), `anthropic_budget`(High=16000,Max=31999),
  `google_budget(model_id)`(2.5-pro Max=32768 else 24576); encoders use Option methods;
  conformance budgets updated. Unit tested (AC2). fmt+clippy green.
- Task 3 (yaca-server): `reasoning_options::{load_compat_config(DISK), resolve_reasoning}`
  + `json_merge` extracted; `config_paths` pub(super). Unit + FILE-BACKED tests (AC3/AC5).
- Task 4: `apply_agent_entry` pure helper wired into `agent_with_guidance` +
  `session_agent_with_guidance` (entry lookup widened). Internal test (AC4).
- Delegated Task 3+4 to trellis-implement; VERIFIED file-by-file. REVERTED its
  unrelated `skill_catalog.rs` change (scope-creep gaming a brittle env-dependent test;
  the 2 `compat_instance_api` failures are ENVIRONMENTAL — global `~/.config/yaca/skills`
  pollute an index-based skill assertion; proven by clean-HOME pass).
- Gate: fmt clean; clippy --workspace clean; `test --workspace` 185 ok / 0 fail (clean HOME).

## Live E2E trace (instrumented, then reverted) — wiring CORRECT
`apply variant=Some("max") resolved=Some(Max)` → `session_agent reasoning=Some(Max)`
→ `anthropic encode model=claude-opus-4-8 reasoning=Some(Max) thinking_set=true`.
So the request to the gateway carries `thinking{type:enabled,budget_tokens:31999}`. ✓

## CRITICAL FINDING — AC1 visible render blocked by GATEWAY (not yaca)
The 12th.day gateway REDACTS thinking content: non-streaming returns a `thinking`
block with `thinking_len=0` (empty), streaming sends `content_block_start(thinking)`
+ `signature_delta` but NO `thinking_delta`. The model thinks (signature proves it)
but the gateway withholds the readable content. So nothing streams to render.
=> Original "reasoning doesn't render" had 3 layers: (a) variant:max not enabling
   thinking [FIXED], (b) Anthropic decoder dropped thinking blocks [FIXED], (c) gateway
   redacts thinking content [GATEWAY limit — unfixable in yaca].

## Decoder fix (yaca-provider/anthropic/decoder.rs) — BEYOND original plan scope
Plan §8 assumed render worked once parts produced; it did NOT (decoder only handled
text/tool_use, dropped `thinking`/`thinking_delta`). Added `BlockKind::Reasoning` +
`thinking`/`thinking_delta`/`content_block_stop` handling → emits ReasoningStart/Delta/End.
Verified by unit test `anthropic_decodes_thinking` (E2E blocked by gateway redaction).
A standard Anthropic provider that exposes thinking content WOULD render reasoning.
