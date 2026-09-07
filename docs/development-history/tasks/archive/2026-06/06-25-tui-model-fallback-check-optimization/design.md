# Design: TUI model fallback and `tui-check` optimization

## Status

- Phase: merged planning draft.
- Inputs merged: conservative/minimal Oracle planner and failure-mode/deep planner.
- Execution gate: no implementation until this plan passes review and the user approves the scope split.

## Goal

1. In yaca, make direct native TUI `/model <id>` fail safely when the requested model is not a valid catalog entry: show a visible system message and preserve the last valid/current model state.
2. For terminal visual QA, stop treating multiple valid independent frames as a `borderMisaligned` failure while preserving detection of genuinely malformed boxes.

## Current facts

- `crates/yaca-cli/src/tui/controller.rs::dispatch_slash` currently mutates `self.app.model` before validating a direct `/model <arguments>` command.
- On miss, that branch synthesizes `ModelEntry { provider: "", id: arguments, reasoning_variants: [] }`, assigns it to `active_model`, and emits `TuiEffect::SelectModel(entry)`.
- `crates/yaca-cli/src/tui.rs` handles `TuiEffect::SelectModel(entry)` by converting the entry to `entry.model_ref()`, assigning `agent.model`, switching the engine model, and updating session model snapshot state.
- `ModelEntry::matches_model_ref` accepts both bare `id` and provider-prefixed `<provider>/<id>` refs; provider/model identity must not collapse when duplicate ids exist.
- `Controller::set_active_model_by_identity` is used by session resume/hydration, not by failed direct `/model` commands. Its lookup-miss semantics must clear `active_model` on resume misses so stale provider/reasoning metadata from a previous session cannot survive after `controller.app.model` changes to an uncataloged session model.
- The durable `tui-check` source is upstream `oh-my-openagent`, path `packages/shared-skills/skills/visual-qa/scripts/tui-grid.ts`; the installed package cache is generated output and must not be treated as the owned source.

## Scope decision

### D1: yaca change in this repository

Implement and verify the safe `/model` behavior in yaca now:

- Validate the requested model ref before mutating controller state.
- Preserve `app.model`, `active_model`, runtime `AgentSpec.model`, reasoning defaults, and session model snapshot on invalid or ambiguous requests.
- Preserve a current `app.model` string even when it is not present in the catalog; failed direct commands must not force a default or synthesize a replacement entry.
- Ensure active-model identity helper semantics are explicit: failed direct `/model` commands do not call the helper and therefore preserve state; session resume/hydration lookup misses clear `active_model` to avoid stale catalog metadata for an uncataloged resumed model.
- Keep `/model` with no argument opening the picker.
- Keep valid provider-prefixed refs such as `/model openai/shared` selecting the matching catalog `ModelEntry`.
- Keep unique bare ids such as `/model gpt-5.5` working.
- Reject ambiguous bare ids when multiple providers expose the same `id`; require the provider-prefixed ref instead.

### D2: upstream `oh-my-openagent` change tracked from this task

Do not patch yaca's installed `visual-qa` package cache. The durable checker fix should be implemented as an upstream patch to `oh-my-openagent`:

- Add tests for valid independent frame layouts that currently false-positive.
- Change `tui-grid.ts` border analysis to evaluate frame-line width consistency per independent frame/row-band group instead of across the whole capture.
- Keep existing overflow, ANSI, wide-character, and malformed-box checks green.

Yaca can keep task fixtures and a spec note to document why a `borderMisaligned` result from the currently installed checker may be non-actionable until upstream is patched and released.

## Planner conflict resolution

| Topic | Conservative planner | Failure-mode planner | Merged decision |
| --- | --- | --- | --- |
| `tui-check` ownership | Split yaca `/model` fix from upstream checker PR. | Same durable-source concern; warn against cache patch. | Use split scope: yaca D1 now, upstream D2 separately, with yaca tracking/spec note only. |
| Direct `/model` implementation shape | Small local validation in `dispatch_slash`; no signature changes. | Add a resolver/error path to catch ambiguity and no-mutation edge cases. | Add a small private resolver/helper only if needed to keep exact-ref/unique-bare/ambiguous-bare behavior testable; do not modify `ModelEntry` or provider routing. |
| Bare duplicate ids | Not emphasized. | Reject ambiguous bare `/model shared`. | Reject ambiguous bare ids because provider identity is a routing contract in `.trellis/spec/frontend/quality-guidelines.md`. |
| Malformed refs | Treat unknown refs as misses. | Flag malformed provider refs. | Do not reject `a/b/c` solely for multiple slashes because model ids may contain slashes. Exact `entry.model_ref()` match takes precedence; otherwise unique bare `entry.id` match is allowed; misses return an error without mutation. |
| Empty/unknown current model | Preserve last valid state. | Preserve even when current model is absent from catalog. | Preserve current `app.model` and current `active_model` exactly on every failed direct command. |

## Model command resolution design

Direct `/model <requested>` should resolve in this order:

1. Exact model ref match: `entry.model_ref() == requested`. This preserves provider-prefixed refs and providerless entries whose ids may contain `/`.
2. Unique bare id match: exactly one catalog entry where `entry.id == requested`.
3. Ambiguous bare id error: more than one catalog entry where `entry.id == requested`.
4. Unknown model error: no exact or unique bare match.

Only successful resolution mutates controller state and emits `TuiEffect::SelectModel(entry)`. Errors emit `TuiEffect::SystemMessage(message)` and leave existing state untouched.

For `set_active_model_by_identity`, the implementation phase found the production caller is session resume/hydration. That path first assigns `controller.app.model = meta.model`; therefore a lookup miss must clear `active_model` and return `None`, rather than preserving a stale catalog entry from the previous session. Failed direct `/model <id>` commands preserve state by resolving before mutation and never invoking this helper on error.

Recommended user-visible messages:

- Unknown: `unknown model '<requested>'; type /model to pick from the list`
- Ambiguous: `model '<requested>' is ambiguous; use one of: <provider>/<id>, <provider>/<id>`

The exact text may be adjusted during implementation, but tests should lock the observable guarantees: system message, offending argument present, no `SelectModel`, no mutation.

## `tui-check` algorithm design

The current checker treats every line containing any box-drawing glyph as one global set of frame widths. The upstream fix should instead derive frame-line groups before comparing widths:

1. Strip ANSI and compute display-column spans for box-drawing glyphs on each line.
2. For each row with box glyphs, record one or more contiguous box-glyph spans and their row index.
3. Group spans into independent frame candidates using row adjacency and overlapping/touching column spans. Separate non-touching panes and vertically separated modal/prompt regions stay in separate groups.
4. Compute width consistency within each group, not globally across all frame rows.
5. Set `borderMisaligned` when any group has inconsistent border/content spans that represent one malformed box.

This keeps the existing malformed CJK-width fixture failing while allowing valid captures with a full-width prompt and a smaller centered dialog to pass.

## Out of scope

- Fuzzy model matching or suggestions beyond ambiguous candidate refs.
- Model catalog refresh, provider routing changes, `ModelEntry` schema changes, or `/think` redesign.
- Patching generated installed package caches for `visual-qa`.
- Changing yaca's terminal rendering to satisfy a false-positive checker unless a manual visual inspection proves the frame is actually malformed.
- Starting `task.py start` or implementation work before the plan review verdict is recorded and the user approves the split scope.

## Multi-deliverable task structure

Recommended Trellis structure after approval:

- Keep this task as the parent planning/coordination record.
- Child D1: yaca native TUI `/model` safe rejection. Dependency: this merged plan and review verdict; independently verifiable by Rust controller/runtime tests and native TUI manual QA.
- Child D2: upstream `oh-my-openagent` `tui-check` frame grouping. Dependency: confirmed upstream source path and yaca fixture evidence; independently verifiable by upstream Bun tests and checker CLI fixture runs.

If child tasks are created, write those dependencies into each child `prd.md`/`implement.md`; do not rely on tree position alone to imply ordering.

## Approval question before execution

Proceed with this split-scope plan?

- D1: implement yaca `/model` safe rejection now in this repository.
- D2: prepare or track the durable `tui-check` fix as an upstream `oh-my-openagent` change rather than editing the installed package cache.

## Plan Review

### Round 1 — Oracle — VERDICT: PASS (superseded)

This review passed the earlier merged draft, but the plan was later revised after collecting the official background planner outputs. Treat this verdict as advisory evidence only; execution remains blocked until the fresh review of the current on-disk plan completes.

```text
D1 PASS - goals split D1 yaca / D2 upstream with explicit out-of-scope and falsifiable acceptance criteria [design.md:9-13,86-90; prd.md:41-62]
D2 PASS - tasks ordered red-tests→resolver→runtime-proof→spec→upstream→gates; each step is 1-3 tool calls [implement.md:41-396]
D3 PASS - cited APIs verified: dispatch_slash [controller.rs:449-468], SystemMessage variant [controller.rs:35], available_models [controller.rs:59], model_ref behavior locked by existing test slash_model_provider_prefixed_selects_matching_provider_entry [controller.rs:1081-1108]; upstream tui-grid.ts ownership documented [evidence.md:46-51]
D4 PASS - named ModelCommandError enum with three error cases and explicit no-mutation contract [design.md:55-71; implement.md:142-188]; pre-existing yaca-server test failure acknowledged so it cannot mask regressions [implement.md:372]
D5 PASS - per-task gate commands present: focused cargo tests [implement.md:122,228], workspace fmt/clippy/test trio [implement.md:367-369], bun test for upstream [implement.md:344], task.py validate [implement.md:292], manual QA steps [implement.md:386-396]; each PRD acceptance criterion maps to a task [prd.md:41-62]
D6 PASS - changes confined to private resolver in controller.rs + spec note; ModelEntry / provider routing edits forbidden [design.md:50,86-90]; tui-check fix routed upstream rather than patching installed cache [design.md:35-44; implement.md:299-352]
VERDICT: PASS
```

### Round 2 — Oracle — VERDICT: PASS

Round 2 reviewed the current revised plan after adding `set_active_model_by_identity` miss semantics, uncataloged-current-model preservation, and multi-deliverable parent/child dependency guidance. This is the active review gate for execution.

```text
D1 PASS: goals D1/D2 explicit and falsifiable [design.md:11-12, prd.md:40-62, out-of-scope at design.md:90-96]
D2 PASS: 7 tasks decomposed into atomic 1–3-call steps with TDD red→green, conditional gating in Task 4 [implement.md:43-128, 131-266, 270-324, 328-350, 392-444]
D3 PASS: cited symbols verified — dispatch_slash branch synthesizing entry [controller.rs:449-467], set_active_model_by_identity miss assignment [controller.rs:232-253], runtime SelectModel→switch_model path [tui.rs:650-665], hydration caller [tui.rs:782-785], ModelEntry::model_ref/matches_model_ref [config.rs:35-54], helpers `with_models_and_sessions` [controller.rs:100] and `model_entry` [controller.rs:912] all present
D4 PASS: explicit unknown/ambiguous error paths with no-mutation contract, latent-bug abort handled in Task 3 caller audit, pre-existing unrelated test failure flagged [implement.md:192-223, 270-314, 465-466]
D5 PASS: each task ends with focused `cargo test` and gates `cargo fmt/clippy/test --workspace` + manual QA; upstream `bun test` for D2 [implement.md:122-127, 228-232, 262-265, 318-322, 437-440, 457-490]
D6 PASS: defers checker to upstream, no ModelEntry/provider/router changes, resolver private, "Out of scope" explicit, audit scoped to PRD-required active_model preservation [design.md:90-96, implement.md:11-19, prd.md:89-92]
VERDICT: PASS
```
