#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
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

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-core-model-selection-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct SelectedModelProvider {
    models: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Provider for SelectedModelProvider {
    fn id(&self) -> &str {
        "selected"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "selected").then_some(Capabilities {
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
        self.models.lock().unwrap().push(req.model.to_string());
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
            },
        )])))
    }
}

#[tokio::test]
async fn run_turn_uses_session_selected_model() {
    let dir = tempdir();
    let models = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectedModelProvider {
        models: models.clone(),
    })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("base"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .switch_model(session, ModelRef::new("selected"))
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "use selected model".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("base"),
                system_prompt: "x".to_string(),
                workdir: dir,
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    assert_eq!(models.lock().unwrap().as_slice(), ["selected"]);
}
