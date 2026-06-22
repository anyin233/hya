#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, FinishReason, ModelRef, PartProjection, Role, ToolPartState};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-core-shell-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn direct_shell_runs_command_and_records_tool_part() {
    // Given
    let dir = tempdir();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };

    // When
    let (assistant, finish) = engine
        .run_shell(
            session,
            &agent,
            "printf direct-shell-ok".to_string(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(finish, FinishReason::Stop);
    let projection = engine.store().read_projection(session).await.unwrap();
    assert!(projection.session.messages.iter().any(|message| {
        message.role == Role::User
            && message.parts.iter().any(|part| {
                matches!(
                    part,
                    PartProjection::Text { text, .. }
                        if text == "The following tool was executed by the user"
                )
            })
    }));
    let assistant_message = projection
        .session
        .messages
        .iter()
        .find(|message| message.id == assistant)
        .expect("assistant shell message");
    assert_eq!(assistant_message.role, Role::Assistant);
    assert_eq!(assistant_message.finish, Some(FinishReason::Stop));
    assert!(assistant_message.parts.iter().any(|part| {
        matches!(
            part,
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "shell" && output["output"].as_str().unwrap().contains("direct-shell-ok")
        )
    }));
}
