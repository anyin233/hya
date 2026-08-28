# ADR 0013: User-assembled agent workflows (DAG over team primitives)

Date: 2026-08-26
Status: Accepted

## Context

hya already spawns bounded subagent teams (`MemberSpec` + `run_team`, governor
caps in `SubagentLimits`), but compositions are decided ad hoc by the model on
each `task` call. There is no way for a user to declare "run this stage graph"
reusably. Argus-style harnesses solve this with a fixed preset pipeline
(plan→impl→review), which hya deliberately rejects: preset combinations
hardcode one team topology for all tasks.

## Decision

1. Workflows are **user-authored files** (markdown+frontmatter or YAML) stored
   under `<workdir>/.hya/workflows` or `$HOME/.config/hya/workflows`
   (first-name-wins, mirroring skill discovery). hya ships **zero** built-in
   workflows.
2. A workflow is a **DAG of stages**, not a linear pipeline:
   - `needs:` edges define ordering and fan-out; stages whose dependencies are
     satisfied at the same topological level run as ONE parallel member batch.
   - Fan-in is explicit: a consuming stage's prompt template references
     upstream outputs via `{{stage_id}}` placeholders; ordering within the join
     is declaration order; per-workflow `on_member_failure`
     (`fail_fast` | `collect_all`) declares the partial-failure contract.
   - `mode: loop` stages iterate through the existing `IterationDriver` with an
     independent verifier agent — never a second loop implementation.
3. Execution reuses the team/governor path (`pre_admit_team` /
   `run_pre_admitted_team`); user DAGs cannot bypass max-depth, concurrency, or
   per-run spawn budgets.
4. Stage agents resolve through the caller's `can_spawn` authorization in
   `AgentCatalog` / `TurnBinding::resolve_spawn`; no new identity namespace.

## Consequences

- Model-decided batches remain available; workflows add deterministic,
  reusable composition on top without replacing the task tool.
- Because nothing is preset, out-of-the-box behavior is unchanged; users opt in
  by authoring files.
- Fan-in outputs are bounded text sections, so upstream transcripts stay out of
  the joining context beyond the declared cap.
