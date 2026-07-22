#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{
    AgentSpec, CompactionConfig, CoreError, CreateSession, EventBus, SessionEngine, Summarizer,
};
use hya_proto::{
    AgentName, FinishReason, Message, ModelRef, PartProjection, Role, TokenUsage, ToolPartState,
};
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
    let dir = std::env::temp_dir().join(format!("hya-core-test-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn text_tool_result_text_round_trip() {
    let dir = tempdir();
    let file = dir.join("foo.txt");
    tokio::fs::write(&file, "42 lines").await.unwrap();
    let path = file.to_string_lossy().into_owned();

    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("I'll read it".to_string()),
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": path }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("The file says 42 lines".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);

    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
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

    engine
        .admit_user_prompt(session, "read foo.txt".to_string())
        .await
        .unwrap();

    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "you are build".to_string(),
        workdir: dir.clone(),
        reasoning: None,
    };
    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message");

    let completed_read = assistant.parts.iter().any(|p| {
        matches!(
            p,
            PartProjection::Tool { name, state: ToolPartState::Completed { output, .. }, .. }
                if name.as_str() == "read" && output["content"] == "42 lines"
        )
    });
    assert!(completed_read, "expected a completed read tool part");

    let final_text = assistant
        .parts
        .iter()
        .any(|p| matches!(p, PartProjection::Text { text, .. } if text.contains("42 lines")));
    assert!(final_text, "expected final assistant text");
}

#[tokio::test]
async fn turn_continues_past_twenty_five_tool_rounds() {
    let dir = tempdir();
    let mut scripts = (0..26)
        .map(|_| {
            vec![
                FakeStep::ToolCall {
                    name: "unknown".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        })
        .collect::<Vec<_>>();
    let final_text = "continued after twenty-five tool rounds";
    scripts.push(vec![
        FakeStep::Text(final_text.to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]);

    let provider = FakeProvider::scripted_turns(scripts);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
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

    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("assistant message");
    assert!(
        assistant
            .parts
            .iter()
            .any(|part| matches!(part, PartProjection::Text { text, .. } if text == final_text)),
        "expected final response after the tool rounds"
    );
}

#[tokio::test]
async fn cancelled_turn_finishes_cancelled() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("hi".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
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
    let cancel = CancellationToken::new();
    cancel.cancel();
    let finish = engine.run_turn(session, &agent, cancel).await.unwrap();
    assert_eq!(finish, FinishReason::Cancelled);
}

#[tokio::test]
async fn provider_usage_is_recorded_on_assistant_message_projection() {
    let dir = tempdir();
    let usage = TokenUsage {
        input: 11,
        output: 3,
        reasoning: 2,
        cache_read: 5,
        cache_write: 0,
    };
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("hi".to_string()),
        FakeStep::Usage(usage),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
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

    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("assistant message");

    assert_eq!(assistant.tokens, Some(usage));
}

struct Recording(Arc<AtomicBool>);

#[async_trait::async_trait]
impl Summarizer for Recording {
    async fn summarize(&self, _messages: &[Message]) -> Result<String, CoreError> {
        self.0.store(true, Ordering::SeqCst);
        Ok("SUMMARY".to_string())
    }
}

#[tokio::test]
async fn compaction_auto_triggers_when_over_threshold() {
    let dir = tempdir();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default())
        .with_compaction(
            Arc::new(Recording(called.clone())),
            CompactionConfig {
                token_threshold: 1,
                keep_recent: 1,
            },
        );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    for _ in 0..3 {
        engine
            .admit_user_prompt(session, "some earlier message text".to_string())
            .await
            .unwrap();
    }
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        called.load(Ordering::SeqCst),
        "summarizer must be invoked when over threshold"
    );
}

#[tokio::test]
async fn provider_error_still_finishes_the_assistant_message() {
    let dir = tempdir();
    let router = Arc::new(ProviderRouter::new());
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("no-such-model"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "hello".to_string())
        .await
        .unwrap();

    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("no-such-model"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    let result = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await;
    assert!(result.is_err(), "an unresolved model must surface an error");

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message exists");
    assert_eq!(
        assistant.finish,
        Some(FinishReason::Error),
        "the assistant message must be terminally finished on provider error so UI clients never hang"
    );
}
