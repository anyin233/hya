//! Hot-reload handoff (`hya serve restart` invoked inside a shell turn): the
//! root turn closes at a safe round boundary with cause `handoff` and a
//! pending resume is recorded in the same transaction; the process that drives
//! the continuation acknowledges the row exactly once — with a durable
//! continuation-start marker in the same transaction — so a second restart can
//! checkpoint the session again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{
    AgentName, Event, FinishCause, MemberId, MemberRunStatus, MessageId, OwnerRunId, PartId, Role,
    SessionId, ToolCallId, ToolName, WorkflowIdentity, WorkflowRevision, WorkflowRunId,
    WorkflowRunStatus, WorkflowSourceId, WorkflowStagePlan,
};
use hya_store::{PendingResume, SessionStore, StoreError};

fn temp_db() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "hya-handoff-{nanos}-{}-{id}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn remove_db(path: &str) {
    for suffix in ["", "-wal", "-shm", ".runtime-owner.lock"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

/// One root turn with an open assistant message (the shell call just
/// committed; the next round has not started).
fn open_turn_log(session: SessionId) -> Vec<Event> {
    vec![Event::MessageStarted {
        session,
        message: MessageId::new(),
        role: Role::Assistant,
        agent: None,
        model: None,
    }]
}

/// A running member row on `session`'s log: a resident the resident supervisor
/// started without a tool call.
fn running_member_log(session: SessionId) -> Event {
    Event::MemberSpawned {
        session,
        member: MemberId::new(),
        child: None,
        subagent_type: AgentName::new("scout"),
        description: "scout the area".to_string(),
        depth: 1,
        directive: String::new(),
        tool_call: None,
    }
}

#[tokio::test]
async fn handoff_checkpoint_queues_a_resume_the_continuation_acks_exactly_once() {
    // `FinishCause::Handoff` round-trips as `handoff` on the wire.
    let json = serde_json::to_string(&FinishCause::Handoff).unwrap();
    assert_eq!(json, "\"handoff\"", "handoff uses snake_case on the wire");
    let back: FinishCause = serde_json::from_str(&json).unwrap();
    assert_eq!(back, FinishCause::Handoff);

    let path = temp_db();
    let session = SessionId::new();
    let prompt = "continue where the restart interrupted you";
    let predecessor = OwnerRunId::new();
    {
        let store = SessionStore::connect(&path).await.unwrap();
        store.claim_runtime_owner(predecessor).unwrap();
        for event in open_turn_log(session) {
            store.append_event(session, &event).await.unwrap();
        }

        // The checkpoint closes the open assistant turn with `handoff` and
        // records the pending resume in one transaction.
        let checkpoint = store
            .checkpoint_for_handoff(session, predecessor, 7, prompt.to_string())
            .await
            .unwrap();
        assert_eq!(checkpoint.resume.session, session);
        assert_eq!(checkpoint.resume.owner, predecessor);
        assert_eq!(checkpoint.resume.generation, 7);
        assert_eq!(checkpoint.resume.prompt, prompt);
        let projection = store.read_projection(session).await.unwrap();
        let message = projection.session.messages.last().unwrap();
        assert_eq!(message.cause, Some(FinishCause::Handoff));

        // Exactly one live resume; a second checkpoint for the same session is
        // refused so the successor can never be queued twice.
        let listed = store.list_pending_resumes().await.unwrap();
        assert_eq!(listed, vec![checkpoint.resume.clone()]);
        assert!(matches!(
            store
                .checkpoint_for_handoff(session, predecessor, 8, "again".to_string())
                .await,
            Err(StoreError::ResumeAlreadyPending { .. })
        ));
        // The store handle drops here: the predecessor process "exits".
    }

    // The successor starts, claims the store, and acknowledges the resume.
    let store = SessionStore::connect(&path).await.unwrap();
    let successor = OwnerRunId::new();
    store.claim_runtime_owner(successor).unwrap();

    let start = store
        .begin_handoff_resume(session, successor)
        .await
        .unwrap()
        .expect("the pending resume is acknowledged exactly once");
    assert_eq!(start.resume.session, session);
    assert_eq!(
        start.resume.owner, predecessor,
        "the row survives the cutover"
    );
    assert_eq!(start.resume.generation, 7);
    assert_eq!(start.resume.prompt, prompt);
    assert!(
        matches!(&start.envelope.event, Event::SessionStatus { session: marked, status }
            if *marked == session && status.get("reason").and_then(|r| r.as_str()) == Some("handoff-resume")),
        "the ack appends a durable continuation-start marker: {:?}",
        start.envelope.event
    );
    let marker_in_log = store
        .replay(session)
        .await
        .unwrap()
        .iter()
        .any(|envelope| matches!(envelope.event, Event::SessionStatus { .. }));
    assert!(marker_in_log, "the marker is durable in the event log");
    // The marker touches no transcript row: the continuation stays prompt-less.
    let messages = store
        .read_projection(session)
        .await
        .unwrap()
        .session
        .messages;
    assert_eq!(messages.len(), 1, "the ack added no transcript message");

    // At most once: the row is consumed, later acks find nothing.
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    assert_eq!(
        store
            .begin_handoff_resume(session, successor)
            .await
            .unwrap(),
        None,
        "a resume is acknowledged exactly once"
    );
    assert_eq!(
        store
            .begin_handoff_resume(SessionId::new(), successor)
            .await
            .unwrap(),
        None,
        "an unrelated session has nothing to acknowledge"
    );

    // The in-process re-drive case: this process checkpoints again and
    // acknowledges its OWN row (an aborted handoff re-drives in place).
    let checkpoint = store
        .checkpoint_for_handoff(session, successor, 8, prompt.to_string())
        .await
        .unwrap();
    assert!(
        checkpoint.envelopes.is_empty(),
        "no open turn: no terminal events"
    );
    let start = store
        .begin_handoff_resume(session, successor)
        .await
        .unwrap()
        .expect("an owner acknowledges its own checkpoint");
    assert_eq!(start.resume.owner, successor);
    assert!(store.list_pending_resumes().await.unwrap().is_empty());

    // The consumed row no longer blocks: the next restart checkpoints cleanly
    // instead of failing with `ResumeAlreadyPending`.
    store
        .checkpoint_for_handoff(session, successor, 9, prompt.to_string())
        .await
        .expect("the acknowledged row never blocks a later checkpoint");
    drop(store);
    remove_db(&path);
}

/// A handoff checkpoint refuses a turn whose tool parts are still open —
/// erroring them would discard the tool's result — and once the tool
/// completes, the checkpoint keeps the completed call.
#[tokio::test]
async fn handoff_checkpoint_refuses_a_turn_with_open_tools() {
    let path = temp_db();
    let session = SessionId::new();
    let owner = OwnerRunId::new();
    let store = SessionStore::connect(&path).await.unwrap();
    store.claim_runtime_owner(owner).unwrap();

    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    for event in [
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: ToolName::new("bash"),
        },
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name: ToolName::new("bash"),
            input: serde_json::json!({ "command": "hya serve restart" }),
        },
    ] {
        store.append_event(session, &event).await.unwrap();
    }

    let error = store
        .checkpoint_for_handoff(session, owner, 1, String::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::HandoffBoundaryUnsafe { session: stuck } if stuck == session),
        "an open tool part is not a safe boundary: {error:?}"
    );
    assert!(
        store.list_pending_resumes().await.unwrap().is_empty(),
        "the refused checkpoint queued nothing"
    );

    // The tool completes; the checkpoint now carries its result instead of
    // erroring it away.
    store
        .append_event(
            session,
            &Event::ToolResult {
                session,
                message,
                part,
                call,
                output: serde_json::json!({ "stdout": "restarted" }),
                time_ms: 12,
            },
        )
        .await
        .unwrap();
    let checkpoint = store
        .checkpoint_for_handoff(session, owner, 2, String::new())
        .await
        .unwrap();
    assert!(
        checkpoint
            .envelopes
            .iter()
            .all(|envelope| !matches!(envelope.event, Event::ToolError { .. })),
        "a completed tool call is kept, not errored: {:?}",
        checkpoint.envelopes
    );
    let finished = store
        .begin_handoff_resume(session, owner)
        .await
        .unwrap()
        .expect("the checkpoint queued one resume");
    assert_eq!(finished.resume.generation, 2);
    drop(store);
    remove_db(&path);
}

/// A handoff checkpoint leaves member rows running (the successor's resident
/// recovery revives them); ordinary crash recovery still cancels them.
#[tokio::test]
async fn handoff_checkpoint_leaves_member_rows_running() {
    let path = temp_db();
    let owner = OwnerRunId::new();
    let store = SessionStore::connect(&path).await.unwrap();
    store.claim_runtime_owner(owner).unwrap();

    // Crash recovery: the process is gone, the member row must close.
    let crashed = SessionId::new();
    for event in [
        open_turn_log(crashed).remove(0),
        running_member_log(crashed),
    ] {
        store.append_event(crashed, &event).await.unwrap();
    }
    let envelopes = store
        .close_open_turns(crashed, FinishCause::Interrupted, "interrupted")
        .await
        .unwrap();
    assert!(
        envelopes.iter().any(|envelope| matches!(
            &envelope.event,
            Event::MemberFinished {
                status: MemberRunStatus::Cancelled,
                ..
            }
        )),
        "crash recovery cancels the member row: {:?}",
        envelopes
    );
    let projection = store.read_projection(crashed).await.unwrap();
    assert_eq!(
        projection.session.members[0].status,
        MemberRunStatus::Cancelled
    );

    // Handoff checkpoint: the member stays live across the cutover.
    let handed_off = SessionId::new();
    for event in [
        open_turn_log(handed_off).remove(0),
        running_member_log(handed_off),
    ] {
        store.append_event(handed_off, &event).await.unwrap();
    }
    let checkpoint = store
        .checkpoint_for_handoff(handed_off, owner, 1, String::new())
        .await
        .unwrap();
    assert!(
        checkpoint
            .envelopes
            .iter()
            .all(|envelope| !matches!(envelope.event, Event::MemberFinished { .. })),
        "the checkpoint never closes a member row: {:?}",
        checkpoint.envelopes
    );
    let projection = store.read_projection(handed_off).await.unwrap();
    assert_eq!(
        projection.session.members[0].status,
        MemberRunStatus::Spawning,
        "the member row keeps running across the handoff"
    );
    drop(store);
    remove_db(&path);
}

/// A cutover cannot carry a `Running` Workflow run: the scan names it, and a
/// finished run stops blocking.
#[tokio::test]
async fn sessions_with_running_workflows_reports_only_nonterminal_runs() {
    let path = temp_db();
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    let run = WorkflowRunId::new();
    store
        .append_event(
            session,
            &Event::WorkflowRunStarted {
                session,
                run,
                workflow: WorkflowIdentity {
                    source: WorkflowSourceId::new("test:restart-flow"),
                    name: "restart-flow".to_string(),
                    revision: WorkflowRevision::from_bytes([1; 32]),
                },
                request_hash: "inputs".to_string(),
                owner,
                stages: vec![WorkflowStagePlan {
                    id: "stage".to_string(),
                    title: None,
                    agent: AgentName::new("general"),
                    mode: "once".to_string(),
                    level: 0,
                    worker_model: None,
                    selected_worker_model: None,
                    verifier_model: None,
                    selected_verifier_model: None,
                }],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store.sessions_with_running_workflows().await.unwrap(),
        vec![session],
        "a running Workflow run blocks the cutover"
    );
    store
        .append_event(
            session,
            &Event::WorkflowRunFinished {
                session,
                run,
                status: WorkflowRunStatus::Completed,
                error: None,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .sessions_with_running_workflows()
            .await
            .unwrap()
            .is_empty(),
        "a finished run no longer blocks"
    );
    drop(store);
    remove_db(&path);
}

/// The `PendingResume` shape stays importable for downstream consumers.
#[test]
fn pending_resume_type_is_stable() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<PendingResume>();
}
