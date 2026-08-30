//! Public compiler contracts for normalized Workflow plans.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_workflow::{
    FailurePolicy, MAX_WORKFLOW_MODEL_ID_CHARS, MAX_WORKFLOW_REASONING_CHARS,
    WorkflowCompileErrorKind, WorkflowModelAssignment, WorkflowModelCandidate, WorkflowSource,
    compile,
};

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

const MODEL_ROUTED_WORKFLOW: &str = r#"---
kind: Workflow
name: routed-flow
description: Assign concrete model routes to worker and verifier.
nodes:
  worker:
    agent: workflow-worker
    directive: Implement the request.
    model:
      id: "  primary-model  "
      reasoning: " high "
      fallback:
        - id: fallback-model
          reasoning: medium
  loop:
    agent: workflow-loop-worker
    directive: Verify the result.
    mode: loop
    verify:
      agent: workflow-verifier
      until: The result is complete.
      model:
        id: verifier-model
        reasoning: low
        fallback:
          - id: verifier-fallback
            reasoning: unknown-effort
---
flowchart TD
  worker --> loop
"#;

/// Public model-route values remain importable without reaching into private modules.
#[test]
fn model_route_values_are_public() {
    let _candidate: Option<WorkflowModelCandidate> = None;
    let _assignment: Option<WorkflowModelAssignment> = None;
}

/// Worker and verifier model blocks retain declaration order and effort values.
#[test]
fn model_assignments_normalize_worker_and_verifier_routes() {
    let compiled = compile(WorkflowSource::new("routed.hya.md", MODEL_ROUTED_WORKFLOW)).unwrap();
    let stages = compiled.plan().stages();

    let worker = stages[0].model().unwrap();
    assert_eq!(worker.id(), "primary-model");
    assert_eq!(worker.reasoning(), Some("high"));
    assert_eq!(worker.fallback().len(), 1);
    assert_eq!(worker.fallback()[0].id(), "fallback-model");
    assert_eq!(worker.fallback()[0].reasoning(), Some("medium"));

    let verifier = stages[1].verify().unwrap().model().unwrap();
    assert_eq!(verifier.id(), "verifier-model");
    assert_eq!(verifier.reasoning(), Some("low"));
    assert_eq!(verifier.fallback()[0].id(), "verifier-fallback");
    assert_eq!(verifier.fallback()[0].reasoning(), Some("unknown-effort"));
}

/// Same model ids with different effort remain separate ordered entries.
#[test]
fn model_route_keeps_same_id_with_different_effort() {
    let source = MODEL_ROUTED_WORKFLOW.replace(
        "        - id: fallback-model\n          reasoning: medium",
        "        - id: primary-model\n          reasoning: medium\n        - id: primary-model\n          reasoning: low",
    );
    let compiled = compile(WorkflowSource::new("same-id.hya.md", &source)).unwrap();
    let assignment = compiled.plan().stages()[0].model().unwrap();
    assert_eq!(assignment.fallback().len(), 2);
    assert_eq!(assignment.fallback()[0].id(), "primary-model");
    assert_eq!(assignment.fallback()[0].reasoning(), Some("medium"));
    assert_eq!(assignment.fallback()[1].id(), "primary-model");
    assert_eq!(assignment.fallback()[1].reasoning(), Some("low"));
}

/// Build a one-node Workflow with a caller-supplied model block.
fn duplicate_model_route_source(route: &str) -> String {
    format!(
        r#"---
kind: Workflow
name: duplicate-route
description: Reject duplicate model route entries.
nodes:
  worker:
    agent: workflow-worker
    directive: Implement the request.
    model:
{route}
---
flowchart TD
  worker
"#,
        route = route
            .lines()
            .map(|line| format!("      {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Exact duplicate preferred/fallback entries fail at the owning Stage location.
#[test]
fn duplicate_preferred_and_fallback_model_entries_are_validation_errors() {
    let source = duplicate_model_route_source(
        "id: model\nreasoning: low\nfallback:\n  - id: model\n    reasoning: low",
    );
    let error = compile(WorkflowSource::new("duplicate-route.hya.md", &source)).unwrap_err();
    assert_eq!(error.kind(), WorkflowCompileErrorKind::Validation);
    assert_eq!(error.location().line(), 6);
    assert!(error.message().contains("duplicate"), "{error}");
    assert!(error.message().contains("model"), "{error}");
}

/// Duplicate fallback entries fail instead of being silently deduplicated.
#[test]
fn duplicate_fallback_model_entries_are_validation_errors() {
    let source =
        duplicate_model_route_source("id: model\nfallback:\n  - id: fallback\n  - id: fallback");
    let error = compile(WorkflowSource::new("duplicate-fallback.hya.md", &source)).unwrap_err();
    assert_eq!(error.kind(), WorkflowCompileErrorKind::Validation);
    assert_eq!(error.location().line(), 6);
    assert!(error.message().contains("duplicate"), "{error}");
}

/// Invalid route scalar values and variant-suffixed ids are rejected uniformly.
#[test]
fn invalid_model_route_values_are_rejected() {
    let cases = [
        ("id: \"  \"", "model id must not be empty"),
        (
            "id: model\nreasoning: \"  \"",
            "reasoning must not be empty",
        ),
        ("id: model#high", "variant suffix"),
        ("id: model#high\nreasoning: low", "variant suffix"),
        (
            "id: model\nfallback:\n  - id: \"  \"",
            "model id must not be empty",
        ),
        (
            "id: model\nfallback:\n  - id: fallback\n    reasoning: \"  \"",
            "reasoning must not be empty",
        ),
        (
            "id: model\nfallback:\n  - id: fallback#high",
            "variant suffix",
        ),
        (
            "id: model\nfallback:\n  - id: fallback#high\n    reasoning: low",
            "variant suffix",
        ),
    ];
    for (route, message) in cases {
        let source = duplicate_model_route_source(route);
        let error = compile(WorkflowSource::new("invalid-route.hya.md", &source)).unwrap_err();
        assert_eq!(
            error.kind(),
            WorkflowCompileErrorKind::Validation,
            "{error}"
        );
        assert!(error.message().contains(message), "{error}");
    }
}

/// Durable route model and reasoning fields reject unbounded source values.
#[test]
fn model_route_fields_are_bounded_before_runtime() {
    let cases = [
        (
            format!("id: {}", "m".repeat(MAX_WORKFLOW_MODEL_ID_CHARS + 1)),
            "model id exceeds",
        ),
        (
            format!(
                "id: model\nreasoning: {}",
                "r".repeat(MAX_WORKFLOW_REASONING_CHARS + 1)
            ),
            "reasoning exceeds",
        ),
    ];
    for (route, message) in cases {
        let source = duplicate_model_route_source(&route);
        let error = compile(WorkflowSource::new("bounded-route.hya.md", &source)).unwrap_err();
        assert_eq!(error.kind(), WorkflowCompileErrorKind::Validation);
        assert!(error.message().contains(message), "{error}");
    }
}

/// Unknown keys inside a model assignment fail the existing frontmatter gate.
#[test]
fn unknown_nested_model_key_is_rejected() {
    let source = duplicate_model_route_source("id: model\nunexpected: value");
    let error = compile(WorkflowSource::new("unknown-model-key.hya.md", &source)).unwrap_err();
    assert_eq!(error.kind(), WorkflowCompileErrorKind::Frontmatter);
    assert!(error.message().contains("unknown field"), "{error}");
}

/// Unknown non-empty effort labels remain available for runtime capability checks.
#[test]
fn unknown_nonempty_reasoning_label_is_preserved() {
    let source = duplicate_model_route_source("id: model\nreasoning: provider-specific");
    let compiled = compile(WorkflowSource::new("unknown-effort.hya.md", &source)).unwrap();
    assert_eq!(
        compiled.plan().stages()[0].model().unwrap().reasoning(),
        Some("provider-specific")
    );
}

/// Assignment semantics affect revisions while no-assignment revisions remain stable.
#[test]
fn model_assignment_changes_revision_without_changing_no_assignment_baseline() {
    let without = MODEL_ROUTED_WORKFLOW.replace(
        "    model:\n      id: \"  primary-model  \"\n      reasoning: \" high \"\n      fallback:\n        - id: fallback-model\n          reasoning: medium\n",
        "",
    );
    let with = compile(WorkflowSource::new(
        "with-route.hya.md",
        MODEL_ROUTED_WORKFLOW,
    ))
    .unwrap();
    let without = compile(WorkflowSource::new("without-route.hya.md", &without)).unwrap();
    assert_ne!(with.revision(), without.revision());
}
