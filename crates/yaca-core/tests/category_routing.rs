#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use yaca_core::{
    AgentSpec, CategoryRegistry, CreateSession, EventBus, MemberSpec, MemberStatus, SessionEngine,
    build_member_agent, inject_skills, run_team,
};
use yaca_proto::{AgentName, Event, FinishReason, MemberId, ModelRef, Role};
use yaca_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

#[test]
fn categories_resolve_to_distinct_models_with_prompt_appends() {
    let reg = CategoryRegistry::builtins();
    let cats = ["quick", "deep", "ultrabrain", "writing"];
    let models: HashSet<String> = cats
        .iter()
        .map(|c| reg.resolve(c).unwrap().model.to_string())
        .collect();
    assert_eq!(
        models.len(),
        4,
        "each category must map to a distinct model"
    );

    let deep = reg.resolve("deep").unwrap();
    assert!(!deep.prompt_append.is_empty());
    assert_eq!(deep.fallback_chain.first().unwrap(), &deep.model);
    assert!(reg.resolve("nonexistent").is_none());
}

#[test]
fn skills_are_injected_into_member_prompt() {
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base"),
        system_prompt: "base prompt".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let resolved = CategoryRegistry::builtins().resolve("deep").unwrap();
    let agent = build_member_agent(&base, &resolved, &["use-the-foo-skill".to_string()]);
    assert_eq!(agent.model, ModelRef::new("tier-strong"));
    assert!(agent.system_prompt.contains("base prompt"));
    assert!(agent.system_prompt.contains("Think deeply"));
    assert!(agent.system_prompt.contains("use-the-foo-skill"));

    assert_eq!(inject_skills("p", &[]), "p");
}

struct RecordingProvider {
    models: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
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
        self.models.lock().unwrap().push(req.model.to_string());
        let event = Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
        };
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

#[tokio::test]
async fn four_categories_drive_four_distinct_model_calls() {
    let models = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
        models: models.clone(),
    })));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        tools,
        perm,
        EventBus::default(),
    ));

    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("base"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();

    let reg = CategoryRegistry::builtins();
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let specs: Vec<MemberSpec> = ["quick", "deep", "ultrabrain", "writing"]
        .iter()
        .map(|cat| {
            let resolved = reg.resolve(cat).unwrap();
            MemberSpec {
                id: MemberId::new(),
                agent: build_member_agent(&base, &resolved, &[]),
                directive: format!("work as {cat}"),
                session: None,
            }
        })
        .collect();

    let evidence = run_team(engine, lead, specs, CancellationToken::new()).await;
    assert_eq!(evidence.len(), 4);
    assert!(evidence.iter().all(|e| e.status == MemberStatus::Done));

    let recorded: HashSet<String> = models.lock().unwrap().iter().cloned().collect();
    assert_eq!(
        recorded,
        HashSet::from([
            "tier-cheap".to_string(),
            "tier-strong".to_string(),
            "tier-max".to_string(),
            "tier-writer".to_string(),
        ])
    );
}
