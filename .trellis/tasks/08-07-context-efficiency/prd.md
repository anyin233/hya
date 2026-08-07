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

- [ ] TBD at planning time. At minimum, each of E1–E4 must have a test proving
      the new model input is correct, and a measurement showing the change is an
      improvement — using the `ContextCompacted` numbers recorded by the sibling
      task.

## Dependency note

Do not start before `08-07-context-observability` lands. Without `ContextCompacted`
there is no way to measure whether any change here helps or hurts.
