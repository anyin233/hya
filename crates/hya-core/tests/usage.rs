//! Provider usage is read from the event-sourced session projection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef, TokenUsage};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use tokio_util::sync::CancellationToken;

fn tempdir() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-usage-{nanos}-{seq}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn engine_with(script: Vec<FakeStep>) -> (SessionEngine, PathBuf) {
    let dir = tempdir();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(script))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    (engine, dir)
}

async fn run_turn(
    engine: &SessionEngine,
    dir: &std::path::Path,
    model: &str,
) -> hya_proto::SessionId {
    let agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new(model),
        system_prompt: "x".to_string(),
        workdir: dir.to_path_buf(),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "hello please answer".to_string())
        .await
        .unwrap();
    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    session
}

#[tokio::test]
async fn projection_records_provider_reported_usage() {
    let (engine, dir) = engine_with(vec![
        FakeStep::Text("measured answer".to_string()),
        FakeStep::Usage(TokenUsage {
            input: 120,
            output: 34,
            reasoning: 0,
            cache_read: 10,
            cache_write: 5,
            reasoning_unknown: false,
        }),
        FakeStep::Finish(FinishReason::Stop),
    ])
    .await;
    let session = run_turn(&engine, &dir, "fake").await;
    let projection = engine.read_projection(session).await.unwrap();
    let usage = &projection.session.usage;
    assert_eq!(usage.by_model.len(), 1);
    let totals = usage.by_model[&ModelRef::new("fake")];
    assert_eq!(totals.input, 120);
    assert_eq!(totals.cache_read, 10);
    assert_eq!(totals.cache_write, 5);
    assert_eq!(totals.prompt(), 135);
    assert_eq!(totals.output, 34);
    assert_eq!(
        totals.rounds, 1,
        "message finish must not count usage again"
    );
    assert_eq!(totals.legacy_messages, 0);
    assert_eq!(usage.by_purpose[&hya_proto::UsagePurpose::Turn], totals);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn missing_provider_usage_is_not_estimated() {
    for model in ["qwen/family-test", "somecustom/mystery-9b"] {
        let (engine, dir) = engine_with(vec![
            FakeStep::Text("plain answer".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ])
        .await;
        let session = run_turn(&engine, &dir, model).await;
        let projection = engine.read_projection(session).await.unwrap();
        assert!(projection.session.usage.is_empty(), "{model}");
        assert_eq!(projection.session.usage.total(), Default::default());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
