//! Public compiler contracts for normalized Workflow plans.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_workflow::{FailurePolicy, WorkflowCompileErrorKind, WorkflowSource, compile};

const LINEAR_WORKFLOW: &str = r#"---
kind: Workflow
name: feature-delivery
description: Plan, implement, and review one request.
inputs:
  request: Work request to complete.
on_failure: collect_all
nodes:
  plan:
    title: Produce plan
    agent: workflow-planner
    directive: Plan {{input.request}}.
  impl:
    title: Implement request
    agent: workflow-implementer
    directive: Implement the approved plan.
  review:
    title: Review result
    agent: workflow-reviewer
    directive: Review the implementation.
---
flowchart TD
  plan --> impl
  impl --> review
"#;

/// The compiler normalizes one linear document into stable Stage and level
/// order without exposing a constructible unvalidated plan.
#[test]
fn linear_document_compiles_to_exact_normalized_plan() {
    let compiled = compile(WorkflowSource::new("linear.hya.md", LINEAR_WORKFLOW)).unwrap();

    let definition = compiled.definition();
    assert_eq!(definition.name(), "feature-delivery");
    assert_eq!(
        definition.description(),
        "Plan, implement, and review one request."
    );
    assert_eq!(definition.on_failure(), FailurePolicy::CollectAll);
    assert_eq!(
        definition.inputs().get("request").map(String::as_str),
        Some("Work request to complete.")
    );

    let plan = compiled.plan();
    assert_eq!(
        plan.stages()
            .iter()
            .map(|stage| stage.id())
            .collect::<Vec<_>>(),
        ["plan", "impl", "review"]
    );
    assert_eq!(
        plan.levels()
            .iter()
            .map(|level| {
                level
                    .stage_indices()
                    .iter()
                    .map(|&index| plan.stages()[index].id())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        [["plan"], ["impl"], ["review"]]
    );
    assert_eq!(plan.stages()[0].predecessor_indices(), &[]);
    assert_eq!(plan.stages()[1].predecessor_indices(), &[0]);
    assert_eq!(plan.stages()[2].predecessor_indices(), &[1]);
    assert_eq!(plan.stages()[0].title(), Some("Produce plan"));
    assert_eq!(plan.stages()[0].agent(), "workflow-planner");
    assert_eq!(plan.stages()[0].directive(), "Plan {{input.request}}.");
}

const FAN_GRAPH_WORKFLOW: &str = r#"---
kind: Workflow
name: fan-review
description: Fan out and join in author-declared graph order.
nodes:
  review:
    agent: workflow-reviewer
    directive: Review direct predecessor evidence.
  impl_b:
    agent: workflow-implementer
    directive: Implement tests.
  isolated:
    agent: workflow-researcher
    directive: Record independent context.
  plan:
    agent: workflow-planner
    directive: Produce a plan.
  impl_a:
    agent: workflow-implementer
    directive: Implement the core path.
---
flowchart TD
  %% Standalone nodes and comments are part of the restricted grammar.
  isolated
  plan --> impl_a & impl_b
  impl_b --> review
  impl_a --> review
"#;

/// First graph occurrence controls Stage order, while incoming edge declaration
/// controls deterministic direct-predecessor join order.
#[test]
fn fan_out_fan_in_and_standalone_nodes_normalize_in_graph_order() {
    let compiled = compile(WorkflowSource::new("fan.hya.md", FAN_GRAPH_WORKFLOW)).unwrap();
    let plan = compiled.plan();

    assert_eq!(
        plan.stages()
            .iter()
            .map(|stage| stage.id())
            .collect::<Vec<_>>(),
        ["isolated", "plan", "impl_a", "impl_b", "review"]
    );
    assert_eq!(
        plan.levels()
            .iter()
            .map(|level| {
                level
                    .stage_indices()
                    .iter()
                    .map(|&index| plan.stages()[index].id())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        vec![
            vec!["isolated", "plan"],
            vec!["impl_a", "impl_b"],
            vec!["review"],
        ]
    );
    assert_eq!(
        plan.stages()[4]
            .predecessor_indices()
            .iter()
            .map(|&index| plan.stages()[index].id())
            .collect::<Vec<_>>(),
        ["impl_b", "impl_a"]
    );
}

/// Build a fixed-line invalid-source fixture so error positions are independent
/// literals rather than values recomputed by the compiler implementation.
fn invalid_graph_document(graph: &str) -> String {
    format!(
        r#"---
kind: Workflow
name: invalid-flow
description: Exercise one invalid graph contract.
nodes:
  a:
    agent: workflow-a
    directive: Run A.
  b:
    agent: workflow-b
    directive: Run B.
---
flowchart TD
{graph}
"#
    )
}

/// Unsupported syntax, membership errors, self edges, and cycles fail with a
/// typed category and the exact one-based source position that owns the defect.
#[test]
fn invalid_graphs_report_typed_source_locations() {
    let cases = [
        (
            "  a[Plan] --> b",
            WorkflowCompileErrorKind::Graph,
            14,
            3,
            "stable identifiers",
        ),
        (
            "  a & b --> a & b",
            WorkflowCompileErrorKind::Graph,
            14,
            3,
            "fan sugar may appear on only one side",
        ),
        (
            "  a --> ghost",
            WorkflowCompileErrorKind::Validation,
            14,
            9,
            "no frontmatter definition",
        ),
        (
            "  a --> a\n  b",
            WorkflowCompileErrorKind::Validation,
            14,
            9,
            "cannot depend on itself",
        ),
        (
            "  a --> b\n  b --> a",
            WorkflowCompileErrorKind::Validation,
            15,
            3,
            "cycle",
        ),
        (
            "  a",
            WorkflowCompileErrorKind::Validation,
            9,
            3,
            "does not occur in the graph",
        ),
    ];

    for (graph, kind, line, column, message) in cases {
        let source = invalid_graph_document(graph);
        let error = compile(WorkflowSource::new("invalid.hya.md", &source)).unwrap_err();
        assert_eq!(error.kind(), kind, "source:\n{source}");
        assert_eq!(error.location().line(), line, "source:\n{source}");
        assert_eq!(error.location().column(), column, "source:\n{source}");
        assert!(error.message().contains(message), "{error}");
    }
}

const DUPLICATE_NODE_WORKFLOW: &str = r#"---
kind: Workflow
name: duplicate-flow
description: Duplicate node keys are ambiguous.
nodes:
  a:
    agent: workflow-a
    directive: First A.
  a:
    agent: workflow-a
    directive: Second A.
---
flowchart TD
  a
"#;

/// Duplicate frontmatter node keys are rejected rather than silently taking a
/// YAML parser's first or last value.
#[test]
fn duplicate_frontmatter_nodes_are_rejected_at_the_second_key() {
    let error = compile(WorkflowSource::new(
        "duplicate.hya.md",
        DUPLICATE_NODE_WORKFLOW,
    ))
    .unwrap_err();
    assert_eq!(error.kind(), WorkflowCompileErrorKind::Frontmatter);
    assert_eq!(error.location().line(), 9);
    assert_eq!(error.location().column(), 3);
    assert!(error.message().contains("duplicate node `a`"), "{error}");
}
