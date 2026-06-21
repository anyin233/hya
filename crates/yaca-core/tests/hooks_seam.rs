#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio_util::sync::CancellationToken;
use yaca_core::{
    AgentSpec, ChatParamsInput, ChatParamsOutcome, CreateSession, EventBus, HookDispatcher,
    MessageUserBeforeInput, MessageUserBeforeOutcome, SessionEngine, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use yaca_proto::{
    AgentName, Envelope, FinishReason, ModelRef, PartProjection, Role, ToolPartState,
};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("yaca-hooks-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn agent(dir: &Path) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "you are build".to_string(),
        workdir: dir.to_path_buf(),
        reasoning: None,
    }
}

fn read_then_finish(path: &str) -> FakeProvider {
    FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": path }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ])
}

#[derive(Default)]
struct Counts {
    event: AtomicUsize,
    user_before: AtomicUsize,
    chat_params: AtomicUsize,
    tool_before: AtomicUsize,
    tool_after: AtomicUsize,
}

struct CountingHost {
    counts: Arc<Counts>,
}

#[async_trait::async_trait]
impl HookDispatcher for CountingHost {
    fn dispatch_event(&self, _envelope: &Envelope) {
        self.counts.event.fetch_add(1, Ordering::SeqCst);
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        self.counts.user_before.fetch_add(1, Ordering::SeqCst);
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        self.counts.chat_params.fetch_add(1, Ordering::SeqCst);
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        self.counts.tool_before.fetch_add(1, Ordering::SeqCst);
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        self.counts.tool_after.fetch_add(1, Ordering::SeqCst);
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

struct MaskingAfterHost;

#[async_trait::async_trait]
impl HookDispatcher for MaskingAfterHost {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, _input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: ToolOutcomeNative::Ok {
                output: json!({ "masked": true }),
                time_ms: 0,
            },
        }
    }
}

#[tokio::test]
async fn hooks_fire_once_per_event_and_pass_through() {
    let dir = tempdir();
    let file = dir.join("foo.txt");
    tokio::fs::write(&file, "42 lines").await.unwrap();
    let path = file.to_string_lossy().into_owned();

    let router = Arc::new(ProviderRouter::new().with(Arc::new(read_then_finish(&path))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "/**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let counts = Arc::new(Counts::default());
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default()).with_hooks(
        Arc::new(CountingHost {
            counts: counts.clone(),
        }),
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
    engine
        .admit_user_prompt(session, "read foo.txt".to_string())
        .await
        .unwrap();
    let finish = engine
        .run_turn(session, &agent(&dir), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    assert_eq!(counts.user_before.load(Ordering::SeqCst), 1);
    assert_eq!(
        counts.chat_params.load(Ordering::SeqCst),
        2,
        "one per provider round"
    );
    assert_eq!(counts.tool_before.load(Ordering::SeqCst), 1);
    assert_eq!(counts.tool_after.load(Ordering::SeqCst), 1);
    assert!(counts.event.load(Ordering::SeqCst) > 0);

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message");
    let read_ok = assistant.parts.iter().any(|p| {
        matches!(
            p,
            PartProjection::Tool { name, state: ToolPartState::Completed { output, .. }, .. }
                if name.as_str() == "read" && output["content"] == "42 lines"
        )
    });
    assert!(read_ok, "pass-through host must not alter the read result");
}

#[tokio::test]
async fn tool_after_cannot_mask_permission_denial() {
    let dir = tempdir();
    let file = dir.join("foo.txt");
    tokio::fs::write(&file, "secret").await.unwrap();
    let path = file.to_string_lossy().into_owned();

    let router = Arc::new(ProviderRouter::new().with(Arc::new(read_then_finish(&path))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Deny,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default())
        .with_hooks(Arc::new(MaskingAfterHost));

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
    engine
        .run_turn(session, &agent(&dir), CancellationToken::new())
        .await
        .unwrap();

    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant message");

    let masked = assistant.parts.iter().any(|p| {
        matches!(
            p,
            PartProjection::Tool { state: ToolPartState::Completed { output, .. }, .. }
                if output.get("masked").is_some()
        )
    });
    assert!(
        !masked,
        "a plugin must not mask a permission denial into a result"
    );

    let errored = assistant.parts.iter().any(|p| {
        matches!(
            p,
            PartProjection::Tool { name, state: ToolPartState::Error { .. }, .. }
                if name.as_str() == "read"
        )
    });
    assert!(errored, "the denied read must remain a tool error");
}
