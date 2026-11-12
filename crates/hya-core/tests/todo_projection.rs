//! Todo lists are event-sourced: a todo tool call records `TodosUpdated` and
//! the list survives a process restart (the next edit builds on it).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, SessionId, TodoItem, TodoStatus};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-core-todo-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fresh engine (and so a fresh in-memory todo plane) over `store`.
fn engine(store: SessionStore, script: Vec<Vec<FakeStep>>) -> SessionEngine {
    let router =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(script))));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::TodoWrite,
        "*",
        Mode::Allow,
    )]));
    SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
}

fn call(name: &str, input: serde_json::Value) -> Vec<Vec<FakeStep>> {
    vec![
        vec![
            FakeStep::ToolCall {
                name: name.to_string(),
                input,
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("ok".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]
}

async fn turn(engine: &SessionEngine, session: SessionId, dir: &std::path::Path) {
    engine
        .admit_user_prompt(session, "go".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir.to_path_buf(),
        reasoning: None,
    };
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
}

fn item(id: &str, content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        id: id.to_string(),
        content: content.to_string(),
        status,
    }
}

#[tokio::test]
async fn the_todo_list_survives_a_restart_and_the_next_edit_builds_on_it() {
    let dir = tempdir();
    let store = SessionStore::connect_memory().await.unwrap();
    let first = engine(
        store.clone(),
        call(
            "todo__update_content",
            json!({"operations": [
                {"op": "add", "content": "first"},
                {"op": "add", "content": "second"}
            ]}),
        ),
    );
    let session = first
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    turn(&first, session, &dir).await;
    drop(first);

    // "Restart": a new engine whose todo plane holds nothing for the session.
    let second = engine(
        store.clone(),
        call(
            "todo__update_status",
            json!({"updates": [{"id": "2", "status": "completed"}]}),
        ),
    );
    turn(&second, session, &dir).await;

    let expected = vec![
        item("1", "first", TodoStatus::Pending),
        item("2", "second", TodoStatus::Completed),
    ];
    assert_eq!(second.todos(session).await, expected);
    let projection = store.read_projection(session).await.unwrap();
    assert_eq!(projection.session.todos, Some(expected));
    let updates = store
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter(|envelope| matches!(envelope.event, Event::TodosUpdated { .. }))
        .count();
    assert_eq!(updates, 2);
    let _ = std::fs::remove_dir_all(&dir);
}
