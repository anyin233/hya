---
kind: Workflow
name: plan-impl-review
description: Plan a change, implement it, and review the resulting work.
inputs:
  request: The change request to carry through the Workflow.
on_failure: collect_all
nodes:
  plan:
    title: Plan the change
    agent: plan-impl-review-planner
    directive: |
      Analyze the requested change and produce a concise implementation plan.
      Identify the observable behavior, affected boundaries, and verification
      needed before any edits begin.

      Request:
      {{input.request}}
  implement:
    title: Implement the plan
    agent: plan-impl-review-implementer
    directive: |
      Implement the requested change using the plan evidence from the previous
      Stage. Keep the change scoped, preserve established project patterns, and
      leave a clear verification trail.

      Request:
      {{input.request}}
  review:
    title: Review the result
    agent: plan-impl-review-reviewer
    directive: |
      Review the implementation against the request and the plan. Check
      behavior, edge cases, regressions, and maintainability. Report concrete
      findings and the final recommendation.

      Request:
      {{input.request}}
---
flowchart TD
  plan --> implement
  implement --> review
