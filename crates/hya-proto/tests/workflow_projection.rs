//! Durable Workflow state is a replay-only Session projection.

#![allow(clippy::expect_used)]

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MemberId, MessageId, OwnerRunId, PartId, Projection,
    Role, SessionId, WorkflowAvailability, WorkflowIdentity, WorkflowMemberRole, WorkflowRevision,
    WorkflowRunId, WorkflowRunStatus, WorkflowSourceId, WorkflowStagePlan, WorkflowStageStatus,
};

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
