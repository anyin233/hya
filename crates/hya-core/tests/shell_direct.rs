#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef, PartProjection, Role, ToolPartState};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    Action, Decision, ExactSubject, InvocationPolicy, Mode, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, RememberScope, Rule, ToolRegistry,
};
use tokio_util::sync::CancellationToken;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-core-shell-test-{nanos}-{}",
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

#[tokio::test]
async fn direct_shell_authorizes_once_with_call_correlation() {
    let dir = tempdir();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, mut asks) = PermissionPlane::new_with_policy(
        PermissionRules::default(),
        InvocationPolicy::compile(PermissionModel::Default, Vec::new()).unwrap(),
    );
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        Arc::new(ToolRegistry::builtins()),
        permission,
        EventBus::default(),
    ));
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
    let runner = engine.clone();
    let task = tokio::spawn(async move {
        runner
            .run_shell(
                session,
                &agent,
                "printf direct-policy".to_string(),
                CancellationToken::new(),
            )
            .await
    });

    let request = tokio::time::timeout(std::time::Duration::from_secs(1), asks.recv())
        .await
        .expect("permission request timeout")
        .expect("permission request");
    let correlation = (request.session, request.message_id, request.call_id);
    let remember = request.remember.clone();
    request.reply.send(Decision::AllowOnce).unwrap();
    let (message, finish) = task.await.unwrap().unwrap();

    assert_eq!(finish, FinishReason::Stop);
    assert_eq!(correlation.0, Some(session));
    assert_eq!(correlation.1, Some(message));
    assert!(correlation.2.is_some());
    assert_eq!(
        remember,
        RememberScope::Exact(ExactSubject::new(
            PermissionTarget::Command,
            "printf direct-policy",
        ))
    );
    assert!(asks.try_recv().is_err(), "direct shell must prompt once");
}
