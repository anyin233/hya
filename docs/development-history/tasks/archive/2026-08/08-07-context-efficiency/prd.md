# Context efficiency: model-aware thresholds and selective eviction

Child of `08-07-context-management`. **Blocked on `08-07-context-observability`** —
the numbers that make this work measurable are what that task records.

Planning is deliberately deferred. This file records the agreed scope so it is
not lost; `design.md` and `implement.md` are written when the sibling lands.

## Goal

Fix the four context-efficiency gaps found in the audit. Unlike its sibling, every
item here **changes what the model sees**, so it carries a heavier verification
burden.

## Scope

### E1 — Threshold ignores the model's real context window

`CompactionConfig::token_threshold` defaults to a flat `100_000`
(`crates/hya-core/src/compaction.rs:34`). Provider capabilities already carry
`max_context` (`crates/hya-provider/src/lib.rs:109`), but it is only reported to
the catalog API (`crates/hya-server/src/compat/catalog.rs:190`) and never
consulted when deciding to compact. Make the threshold a fraction of the active
model's advertised window.

### E2 — Estimator is `chars / 4`

`estimate_tokens` (`compaction.rs:103`) approximates. Real counts already exist:
the `token_ledger` table records `prompt_tokens` / `completion_tokens` per
session and turn, and assistant messages carry `tokens`. Feed real counts back
into the decision and keep the estimator only as the cold-start fallback.

### E3 — Compaction is all-or-nothing

Today compaction splits at `len - keep_recent` and folds everything before it.
Add selective eviction — for example dropping stale tool outputs while keeping
their calls, which `cap_tool_output` (`crates/hya-tool/src/output_cap.rs`) shows
is the dominant bulk.

### E4 — Cross-agent `AGENTS.md` duplication

Every subagent re-walks and re-renders the same `AGENTS.md` chain into its own
system prompt (`crates/hya-core/src/prompt.rs:32`, `:62`). Share or cache it.

## Constraints

- Every item changes model input. Each needs a before/after behavioral test.
- Preserve the event-sourced architecture; no parallel read-model.
- No new tables unless a design step proves one is unavoidable.

## Acceptance Criteria

- [x] E1 — The compaction threshold is a share of the route's advertised
      `max_context`, with the flat threshold as fallback.
      *`resolved_threshold_scales_to_the_window_and_guards_bad_input` (table:
      absent / zero / normal / out-of-range / floor clamp) and
      `compaction_threshold_scales_to_the_advertised_context_window`, verified
      failing when the window is ignored.*
- [x] E2 — Compaction uses provider-reported token counts when available.
      *`measured_tokens_uses_reported_usage_plus_the_delta_since`,
      `measured_tokens_prefers_the_most_recent_report`, plus the regression
      `measured_tokens_ignores_empty_usage_and_falls_back_to_the_estimator`
      proving unchanged behaviour for routes that never report usage.*
- [x] E3 — Stale tool outputs are evicted before any summarization, and eviction
      alone can avoid the summarizer entirely.
      *`eviction_drops_stale_outputs_keeps_inputs_and_respects_keep_recent`,
      `eviction_is_idempotent`, and
      `tool_output_eviction_avoids_summarizing_and_preserves_the_log`, verified
      failing when eviction is disabled.*
- [x] E4 — `AGENTS.md` discovery is cached and correctly invalidated.
      *`repeat_discovery_does_not_re_read_unchanged_files` (verified failing
      without the cache), plus edit- and new-file-invalidation tests.*
- [x] Observability — eviction is recorded as `Event::ContextEvicted` and is
      request-local; the log keeps full tool output.
- [~] Full gate — `cargo test --workspace --exclude hya-e2e` 1337 passing, E2E 30
      passing, clippy clean on all five crates touched. See residuals R2 and R3.

## Residuals after implementation

- **R1 — E4 was rescoped; the PRD's original claim was wrong.** It said "every
  subagent re-walks and re-renders the same `AGENTS.md` chain into its own system
  prompt". Subagents do **not** re-walk: `guidance_at`
  (`crates/hya-server/src/compat/reference.rs:130`) renders once per top-level
  turn into an `Arc<str>` that is cloned into `MemberSpec.guidance`. The real cost
  was one filesystem walk per top-level turn, so E4 became a caching change. Its
  value is smaller than the PRD implied; the remaining token duplication (each
  agent's prompt contains the guidance) is semantically required.
- **R2 — one pre-existing flaky test.**
  `compat_permission_always_resolves_matching_session_requests`
  (`crates/hya-server/tests/compat_permission_question_api.rs`) failed once under
  full parallel load, then passed 4/4 in isolation and across two further full
  runs. It touches nothing in this task. Not fixed here.
- **R3 — `crates/hya-sdk` still fails `cargo fmt --all --check` and workspace-wide
  clippy** (48 errors), untouched by this task and already failing on `main`.
- **R4 — E2/E3 interaction, found during implementation.** `tokens_in_use` prefers
  the provider-measured count, which describes the transcript *before* any
  request-local edit. Naively re-measuring after eviction reported no saving and
  sent every turn to the summarizer anyway. The turn loop now carries one running
  count and applies the eviction saving as an estimated delta; `fold_prefix` exists
  precisely so the already-made decision is not re-derived from a stale measurement.

## Dependency note

Do not start before `08-07-context-observability` lands. Without `ContextCompacted`
there is no way to measure whether any change here helps or hurts.
*Satisfied: that task shipped as 0.34.15.*
