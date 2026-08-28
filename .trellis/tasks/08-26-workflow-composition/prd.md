# User-assembled agent workflow composition

## Goal

Turn hya's loose subagent primitives into a complete, user-composable workflow
mechanism: users author DAG files, hya discovers and executes them through its
governed subagent machinery. Argus-like power without preset combinations —
hya ships **zero** built-in workflows and never hardcodes a
plan→impl→review pipeline.

## Requirements

- One workflow per file (plain YAML or markdown frontmatter) under
  `<workdir>/.hya/workflows` then `$HOME/.config/hya/workflows`
  (first name wins).
- Declarative stage graph: `needs:` edges levelized into parallel batches
  (fan-out), bounded upstream sections rendered into downstream directives via
  `{{stage_id}}` placeholders and declared `{{inputs.key}}` values (fan-in).
- User-declarable join contract: `on_member_failure: fail_fast | collect_all`.
- Loop stages (`mode: loop` + `verify: {agent, until}`) reuse the shared
  iteration driver; the independent verifier — never the worker — owns the stop.
- All fan-out goes through the governed team path (`pre_admit_team` /
  `run_pre_admitted_team`) so depth/concurrency/per-run budgets hold for user
  DAGs exactly like model-decided batches.

## Acceptance Criteria

- [x] Discovery + schema parsing rejects unknown fields, duplicate stage ids,
      missing verify blocks, and invalid identifiers before any spawn.
- [x] Graph planning rejects cycles, self/dangling `needs`, forward template
      references, and undeclared inputs at plan time.
- [x] Execution resolves every stage (and verifier) agent against the caller's
      `can_spawn` roster up front; authorization failures abort pre-spawn.
- [x] Per-run-budget overflow is rejected up front (no member spawns).
- [x] Fan-out/fan-in runs as parallel governed batches; joins receive both
      upstream sections in declaration order.
- [x] Fail-fast aborts downstream levels; collect_all continues and marks
      failed upstreams in joined directives.
- [x] Loop stages iterate until the verifier grants; verdict JSON is parsed
      tolerantly (malformed ⇒ not met toward the cap); verifier sessions are
      fresh and disjoint from the worker session.
- [x] CLI surface: `workflow list | info | run [--input k=v]... [--json]`.
- [x] Agent surface: governed `workflow` tool (`action=list|run`) registered as
      a builtin beside `task`; host-side worker executes through the core
      executor only.
- [x] End-to-end scenario proves real file discovery drives a fan-out → fan-in
      → report run mid-session (never a preset).

## Notes

- Design rationale and seam inventory live in `design.md`; ADR-0013 records why
  composition reuses rather than replaces the task tool machinery.
