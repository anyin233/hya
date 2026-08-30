//! Public mode, verifier, actor, and canonical revision contracts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_workflow::{StageMode, WorkflowCompileErrorKind, WorkflowSource, compile};

const SEMANTIC_WORKFLOW: &str = r#"---
kind: Workflow
name: iterative-actor
description: Exercise normalized Stage semantics.
inputs:
  request: Work request.
nodes:
  plan:
    title: Plan work
    agent: resident-planner
    actor: planner
    directive: Plan {{input.request}}.
  implement:
    agent: worker
    directive: Implement the plan.
    mode: loop
    verify:
      agent: reviewer
      until: The request is implemented.
      max_iterations: 3
  refine:
    agent: resident-planner
    actor: planner
    directive: Refine the plan after implementation.
---
flowchart TD
  %% comments and whitespace do not affect the revision
  plan --> implement
  implement --> refine
"#;

/// Loop and resident declarations survive normalization through read-only
/// getters; repeated sequential actor activations retain one explicit key.
#[test]
fn loop_verifier_and_actor_declarations_normalize() {
    let compiled = compile(WorkflowSource::new("semantic.hya.md", SEMANTIC_WORKFLOW)).unwrap();
    let stages = compiled.plan().stages();

    assert_eq!(stages[0].mode(), StageMode::Once);
    assert_eq!(stages[0].actor(), Some("planner"));
    assert!(stages[0].verify().is_none());
    assert_eq!(stages[1].mode(), StageMode::Loop);
    let verify = stages[1].verify().unwrap();
    assert_eq!(verify.agent(), "reviewer");
    assert_eq!(verify.until(), "The request is implemented.");
    assert_eq!(verify.max_iterations(), 3);
    assert_eq!(stages[2].actor(), Some("planner"));
}

/// Equivalent author formatting hashes identically; any normalized directive
/// or graph-order change produces a different immutable revision.
#[test]
fn canonical_revision_ignores_formatting_but_covers_semantics() {
    let first = compile(WorkflowSource::new("first.hya.md", SEMANTIC_WORKFLOW)).unwrap();
    let reformatted = SEMANTIC_WORKFLOW.replace(
        "  %% comments and whitespace do not affect the revision\n  plan --> implement",
        "plan-->implement",
    );
    let second = compile(WorkflowSource::new("second.hya.md", &reformatted)).unwrap();
    assert_eq!(first.revision(), second.revision());

    let changed_directive = SEMANTIC_WORKFLOW.replace(
        "Implement the plan.",
        "Implement the plan and run focused checks.",
    );
    let changed = compile(WorkflowSource::new("changed.hya.md", &changed_directive)).unwrap();
    assert_ne!(first.revision(), changed.revision());
    assert_eq!(
        first.revision().to_string(),
        "bd3e15aa03c6804cb7b084f0e4be82a793a381b5522f52bfa60f62f4734eca0c"
    );
}

/// Invalid loop and actor shapes are compiler errors, not runtime guesses.
#[test]
fn invalid_loop_and_actor_contracts_fail_compilation() {
    let base = SEMANTIC_WORKFLOW.to_string();
    let cases = [
        (
            base.replace(
                "    mode: loop\n    verify:\n      agent: reviewer\n      until: The request is implemented.\n      max_iterations: 3\n",
                "    mode: loop\n",
            ),
            "mode `loop` requires a verifier",
        ),
        (
            base.replace("      max_iterations: 3", "      max_iterations: 0"),
            "max_iterations must be at least 1",
        ),
        (
            base.replace("actor: planner", "actor: INVALID ACTOR"),
            "invalid actor key",
        ),
        (
            base.replace(
                "  plan --> implement\n  implement --> refine",
                "  plan\n  refine\n  implement",
            ),
            "same actor key `planner` in one level",
        ),
        (
            base.replace(
                "  refine:\n    agent: resident-planner",
                "  refine:\n    agent: another-resident",
            ),
            "actor key `planner` targets both",
        ),
        (
            base.replace(
                "  implement:\n    agent: worker",
                "  implement:\n    agent: worker\n    actor: loop-worker",
            ),
            "cannot combine actor and loop modes",
        ),
    ];

    for (source, message) in cases {
        let error = compile(WorkflowSource::new("invalid-semantics.hya.md", &source)).unwrap_err();
        assert_eq!(error.kind(), WorkflowCompileErrorKind::Validation);
        assert!(error.message().contains(message), "{error}");
    }
}

const ROUTED_REVISION_WORKFLOW: &str = r#"---
kind: Workflow
name: routed-revision
description: Route fields are part of assignment semantics.
nodes:
  worker:
    agent: workflow-worker
    directive: Implement the request.
    model:
      id: primary-model
      reasoning: low
      fallback:
        - id: fallback-a
          reasoning: medium
        - id: fallback-b
  loop:
    agent: workflow-loop-worker
    directive: Verify the result.
    mode: loop
    verify:
      agent: workflow-verifier
      until: The result is complete.
      model:
        id: verifier-model
        reasoning: high
        fallback:
          - id: verifier-fallback
            reasoning: medium
          - id: verifier-fallback-b
---
flowchart TD
  worker --> loop
"#;

/// Every authored worker/verifier route field changes the conditional revision,
/// while normalized formatting remains revision-neutral.
#[test]
fn canonical_revision_covers_every_model_route_field() {
    let base = compile(WorkflowSource::new(
        "routed-base.hya.md",
        ROUTED_REVISION_WORKFLOW,
    ))
    .unwrap()
    .revision();
    let mutations = [
        (
            "worker presence",
            ROUTED_REVISION_WORKFLOW.replace(
                "    model:\n      id: primary-model\n      reasoning: low\n      fallback:\n        - id: fallback-a\n          reasoning: medium\n        - id: fallback-b\n",
                "",
            ),
        ),
        (
            "verifier presence",
            ROUTED_REVISION_WORKFLOW.replace(
                "      model:\n        id: verifier-model\n        reasoning: high\n        fallback:\n          - id: verifier-fallback\n            reasoning: medium\n          - id: verifier-fallback-b\n",
                "",
            ),
        ),
        (
            "worker preferred id value",
            ROUTED_REVISION_WORKFLOW.replace("id: primary-model", "id: primary-model-new"),
        ),
        (
            "worker preferred effort value",
            ROUTED_REVISION_WORKFLOW.replace("reasoning: low", "reasoning: high"),
        ),
        (
            "worker preferred effort presence",
            ROUTED_REVISION_WORKFLOW.replace("      reasoning: low\n", ""),
        ),
        (
            "worker fallback count",
            ROUTED_REVISION_WORKFLOW.replace("        - id: fallback-b\n", ""),
        ),
        (
            "worker fallback id value",
            ROUTED_REVISION_WORKFLOW.replace("id: fallback-a", "id: fallback-a-new"),
        ),
        (
            "worker fallback effort value",
            ROUTED_REVISION_WORKFLOW.replace("reasoning: medium", "reasoning: high"),
        ),
        (
            "worker fallback effort presence",
            ROUTED_REVISION_WORKFLOW.replace(
                "        - id: fallback-a\n          reasoning: medium\n",
                "        - id: fallback-a\n",
            ),
        ),
        (
            "worker fallback order",
            ROUTED_REVISION_WORKFLOW.replace(
                "        - id: fallback-a\n          reasoning: medium\n        - id: fallback-b\n",
                "        - id: fallback-b\n        - id: fallback-a\n          reasoning: medium\n",
            ),
        ),
        (
            "verifier preferred id value",
            ROUTED_REVISION_WORKFLOW.replace("id: verifier-model", "id: verifier-model-new"),
        ),
        (
            "verifier preferred effort value",
            ROUTED_REVISION_WORKFLOW.replace("reasoning: high", "reasoning: low"),
        ),
        (
            "verifier preferred effort presence",
            ROUTED_REVISION_WORKFLOW.replace("        reasoning: high\n", ""),
        ),
        (
            "verifier fallback count",
            ROUTED_REVISION_WORKFLOW.replace("          - id: verifier-fallback-b\n", ""),
        ),
        (
            "verifier fallback effort presence",
            ROUTED_REVISION_WORKFLOW.replace(
                "          - id: verifier-fallback\n            reasoning: medium\n",
                "          - id: verifier-fallback\n",
            ),
        ),
        (
            "verifier fallback order",
            ROUTED_REVISION_WORKFLOW.replace(
                "          - id: verifier-fallback\n            reasoning: medium\n          - id: verifier-fallback-b\n",
                "          - id: verifier-fallback-b\n          - id: verifier-fallback\n            reasoning: medium\n",
            ),
        ),
        (
            "verifier fallback id value",
            ROUTED_REVISION_WORKFLOW.replace("id: verifier-fallback", "id: verifier-fallback-new"),
        ),
        (
            "verifier fallback effort value",
            ROUTED_REVISION_WORKFLOW.replace(
                "id: verifier-fallback\n            reasoning: medium",
                "id: verifier-fallback\n            reasoning: high",
            ),
        ),
    ];
    for (label, source) in mutations {
        let compiled = compile(WorkflowSource::new("routed-mutated.hya.md", &source))
            .unwrap_or_else(|error| panic!("{label}: {error}\n{source}"));
        let revision = compiled.revision();
        assert_ne!(base, revision, "{label} must change the revision");
    }

    let reformatted = ROUTED_REVISION_WORKFLOW
        .replace("id: primary-model", "id: \"  primary-model  \"")
        .replace("reasoning: low", "reasoning: \" low \"")
        .replace("worker --> loop", "worker-->loop");
    let reformatted_revision = compile(WorkflowSource::new("routed-format.hya.md", &reformatted))
        .unwrap()
        .revision();
    assert_eq!(base, reformatted_revision);
}
