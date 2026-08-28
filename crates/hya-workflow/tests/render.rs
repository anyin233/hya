//! Public input interpolation and automatic direct-predecessor evidence contracts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_workflow::{
    StageEvidence, StageEvidenceStatus, WorkflowRenderError, WorkflowSource, compile,
};

const JOIN_WORKFLOW: &str = r#"---
kind: Workflow
name: joined-work
description: Render inputs and direct predecessor evidence.
inputs:
  request: Work request.
on_failure: collect_all
nodes:
  plan:
    agent: planner
    directive: Plan {{input.request}} and restate {{input.request}}.
  impl_a:
    agent: implementer
    directive: Implement A.
  impl_b:
    agent: implementer
    directive: Implement B.
  review:
    agent: reviewer
    directive: Review direct evidence.
---
flowchart TD
  plan --> impl_a & impl_b
  impl_b & impl_a --> review
"#;

/// Rendering requires exactly the declared input set and replaces every input
/// occurrence before any Stage can be admitted.
#[test]
fn declared_inputs_are_required_and_unknown_inputs_are_rejected() {
    let compiled = compile(WorkflowSource::new("join.hya.md", JOIN_WORKFLOW)).unwrap();
    let plan_index = compiled.plan().stage_index("plan").unwrap();
    let evidence = vec![None; compiled.plan().stages().len()];

    let missing = compiled
        .render_stage(plan_index, &BTreeMap::new(), &evidence)
        .unwrap_err();
    assert_eq!(
        missing,
        WorkflowRenderError::MissingInput("request".to_string())
    );

    let mut inputs = BTreeMap::from([("request".to_string(), "fix auth".to_string())]);
    inputs.insert("extra".to_string(), "not declared".to_string());
    let unknown = compiled
        .render_stage(plan_index, &inputs, &evidence)
        .unwrap_err();
    assert_eq!(
        unknown,
        WorkflowRenderError::UnknownInput("extra".to_string())
    );

    inputs.remove("extra");
    let rendered = compiled
        .render_stage(plan_index, &inputs, &evidence)
        .unwrap();
    assert_eq!(rendered.directive(), "Plan fix auth and restate fix auth.");
    assert_eq!(
        rendered.system_context(),
        "<workflow-context>\nworkflow: joined-work\nstage: plan\nlevel: 0\n</workflow-context>"
    );
}

/// A join receives only direct predecessors, in compiled incoming-edge order,
/// with typed status and each output independently clamped on a UTF-8 boundary.
#[test]
fn join_evidence_is_automatic_ordered_typed_and_bounded() {
    let compiled = compile(WorkflowSource::new("join.hya.md", JOIN_WORKFLOW)).unwrap();
    let plan = compiled.plan();
    let review_index = plan.stage_index("review").unwrap();
    let long = "界".repeat(1_334);
    let evidence = vec![
        Some(StageEvidence::new(
            StageEvidenceStatus::Done,
            "secret ancestor",
        )),
        Some(StageEvidence::new(StageEvidenceStatus::Done, "A output")),
        Some(StageEvidence::new(StageEvidenceStatus::Failed, &long)),
        None,
    ];
    let inputs = BTreeMap::from([("request".to_string(), "ignored here".to_string())]);

    let rendered = compiled
        .render_stage(review_index, &inputs, &evidence)
        .unwrap();
    let expected_bounded = "界".repeat(1_333);
    assert_eq!(expected_bounded.len(), 3_999);
    assert_eq!(
        rendered.directive(),
        format!(
            "Review direct evidence.\n\n<workflow-upstream>\n\
             <stage id=\"impl_b\" agent=\"implementer\" status=\"failed\">\n\
             {expected_bounded}\n</stage>\n\
             <stage id=\"impl_a\" agent=\"implementer\" status=\"done\">\n\
             A output\n</stage>\n\
             </workflow-upstream>"
        )
    );
    assert!(!rendered.directive().contains("secret ancestor"));
}

const UNKNOWN_INPUT_WORKFLOW: &str = r#"---
kind: Workflow
name: bad-input
description: Unknown input references fail compilation.
inputs:
  request: Work request.
nodes:
  plan:
    agent: planner
    directive: Plan {{input.typo}}.
---
flowchart TD
  plan
"#;

/// The compiler closes the input namespace and rejects every non-input public
/// placeholder, including the removed Stage-output placeholder contract.
#[test]
fn compiler_rejects_unknown_and_legacy_placeholders() {
    let unknown = compile(WorkflowSource::new(
        "unknown.hya.md",
        UNKNOWN_INPUT_WORKFLOW,
    ))
    .unwrap_err();
    assert!(
        unknown.message().contains("undeclared input `typo`"),
        "{unknown}"
    );

    let legacy_source = UNKNOWN_INPUT_WORKFLOW.replace("{{input.typo}}", "{{plan}}");
    let legacy = compile(WorkflowSource::new("legacy.hya.md", &legacy_source)).unwrap_err();
    assert!(
        legacy.message().contains("only `{{input.<name>}}`"),
        "{legacy}"
    );
}
