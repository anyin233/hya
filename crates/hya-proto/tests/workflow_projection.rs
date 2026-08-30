//! Durable Workflow state is a replay-only Session projection.

#![allow(clippy::expect_used)]

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MemberId, MessageId, OwnerRunId, PartId, Projection,
    Role, SessionId, WorkflowAvailability, WorkflowIdentity, WorkflowMemberRole,
    WorkflowModelAssignment, WorkflowModelCandidate, WorkflowModelResolvedCandidate,
    WorkflowRevision, WorkflowRouteFailureClass, WorkflowRunId, WorkflowRunStatus,
    WorkflowSourceId, WorkflowStagePlan, WorkflowStageRouteOutcome, WorkflowStageStatus,
};
use serde_json::json;

/// Build one durable envelope with a fixed sequence.
fn envelope(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

/// Return one stable Workflow identity with a deterministic revision byte.
fn identity(name: &str, revision: u8) -> WorkflowIdentity {
    WorkflowIdentity {
        source: WorkflowSourceId::new(format!("project:{name}")),
        name: name.to_string(),
        revision: WorkflowRevision::from_bytes([revision; 32]),
    }
}

/// Return one declaration-ordered Stage plan row.
fn stage(id: &str, level: usize) -> WorkflowStagePlan {
    WorkflowStagePlan {
        id: id.to_string(),
        title: Some(format!("{id} title")),
        agent: AgentName::new("planner"),
        mode: "once".to_string(),
        level,
        worker_model: None,
        selected_worker_model: None,
        verifier_model: None,
        selected_verifier_model: None,
    }
}

/// Workflow selection is orthogonal to the transcript and last selection wins.
#[test]
fn selection_switch_preserves_transcript_exactly() {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let events = vec![
        envelope(
            1,
            Event::MessageStarted {
                session,
                message,
                role: Role::User,
            },
        ),
        envelope(
            2,
            Event::TextStart {
                session,
                message,
                part,
            },
        ),
        envelope(
            3,
            Event::TextDelta {
                session,
                message,
                part,
                delta: "keep this exact transcript".to_string(),
            },
        ),
        envelope(
            4,
            Event::TextEnd {
                session,
                message,
                part,
            },
        ),
        envelope(
            5,
            Event::WorkflowSelected {
                session,
                workflow: identity("workflow-a", 1),
            },
        ),
        envelope(
            6,
            Event::WorkflowSelected {
                session,
                workflow: identity("workflow-b", 2),
            },
        ),
    ];

    let before_selection = Projection::from_events(&events[..4]).session.messages;
    let projection = Projection::from_events(&events);
    let selected = projection
        .session
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.selection.as_ref())
        .expect("selected Workflow");

    assert_eq!(selected.name, "workflow-b");
    assert_eq!(selected.revision, WorkflowRevision::from_bytes([2; 32]));
    assert_eq!(projection.session.messages, before_selection);
}

/// Run start materializes the declaration-ordered plan. Lifecycle events are
/// idempotent, run-fenced, member-role-fenced, and terminal-sticky.
#[test]
fn run_projection_deduplicates_links_and_rejects_stale_or_nonterminal_rewrites() {
    let session = SessionId::new();
    let first_run = WorkflowRunId::new();
    let second_run = WorkflowRunId::new();
    let owner = OwnerRunId::new();
    let member = MemberId::new();
    let stale_member = MemberId::new();
    let first_identity = identity("deliver", 1);
    let second_identity = identity("deliver", 2);
    let events = vec![
        envelope(
            1,
            Event::WorkflowRunStarted {
                session,
                run: first_run,
                workflow: first_identity.clone(),
                request_hash: "request-1".to_string(),
                owner,
                stages: vec![stage("plan", 0), stage("execute", 1)],
            },
        ),
        envelope(
            2,
            Event::WorkflowStageStarted {
                session,
                run: first_run,
                stage: "plan".to_string(),
            },
        ),
        envelope(
            3,
            Event::WorkflowStageMemberLinked {
                session,
                run: first_run,
                stage: "plan".to_string(),
                member,
                role: WorkflowMemberRole::Worker,
                iteration: 0,
            },
        ),
        envelope(
            4,
            Event::WorkflowStageMemberLinked {
                session,
                run: first_run,
                stage: "plan".to_string(),
                member,
                role: WorkflowMemberRole::Worker,
                iteration: 0,
            },
        ),
        envelope(
            5,
            Event::WorkflowStageMemberLinked {
                session,
                run: first_run,
                stage: "plan".to_string(),
                member,
                role: WorkflowMemberRole::Verifier,
                iteration: 0,
            },
        ),
        envelope(
            6,
            Event::WorkflowStageFinished {
                session,
                run: first_run,
                stage: "plan".to_string(),
                status: WorkflowStageStatus::Completed,
            },
        ),
        envelope(
            7,
            Event::WorkflowStageStarted {
                session,
                run: first_run,
                stage: "plan".to_string(),
            },
        ),
        envelope(
            8,
            Event::WorkflowRunFinished {
                session,
                run: first_run,
                status: WorkflowRunStatus::Failed,
                error: Some("first failure".to_string()),
            },
        ),
        envelope(
            9,
            Event::WorkflowRunFinished {
                session,
                run: first_run,
                status: WorkflowRunStatus::Completed,
                error: None,
            },
        ),
        envelope(
            10,
            Event::WorkflowRunStarted {
                session,
                run: second_run,
                workflow: second_identity.clone(),
                request_hash: "request-2".to_string(),
                owner,
                stages: vec![stage("new", 0)],
            },
        ),
        envelope(
            11,
            Event::WorkflowStageMemberLinked {
                session,
                run: first_run,
                stage: "plan".to_string(),
                member: stale_member,
                role: WorkflowMemberRole::Worker,
                iteration: 0,
            },
        ),
    ];

    let projection = Projection::from_events(&events);
    let run = projection
        .session
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.run.as_ref())
        .expect("current Workflow run");
    assert_eq!(run.id, second_run);
    assert_eq!(run.workflow, second_identity);
    assert_eq!(run.status, WorkflowRunStatus::Running);
    assert_eq!(run.stages.len(), 1);
    assert_eq!(run.stages[0].plan.id, "new");
    assert!(run.stages[0].members.is_empty(), "old-run link leaked");

    let first_projection = Projection::from_events(&events[..9]);
    let first = first_projection
        .session
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.run.as_ref())
        .expect("first Workflow run");
    assert_eq!(first.workflow, first_identity);
    assert_eq!(first.status, WorkflowRunStatus::Failed);
    assert_eq!(first.error.as_deref(), Some("first failure"));
    assert_eq!(first.stages.len(), 2);
    assert_eq!(first.stages[0].status, WorkflowStageStatus::Completed);
    assert_eq!(first.stages[0].members.len(), 2);
    assert_eq!(first.stages[0].members[0].role, WorkflowMemberRole::Worker);
    assert_eq!(
        first.stages[0].members[1].role,
        WorkflowMemberRole::Verifier
    );
    assert_eq!(first.stages[1].status, WorkflowStageStatus::Skipped);
}

/// Revision wire format stays a validated lowercase 64-character digest.
#[test]
fn workflow_revision_serializes_as_hex_and_rejects_invalid_text() {
    let revision = WorkflowRevision::from_bytes([0xab; 32]);
    let encoded = serde_json::to_string(&revision).expect("serialize revision");
    let decoded: WorkflowRevision = serde_json::from_str(&encoded).expect("decode revision");

    assert_eq!(encoded, format!("\"{}\"", "ab".repeat(32)));
    assert_eq!(decoded, revision);
    assert!("not-a-revision".parse::<WorkflowRevision>().is_err());
}

/// The existing envelope-sequence guard also protects Workflow state.
#[test]
fn duplicate_sequence_cannot_terminalize_workflow_twice() {
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let events = vec![
        envelope(
            1,
            Event::WorkflowRunStarted {
                session,
                run,
                workflow: identity("sequence", 1),
                request_hash: "request".to_string(),
                owner: OwnerRunId::new(),
                stages: vec![stage("only", 0)],
            },
        ),
        envelope(
            1,
            Event::WorkflowRunFinished {
                session,
                run,
                status: WorkflowRunStatus::Failed,
                error: Some("duplicate sequence".to_string()),
            },
        ),
    ];
    let projection = Projection::from_events(&events);
    let current = projection
        .session
        .workflow
        .and_then(|workflow| workflow.run)
        .expect("run projection");
    assert_eq!(current.status, WorkflowRunStatus::Running);
    assert_eq!(current.error, None);
}
/// Replay leaves catalog availability absent, while the wire enum remains tagged by name.
#[test]
fn workflow_availability_is_runtime_only_and_wire_stable() {
    let session = SessionId::new();
    let workflow = identity("runtime-only", 1);
    let projection =
        Projection::from_events(&[envelope(1, Event::WorkflowSelected { session, workflow })]);
    let state = projection
        .session
        .workflow
        .as_ref()
        .expect("Workflow projection");
    assert!(state.availability.is_none());
    let encoded = serde_json::to_value(state).expect("encode replayed state");
    assert!(encoded.get("availability").is_none());
    assert_eq!(
        serde_json::to_value(WorkflowAvailability::Available).expect("encode availability"),
        "available"
    );
    assert_eq!(
        serde_json::to_value(WorkflowAvailability::Stale).expect("encode stale availability"),
        "stale"
    );
    assert_eq!(
        serde_json::to_value(WorkflowAvailability::Unavailable)
            .expect("encode unavailable availability"),
        "unavailable"
    );
}

/// Authored and admitted route values preserve optional versus required efforts.
#[test]
fn workflow_stage_routes_round_trip_with_canonical_selected_effort() {
    let plan = WorkflowStagePlan {
        id: "route".to_string(),
        title: Some("Route".to_string()),
        agent: AgentName::new("worker"),
        mode: "once".to_string(),
        level: 0,
        worker_model: Some(WorkflowModelAssignment {
            id: "primary".to_string(),
            reasoning: None,
            fallback: vec![WorkflowModelCandidate {
                id: "fallback".to_string(),
                reasoning: Some("high".to_string()),
            }],
        }),
        selected_worker_model: Some(WorkflowModelResolvedCandidate {
            index: 1,
            id: "fallback".to_string(),
            reasoning: "high".to_string(),
        }),
        verifier_model: Some(WorkflowModelAssignment {
            id: "verifier".to_string(),
            reasoning: Some("off".to_string()),
            fallback: Vec::new(),
        }),
        selected_verifier_model: Some(WorkflowModelResolvedCandidate {
            index: 0,
            id: "verifier".to_string(),
            reasoning: "none".to_string(),
        }),
    };
    let encoded = serde_json::to_value(&plan).expect("encode routed Stage plan");
    assert_eq!(encoded["worker_model"]["reasoning"], json!(null));
    assert_eq!(encoded["selected_worker_model"]["reasoning"], "high");
    assert_eq!(encoded["selected_verifier_model"]["reasoning"], "none");
    let decoded: WorkflowStagePlan = serde_json::from_value(encoded).expect("decode routed plan");
    assert_eq!(decoded, plan);

    let missing_selected_effort = json!({
        "id": "route",
        "agent": "worker",
        "mode": "once",
        "level": 0,
        "selected_worker_model": {"index": 0, "id": "primary"}
    });
    assert!(serde_json::from_value::<WorkflowStagePlan>(missing_selected_effort).is_err());
}

/// Route outcomes carry bounded role/step/index/model/effort/class fields.
#[test]
fn workflow_route_outcome_round_trips_with_required_effort() {
    let session = SessionId::new();
    let event = Event::WorkflowStageRouteOutcome {
        session,
        run: WorkflowRunId::new(),
        stage: "route".to_string(),
        member: MemberId::new(),
        role: WorkflowMemberRole::Verifier,
        iteration: 2,
        step: 3,
        candidate_index: 1,
        model: "provider/model".into(),
        reasoning: "none".to_string(),
        failure_class: WorkflowRouteFailureClass::None,
    };
    let encoded = serde_json::to_string(&event).expect("encode route outcome");
    assert!(encoded.contains("workflow_stage_route_outcome"));
    assert!(encoded.contains("\"reasoning\":\"none\""));
    let decoded: Event = serde_json::from_str(&encoded).expect("decode route outcome");
    assert_eq!(decoded, event);
    assert_eq!(event.session(), Some(session));
}

/// Route outcomes append only while their Stage is active and deduplicate by
/// Stage/member/role/iteration/step without reordering earlier observations.
#[test]
fn workflow_route_outcomes_fold_in_order_and_ignore_late_events() {
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let member = MemberId::new();
    let events = vec![
        envelope(
            1,
            Event::WorkflowRunStarted {
                session,
                run,
                workflow: identity("route-reducer", 1),
                request_hash: "request".to_string(),
                owner: OwnerRunId::new(),
                stages: vec![stage("route", 0)],
            },
        ),
        envelope(
            2,
            Event::WorkflowStageStarted {
                session,
                run,
                stage: "route".to_string(),
            },
        ),
        envelope(
            3,
            Event::WorkflowStageRouteOutcome {
                session,
                run,
                stage: "route".to_string(),
                member,
                role: WorkflowMemberRole::Worker,
                iteration: 0,
                step: 0,
                candidate_index: 1,
                model: "provider/fallback".into(),
                reasoning: "high".to_string(),
                failure_class: WorkflowRouteFailureClass::None,
            },
        ),
        envelope(
            4,
            Event::WorkflowStageRouteOutcome {
                session,
                run,
                stage: "route".to_string(),
                member,
                role: WorkflowMemberRole::Worker,
                iteration: 0,
                step: 0,
                candidate_index: 0,
                model: "provider/preferred".into(),
                reasoning: "low".to_string(),
                failure_class: WorkflowRouteFailureClass::Transport,
            },
        ),
        envelope(
            5,
            Event::WorkflowStageRouteOutcome {
                session,
                run,
                stage: "route".to_string(),
                member,
                role: WorkflowMemberRole::Verifier,
                iteration: 0,
                step: 1,
                candidate_index: 0,
                model: "provider/verifier".into(),
                reasoning: "none".to_string(),
                failure_class: WorkflowRouteFailureClass::None,
            },
        ),
        envelope(
            6,
            Event::WorkflowStageFinished {
                session,
                run,
                stage: "route".to_string(),
                status: WorkflowStageStatus::Completed,
            },
        ),
        envelope(
            7,
            Event::WorkflowStageRouteOutcome {
                session,
                run,
                stage: "route".to_string(),
                member,
                role: WorkflowMemberRole::Worker,
                iteration: 0,
                step: 2,
                candidate_index: 0,
                model: "provider/late".into(),
                reasoning: "none".to_string(),
                failure_class: WorkflowRouteFailureClass::None,
            },
        ),
    ];

    let projection = Projection::from_events(&events);
    let stage_projection = &projection
        .session
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.run.as_ref())
        .expect("route run")
        .stages[0];
    assert_eq!(stage_projection.route_outcomes.len(), 2);
    assert_eq!(
        stage_projection.route_outcomes[0],
        WorkflowStageRouteOutcome {
            session,
            run,
            stage: "route".to_string(),
            member,
            role: WorkflowMemberRole::Worker,
            iteration: 0,
            step: 0,
            candidate_index: 1,
            model: "provider/fallback".into(),
            reasoning: "high".to_string(),
            failure_class: WorkflowRouteFailureClass::None,
        }
    );
    assert_eq!(
        stage_projection.route_outcomes[1].role,
        WorkflowMemberRole::Verifier
    );
    assert_eq!(stage_projection.route_outcomes[1].step, 1);
}

/// An old Stage plan and projection omit all additive route fields on re-encode.
#[test]
fn old_workflow_json_reencodes_without_empty_route_fields() {
    let old_plan = r#"{"id":"route","agent":"worker","mode":"once","level":0}"#;
    let plan: WorkflowStagePlan = serde_json::from_str(old_plan).expect("decode old Stage plan");
    assert_eq!(
        serde_json::to_string(&plan).expect("encode old Stage plan"),
        old_plan
    );
    let old_projection = json!({});
    let projection: hya_proto::WorkflowProjection =
        serde_json::from_value(old_projection.clone()).expect("decode old projection");
    assert_eq!(
        serde_json::to_value(projection).expect("encode old projection"),
        old_projection
    );
}

/// Duplicate route keys are idempotent and late/terminal outcomes are ignored.
#[test]
fn route_outcomes_fold_only_for_running_stage_and_dedupe_keys() {
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    let member = MemberId::new();
    let outcome = |model: &str| Event::WorkflowStageRouteOutcome {
        session,
        run,
        stage: "route".to_string(),
        member,
        role: WorkflowMemberRole::Worker,
        iteration: 0,
        step: 1,
        candidate_index: 0,
        model: model.into(),
        reasoning: "none".to_string(),
        failure_class: WorkflowRouteFailureClass::None,
    };
    let projection = Projection::from_events(&[
        envelope(
            1,
            Event::WorkflowRunStarted {
                session,
                run,
                workflow: identity("routes", 1),
                request_hash: "hash".to_string(),
                owner: OwnerRunId::new(),
                stages: vec![stage("route", 0)],
            },
        ),
        envelope(
            2,
            Event::WorkflowStageStarted {
                session,
                run,
                stage: "route".to_string(),
            },
        ),
        envelope(3, outcome("first")),
        envelope(4, outcome("duplicate")),
        envelope(
            5,
            Event::WorkflowStageFinished {
                session,
                run,
                stage: "route".to_string(),
                status: WorkflowStageStatus::Completed,
            },
        ),
        envelope(6, outcome("late")),
    ]);
    let workflow = projection.session.workflow.expect("Workflow projection");
    let run = workflow.run.expect("Workflow run projection");
    let stage = &run.stages[0];
    assert_eq!(stage.route_outcomes.len(), 1);
    assert_eq!(stage.route_outcomes[0].model.as_str(), "first");
}
