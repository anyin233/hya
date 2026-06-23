#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, Event, FinishReason, ModelRef, Role};
use yaca_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "yaca-core-tool-filtering-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct RecordingProvider {
    id: &'static str,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: yaca_proto::SessionId,
        message: yaca_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
            },
        )])))
    }
}

async fn tool_ids(provider_id: &'static str, model: &str) -> Vec<String> {
    let dir = tempdir();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
        id: provider_id,
        requests: requests.clone(),
    })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new(model),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "record tools".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new(model),
                system_prompt: "x".to_string(),
                workdir: dir,
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    requests.lock().unwrap()[0]
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

#[tokio::test]
async fn runtime_tool_request_filters_patch_tools_by_model() {
    let gpt_ids = tool_ids("recording", "gpt-5").await;
    assert!(gpt_ids.contains(&"apply_patch".to_string()));
    assert!(!gpt_ids.contains(&"edit".to_string()));
    assert!(!gpt_ids.contains(&"write".to_string()));

    let gpt4_ids = tool_ids("recording", "gpt-4o").await;
    assert!(!gpt4_ids.contains(&"apply_patch".to_string()));
    assert!(gpt4_ids.contains(&"edit".to_string()));
    assert!(gpt4_ids.contains(&"write".to_string()));
}

#[tokio::test]
async fn runtime_tool_request_filters_websearch_by_provider() {
    let opencode_ids = tool_ids("opencode", "test").await;
    assert!(opencode_ids.contains(&"websearch".to_string()));

    let anthropic_ids = tool_ids("anthropic", "test").await;
    assert!(!anthropic_ids.contains(&"websearch".to_string()));
}
