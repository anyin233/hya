//! Integration tests for `hya-core`: tool filtering.

#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, Role};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
    ReasoningEffort,
};
use hya_store::SessionStore;
use hya_tool::{InteractionPlane, PermissionPlane, PermissionRules, ToolRegistry};
use tokio_util::sync::CancellationToken;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-core-tool-filtering-{nanos}-{serial}-{}",
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
            reasoning_request: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

/// Capture the completion request emitted for one model and Agent reasoning default.
async fn completion_request(
    provider_id: &'static str,
    model: &str,
    reasoning: Option<ReasoningEffort>,
    interactive: bool,
) -> CompletionRequest {
    let dir = tempdir();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
        id: provider_id,
        requests: requests.clone(),
    })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let (interaction, questions) = InteractionPlane::new();
    if !interactive {
        drop(questions);
    }
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    )
    .with_interaction(interaction);

    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new(model),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "record request".to_string())
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
                reasoning,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    requests.lock().unwrap()[0].clone()
}

/// Return the model-facing Tool ids emitted for one model.
async fn tool_ids(provider_id: &'static str, model: &str) -> Vec<String> {
    completion_request(provider_id, model, None, false)
        .await
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

#[tokio::test]
async fn runtime_tool_request_advertises_hashline_write_edit_for_every_model() {
    for model in ["gpt-5", "gpt-4o", "gpt-5.6-sol", "glm-5.3"] {
        let ids = tool_ids("recording", model).await;
        assert!(
            ids.contains(&"edit".to_string()) && ids.contains(&"write".to_string()),
            "{model} must advertise hashline write/edit"
        );
        assert!(
            !ids.contains(&"apply_patch".to_string()),
            "{model} must not advertise apply_patch"
        );
    }
}

#[tokio::test]
async fn runtime_model_variant_overrides_agent_reasoning_default() {
    let request =
        completion_request("recording", "gpt-5#low", Some(ReasoningEffort::High), false).await;

    assert_eq!(request.reasoning, Some(ReasoningEffort::Low));
}

#[tokio::test]
async fn runtime_invalid_variant_sends_no_effort_instead_of_the_default() {
    let request = completion_request(
        "recording",
        "gpt-5#bogus",
        Some(ReasoningEffort::High),
        false,
    )
    .await;

    assert_eq!(request.reasoning, None);
}

#[tokio::test]
async fn runtime_tool_request_exposes_websearch_for_every_provider() {
    let compat_ids = tool_ids("compat", "test").await;
    assert!(compat_ids.contains(&"websearch".to_string()));

    let anthropic_ids = tool_ids("anthropic", "test").await;
    assert!(anthropic_ids.contains(&"websearch".to_string()));
}

#[tokio::test]
async fn runtime_only_offers_human_answer_tools_with_a_question_receiver() {
    for interactive in [false, true] {
        let request = completion_request("recording", "test", None, interactive).await;
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        for name in ["ask_user", "plan_exit"] {
            assert_eq!(
                names.contains(&name),
                interactive,
                "{name}, interactive={interactive}"
            );
        }
        assert!(names.contains(&"read"));
    }
}
