//! Adapter-level contracts for the model `workflow` tool.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hya_proto::{
    ToolCallId, WorkflowCommand, WorkflowCommandResult, WorkflowProjection, WorkflowRevision,
};
use hya_tool::{
    Action, InteractionPlane, LspPlane, MailboxPlane, Mode, PermissionPlane, PermissionRules, Rule,
    SkillPlane, SpawnerPlane, TodoPlane, Tool, ToolCtx, ToolOperation, WebSearchPlane,
    WorkflowHostError, WorkflowPlane, WorkflowRequest, WorkflowRequestSink, WorkflowSendError,
    WorkflowTool,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct CaptureSink {
    commands: Mutex<Vec<WorkflowCommand>>,
    operations: Mutex<Vec<ToolOperation>>,
    state: WorkflowProjection,
}

impl WorkflowRequestSink for CaptureSink {
    fn try_send(&self, request: WorkflowRequest) -> Result<(), WorkflowSendError> {
        self.commands
            .lock()
            .expect("commands lock")
            .push(request.command);
        self.operations
            .lock()
            .expect("operations lock")
            .push(request.operation);
        let _ = request.reply.send(Ok(WorkflowCommandResult::State {
            state: self.state.clone(),
        }));
        Ok(())
    }
}

struct RejectingSink;

impl WorkflowRequestSink for RejectingSink {
    fn try_send(&self, request: WorkflowRequest) -> Result<(), WorkflowSendError> {
        let _ = request.reply.send(Err(WorkflowHostError::new(
            "WORKFLOW_BUSY",
            "another run is active",
        )));
        Ok(())
    }
}

fn context(sink: Arc<CaptureSink>, session: hya_proto::SessionId) -> ToolCtx {
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Task,
        "*",
        Mode::Allow,
    )]));
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let (spawner, _spawner_rx) = SpawnerPlane::new();
    ToolCtx {
        workflows: WorkflowPlane::from_sink(sink).for_session(session),
        permission,
        interaction,
        spawner,
        operation: ToolOperation::from_tool_call(ToolCallId::new()),
        mailbox: MailboxPlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: std::env::temp_dir(),
        cancel: CancellationToken::new(),
    }
}

fn routed_projection() -> WorkflowProjection {
    let session = hya_proto::SessionId::new();
    let run = hya_proto::WorkflowRunId::new();
    let member = hya_proto::MemberId::new();
    let revision = WorkflowRevision::from_bytes([9; 32]);
    serde_json::from_value(json!({
        "run": {
            "id": run,
            "workflow": {
                "source": "test:routed",
                "name": "routed",
                "revision": revision.to_string()
            },
            "request_hash": "hash",
            "owner": hya_proto::OwnerRunId::new(),
            "status": "running",
            "stages": [{
                "id": "execute",
                "agent": "general",
                "mode": "once",
                "level": 0,
                "worker_model": {
                    "id": "fake/primary",
                    "reasoning": "high",
                    "fallback": [{"id": "fake/fallback", "reasoning": "medium"}]
                },
                "selected_worker_model": {
                    "index": 0,
                    "id": "fake/primary",
                    "reasoning": "high"
                },
                "status": "running",
                "members": [{"member": member, "role": "worker", "iteration": 0}],
                "route_outcomes": [{
                    "session": session,
                    "run": run,
                    "stage": "execute",
                    "member": member,
                    "role": "worker",
                    "iteration": 0,
                    "step": 0,
                    "candidate_index": 0,
                    "model": "fake/primary",
                    "reasoning": "high",
                    "failure_class": "none"
                }]
            }]
        }
    }))
    .expect("routed Workflow projection fixture")
}

/// Every model-tool action is translated into the shared proto command and
/// receives a shared proto result rather than an adapter-local DTO.
#[tokio::test]
async fn frames_all_workflow_commands_and_preserves_operation_identity() {
    let sink = Arc::new(CaptureSink::default());
    let session = hya_proto::SessionId::new();
    let ctx = context(Arc::clone(&sink), session);
    let tool = WorkflowTool;
    let revision = WorkflowRevision::from_bytes([7; 32]);

    for input in [
        json!({"action": "list"}),
        json!({"action": "info", "name": "alpha"}),
        json!({
            "action": "select",
            "name": "alpha",
            "expected_revision": revision.to_string()
        }),
        json!({"action": "state"}),
        json!({
            "action": "run",
            "name": "alpha",
            "expected_revision": revision.to_string(),
            "inputs": {"count": 3, "message": "hello"}
        }),
    ] {
        let result = tool.execute(&ctx, input).await.expect("workflow result");
        assert_eq!(result["kind"], "state");
    }

    let operation = ctx.operation;
    let commands = sink.commands.lock().expect("commands lock").clone();
    assert_eq!(
        commands,
        vec![
            WorkflowCommand::List,
            WorkflowCommand::Info {
                name: "alpha".to_string(),
            },
            WorkflowCommand::Select {
                name: "alpha".to_string(),
                expected_revision: Some(revision),
            },
            WorkflowCommand::State,
            WorkflowCommand::Run {
                name: Some("alpha".to_string()),
                expected_revision: Some(revision),
                inputs: BTreeMap::from([
                    ("count".to_string(), "3".to_string()),
                    ("message".to_string(), "hello".to_string()),
                ]),
                run: None,
            },
        ]
    );
    assert_eq!(
        sink.operations.lock().expect("operations lock").as_slice(),
        &[operation; 5]
    );
}

#[tokio::test]
async fn preserves_route_fields_in_shared_workflow_results() {
    let sink = Arc::new(CaptureSink {
        state: routed_projection(),
        ..CaptureSink::default()
    });
    let session = hya_proto::SessionId::new();
    let ctx = context(Arc::clone(&sink), session);

    let result = WorkflowTool
        .execute(&ctx, json!({"action": "state"}))
        .await
        .expect("routed Workflow state result");

    assert_eq!(
        result["state"]["run"]["stages"][0]["worker_model"]["fallback"][0]["id"],
        "fake/fallback"
    );
    assert_eq!(
        result["state"]["run"]["stages"][0]["selected_worker_model"]["reasoning"],
        "high"
    );
    assert_eq!(
        result["state"]["run"]["stages"][0]["route_outcomes"][0]["failure_class"],
        "none"
    );
    assert_eq!(
        sink.operations.lock().expect("operations lock").as_slice(),
        &[ctx.operation]
    );
}

/// The default action remains `list`, and malformed action names fail before
/// a request can reach the application control sink.
#[tokio::test]
async fn defaults_to_list_and_rejects_unknown_action() {
    let sink = Arc::new(CaptureSink::default());
    let ctx = context(Arc::clone(&sink), hya_proto::SessionId::new());
    let tool = WorkflowTool;

    tool.execute(&ctx, json!({}))
        .await
        .expect("default list result");
    assert_eq!(
        sink.commands.lock().expect("commands lock").as_slice(),
        &[WorkflowCommand::List]
    );

    let error = tool
        .execute(&ctx, json!({"action": "other"}))
        .await
        .expect_err("unknown action must fail");
    assert!(
        error
            .to_string()
            .contains("expected list|info|select|run|state")
    );
}

/// App control codes survive the tool plane instead of collapsing to `unknown`.
#[tokio::test]
async fn preserves_structured_workflow_host_error() {
    let session = hya_proto::SessionId::new();
    let mut ctx = context(Arc::new(CaptureSink::default()), session);
    ctx.workflows = WorkflowPlane::from_sink(Arc::new(RejectingSink)).for_session(session);
    let error = WorkflowTool
        .execute(&ctx, json!({"action": "state"}))
        .await
        .expect_err("host rejection");
    assert!(matches!(
        error,
        hya_tool::ToolError::WorkflowControl { ref code, ref message }
            if code == "WORKFLOW_BUSY" && message == "another run is active"
    ));
}
