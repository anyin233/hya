---
kind: Workflow
name: argus
description: Investigate, plan, execute, review, synthesize, and verify a change.
inputs:
  request: The change request to investigate and deliver.
on_failure: collect_all
nodes:
  investigate:
    title: Investigate the request
    agent: argus-investigator
    directive: |
      Investigate the request before implementation. Establish the relevant
      behavior, constraints, existing patterns, and likely risks. Return
      concrete evidence for the planning Stage.

      Request:
      {{input.request}}
  plan:
    title: Build the delivery plan
    agent: argus-planner
    directive: |
      Build a bounded delivery plan from the request and investigation
      evidence. Define the intended behavior, affected boundaries, and proof
      required for a safe implementation.

      Request:
      {{input.request}}
  execute_code:
    title: Execute the implementation
    agent: argus-implementer
    directive: |
      Implement the requested change from the plan and investigation evidence.
      Preserve unrelated work, follow local patterns, and leave the source in a
      state that another Agent can verify.

      Request:
      {{input.request}}
  execute_tests:
    title: Execute the verification work
    agent: argus-test-engineer
    directive: |
      Build or exercise the focused verification needed for the requested
      change. Check observable behavior and meaningful boundaries in parallel
      with the implementation Stage.

      Request:
      {{input.request}}
  review_architecture:
    title: Review architecture and safety
    agent: argus-architecture-reviewer
    directive: |
      Review the implementation and verification evidence for architecture,
      ownership, security boundaries, failure handling, and consistency with
      established design. Report concrete findings.
  review_quality:
    title: Review behavior and quality
    agent: argus-quality-reviewer
    directive: |
      Review the implementation and verification evidence for observable
      behavior, edge cases, regressions, determinism, and maintainability.
      Report concrete findings and missing proof.
  synthesize:
    title: Synthesize the reviews
    agent: argus-synthesizer
    directive: |
      Synthesize the two independent review perspectives and the execution
      evidence. Resolve disagreements, identify any remaining risk, and state
      the exact final actions required before delivery.
  verify_behavior:
    title: Verify behavior
    agent: argus-behavior-verifier
    directive: |
      Verify the synthesized result against the requested observable behavior.
      Check that the implementation and evidence support the claimed outcome.
  verify_scope:
    title: Verify scope and closure
    agent: argus-scope-verifier
    directive: |
      Verify that the delivered change is complete and scoped. Check closure of
      all affected boundaries, absence of repository-only assumptions, and the
      remaining review findings.
  verify_final:
    title: Confirm delivery
    agent: argus-final-verifier
    directive: |
      Make the final delivery judgment from both verification perspectives and
      the synthesis evidence. Report success only when the request is fully
      satisfied; otherwise name the exact blocking gap.
---
flowchart TD
  investigate --> plan
  plan --> execute_code & execute_tests
  execute_code & execute_tests --> review_architecture
  execute_code & execute_tests --> review_quality
  review_architecture & review_quality --> synthesize
  synthesize --> verify_behavior & verify_scope
  verify_behavior & verify_scope --> verify_final
