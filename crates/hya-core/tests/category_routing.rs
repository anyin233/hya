#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_core::{
    AgentSpec, CategoryEntry, CategoryRegistry, CreateSession, EventBus, MemberSpec, MemberStatus,
    SessionEngine, build_member_agent, inject_skills, run_team,
};
use hya_proto::{AgentName, Event, FinishReason, MemberId, ModelRef, Role};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio_util::sync::CancellationToken;

/// Build a registry mapping the four logical category names to concrete refs,
/// mirroring what a `categories:` config block produces.
fn config_registry() -> CategoryRegistry {
    let mut overrides = HashMap::new();
    overrides.insert(
        "quick".to_string(),
        CategoryEntry::from_candidates(&["gw/quick-model".to_string()]).unwrap(),
    );
    overrides.insert(
        "deep".to_string(),
        CategoryEntry::from_candidates(&["gw/deep-model".to_string()]).unwrap(),
    );
    overrides.insert(
        "ultrabrain".to_string(),
        CategoryEntry::from_candidates(&["gw/ultra-model".to_string()]).unwrap(),
    );
    overrides.insert(
        "writing".to_string(),
        CategoryEntry::from_candidates(&["gw/writer-model".to_string()]).unwrap(),
    );
    CategoryRegistry::new().with_overrides(overrides)
}

#[test]
fn categories_resolve_to_distinct_configured_models() {
    let reg = config_registry();
    let cats = ["quick", "deep", "ultrabrain", "writing"];
    let models: HashSet<String> = cats
        .iter()
        .map(|c| reg.resolve(c).unwrap().model.to_string())
        .collect();
    assert_eq!(
        models.len(),
        4,
        "each configured category must map to a distinct concrete model"
    );

    let deep = reg.resolve("deep").unwrap();
    assert_eq!(deep.fallback_chain.first().unwrap(), &deep.model);
    // An unconfigured category does not resolve; the caller falls back to the
    // global default rather than a dangling placeholder ref.
    assert!(reg.resolve("nonexistent").is_none());
    assert!(CategoryRegistry::new().is_empty());
}

#[test]
fn category_failover_picks_first_servable_candidate() {
    // A category with two ordered candidates: prefer #1, fail over to #2 when
    // #1's provider is not configured/servable.
    let mut overrides = HashMap::new();
    overrides.insert(
        "deep".to_string(),
        CategoryEntry::from_candidates(&["primary/opus".to_string(), "backup/sonnet".to_string()])
            .unwrap(),
    );
    let reg = CategoryRegistry::new().with_overrides(overrides);

    // Both providers configured → first candidate wins.
    let both = reg.resolve_servable("deep", |_| true).unwrap();
    assert_eq!(both.model, ModelRef::new("primary/opus"));

    // Primary provider absent → fail over to the second candidate.
    let failover = reg
        .resolve_servable("deep", |m| m.as_str() != "primary/opus")
        .unwrap();
    assert_eq!(failover.model, ModelRef::new("backup/sonnet"));

    // No candidate servable → best-effort first candidate (stream errors for
    // real instead of silently misrouting).
    let none = reg.resolve_servable("deep", |_| false).unwrap();
    assert_eq!(none.model, ModelRef::new("primary/opus"));
    assert_eq!(none.fallback_chain.len(), 2);
}

#[test]
fn skills_and_prompt_append_are_injected_into_member_prompt() {
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base"),
        system_prompt: "base prompt".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    let mut overrides = HashMap::new();
    overrides.insert(
        "deep".to_string(),
        CategoryEntry::new("gw/deep-model", "Think deeply and thoroughly."),
    );
    let reg = CategoryRegistry::new().with_overrides(overrides);
    let resolved = reg.resolve("deep").unwrap();
    let agent = build_member_agent(&base, &resolved, &["use-the-foo-skill".to_string()]);
    assert_eq!(agent.model, ModelRef::new("gw/deep-model"));
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
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
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

    let reg = config_registry();
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
                description: String::new(),
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
            "gw/quick-model".to_string(),
            "gw/deep-model".to_string(),
            "gw/ultra-model".to_string(),
            "gw/writer-model".to_string(),
        ])
    );
}
