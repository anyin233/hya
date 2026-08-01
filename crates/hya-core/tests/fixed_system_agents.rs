#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Fixed Harness system agents (Commit 2): exact stable-ID lookup for
//! `compaction` / `title` / `summary` from the same TurnBinding catalog.
//!
//! These are not agent spawn. Ordinary can_spawn/roster must not list them,
//! and missing definitions must fail closed with AGENT_DEFINITION_MISSING
//! before any provider call.

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::{
    AgentSpec, CompactionConfig, CreateSession, EventBus, ModelSummarizer, RuntimeRegistry,
    SessionEngine, Summarizer,
};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, PartId, Role};
use hya_provider::{
    Capabilities, CompactedWindow, CompletionRequest, EventStream, Provider, ProviderError,
    ProviderRouter, ReasoningEffort,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use tokio_util::sync::CancellationToken;

const TITLE_PROMPT: &str = "FIXED_TITLE_BUNDLE_PROMPT_MARKER";
const COMPACTION_PROMPT: &str = "FIXED_COMPACTION_BUNDLE_PROMPT_MARKER";
const SUMMARY_PROMPT: &str = "FIXED_SUMMARY_BUNDLE_PROMPT_MARKER";
const ROOT_PROMPT: &str = "ROOT_AGENT_SYSTEM_PROMPT_MUST_NOT_BE_COMPACTION";
const HARDCODED_TITLE: &str =
    "You are a title generator. Output only a concise single-line conversation title";
const HARDCODED_COMPACTION: &str = "You compress conversation history. No tools.";

struct CaptureProvider {
    requests: Mutex<Vec<CompletionRequest>>,
}

/// Captures provider-native `/responses/compact` model + system for RED/GREEN.
struct CompactCall {
    model: String,
    system: Option<String>,
}

struct NativeCompactProvider {
    requests: Mutex<Vec<CompletionRequest>>,
    compact_calls: Mutex<Vec<CompactCall>>,
}

fn stream_capture_output(
    request: CompletionRequest,
    session: hya_proto::SessionId,
    message: hya_proto::MessageId,
    sink: &Mutex<Vec<CompletionRequest>>,
) -> Result<EventStream, ProviderError> {
    sink.lock().unwrap().push(request);
    // Emit text so title cleaning and summary injection can succeed.
    Ok(Box::pin(futures::stream::iter([
        Ok(Event::TextDelta {
            session,
            message,
            part: PartId::new(),
            delta: "Captured system agent output".to_string(),
        }),
        Ok(Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
        }),
    ])))
}

#[async_trait]
impl Provider for CaptureProvider {
    fn id(&self) -> &str {
        "capture"
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
        request: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        stream_capture_output(request, session, message, &self.requests)
    }
}

#[async_trait]
impl Provider for NativeCompactProvider {
    fn id(&self) -> &str {
        "native-compact"
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
        request: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        stream_capture_output(request, session, message, &self.requests)
    }

    async fn compact_responses(
        &self,
        model: &ModelRef,
        _messages: &[hya_proto::Message],
        system: Option<&str>,
    ) -> Result<Option<CompactedWindow>, ProviderError> {
        self.compact_calls.lock().unwrap().push(CompactCall {
            model: model.to_string(),
            system: system.map(str::to_string),
        });
        Ok(Some(CompactedWindow {
            items: vec![serde_json::json!({
                "role": "user",
                "content": "native-compacted-window"
            })],
        }))
    }
}

#[derive(Clone)]
struct AgentFixture {
    stable_id: String,
    role: AgentRole,
    prompt: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
    can_spawn: Vec<String>,
}

impl AgentFixture {
    fn main(stable_id: impl Into<String>) -> Self {
        Self {
            stable_id: stable_id.into(),
            role: AgentRole::Main,
            prompt: None,
            model: None,
            reasoning: None,
            can_spawn: Vec::new(),
        }
    }

    fn system(stable_id: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            stable_id: stable_id.into(),
            role: AgentRole::Subagent,
            prompt: Some(prompt.into()),
            model: None,
            reasoning: None,
            can_spawn: Vec::new(),
        }
    }

    fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    fn reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    fn can_spawn(mut self, ids: &[&str]) -> Self {
        self.can_spawn = ids.iter().map(|id| (*id).to_string()).collect();
        self
    }
}

fn catalog(agents: &[AgentFixture]) -> Arc<BundleCatalog> {
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/fixed-system-agents".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: agents
            .iter()
            .map(|agent| PreparedAgent {
                local_id: agent.stable_id.clone(),
                stable_id: AgentName::new(&agent.stable_id),
                description: None,
                role: agent.role,
                color: None,
                prompt: agent.prompt.clone(),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy {
                    model: agent.model.clone(),
                    category: None,
                    reasoning: agent.reasoning.clone(),
                },
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view: ResourceView::default(),
                can_spawn: agent
                    .can_spawn
                    .iter()
                    .map(|id| AgentName::new(id.as_str()))
                    .collect(),
                hook_refs: Vec::new(),
            })
            .collect(),
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    Arc::new(BundleCatalog::from_prepared(&[bundle]).expect("valid fixed-system catalog"))
}

async fn engine_with(
    catalog: Arc<BundleCatalog>,
    provider: Arc<dyn Provider>,
    with_summarizer: bool,
) -> SessionEngine {
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        catalog,
    ));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let mut engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router.clone(),
        runtime,
        permission,
        EventBus::default(),
    );
    if with_summarizer {
        let summarizer: Arc<dyn Summarizer> = Arc::new(ModelSummarizer::new(
            router,
            ModelRef::new("summarizer-fallback-model"),
        ));
        engine = engine.with_compaction(
            summarizer,
            CompactionConfig {
                token_threshold: 1,
                keep_recent: 1,
            },
        );
    }
    engine
}

async fn engine_with_capture(
    catalog: Arc<BundleCatalog>,
    provider: Arc<CaptureProvider>,
    with_summarizer: bool,
) -> SessionEngine {
    engine_with(catalog, provider as Arc<dyn Provider>, with_summarizer).await
}

fn base_agent(workdir: &std::path::Path) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("session-model"),
        system_prompt: ROOT_PROMPT.to_string(),
        workdir: workdir.to_path_buf(),
        reasoning: None,
    }
}

#[tokio::test]
async fn auto_title_exact_resolves_title_bundle_prompt_model_and_reasoning() {
    let workdir = support::TestDir::new("fixed-title-full");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build").can_spawn(&["general"]),
            AgentFixture::system("title", TITLE_PROMPT)
                .model("title-bundle-model")
                .reasoning("high"),
            AgentFixture::main("general"),
        ]),
        provider.clone(),
        false,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-fallback-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "debug production 500 errors".to_string())
        .await
        .unwrap();

    let titled = engine
        .auto_title_session(session, &ModelRef::new("session-fallback-model"))
        .await
        .expect("title definition present");
    assert!(titled, "auto title should apply a cleaned title");

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one title completion");
    let req = &requests[0];
    assert_eq!(
        req.system.as_deref(),
        Some(TITLE_PROMPT),
        "title must send prepared Bundle system prompt, not hardcoded TITLE_SYSTEM_PROMPT"
    );
    assert!(
        !req.system
            .as_deref()
            .unwrap_or("")
            .contains(HARDCODED_TITLE),
        "hardcoded title system prompt must not be used: {:?}",
        req.system
    );
    assert_eq!(req.model.as_str(), "title-bundle-model");
    assert_eq!(req.reasoning, Some(ReasoningEffort::High));
    assert!(req.tools.is_empty(), "title remains no-tools");
    assert_eq!(req.messages.len(), 1);
}

#[tokio::test]
async fn auto_title_absent_bundle_model_preserves_session_fallback_model() {
    let workdir = support::TestDir::new("fixed-title-fallback-model");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build"),
            AgentFixture::system("title", TITLE_PROMPT),
        ]),
        provider.clone(),
        false,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-fallback-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "hello there".to_string())
        .await
        .unwrap();

    engine
        .auto_title_session(session, &ModelRef::new("session-fallback-model"))
        .await
        .expect("title definition present");

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].model.as_str(),
        "session-fallback-model",
        "absent Bundle model must preserve caller/session fallback"
    );
    assert_eq!(requests[0].system.as_deref(), Some(TITLE_PROMPT));
    assert_eq!(requests[0].reasoning, None);
}

#[tokio::test]
async fn compaction_in_root_turn_uses_compaction_from_captured_binding_not_root_prompt() {
    let workdir = support::TestDir::new("fixed-compaction-turn");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build").can_spawn(&["general"]),
            AgentFixture::system("compaction", COMPACTION_PROMPT)
                .model("compaction-bundle-model")
                .reasoning("medium"),
            AgentFixture::main("general"),
        ]),
        provider.clone(),
        true,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    // Exceed keep_recent + token threshold so local ModelSummarizer runs.
    for i in 0..4 {
        engine
            .admit_user_prompt(session, format!("earlier detail {i} {}", "x".repeat(40)))
            .await
            .unwrap();
    }

    let finish = engine
        .run_turn(
            session,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect("root turn with compaction definition");
    assert_eq!(finish, FinishReason::Stop);

    let requests = provider.requests.lock().unwrap();
    let compaction = requests
        .iter()
        .find(|req| req.system.as_deref() == Some(COMPACTION_PROMPT))
        .expect("compaction completion must use prepared Bundle system prompt");
    assert!(
        !compaction
            .system
            .as_deref()
            .unwrap_or("")
            .contains(HARDCODED_COMPACTION),
        "hardcoded compaction system prompt must not be used"
    );
    assert_ne!(
        compaction.system.as_deref(),
        Some(ROOT_PROMPT),
        "compaction must not reuse the root agent system prompt"
    );
    assert_eq!(compaction.model.as_str(), "compaction-bundle-model");
    assert_eq!(compaction.reasoning, Some(ReasoningEffort::Medium));
    assert!(compaction.tools.is_empty(), "compaction remains no-tools");
}

#[tokio::test]
async fn summarize_session_exact_resolves_summary_from_one_captured_binding() {
    let workdir = support::TestDir::new("fixed-summary-session");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build"),
            AgentFixture::system("summary", SUMMARY_PROMPT)
                .model("summary-bundle-model")
                .reasoning("low"),
        ]),
        provider.clone(),
        true,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "implemented the feature".to_string())
        .await
        .unwrap();

    engine
        .summarize_session(session)
        .await
        .expect("summary definition present");

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one summary completion");
    let req = &requests[0];
    assert_eq!(req.system.as_deref(), Some(SUMMARY_PROMPT));
    assert_eq!(req.model.as_str(), "summary-bundle-model");
    assert_eq!(req.reasoning, Some(ReasoningEffort::Low));
    assert!(req.tools.is_empty());
}

#[tokio::test]
async fn missing_title_definition_fails_before_provider_without_hardcoded_fallback() {
    let workdir = support::TestDir::new("fixed-title-missing");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[AgentFixture::main("build")]),
        provider.clone(),
        false,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "needs a title".to_string())
        .await
        .unwrap();

    let err = engine
        .auto_title_session(session, &ModelRef::new("session-model"))
        .await
        .expect_err("missing title must fail closed");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        err.to_string().contains("title"),
        "error should name the fixed id, got {err}"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "must not call provider or fall back to hardcoded title prompt"
    );
}

#[tokio::test]
async fn missing_compaction_definition_fails_before_provider_without_root_or_hardcoded_fallback() {
    let workdir = support::TestDir::new("fixed-compaction-missing");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[AgentFixture::main("build")]),
        provider.clone(),
        true,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    for i in 0..4 {
        engine
            .admit_user_prompt(session, format!("detail {i} {}", "y".repeat(40)))
            .await
            .unwrap();
    }

    let err = engine
        .run_turn(
            session,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect_err("missing compaction must fail closed when compaction runs");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        err.to_string().contains("compaction"),
        "error should name the fixed id, got {err}"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "must not call provider with hardcoded compaction/root prompts"
    );
}

#[tokio::test]
async fn missing_summary_definition_fails_before_provider_without_hardcoded_fallback() {
    let workdir = support::TestDir::new("fixed-summary-missing");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[AgentFixture::main("build")]),
        provider.clone(),
        true,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "summarize me".to_string())
        .await
        .unwrap();

    let err = engine
        .summarize_session(session)
        .await
        .expect_err("missing summary must fail closed");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        err.to_string().contains("summary"),
        "error should name the fixed id, got {err}"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "must not call provider or fall back to hardcoded summary prompt"
    );
}

#[tokio::test]
async fn fixed_system_ids_remain_absent_from_ordinary_spawnable_roster() {
    let workdir = support::TestDir::new("fixed-roster");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build").can_spawn(&["general", "explore"]),
            AgentFixture::main("general"),
            AgentFixture::main("explore"),
            AgentFixture::system("compaction", COMPACTION_PROMPT),
            AgentFixture::system("title", TITLE_PROMPT),
            AgentFixture::system("summary", SUMMARY_PROMPT),
        ]),
        provider,
        false,
    )
    .await;

    let binding = engine.bind_runtime(workdir.path()).unwrap();
    let roster = engine
        .agent_roster_for_binding(&binding, "build")
        .expect("ordinary roster");
    let names: Vec<&str> = roster.iter().map(|agent| agent.name.as_str()).collect();
    assert_eq!(names.len(), 2, "ordinary roster: {names:?}");
    assert!(names.contains(&"explore") && names.contains(&"general"));
    for reserved in ["compaction", "title", "summary"] {
        assert!(
            !names.contains(&reserved),
            "{reserved} must not appear in ordinary spawnable roster"
        );
        let denied = binding.resolve_spawn("build", reserved);
        assert!(
            denied.is_err(),
            "ordinary spawn of {reserved} must fail: {denied:?}"
        );
        assert!(
            binding.resolve_agent(reserved).is_some(),
            "exact catalog lookup of {reserved} remains available to fixed Harness callsites"
        );
    }
}

#[tokio::test]
async fn provider_native_compact_uses_compaction_prompt_not_root_and_session_model() {
    // Owner requirement applies to the fixed compaction callsite for BOTH the
    // provider-native compact path and the local ModelSummarizer fallback.
    let workdir = support::TestDir::new("fixed-native-compact-prompt");
    let provider = Arc::new(NativeCompactProvider {
        requests: Mutex::new(Vec::new()),
        compact_calls: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[
            AgentFixture::main("build"),
            AgentFixture::system("compaction", COMPACTION_PROMPT)
                .model("compaction-bundle-model")
                .reasoning("medium"),
        ]),
        provider.clone() as Arc<dyn Provider>,
        // Summarizer present only as unused fallback; native compact returns Some.
        true,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    for i in 0..4 {
        engine
            .admit_user_prompt(session, format!("earlier detail {i} {}", "x".repeat(40)))
            .await
            .unwrap();
    }

    let finish = engine
        .run_turn(
            session,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect("native compact with compaction definition");
    assert_eq!(finish, FinishReason::Stop);

    let compact_calls = provider.compact_calls.lock().unwrap();
    assert_eq!(
        compact_calls.len(),
        1,
        "exactly one native compact_responses call per over-threshold round"
    );
    let call = &compact_calls[0];
    assert_eq!(
        call.system.as_deref(),
        Some(COMPACTION_PROMPT),
        "native compact must exact-resolve the fixed compaction prompt, not root"
    );
    assert_ne!(
        call.system.as_deref(),
        Some(ROOT_PROMPT),
        "native compact must not reuse the root agent system prompt"
    );
    assert_eq!(
        call.model.as_str(),
        "session-model",
        "provider-native compact keeps the active session model (route resolution)"
    );
    // Local summarizer fallback must not run when native compact succeeds.
    let stream_requests = provider.requests.lock().unwrap();
    assert!(
        stream_requests
            .iter()
            .all(|req| req.system.as_deref() != Some(COMPACTION_PROMPT)),
        "native path must not also invoke ModelSummarizer with compaction prompt"
    );
}

#[tokio::test]
async fn missing_compaction_definition_fails_before_native_compact_responses() {
    let workdir = support::TestDir::new("fixed-native-compact-missing");
    let provider = Arc::new(NativeCompactProvider {
        requests: Mutex::new(Vec::new()),
        compact_calls: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::main("build")]),
        provider.clone() as Arc<dyn Provider>,
        true,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    for i in 0..4 {
        engine
            .admit_user_prompt(session, format!("detail {i} {}", "y".repeat(40)))
            .await
            .unwrap();
    }

    let err = engine
        .run_turn(
            session,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect_err("missing compaction must fail closed before native compact");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        err.to_string().contains("compaction"),
        "error should name the fixed id, got {err}"
    );
    assert!(
        provider.compact_calls.lock().unwrap().is_empty(),
        "must not call compact_responses when compaction definition is missing"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "must not stream or fall back after missing fixed compaction definition"
    );
}

#[tokio::test]
async fn under_threshold_turn_does_not_require_or_lookup_fixed_compaction() {
    let workdir = support::TestDir::new("fixed-under-threshold-no-lookup");
    let provider = Arc::new(NativeCompactProvider {
        requests: Mutex::new(Vec::new()),
        compact_calls: Mutex::new(Vec::new()),
    });
    // No compaction/title/summary definitions in catalog — under threshold must still run.
    let engine = engine_with(
        catalog(&[AgentFixture::main("build")]),
        provider.clone() as Arc<dyn Provider>,
        true,
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "short".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect("under-threshold turn must not require fixed compaction definition");
    assert_eq!(finish, FinishReason::Stop);
    assert!(
        provider.compact_calls.lock().unwrap().is_empty(),
        "under threshold must not call compact_responses"
    );
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "exactly one ordinary completion stream under threshold"
    );
}

const ORDINARY_GUIDANCE_MARKER: &str = "ORDINARY_PROJECT_GUIDANCE_MUST_NOT_REACH_FIXED_SYSTEM";

#[tokio::test]
async fn fixed_title_summary_compaction_exclude_ordinary_guidance_marker() {
    // Fixed system agents exact-resolve dedicated Bundle prompts only. Even when
    // project AGENTS.md (ordinary guidance source) is present in the workdir,
    // title/summary/compaction must not include that marker.
    let workdir = support::TestDir::new("fixed-no-ordinary-guidance");
    std::fs::write(workdir.path().join("AGENTS.md"), ORDINARY_GUIDANCE_MARKER).unwrap();

    // --- title ---
    let title_provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let title_engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build"),
            AgentFixture::system("title", TITLE_PROMPT),
        ]),
        title_provider.clone(),
        false,
    )
    .await;
    let title_session = title_engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    title_engine
        .admit_user_prompt(title_session, "needs a title".to_string())
        .await
        .unwrap();
    title_engine
        .auto_title_session(title_session, &ModelRef::new("session-model"))
        .await
        .expect("title present");
    {
        let reqs = title_provider.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        let system = reqs[0].system.as_deref().unwrap_or("");
        assert_eq!(system, TITLE_PROMPT);
        assert!(
            !system.contains(ORDINARY_GUIDANCE_MARKER),
            "title must not include ordinary project guidance: {system}"
        );
    }

    // --- summary ---
    let summary_provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let summary_engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build"),
            AgentFixture::system("summary", SUMMARY_PROMPT),
        ]),
        summary_provider.clone(),
        true,
    )
    .await;
    let summary_session = summary_engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    summary_engine
        .admit_user_prompt(summary_session, "summarize me".to_string())
        .await
        .unwrap();
    summary_engine
        .summarize_session(summary_session)
        .await
        .expect("summary present");
    {
        let reqs = summary_provider.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        let system = reqs[0].system.as_deref().unwrap_or("");
        assert_eq!(system, SUMMARY_PROMPT);
        assert!(
            !system.contains(ORDINARY_GUIDANCE_MARKER),
            "summary must not include ordinary project guidance: {system}"
        );
    }

    // --- compaction (local summarizer path under threshold=1) ---
    let compaction_provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let compaction_engine = engine_with_capture(
        catalog(&[
            AgentFixture::main("build"),
            AgentFixture::system("compaction", COMPACTION_PROMPT).model("compaction-bundle-model"),
        ]),
        compaction_provider.clone(),
        true,
    )
    .await;
    let compaction_session = compaction_engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    for i in 0..8 {
        compaction_engine
            .admit_user_prompt(
                compaction_session,
                format!("bulk context line {i} with padding text for tokens"),
            )
            .await
            .unwrap();
    }
    compaction_engine
        .run_turn(
            compaction_session,
            &base_agent(workdir.path()),
            CancellationToken::new(),
        )
        .await
        .expect("compaction turn");
    {
        let reqs = compaction_provider.requests.lock().unwrap();
        let compaction = reqs
            .iter()
            .find(|r| r.system.as_deref() == Some(COMPACTION_PROMPT))
            .expect("compaction completion must use fixed Bundle prompt");
        let system = compaction.system.as_deref().unwrap_or("");
        assert!(
            !system.contains(ORDINARY_GUIDANCE_MARKER),
            "compaction must not include ordinary project guidance: {system}"
        );
        assert!(
            !system.contains(ROOT_PROMPT) || system == COMPACTION_PROMPT,
            "compaction must stay on fixed Bundle prompt only"
        );
    }
}
