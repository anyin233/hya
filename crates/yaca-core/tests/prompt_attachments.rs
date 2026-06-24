#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio_util::sync::CancellationToken;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, Event, FinishReason, Message, ModelRef, Part, Role};
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
        "yaca-core-prompt-attachments-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
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
                tokens: None,
            },
        )])))
    }
}

#[tokio::test]
async fn opencode_prompt_files_are_replayed_as_media_parts() {
    let dir = tempdir();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
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
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let message = engine
        .admit_user_prompt(session, "inspect attached image".to_string())
        .await
        .unwrap();
    engine
        .record_user_prompt_context(
            session,
            message,
            vec![json!({
                "uri": "data:image/png;base64,aGVsbG8=",
                "mime": "image/png",
                "name": "pixel.png",
                "description": "tiny fixture",
            })],
            Vec::new(),
        )
        .await
        .unwrap();

    engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("fake"),
                system_prompt: "x".to_string(),
                workdir: dir,
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = requests.lock().unwrap();
    let Message::User { parts, .. } = &requests[0].messages[0] else {
        panic!("expected user message");
    };
    assert!(parts.iter().any(|part| {
        matches!(
            part,
            Part::Media { media_type, data, filename, .. }
                if media_type == "image/png"
                    && data == "data:image/png;base64,aGVsbG8="
                    && filename.as_deref() == Some("pixel.png")
        )
    }));
}
