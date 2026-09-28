//! Engine-level regression coverage for the read-only project activity tool.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, FinishReason, ModelRef, PartProjection, ProjectId, Role, SessionKind, ToolPartState,
};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn agent(workdir: &std::path::Path) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "You are build".to_string(),
        workdir: workdir.to_path_buf(),
        reasoning: None,
    }
}

async fn session(
    engine: &SessionEngine,
    workdir: &std::path::Path,
    project: ProjectId,
    parent: Option<hya_proto::SessionId>,
) -> hya_proto::SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: Some(project),
            kind: SessionKind::Project,
        })
        .await
        .expect("session")
}

async fn run(engine: &SessionEngine, id: hya_proto::SessionId, workdir: &std::path::Path) {
    engine
        .admit_user_prompt(id, "do the requested work".to_string())
        .await
        .expect("prompt");
    engine
        .run_turn(id, &agent(workdir), CancellationToken::new())
        .await
        .expect("turn");
}

async fn tool_output(engine: &SessionEngine, id: hya_proto::SessionId, name: &str) -> Value {
    let projection = engine
        .store()
        .read_projection(id)
        .await
        .expect("projection");
    projection
        .session
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .flat_map(|message| message.parts.iter())
        .find_map(|part| match part {
            PartProjection::Tool {
                name: tool,
                state: ToolPartState::Completed { output, .. },
                ..
            } if tool.as_str() == name => Some(output.clone()),
            _ => None,
        })
        .expect("completed tool output")
}

#[tokio::test]
async fn project_activity_lists_project_peer_and_created_file_but_not_outsider_or_self() {
    let root = support::TestDir::new("project-activity");
    let outsider_root = support::TestDir::new("project-activity-outsider");
    let store = SessionStore::connect_memory().await.expect("store");
    let project_p = store
        .create_project("P", &[root.path().to_string_lossy().into_owned()])
        .await
        .expect("project");
    let project_q = store
        .create_project("Q", &[outsider_root.path().to_string_lossy().into_owned()])
        .await
        .expect("project");

    let path = root.path().join("created.txt");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "write".to_string(),
                input: json!({"path": path, "content": "created"}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("write complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::ToolCall {
                name: "project_activity".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("activity complete".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (permission, _asks) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Edit,
        "/**",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    );
    let a = session(&engine, root.path(), project_p.id, None).await;
    let b = session(&engine, root.path(), project_p.id, None).await;
    let child = session(&engine, root.path(), project_p.id, Some(a)).await;
    let outsider = session(&engine, outsider_root.path(), project_q.id, None).await;

    run(&engine, b, root.path()).await;
    run(&engine, a, root.path()).await;

    let output = tool_output(&engine, a, "project_activity").await;
    let sessions = output["sessions"].as_array().expect("sessions");
    let peer = sessions
        .iter()
        .find(|row| row["id"] == b.to_string())
        .expect("peer B");
    assert_eq!(peer["relation"], "unrelated");
    assert_eq!(peer["status"], "idle");
    assert!(!sessions.iter().any(|row| row["id"] == a.to_string()));
    assert!(!sessions.iter().any(|row| row["id"] == outsider.to_string()));
    let sub = sessions
        .iter()
        .find(|row| row["id"] == child.to_string())
        .expect("subagent child of A");
    assert_eq!(sub["relation"], "child");
    assert_eq!(sub["parent"], a.to_string());
    assert_eq!(sub["lineage_root"], a.to_string());
    assert_eq!(peer["lineage_root"], b.to_string());

    let files = output["files"].as_array().expect("files");
    let file = files
        .iter()
        .find(|row| row["session"] == b.to_string())
        .expect("created file");
    assert_eq!(file["change_kind"], "created");
    assert!(
        file["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("created.txt"))
    );
}
