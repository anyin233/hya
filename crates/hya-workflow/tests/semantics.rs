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
