# Implement: Per-model reasoning variants with OpenCode parity

Execution plan for `design.md`. TDD (red→green) where feasible. Verification gate
after each task: `cargo test -p <crate>`; full gate before finish. Library crates
deny `unwrap_used`/`expect_used` (tests `#![allow(...)]`).

## Task 0 — Verify load-bearing assumptions (no code) [pre-flight]

- Confirm `session_agent_with_guidance` is on the native `hya` prompt path
  (DONE: `session_prompt.rs:113,155`). Spot-confirm the in-process bridge hits
  `/session/{id}/...prompt` → `session_prompt.rs`.
- Read `crates/yaca-provider/src/anthropic.rs` current `max_tokens` bump and
  `crates/yaca-provider/tests/conformance.rs:366` to confirm the High=16000 budget
  keeps `max_tokens > 16384`.
- Confirm `HttpProvider::new` sets `reasoning_request: true` (`http.rs:117`).
- Confirm `model_ref::model_ref_parts` / `split_variant` signatures for the resolver.
- VERIFIED: `st.global.config()` starts EMPTY and is runtime-PATCH-only
  (`global.rs:23,31`) → the resolver reads opencode.json from DISK via
  `config_paths(workdir)` + `jsonc::from_str::<Value>` (with `location::workdir(st)`),
  NOT from `global.config()`.
> Output: a 5-line note in `progress.md`. If any assumption is false, return to
> design before coding.

## Task 1 — Extend `ReasoningEffort` vocabulary + per-provider methods  [AC2, AC6]

Files: `crates/yaca-provider/src/lib.rs` (+ tests in same file or
`crates/yaca-provider/tests/reasoning_levels.rs` new).

1. RED: tests — `parse` accepts none/off,minimal,low,medium,med,high,xhigh,max and
   rejects junk; `openai_label(Max,"gpt-5.5")=="xhigh"` and never "max"; `anthropic_budget`:
   High=16000, Max=31999, Off/Minimal=None; `google_budget("gemini-2.5-pro",Max)=32768`,
   `google_budget("gemini-2.5-flash",Max)=24576`, High=16000, Low/Medium=None.
2. GREEN: rewrite the enum (7 variants `Off..Max`), `parse`, `as_str`,
   `openai_label(model_id)`, `anthropic_budget`, `google_budget(model_id)`. Keep
   them `Option`-returning. No serde derives.
3. `cargo test -p yaca-provider`; `cargo check -p yaca-core` (it imports the enum).

## Task 2 — Switch provider encoders to the Option methods  [AC2, AC3, AC6]

Files: `anthropic.rs` (42-47), `openai.rs` (59-60), `google.rs` (180-183);
`crates/yaca-provider/tests/conformance.rs` (update existing High-budget assertions).

1. RED: encoder tests — Anthropic `Max` → body `thinking.budget_tokens==31999` and
   `max_tokens > 31999`; Anthropic `High` → 16000; OpenAI `Max` → `reasoning_effort=="xhigh"`;
   Google 2.5-pro `Max` → `thinkingBudget==32768`. (Drive `Protocol::encode`
   directly so `caps.reasoning_request` is not in the way.)
2. GREEN: edit the three encoders per design §7. Preserve the existing
   `max_tokens` bump (`budget + 4096`, so `conformance.rs:366` `max_tokens > 16384`
   still holds: 16000+4096=20096).
3. GREEN — update the EXISTING assertions that move with the parity budgets (they
   WILL fail otherwise): `conformance.rs:365` `budget_tokens == 16384` → `16000`
   (Anthropic High); `conformance.rs:372` `thinkingBudget == 24576` → `16000`
   (Google High — 24576 is OpenCode's *max*, not *high*). VERIFIED budget literals:
   the enum source `lib.rs:105` (16384) / `:114` (24576) — rewritten in Task 1 —
   plus these two test assertions; dispatcher/harness use `parse`/`as_str`, not budgets.
4. `cargo test -p yaca-provider` (incl. existing `conformance.rs`, `multiprovider.rs`).

## Task 2.5 — EARLY AC1 gateway smoke (de-risk BEFORE building the resolver)  [AC1 risk]

Verify the load-bearing AC1 assumption NOW, before Tasks 3–5: that the 12th.day
gateway honors Anthropic `thinking:{type:"enabled",budget_tokens}` for
`claude-opus-4-8` (design §7 / §11#1 — opus-4-8 is in OpenCode's adaptive family,
so the enabled+budget shape is the risk point).

1. Write a throwaway, `#[ignore]`d integration test (or small script) that builds
   a `CompletionRequest { model: claude-opus-4-8, reasoning: Some(High), .. }` and
   streams it through the real `12th-anth` `HttpProvider` (creds from the user's
   config; never runs in plain CI).
2. Assert the streamed events include a reasoning/thinking part (`Event::Reasoning*`).
3. If ABSENT → STOP. The adaptive-gateway risk is real: report to the user; do NOT
   proceed to the resolver/wiring and do NOT bolt on speculative adaptive. If
   PRESENT → AC1 is de-risked; continue. Record the outcome in `progress.md`.

## Task 3 — Reasoning resolver + DISK opencode.json loader  [AC3, AC4, AC5, AC6]

Files: new `crates/yaca-server/src/opencode/reasoning_options.rs` (BOTH
`load_opencode_config(workdir)` and `resolve_reasoning`); new
`crates/yaca-server/src/opencode/json_merge.rs` (extract `merge_json_value` from
`agent_catalog.rs`, update its callsite); declare both modules in
`crates/yaca-server/src/opencode.rs` (the module root — there is NO `opencode/mod.rs`).
Make `config_paths` `pub(super)` in `agent_sources.rs` (currently private) so
`reasoning_options.rs` can reuse it — do NOT duplicate the 4-path list.
`load_opencode_config` reads `config_paths(workdir)` [+ `~/.config/opencode/opencode.json{,c}`]
via `super::jsonc::from_str::<Value>` and deep-merges (workdir wins); returns `{}` if none.

1. RED (in-memory unit tests — `resolve_reasoning` with a constructed `config: Value`):
   - `provider.12th-anth.models.claude-opus-4-8.variants.max` (thinking.budgetTokens 31999)
     + agent variant "max" → `Some(Max)`.
   - merge precedence: model `options.reasoningEffort:"high"` + agent option
     `reasoningEffort:"low"` + variant "max" → `Max` (variant name wins).
   - `reasoningEffort:"xhigh"` in agent options → `XHigh`.
   - `variants.max.disabled=true` → ignored.
   - empty (no variant/options/`config={}`) → `None` (AC5).
   - explicit `"none"` → `Some(Off)`.
   - bare-id provider scan resolves `claude-opus-4-8` to `12th-anth`.
2. RED — FILE-BACKED test proving the DISK bundle is actually read (NOT the
   variant-name shortcut): in a `tempfile` workdir write `opencode.json` with
   `provider.p.models.m.variants.deep = { "thinking": { "budgetTokens": 31999 } }`
   — variant name "deep" is NOT a level keyword, so step 5a CANNOT supply the
   effort; only the disk bundle can. Then
   `resolve_reasoning(Some("deep"), &{}, &ModelRef("p/m#deep"), &load_opencode_config(dir))`
   → `Some(Max)`; encode an Anthropic request with that effort and assert
   `thinking.budget_tokens == 31999`. Proves disk file → request body. [AC3]
3. GREEN: implement `load_opencode_config` + `resolve_reasoning` per design §4 +
   the extracted deep-merge.
4. `cargo test -p yaca-server reasoning_options`.

## Task 4 — Wire variant → `AgentSpec.reasoning` at the seam  [AC1, AC4, AC5, AC6]

Files: `crates/yaca-server/src/opencode/reference.rs` (shared `apply_agent_entry`
in `agent_with_guidance` + `session_agent_with_guidance`). No `runtime.rs` change —
the `model#variant` direct-selection fallback is out of scope (design §8).

1. RED (INTERNAL unit test in `reference.rs` `#[cfg(test)] mod` — the seam helpers
   are `pub(in crate::opencode)`, unreachable from `crates/yaca-server/tests`):
   - build `AgentEntry { variant: Some("max"), model: Some("<prefixed>/claude-opus-4-8"), .. }`
     + an opencode-config `Value` with the matching `provider.<id>.models.<id>`; call
     the pure `apply_agent_entry(&mut agent, &entry, &model, &config)` and assert
     `agent.reasoning == Some(Max)` (ONE concrete path; no ServerState/fake provider).
   - entry with no variant and no reasoning option → `agent.reasoning == None` (AC5).
   (End-to-end glue through `session_agent_with_guidance` is validated by the AC1
   smoke in Task 2.5 + the Task 6 manual run.)
2. GREEN: implement `apply_agent_entry`; in `session_agent_with_guidance` load
   `config = load_opencode_config(location::workdir(st))` (DISK) and pass it; widen
   the existing entry lookup; set `agent.reasoning` from `resolve_reasoning` with
   session/model-variant precedence.
3. `cargo test -p yaca-server apply_agent_entry` (internal `reference.rs` test).

## Task 5 — Backward-compat sweep + existing literals  [AC5, AC6]

1. RED: backward-compat test — no variant anywhere → encoded body has no
   `thinking`/`reasoning_effort`/`thinkingConfig`.
2. GREEN: fix any `AgentSpec` / `ReasoningEffort` match sites that no longer
   compile (exhaustive matches now have 7 variants): grep `ReasoningEffort::`
   and `match .* reasoning`. Prefer not adding `AgentSpec` fields.
3. `cargo test -p yaca-core && cargo test -p yaca-server`.

## Task 6 — Full gate + manual AC1 verification  [AC1, AC6]

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. Manual (tmux, no-MCP config copy + `HYA_DEBUG_EVENTS=1` for evidence): build
   debug `hya`, run default `Sisyphus - ultraworker` (opus-4-8, `variant: max`) on
   a reasoning-forcing prompt; CONFIRM a `type=reasoning` part appears in the event
   log AND a Thinking block renders in the TUI. Capture evidence to `progress.md`.
   - If no reasoning part: check (a) `req.reasoning` is `Some(Max)` at the encoder,
     (b) the gateway response contains thinking blocks → if absent, this is the
     adaptive-gateway risk (design §7/§11) — STOP and report, do not bolt on
     speculative adaptive.

## Review gates

- After Task 2: provider mapping locked (all downstream depends on it).
- After Task 3: resolver logic locked, independent of wiring.
- After Task 4: integration proven for AC1/AC3/AC4.
- Tasks 5–6 are verification, not new behavior.

## Test → AC map

| Test | AC |
|---|---|
| vocab parse/reject; openai never-max; anthropic high/max budgets; google 2.5 budgets | AC2 |
| FILE-BACKED: real disk opencode.json `variants.deep` bundle → request body budget 31999 (Task 3 step 2) | AC3 |
| agent frontmatter `variant:max` → AgentSpec.reasoning; `reasoningEffort` option honored | AC4 |
| no-variant → no thinking/effort fields (resolver + encoder) | AC5 |
| manual tmux opus-4-8 variant:max renders reasoning part | AC1 |
| `cargo fmt --check && clippy -D warnings && test --workspace` | AC6 |

## Rollback points

Each task is a separate logical commit; `git restore <file>` reverts one task.
New files (`reasoning_options.rs`, `json_merge.rs`, test files) are deletable.
No migrations, no event-schema changes → no data rollback needed.
