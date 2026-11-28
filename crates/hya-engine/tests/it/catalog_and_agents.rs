//! Integration tests for `hya-engine`: agent catalogs, builtins, fixed system
//! agents, historical identity, resource views, effort/model/category routing,
//! catalog scopes, runtime registry/generations/refresh/rebind, root turn
//! bundle precedence & sidecar lifecycle, and bundle API routing.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;
use support::resident::{ResidentTurn, run_residents};
use support::{
    MarkerTool, TestDir, ZERO_RETRY_WAITS, agent_spec, builtin_only_catalog, create_root_session,
    discover_local_skills, engine_with_router_and_runtime, test_catalog, test_runtime,
    test_runtime_with_catalog,
};

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleError, BundleIdentity, BundleSource, ExportKind,
    HandoffPolicy, ModelPolicy, PreparedAgent, PreparedCatalog, PreparedResource, ResourceView,
    SourceFile, prepare_package,
};
use hya_engine::builtin_agents::{builtin_agent_ids, core_agents_preset, preset_agent};
use hya_engine::runtime_registry::RuntimeSourceSkill;
use hya_engine::{
    AgentCatalog, AgentDefinition, AgentEffortSource, AgentModelConfiguration, AgentOrigin,
    AgentSpec, ApiMethod, ApiPathTemplate, ApiScope, BoundSidecarFactory, BundleApiCall,
    BundleApiError, BundleApiProvider, BundleApiReply, BundleApiRequest, CatalogScope,
    CatalogScopeCacheConfig, CategoryEntry, CategoryRegistry, ChatParamsInput, ChatParamsOutcome,
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, CompactionConfig, CoreError,
    CreateSession, EventBus, EvidenceQuality, HookChain, HookDispatcher, LoopConfig, LoopPlanner,
    LoopVerifier, MAX_BUNDLE_API_BODY_BYTES, MessageUserBeforeInput, MessageUserBeforeOutcome,
    ModelSummarizer, PlannerOutput, RunOutcome, RuntimeCandidate, RuntimeCatalogRefresh,
    RuntimePermissionMode, RuntimeRefreshError, RuntimeRegistry, RuntimeSource,
    RuntimeSourceExport, RuntimeSourceId, RuntimeSourceKind, RuntimeSourceOwner, ScopeKey,
    ScopeOverlay, SessionEngine, SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart,
    SourceApi, SourceApis, SourceSchema, Summarizer, TextCompleteInput, TextCompleteOutcome,
    ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome, TurnBinding, VerifierVerdict, apply_agent_model_preference,
    resolve_configured_agent_model, run_loop,
};
use hya_proto::{
    AgentName, CompactionStrategy, ConfigGeneration, Envelope, Event, FinishReason, MessageId,
    ModelRef, PartId, PartProjection, ProjectId, Role, SessionId, SessionKind, TokenUsage,
    ToolCallId, ToolName, ToolPartState, ToolSchema, UsagePurpose,
};
use hya_provider::{
    Capabilities, CompactedWindow, CompletionRequest, EventStream, FakeProvider, FakeStep,
    Provider, ProviderError, ProviderRouter, ReasoningEffort,
};
use hya_store::{OwnerRunId, SessionStore, StoreError, StoreHealth};
use hya_tool::{
    Action, AgentDef, Mode, PermissionPlane, PermissionRules, ResolvedTool, Rule,
    SkillCatalogEntry, SkillCatalogOrigin, Tool, ToolCtx, ToolError, ToolPermission, ToolRegistry,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Compact Shared Fixtures
// ---------------------------------------------------------------------------

fn prep_agent(id: &str) -> PreparedAgent {
    PreparedAgent {
        private: false,
        id: AgentName::new(id),
        description: Some(format!("{id} description")),
        role: AgentRole::Main,
        color: None,
        prompt: None,
        prompt_source: None,
        prompt_digest: None,
        model_policy: ModelPolicy::default(),
        workdir: None,
        resource_view: ResourceView::default(),
        can_spawn: Vec::new(),
        hook_refs: Vec::new(),
        advertise: Vec::new(),
        handoff: HandoffPolicy::Model,
    }
}

fn make_bundle(bundle_id: &str, agent: PreparedAgent, skills: Vec<PreparedResource>) -> Bundle {
    Bundle {
        check: None,
        format_version: 4,
        identity: BundleIdentity {
            id: bundle_id.to_string(),
            version: "1.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        digest: format!("digest-{bundle_id}"),
        agents: vec![agent],
        tools: Vec::new(),
        skills,
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        tui: None,
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        commands: Vec::new(),
        runtime: None,
        permission_modes: vec![],
        overrides: None,
    }
}

fn bundle_skill(bundle_id: &str, local_id: &str, desc: &str, body: &str) -> PreparedResource {
    PreparedResource {
        local_id: local_id.into(),
        stable_id: format!("bundle:{bundle_id}/skill/{local_id}"),
        source_path: format!("resources/skills/{local_id}.md"),
        digest: "test-only".into(),
        content: format!("---\nname: {local_id}\ndescription: {desc}\n---\n{body}\n"),
        binary_base64: None,
        aliases: Vec::new(),
        overrides: None,
    }
}

fn catalog_from_bundles(bundles: &[Bundle]) -> Arc<AgentCatalog> {
    let bc = BundleCatalog::from_prepared(bundles).expect("bundle catalog");
    Arc::new(AgentCatalog::new(Arc::new(bc)).expect("agent catalog"))
}

fn catalog_from_agents(agents: Vec<PreparedAgent>) -> Arc<AgentCatalog> {
    let bundles: Vec<Bundle> = agents
        .into_iter()
        .filter(|a| preset_agent(a.id.as_str()).is_none())
        .map(|a| {
            let id = format!("hya/test-fixture-{}", a.id.as_str());
            make_bundle(&id, a, Vec::new())
        })
        .collect();
    catalog_from_bundles(&bundles)
}

fn catalog_from_verified(catalogs: &[&PreparedCatalog]) -> Arc<AgentCatalog> {
    let bc = BundleCatalog::from_verified_catalogs(catalogs).expect("verified catalog");
    Arc::new(AgentCatalog::new(Arc::new(bc)).expect("agent catalog"))
}

fn local_runtime(tools: ToolRegistry, catalog: Arc<AgentCatalog>) -> RuntimeRegistry {
    let sources: Vec<RuntimeSource> = catalog
        .bundles()
        .bundles()
        .iter()
        .filter_map(|bundle| {
            let skills: Vec<RuntimeSourceSkill> = bundle
                .resources(ExportKind::Skill)
                .iter()
                .map(|r| {
                    let parsed = hya_engine::skill_catalog::parse_skill(&r.content).unwrap();
                    let path =
                        PathBuf::from(format!("bundle:{}/{}", bundle.identity().id, r.source_path));
                    let dir = path.parent().unwrap().to_path_buf();
                    RuntimeSourceSkill::new(
                        r.stable_id.clone(),
                        r.local_id.clone(),
                        r.aliases.clone(),
                        r.digest.clone(),
                        r.content.clone(),
                        SkillCatalogEntry {
                            name: parsed.name,
                            description: parsed.description,
                            content: parsed.content,
                            allowed_tools: parsed.allowed_tools,
                            model: parsed.model,
                            path,
                            dir,
                            origin: SkillCatalogOrigin::Virtual,
                            disable_model_invocation: false,
                        },
                    )
                })
                .collect();
            (!skills.is_empty()).then(|| {
                RuntimeSource::new(
                    RuntimeSourceId::bundle(bundle.identity().id.clone()),
                    [0; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_skills(skills)
            })
        })
        .collect();
    let reg = RuntimeRegistry::new(tools, catalog).with_skill_discovery(
        discover_local_skills,
        hya_engine::skill_catalog::builtin_skills,
    );
    if !sources.is_empty() {
        reg.refresh(|c| c.replace_sources_of_kind(RuntimeSourceKind::Bundle, sources))
            .unwrap();
    }
    reg
}

fn binding_tool_names(b: &TurnBinding) -> BTreeSet<String> {
    b.tool_schemas()
        .into_iter()
        .map(|s| s.name.as_str().into())
        .collect()
}

fn req_tools(r: &CompletionRequest) -> Vec<String> {
    r.tools.iter().map(|s| s.name.as_str().into()).collect()
}

fn binding_skills(b: &TurnBinding) -> BTreeSet<String> {
    b.skills().iter().map(|s| s.name.clone()).collect()
}

fn allow_all() -> PermissionRules {
    PermissionRules::new(vec![
        Rule::new(Action::Tool, "*", Mode::Allow),
        Rule::new(Action::Bash, "**", Mode::Allow),
        Rule::new(Action::Skill, "*", Mode::Allow),
    ])
}

async fn eng_with(cat: Arc<AgentCatalog>, provider: Arc<dyn Provider>) -> SessionEngine {
    engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(provider)),
        test_runtime_with_catalog(Arc::new(hya_tool_loader::builtins()), cat),
        allow_all(),
    )
    .await
}

async fn prompt_turn(
    engine: &SessionEngine,
    sess: SessionId,
    prompt: &str,
    spec: &AgentSpec,
) -> Result<FinishReason, CoreError> {
    engine.admit_user_prompt(sess, prompt.into()).await.unwrap();
    engine.run_turn(sess, spec, CancellationToken::new()).await
}

struct CompactCall {
    model: String,
    system: Option<String>,
}

struct RecordingProvider {
    requests: Mutex<Vec<CompletionRequest>>,
    compact_calls: Mutex<Vec<CompactCall>>,
    turn: AtomicUsize,
    seen_sessions: Mutex<HashSet<SessionId>>,
    per_session_tools: bool,
    first_round_tools: Vec<(String, Value)>,
    finish: FinishReason,
    usage: Option<TokenUsage>,
    max_context: u32,
    native_compact: Option<Value>,
    only_model: Option<&'static str>,
    gate: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl RecordingProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            compact_calls: Mutex::new(Vec::new()),
            turn: AtomicUsize::new(0),
            seen_sessions: Mutex::new(HashSet::new()),
            per_session_tools: false,
            first_round_tools: Vec::new(),
            finish: FinishReason::Stop,
            usage: None,
            max_context: 200_000,
            native_compact: None,
            only_model: None,
            gate: None,
        })
    }
    fn with_finish(mut self: Arc<Self>, finish: FinishReason) -> Arc<Self> {
        Arc::get_mut(&mut self).unwrap().finish = finish;
        self
    }
    fn with_usage(mut self: Arc<Self>, usage: TokenUsage) -> Arc<Self> {
        Arc::get_mut(&mut self).unwrap().usage = Some(usage);
        self
    }
    fn with_native_compact(mut self: Arc<Self>, max_context: u32, payload: Value) -> Arc<Self> {
        let m = Arc::get_mut(&mut self).unwrap();
        m.max_context = max_context;
        m.native_compact = Some(payload);
        self
    }
    fn with_first_tools(
        mut self: Arc<Self>,
        per_session: bool,
        tools: &[(&str, Value)],
    ) -> Arc<Self> {
        let m = Arc::get_mut(&mut self).unwrap();
        m.per_session_tools = per_session;
        m.first_round_tools = tools
            .iter()
            .map(|(n, v)| ((*n).into(), v.clone()))
            .collect();
        self
    }
    fn with_only_model(mut self: Arc<Self>, model: &'static str) -> Arc<Self> {
        Arc::get_mut(&mut self).unwrap().only_model = Some(model);
        self
    }
    fn with_gate(mut self: Arc<Self>, entered: Arc<Notify>, release: Arc<Notify>) -> Arc<Self> {
        Arc::get_mut(&mut self).unwrap().gate = Some((entered, release));
        self
    }
    fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        if self
            .only_model
            .is_some_and(|allowed| model.as_str() != allowed)
        {
            return None;
        }
        Some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            reasoning_request: true,
            max_context: self.max_context,
            ..Capabilities::default()
        })
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        if let Some((entered, release)) = &self.gate {
            entered.notify_one();
            let (release, finish) = (release.clone(), self.finish);
            return Ok(Box::pin(stream::once(async move {
                release.notified().await;
                Ok(Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish,
                    tokens: None,
                    cause: None,
                })
            })));
        }
        let emit_tools = !self.first_round_tools.is_empty()
            && if self.per_session_tools {
                self.seen_sessions.lock().unwrap().insert(session)
            } else {
                self.turn.fetch_add(1, Ordering::SeqCst) == 0
            };
        if emit_tools {
            let mut steps: Vec<FakeStep> = self
                .first_round_tools
                .iter()
                .map(|(name, input)| FakeStep::ToolCall {
                    name: name.clone(),
                    input: input.clone(),
                })
                .collect();
            steps.push(FakeStep::Finish(FinishReason::ToolCalls));
            return Ok(Box::pin(stream::iter(
                FakeProvider::materialize(&steps, session, message)
                    .into_iter()
                    .map(Ok),
            )));
        }
        Ok(Box::pin(stream::iter([
            Ok(Event::TextDelta {
                session,
                message,
                part: PartId::new(),
                delta: "Captured output".into(),
            }),
            Ok(Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: self.finish,
                tokens: self.usage,
                cause: None,
            }),
        ])))
    }
    async fn compact_responses(
        &self,
        model: &ModelRef,
        _: &[hya_proto::Message],
        system: Option<&str>,
    ) -> Result<Option<CompactedWindow>, ProviderError> {
        let Some(payload) = &self.native_compact else {
            return Ok(None);
        };
        self.compact_calls.lock().unwrap().push(CompactCall {
            model: model.to_string(),
            system: system.map(str::to_string),
        });
        Ok(Some(CompactedWindow {
            items: vec![payload.clone()],
        }))
    }
}

struct CountingTool(&'static str, Arc<AtomicUsize>);

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        self.0
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: format!("{} tool", self.0),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, _: &ToolCtx, _: Value) -> Result<Value, ToolError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    }
}

type RefreshFn = Box<dyn Fn(usize, &RuntimeRegistry) -> Result<bool, CoreError> + Send + Sync>;

struct FnRefresh {
    calls: Arc<AtomicUsize>,
    f: RefreshFn,
}

impl FnRefresh {
    fn new(
        calls: Arc<AtomicUsize>,
        f: impl Fn(usize, &RuntimeRegistry) -> Result<bool, CoreError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls,
            f: Box::new(f),
        })
    }
}

#[async_trait]
impl RuntimeCatalogRefresh for FnRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        (self.f)(call, runtime)
    }
}

// ===========================================================================
// Section 1: Builtin Agents, Agent Catalog & Role/Roster
// ===========================================================================

#[test]
fn builtin_agents_preset_and_core_catalog_invariants() {
    const EXPECTED: &[&str] = &[
        "hya-main",
        "hya-reviewer",
        "hya-scout",
        "hya-task",
        "hya-title",
    ];
    assert_eq!(builtin_agent_ids(), EXPECTED);
    assert!(builtin_agent_ids().windows(2).all(|p| p[0] < p[1]));
    for id in EXPECTED {
        assert!(preset_agent(id).is_some(), "missing builtin {id}");
    }
    assert!(preset_agent("no-such-agent").is_none());

    let preset = core_agents_preset().unwrap();
    let fp = hya_bundle::first_party_bundle("hya/core-agents").unwrap();
    assert_eq!(preset.bundle_id(), "hya/core-agents");
    assert!(!preset.prepared_bytes().is_empty() && preset.digest().len() == 64);
    assert!(std::ptr::eq(preset.prepared_bytes(), fp.bytes()) && preset.digest() == fp.digest());

    let mut ordinary = 0;
    for a in preset.agents() {
        let id = a.id.as_str();
        assert!(id.starts_with("hya-"));
        let reserved = id == "hya-title";
        assert_eq!(preset.is_reserved(id), reserved);
        assert_eq!(
            a.role,
            if id == "hya-main" {
                AgentRole::Main
            } else {
                AgentRole::Subagent
            }
        );
        assert!(a.prompt.as_deref().is_some_and(|p| !p.trim().is_empty()));
        if !reserved {
            ordinary += 1;
            assert!(
                a.description
                    .as_deref()
                    .is_some_and(|d| !d.trim().is_empty())
            );
        }
    }
    assert_eq!(ordinary, 4);

    let empty = catalog_from_bundles(&[]);
    let preset_ids: Vec<_> = preset.agents().iter().map(|a| a.id.as_str()).collect();
    let cat_ids: Vec<_> = empty.all().iter().map(|a| a.stable_id).collect();
    assert_eq!(cat_ids, preset_ids);
    for d in empty.all() {
        assert_eq!(d.origin, AgentOrigin::Builtin);
        assert!(
            d.model_policy.model.is_none()
                && d.model_policy.category.is_none()
                && d.model_policy.reasoning.is_none()
        );
    }
    assert_eq!(
        empty.resolve("hya-main").unwrap(),
        empty
            .resolve("bundle:hya/core-agents/agent/hya-main")
            .unwrap()
    );

    let mut rev = prep_agent("acme-reviewer");
    rev.role = AgentRole::Subagent;
    let populated = catalog_from_bundles(&[make_bundle("acme/reviewer", rev, vec![])]);
    assert_eq!(empty.builtin_digest(), populated.builtin_digest());
}

#[tokio::test]
async fn agent_catalog_bundle_resolution_shadowing_and_spawn_authorization() {
    let mut shadow = prep_agent("hya-main");
    shadow.role = AgentRole::Subagent;
    let shadow_bc =
        BundleCatalog::from_prepared(&[make_bundle("acme/impostor", shadow, vec![])]).unwrap();
    assert_eq!(
        AgentCatalog::new(Arc::new(shadow_bc)).unwrap_err(),
        BundleError::BuiltinAgentIdShadowed {
            bundle_id: "acme/impostor".into(),
            agent_id: "hya-main".into(),
        }
    );

    let empty = catalog_from_bundles(&[]);
    assert!(
        !empty
            .spawnable("hya-main")
            .unwrap()
            .iter()
            .any(|a| a.stable_id == "acme-alpha")
    );
    assert!(matches!(
        empty.spawnable("no-such-agent"),
        Err(BundleError::UnknownAgentId { .. })
    ));

    let sub = |bid, aid, spawn: &[&str]| {
        let mut a = prep_agent(aid);
        a.role = AgentRole::Subagent;
        a.can_spawn = spawn.iter().map(|s| AgentName::new(*s)).collect();
        make_bundle(bid, a, vec![])
    };
    let cat = catalog_from_bundles(&[
        sub("acme/zeta", "acme-zeta", &[]),
        sub("acme/alpha", "acme-alpha", &[]),
        sub(
            "acme/lead",
            "acme-lead",
            &["hya-scout", "acme-alpha", "not-installed"],
        ),
    ]);
    let alpha = cat.resolve("acme-alpha").unwrap();
    assert_eq!(
        alpha.origin,
        AgentOrigin::Bundle {
            bundle_id: "acme/alpha"
        }
    );
    assert_eq!(alpha.origin.bundle_id(), Some("acme/alpha"));
    assert_eq!(
        cat.resolve("bundle:acme/alpha/agent/acme-alpha")
            .unwrap()
            .stable_id,
        "acme-alpha"
    );

    let ids = |caller| {
        cat.spawnable(caller)
            .unwrap()
            .iter()
            .map(|a| a.stable_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids("hya-main"),
        vec![
            "acme-alpha",
            "acme-lead",
            "acme-zeta",
            "hya-main",
            "hya-reviewer",
            "hya-scout",
            "hya-task"
        ]
    );
    assert!(cat.resolve_spawn("hya-main", "acme-alpha").is_ok());
    assert!(matches!(
        cat.resolve_spawn("hya-main", "hya-title"),
        Err(BundleError::AgentSpawnNotAllowed { .. })
    ));
    assert!(cat.spawnable("hya-title").unwrap().is_empty());

    assert_eq!(ids("acme-lead"), vec!["hya-scout", "acme-alpha"]);
    assert!(cat.resolve_spawn("acme-lead", "hya-scout").is_ok());
    assert!(cat.resolve_spawn("acme-lead", "acme-alpha").is_ok());
    assert!(matches!(
        cat.resolve_spawn("acme-lead", "hya-task"),
        Err(BundleError::AgentSpawnNotAllowed { .. })
    ));
    assert!(matches!(
        cat.resolve_spawn("acme-lead", "not-installed"),
        Err(BundleError::UnknownAgentId { .. })
    ));

    // Multi-agent bundle uses bundle origin and explicit can_spawn.
    let prepared = prepare_package(BundleSource::new(
        "set-runtime",
        vec![SourceFile::new(
            "bundle.yaml",
            "schema_version: 2\nkind: Bundle\nidentity: { id: acme/team, version: 1.0.0, publisher: acme }\nagents:\n  - { id: team-lead, role: main, can_spawn: [team-worker] }\n  - { id: team-worker, role: subagent }\n",
        )],
    ))
    .unwrap();
    let team_cat = catalog_from_verified(&[&prepared]);
    assert_eq!(
        team_cat
            .resolve_spawn("team-lead", "team-worker")
            .unwrap()
            .origin,
        AgentOrigin::Bundle {
            bundle_id: "acme/team"
        }
    );
    assert!(matches!(
        team_cat.resolve_spawn("team-worker", "team-lead"),
        Err(BundleError::AgentSpawnNotAllowed { .. })
    ));
    assert!(
        team_cat
            .resolve("bundle:acme/team/agent/team-worker")
            .is_some()
    );

    // Role selector vs can_spawn roster via SessionEngine.
    let engine = eng_with(
        test_catalog(&[
            ("lead", AgentRole::Main, &["research"]),
            ("research", AgentRole::Subagent, &[]),
            ("unlisted", AgentRole::Main, &[]),
        ]),
        Arc::new(FakeProvider::scripted(vec![])),
    )
    .await;
    let dir = TestDir::new("role-vs-roster");
    let binding = engine.bind_runtime(dir.path()).unwrap();

    let builtin_roster = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let names: Vec<_> = builtin_roster.iter().map(|a| a.name.as_str()).collect();
    assert!(
        names.contains(&"research")
            && names.contains(&"hya-scout")
            && !names.contains(&"hya-title")
    );
    assert_eq!(
        builtin_roster
            .iter()
            .find(|a| a.name == "research")
            .map(|a| a.mode.as_str()),
        Some("subagent")
    );
    assert!(
        binding.resolve_spawn("hya-main", "hya-title").is_err()
            && binding.resolve_agent("hya-title").is_some()
    );
    assert_eq!(
        binding.resolve_agent("hya-main").unwrap().role,
        AgentRole::Main
    );
    assert_eq!(
        binding.resolve_agent("research").unwrap().role,
        AgentRole::Subagent
    );

    let lead_roster: Vec<_> = engine
        .agent_roster_for_binding(&binding, "lead")
        .unwrap()
        .iter()
        .map(|a| a.name.clone())
        .collect();
    assert_eq!(lead_roster, vec!["research"]);
    assert!(binding.resolve_spawn("lead", "research").is_ok());
    assert!(
        binding.resolve_spawn("lead", "unlisted").is_err()
            && binding.resolve_spawn("lead", "general").is_err()
    );
}

#[test]
fn agent_catalog_private_and_first_party_workflow_agents() {
    let mk = |bid, aid, priv_, spawn: &[&str]| {
        let mut a = prep_agent(aid);
        a.role = AgentRole::Subagent;
        a.private = priv_;
        a.can_spawn = spawn.iter().map(|s| AgentName::new(*s)).collect();
        make_bundle(bid, a, vec![])
    };
    let cat = catalog_from_bundles(&[
        mk("acme/helper", "acme-helper", true, &[]),
        mk("acme/lead", "acme-lead", false, &["acme-helper"]),
        mk("acme/other", "acme-other", false, &[]),
    ]);
    for builtin in [
        "hya-main",
        "hya-task",
        "bundle:hya/core-agents/agent/hya-main",
    ] {
        assert!(
            !cat.spawnable(builtin)
                .unwrap()
                .iter()
                .any(|a| a.stable_id == "acme-helper")
        );
        for r in ["acme-helper", "bundle:acme/helper/agent/acme-helper"] {
            assert!(matches!(
                cat.resolve_spawn(builtin, r),
                Err(BundleError::AgentSpawnNotAllowed { .. })
            ));
            assert!(cat.resolve_workflow_agent(builtin, r).unwrap().private);
        }
    }
    assert!(
        !cat.selectable()
            .iter()
            .any(|a| a.stable_id == "acme-helper")
    );
    assert!(
        cat.all()
            .iter()
            .any(|a| a.stable_id == "acme-helper" && a.private)
    );
    assert_eq!(
        cat.spawnable("acme-lead").unwrap()[0].stable_id,
        "acme-helper"
    );
    assert!(cat.resolve_spawn("acme-lead", "acme-helper").is_ok());
    assert!(
        cat.resolve_workflow_agent("acme-lead", "acme-helper")
            .is_ok()
    );
    for (caller, target, unknown) in [
        ("acme-other", "acme-helper", false),
        ("hya-title", "acme-helper", false),
        ("hya-main", "hya-title", false),
        ("hya-main", "missing", true),
        ("missing", "acme-helper", true),
    ] {
        let err = cat.resolve_workflow_agent(caller, target).unwrap_err();
        assert_eq!(
            matches!(err, BundleError::UnknownAgentId { .. }),
            unknown,
            "{caller}->{target}: {err:?}"
        );
    }

    for bundle_id in ["hya/goal", "hya/plan-impl-review"] {
        let prepared = hya_bundle::first_party_bundle(bundle_id).unwrap();
        let fp_cat = catalog_from_verified(&[prepared]);
        let agents = prepared.bundles()[0].agents();
        assert!(!agents.is_empty());
        for a in agents {
            assert!(a.private && fp_cat.resolve_spawn("hya-main", a.id.as_str()).is_err());
            assert!(
                fp_cat
                    .resolve_workflow_agent("hya-main", a.id.as_str())
                    .is_ok()
            );
        }
        if bundle_id == "hya/goal" {
            assert!(
                fp_cat
                    .resolve_spawn("hya-goal-guide", "hya-goal-verifier")
                    .is_ok()
            );
        }
    }
}

// ===========================================================================
// Section 2: Fixed System Agents & Historical Agent Identity
// ===========================================================================

#[tokio::test]
async fn fixed_system_title_prompt_model_guidance_and_usage() {
    let title_prompt = preset_agent("hya-title")
        .and_then(|a| a.prompt.as_deref())
        .unwrap();
    let dir = TestDir::new("fixed-title");
    std::fs::write(dir.path().join("AGENTS.md"), "ORDINARY_GUIDANCE_MARKER").unwrap();

    let usage = TokenUsage {
        input: 21,
        output: 8,
        reasoning: 0,
        cache_read: 4,
        cache_write: 1,
        reasoning_unknown: true,
    };
    let provider = RecordingProvider::new().with_usage(usage);
    let engine = eng_with(builtin_only_catalog(), provider.clone()).await;

    let binding = engine.bind_runtime(dir.path()).unwrap();
    let def = binding.resolve_agent("hya-title").unwrap();
    assert_eq!(
        (def.origin, def.prompt),
        (AgentOrigin::Builtin, Some(title_prompt))
    );
    assert!(
        !engine
            .agent_roster_for_binding(&binding, "hya-main")
            .unwrap()
            .iter()
            .any(|a| a.name == "hya-title")
    );
    assert!(binding.resolve_spawn("hya-main", "hya-title").is_err());

    let s1 = create_root_session(&engine, "hya-main", "session-fallback", dir.path()).await;
    engine
        .admit_user_prompt(s1, "debug 500s".into())
        .await
        .unwrap();
    assert!(
        engine
            .auto_title_session(s1, &ModelRef::new("session-fallback"))
            .await
            .unwrap()
    );

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].system.as_deref(), Some(title_prompt));
    assert!(
        !reqs[0]
            .system
            .as_deref()
            .unwrap()
            .contains("ORDINARY_GUIDANCE_MARKER")
    );
    assert_eq!(
        (
            reqs[0].model.as_str(),
            reqs[0].reasoning,
            reqs[0].tools.is_empty(),
            reqs[0].messages.len()
        ),
        ("session-fallback", None, true, 1)
    );

    let records: Vec<_> = engine
        .replay(s1)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::UsageRecorded {
                message,
                step,
                model,
                purpose,
                tokens,
                ..
            } => Some((message, step, model, purpose, tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(
        records,
        vec![(
            None,
            None,
            ModelRef::new("session-fallback"),
            UsagePurpose::Title,
            usage
        )]
    );
    let proj = engine.read_projection(s1).await.unwrap();
    assert_eq!(
        proj.session
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count(),
        0
    );
    let tu = proj.session.usage.by_purpose[&UsagePurpose::Title];
    assert_eq!((tu.prompt(), tu.output, tu.rounds), (26, 8, 1));

    // Eligible remembered preference overrides caller fallback model.
    engine
        .runtime_registry()
        .publish_agent_model_preferences(BTreeMap::from([(
            "hya-title".into(),
            ModelRef::new("remembered/title"),
        )]));
    let s2 = create_root_session(&engine, "hya-main", "session-fallback", dir.path()).await;
    engine.admit_user_prompt(s2, "hello".into()).await.unwrap();
    engine
        .auto_title_session(s2, &ModelRef::new("session-fallback"))
        .await
        .unwrap();
    let reqs = provider.requests();
    assert_eq!(
        (
            reqs.len(),
            reqs[1].model.as_str(),
            reqs[1].system.as_deref(),
            reqs[1].reasoning
        ),
        (2, "remembered/title", Some(title_prompt), None)
    );
}

#[tokio::test]
async fn fixed_system_native_compact_and_manual_compaction_turn_lease() {
    let dir = TestDir::new("fixed-compact");
    let provider = RecordingProvider::new().with_native_compact(
        30_000,
        json!({ "role": "user", "content": "native-compacted-window" }),
    );
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let summarizer: Arc<dyn Summarizer> = Arc::new(ModelSummarizer::new(
        router.clone(),
        ModelRef::new("summarizer-fallback"),
    ));
    let engine = engine_with_router_and_runtime(
        router,
        test_runtime(Arc::new(hya_tool_loader::builtins())),
        allow_all(),
    )
    .await
    .with_compaction(
        summarizer,
        CompactionConfig {
            keep_recent: 1,
            ..CompactionConfig::default()
        },
    );

    let sess = create_root_session(&engine, "hya-main", "session-model", dir.path()).await;
    let tok = hya_engine::tokens::TokenAccounting::default();
    let unit = "lorem ipsum dolor sit amet ".repeat(100);
    let filler = unit.repeat(5_000 / tok.tokenizer().count_text(&unit) + 1);
    for i in 0..4 {
        engine
            .admit_user_prompt(sess, format!("detail {i} {filler}"))
            .await
            .unwrap();
    }
    let mut spec = agent_spec("hya-main", "session-model", dir.path());
    spec.system_prompt = "ROOT_PROMPT".into();
    assert_eq!(
        engine
            .run_turn(sess, &spec, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    {
        let calls = provider.compact_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            (calls[0].model.as_str(), calls[0].system.as_deref()),
            ("session-model", None)
        );
    }
    let strategies: Vec<_> = engine
        .replay(sess)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::ContextCompacted { strategy, .. } => Some(strategy),
            _ => None,
        })
        .collect();
    assert_eq!(strategies, [CompactionStrategy::Native]);
    assert!(format!("{:?}", provider.requests()[0].messages).contains("native-compacted-window"));

    // Manual compaction waits while a turn lease is held, then runs after drop.
    provider.requests.lock().unwrap().clear();
    let empty = create_root_session(&engine, "hya-main", "session-model", dir.path()).await;
    let lease = engine.try_begin_turn(empty).unwrap();
    let summary = engine.summarize_session(empty);
    tokio::pin!(summary);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut summary)
            .await
            .is_err()
    );
    assert!(provider.requests().is_empty());
    drop(lease);
    assert!(
        summary
            .await
            .unwrap_err()
            .to_string()
            .contains("nothing to summarize")
    );
}

#[tokio::test]
async fn historical_agent_identity_replay_fork_and_task_fallback() {
    const HISTORICAL: &str = "legacy-custom-agent-v1";
    let dir = TestDir::new("historical-identity");
    let provider = RecordingProvider::new();
    let mut base_a = prep_agent("base");
    base_a.prompt = Some("base prompt".into());
    let engine = eng_with(catalog_from_agents(vec![base_a]), provider.clone()).await;

    let src = create_root_session(&engine, HISTORICAL, "session-model", dir.path()).await;
    engine
        .admit_user_prompt(src, "historical msg".into())
        .await
        .unwrap();

    let created = engine
        .replay(src)
        .await
        .unwrap()
        .into_iter()
        .find_map(|e| match e.event {
            Event::SessionCreated { agent, .. } => Some(agent.as_str().to_string()),
            _ => None,
        });
    assert_eq!(created.as_deref(), Some(HISTORICAL));
    let src_proj = engine.read_projection(src).await.unwrap();
    assert_eq!(
        src_proj.session.agent.as_ref().map(AgentName::as_str),
        Some(HISTORICAL)
    );
    assert!(provider.requests().is_empty());

    // Read-only fork preserves exact bytes without provider/catalog lookup.
    let forked = create_root_session(&engine, HISTORICAL, "session-model", dir.path()).await;
    engine
        .copy_messages_to_session(forked, &src_proj, None)
        .await
        .unwrap();
    assert_eq!(
        engine
            .read_projection(forked)
            .await
            .unwrap()
            .session
            .agent
            .as_ref()
            .map(AgentName::as_str),
        Some(HISTORICAL)
    );
    assert!(provider.requests().is_empty());

    // Continuing both original and forked sessions falls back durably to hya-task.
    engine
        .admit_user_prompt(forked, "fork continue".into())
        .await
        .unwrap();
    let base = agent_spec("base", "base-model", dir.path());
    for (idx, sess) in [src, forked].into_iter().enumerate() {
        engine
            .run_turn(sess, &base, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(provider.requests().len(), idx + 1);
        assert_eq!(
            engine
                .read_projection(sess)
                .await
                .unwrap()
                .session
                .agent
                .as_ref()
                .map(AgentName::as_str),
            Some(hya_engine::TASK_AGENT_ID)
        );
        let events = engine.replay(sess).await.unwrap();
        assert!(events.iter().any(|e| matches!(&e.event, Event::AgentSwitched { agent, .. } if agent.as_str() == hya_engine::TASK_AGENT_ID)));
        assert!(events.iter().any(|e| matches!(&e.event, Event::SessionCreated { agent, .. } if agent.as_str() == HISTORICAL)));
    }
}

// ===========================================================================
// Section 3: Agent Resource View
// ===========================================================================

#[tokio::test]
async fn agent_resource_view_origin_plane_and_out_of_plane_refusal() {
    let dir = TestDir::new("resource-view-plane");
    dir.write_skill("workdir-skill");

    let (nb, cb) = ("hya/rv-narrowed", "hya/rv-clamped");
    let mut na = prep_agent("narrowed-agent");
    na.prompt = Some("narrowed".into());
    na.resource_view.allow = vec![format!("bundle:{nb}/skill/bundle-skill")];
    let mut ca = prep_agent("clamped-agent");
    ca.prompt = Some("clamped".into());
    let catalog = catalog_from_bundles(&[
        make_bundle(
            nb,
            na,
            vec![bundle_skill(nb, "bundle-skill", "embedded", "bundle body")],
        ),
        make_bundle(
            cb,
            ca,
            vec![bundle_skill(cb, "bundle-skill", "embedded", "bundle body")],
        ),
    ]);

    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(local_runtime(hya_tool_loader::builtins(), catalog));
    runtime
        .refresh(|c| c.register_tool(Arc::new(CountingTool("dynamic_marker", calls.clone()))))
        .unwrap();
    let provider =
        RecordingProvider::new().with_first_tools(true, &[("dynamic_marker", json!({}))]);
    let engine = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(provider.clone())),
        runtime,
        allow_all(),
    )
    .await;
    let base = agent_spec("general", "fake", dir.path());

    for id in ["narrowed-agent", "clamped-agent", "general"] {
        let sess = create_root_session(&engine, id, "fake", dir.path()).await;
        prompt_turn(&engine, sess, "exercise", &base).await.unwrap();
    }

    let first: Vec<_> = provider.requests().into_iter().step_by(2).collect();
    assert_eq!(first.len(), 3);
    let t0 = req_tools(&first[0]);
    for forbidden in ["bash", "grep", "dynamic_marker", "skill"] {
        assert!(!t0.contains(&forbidden.to_string()));
    }
    assert!(
        first[0]
            .tools
            .iter()
            .filter(|s| s.name.as_str() == "read")
            .all(|s| s.description.starts_with("Read team mail"))
    );
    let sys0 = first[0].system.as_deref().unwrap_or("");
    assert!(
        sys0.contains("bundle-skill")
            && sys0.contains("bundle body")
            && !sys0.contains("workdir-skill")
    );

    let t1 = req_tools(&first[1]);
    assert!(t1.contains(&"read".into()) && !t1.contains(&"dynamic_marker".into()));
    let sys1 = first[1].system.as_deref().unwrap_or("");
    assert!(sys1.contains("bundle-skill") && !sys1.contains("workdir-skill"));

    let t2 = req_tools(&first[2]);
    assert!(t2.contains(&"read".into()) && t2.contains(&"dynamic_marker".into()));
    let sys2 = first[2].system.as_deref().unwrap_or("");
    assert!(sys2.contains("workdir-skill") && !sys2.contains("bundle-skill"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    for (slug, reference) in [
        ("mcp", "harness:mcp/mcp__fixture__ping"),
        ("skill", "harness:skill/anything"),
    ] {
        let aid = format!("plane-refusal-{slug}");
        let mut a = prep_agent(&aid);
        a.prompt = Some("plane refusal".into());
        a.resource_view.allow = vec![reference.into()];
        let rt = local_runtime(
            hya_tool_loader::builtins(),
            catalog_from_bundles(&[make_bundle(&format!("hya/plane-{slug}"), a, vec![])]),
        );
        let err = rt
            .bind_turn(dir.path())
            .unwrap()
            .has_selected_bundle_sidecar_capability(&aid);
        assert!(
            matches!(err, Err(BundleError::ResourceNotInPlane { ref plane, .. }) if plane == "internal-public")
        );
    }
}

#[tokio::test]
async fn agent_resource_view_canonical_allow_deny_alias_and_mcp_dispatch() {
    let dir = TestDir::new("resource-view-alias-mcp");

    // Part A: allow + deny + alias share schema and dispatch.
    let calls = Arc::new(AtomicUsize::new(0));
    let tools = hya_tool_loader::builtins();
    tools
        .register(Arc::new(CountingTool("dynamic_marker", calls.clone())))
        .unwrap();
    let mut a = prep_agent("alias-agent");
    a.prompt = Some("alias".into());
    a.resource_view = ResourceView {
        allow: vec![
            "harness:tool/dynamic_marker".into(),
            "harness:tool/read".into(),
        ],
        deny: vec!["harness:tool/read".into()],
        aliases: BTreeMap::from([("marker".into(), "harness:tool/dynamic_marker".into())]),
        namespace: None,
    };
    let cat = catalog_from_bundles(&[make_bundle("hya/alias-test", a, vec![])]);
    let provider = RecordingProvider::new().with_first_tools(
        false,
        &[("marker", json!({})), ("dynamic_marker", json!({}))],
    );
    let engine = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(provider.clone())),
        Arc::new(local_runtime(tools, cat)),
        PermissionRules::new(vec![Rule::new(Action::Tool, "dynamic_marker", Mode::Allow)]),
    )
    .await;
    let sess = create_root_session(&engine, "alias-agent", "fake", dir.path()).await;
    prompt_turn(
        &engine,
        sess,
        "marker",
        &agent_spec("alias-agent", "fake", dir.path()),
    )
    .await
    .unwrap();
    let req0 = &provider.requests()[0];
    let names = req_tools(req0);
    assert!(names.contains(&"marker".into()) && !names.contains(&"dynamic_marker".into()));
    assert!(
        req0.tools
            .iter()
            .filter(|s| s.name.as_str() == "read")
            .all(|s| s.description.starts_with("Read team mail"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Part B: selected MCP public name dispatches once with canonical MCP permission.
    let mcp_calls = Arc::new(AtomicUsize::new(0));
    let mcp_rt = Arc::new(local_runtime(
        hya_tool_loader::builtins(),
        builtin_only_catalog(),
    ));
    mcp_rt
        .refresh(|c| {
            c.upsert_sources(vec![RuntimeSource::new(
                RuntimeSourceId::mcp("fixture"),
                [9; 32],
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "ping",
                    "mcp__fixture__ping",
                    vec![],
                    Arc::new(CountingTool("mcp__fixture__ping", mcp_calls.clone())),
                    ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();
    let mcp_prov =
        RecordingProvider::new().with_first_tools(false, &[("mcp__fixture__ping", json!({}))]);
    let mcp_eng = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(mcp_prov.clone())),
        mcp_rt,
        PermissionRules::new(vec![Rule::new(
            Action::Mcp,
            "mcp__fixture__ping",
            Mode::Allow,
        )]),
    )
    .await;
    let ms = create_root_session(&mcp_eng, "general", "fake", dir.path()).await;
    prompt_turn(
        &mcp_eng,
        ms,
        "ping",
        &agent_spec("general", "fake", dir.path()),
    )
    .await
    .unwrap();
    assert!(req_tools(&mcp_prov.requests()[0]).contains(&"mcp__fixture__ping".into()));
    assert_eq!(mcp_calls.load(Ordering::SeqCst), 1);
}

// ===========================================================================
// Section 4: Agent Effort, Model Config, Preferences, Categories & Selection
// ===========================================================================

type HookBoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

struct BetweenRoundsTool(&'static str, Arc<dyn Fn() -> HookBoxFuture + Send + Sync>);

#[async_trait]
impl Tool for BetweenRoundsTool {
    fn name(&self) -> &str {
        self.0
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: "between-round hook".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, _: &ToolCtx, _: Value) -> Result<Value, ToolError> {
        (self.1)().await;
        Ok(json!({ "ok": true }))
    }
}

fn effort_engine(
    store: SessionStore,
    provider: Arc<RecordingProvider>,
    hook: BetweenRoundsTool,
) -> SessionEngine {
    let tools = hya_tool_loader::builtins();
    tools.register(Arc::new(hook)).unwrap();
    let (perm, _asks) = PermissionPlane::new(allow_all());
    SessionEngine::new(
        store,
        Arc::new(ProviderRouter::new().with(provider)),
        test_runtime(Arc::new(tools)),
        perm,
        EventBus::default(),
    )
    .with_round_retry_waits(&ZERO_RETRY_WAITS)
}

#[tokio::test]
async fn agent_effort_precedence_between_round_updates_and_corrupt_propagation() {
    let dir = TestDir::new("effort-precedence");

    // Part A: roster annotation precedence (preference > configured > authored > none).
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let roster_eng = SessionEngine::new(
        store.clone(),
        Arc::new(ProviderRouter::new()),
        test_runtime(Arc::new(hya_tool_loader::builtins())),
        perm,
        EventBus::default(),
    );
    roster_eng
        .runtime_registry()
        .publish_agent_effort_configuration(
            [
                ("scout".into(), ReasoningEffort::Low),
                ("review".into(), ReasoningEffort::Medium),
            ]
            .into_iter()
            .collect(),
        );
    store
        .upsert_agent_effort_preference(owner, "review", "high", 1)
        .await
        .unwrap();
    let row = |name: &str, authored: Option<&str>| AgentDef {
        name: name.into(),
        description: None,
        category: None,
        mode: "subagent".into(),
        effort: authored.map(str::to_string),
        effort_source: authored.map(|_| "authored".into()),
    };
    let annotated = roster_eng
        .annotate_agent_efforts(
            vec![
                row("scout", Some("max")),
                row("review", Some("low")),
                row("deep", Some("xhigh")),
                row("general", None),
            ]
            .into(),
            None,
        )
        .await
        .unwrap();
    let shown: Vec<_> = annotated
        .iter()
        .map(|a| {
            (
                a.name.as_str(),
                a.effort.as_deref(),
                a.effort_source.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("scout", Some("low"), Some("configured")),
            ("review", Some("high"), Some("preference")),
            ("deep", Some("xhigh"), Some("authored")),
            ("general", None, None),
        ]
    );
    assert_eq!(
        roster_eng.agent_effort("review", None, None).await.unwrap(),
        Some((ReasoningEffort::High, AgentEffortSource::Preference))
    );
    assert_eq!(
        roster_eng
            .agent_effort("general", None, None)
            .await
            .unwrap(),
        None
    );

    // Part B: healthy preference change between rounds takes effect in round 2.
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    store
        .upsert_agent_effort_preference(owner, "hya-main", "low", 1)
        .await
        .unwrap();
    let s_clone = store.clone();
    let prov = RecordingProvider::new().with_first_tools(false, &[("bump_effort", json!({}))]);
    let eng = effort_engine(
        store,
        prov.clone(),
        BetweenRoundsTool(
            "bump_effort",
            Arc::new(move || {
                let s = s_clone.clone();
                Box::pin(async move {
                    s.upsert_agent_effort_preference(owner, "hya-main", "high", 2)
                        .await
                        .unwrap();
                })
            }),
        ),
    );
    let sess = create_root_session(&eng, "hya-main", "fake", dir.path()).await;
    assert_eq!(
        prompt_turn(
            &eng,
            sess,
            "check",
            &agent_spec("hya-main", "fake", dir.path())
        )
        .await
        .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(
        prov.requests()
            .iter()
            .map(|r| r.reasoning)
            .collect::<Vec<_>>(),
        vec![Some(ReasoningEffort::Low), Some(ReasoningEffort::High)]
    );

    // Part C: corrupt effort data propagates InvalidPreferenceData without fallback.
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    store
        .upsert_agent_effort_preference(owner, "hya-main", "low", 1)
        .await
        .unwrap();
    let s_clone = store.clone();
    let prov = RecordingProvider::new().with_first_tools(false, &[("corrupt_effort", json!({}))]);
    let eng = effort_engine(
        store,
        prov.clone(),
        BetweenRoundsTool(
            "corrupt_effort",
            Arc::new(move || {
                let s = s_clone.clone();
                Box::pin(async move {
                    let mut tx = s.begin_transaction().await.unwrap();
                    sqlx::query("UPDATE agent_effort_preference SET effort = '' WHERE agent_id = 'hya-main'")
                        .execute(&mut *tx)
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                })
            }),
        ),
    );
    let sess = create_root_session(&eng, "hya-main", "fake", dir.path()).await;
    let err = prompt_turn(
        &eng,
        sess,
        "corrupt",
        &agent_spec("hya-main", "fake", dir.path()),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        CoreError::Store(StoreError::InvalidPreferenceData { .. })
    ));
    assert_eq!(
        prov.requests()
            .iter()
            .map(|r| r.reasoning)
            .collect::<Vec<_>>(),
        vec![Some(ReasoningEffort::Low)]
    );
}

#[cfg(target_os = "linux")]
struct ShmWriteHold(std::fs::File);

#[cfg(target_os = "linux")]
impl ShmWriteHold {
    fn lock_op(file: &std::fs::File, l_type: libc::c_short) -> libc::c_int {
        use std::os::unix::io::AsRawFd;
        let lock = libc::flock {
            l_type,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: 120,
            l_len: 8,
            l_pid: 0,
        };
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) }
    }
    fn acquire(db: &Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("{}-shm", db.display()))
            .unwrap();
        assert_eq!(Self::lock_op(&file, libc::F_WRLCK as libc::c_short), 0);
        Self(file)
    }
}

#[cfg(target_os = "linux")]
impl Drop for ShmWriteHold {
    fn drop(&mut self) {
        let _ = Self::lock_op(&self.0, libc::F_UNLCK as libc::c_short);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn agent_effort_storage_fault_round_one_fail_vs_round_two_reuse() {
    // Part A: read fault after round 1 reuses round 1 effort.
    let dir_a = TestDir::new("effort-shm-reuse");
    let db_a = dir_a.path().join("hya.db");
    let store_a =
        SessionStore::connect_with_write_timeout(db_a.to_str().unwrap(), Duration::from_millis(40))
            .await
            .unwrap();
    let owner = OwnerRunId::new();
    store_a.claim_runtime_owner(owner).unwrap();
    store_a
        .upsert_agent_effort_preference(owner, "hya-main", "high", 1)
        .await
        .unwrap();
    store_a
        .upsert_model_effort_preference(owner, "fake", "model", "low", 1)
        .await
        .unwrap();
    let slot: Arc<Mutex<Option<ShmWriteHold>>> = Arc::new(Mutex::new(None));
    let (slot_c, db_c) = (Arc::clone(&slot), db_a.clone());
    let prov_a = RecordingProvider::new().with_first_tools(false, &[("stall_disk", json!({}))]);
    let eng_a = effort_engine(
        store_a.clone(),
        prov_a.clone(),
        BetweenRoundsTool(
            "stall_disk",
            Arc::new(move || {
                let (s, d) = (Arc::clone(&slot_c), db_c.clone());
                Box::pin(async move {
                    *s.lock().unwrap() = Some(ShmWriteHold::acquire(&d));
                })
            }),
        ),
    );
    let work_a = dir_a.path().join("work");
    std::fs::create_dir_all(&work_a).unwrap();
    let sess_a = create_root_session(&eng_a, "hya-main", "fake/model", &work_a).await;
    eng_a
        .admit_user_prompt(sess_a, "stall".into())
        .await
        .unwrap();
    store_a.read_projection(sess_a).await.unwrap();
    assert_eq!(
        eng_a
            .run_turn(
                sess_a,
                &agent_spec("hya-main", "fake/model", &work_a),
                CancellationToken::new()
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(
        prov_a
            .requests()
            .iter()
            .map(|r| r.reasoning)
            .collect::<Vec<_>>(),
        vec![Some(ReasoningEffort::High), Some(ReasoningEffort::High)]
    );
    drop(slot.lock().unwrap().take());

    // Part B: first preference read failure fails the turn with StorageTimeout.
    let dir_b = TestDir::new("effort-shm-first-fail");
    let db_b = dir_b.path().join("hya.db");
    let store_b =
        SessionStore::connect_with_write_timeout(db_b.to_str().unwrap(), Duration::from_millis(40))
            .await
            .unwrap();
    let owner_b = OwnerRunId::new();
    store_b.claim_runtime_owner(owner_b).unwrap();
    let eng_b = effort_engine(
        store_b.clone(),
        RecordingProvider::new(),
        BetweenRoundsTool("noop", Arc::new(|| Box::pin(async {}))),
    );
    let work_b = dir_b.path().join("work");
    std::fs::create_dir_all(&work_b).unwrap();
    let sess_b = create_root_session(&eng_b, "hya-main", "fake", &work_b).await;
    eng_b
        .admit_user_prompt(sess_b, "first fail".into())
        .await
        .unwrap();
    store_b.read_projection(sess_b).await.unwrap();
    let _stall = ShmWriteHold::acquire(&db_b);
    store_b
        .append_event(
            sess_b,
            &Event::SessionTitled {
                session: sess_b,
                title: "degrade".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(store_b.health(), StoreHealth::Degraded { .. }));
    let err = eng_b
        .run_turn(
            sess_b,
            &agent_spec("hya-main", "fake", &work_b),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        CoreError::Store(StoreError::StorageTimeout { .. })
    ));
}

#[tokio::test]
async fn agent_model_configuration_session_override_and_model_selection() {
    let dir = TestDir::new("agent-model-config");
    let m = |v: &str| ModelRef::new(format!("fake/{v}"));

    // Empty layers preserve semantic binding identity.
    let rt = local_runtime(hya_tool_loader::builtins(), builtin_only_catalog());
    let baseline = rt.bind_turn(dir.path()).unwrap();
    let empty = baseline
        .clone()
        .with_agent_model_configuration(AgentModelConfiguration::default())
        .with_session_agent_models(BTreeMap::new());
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    assert_eq!(
        baseline.semantic_fingerprint_v1(&perm),
        empty.semantic_fingerprint_v1(&perm)
    );

    // Pre-session configuration + root-scoped override replay/isolation/immutability/precedence.
    let provider = RecordingProvider::new();
    let engine = eng_with(builtin_only_catalog(), provider.clone()).await;
    engine
        .runtime_registry()
        .publish_agent_model_configuration(AgentModelConfiguration {
            builtin: BTreeMap::from([("hya-task".into(), m("configured"))]),
            bundles: BTreeMap::new(),
        });
    let root = create_root_session(&engine, "hya-task", "fake/base", dir.path()).await;
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("hya-task"),
            model: m("base"),
            workdir: dir.path().to_string_lossy().into_owned(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap();
    let other_root = create_root_session(&engine, "hya-task", "fake/base", dir.path()).await;

    let cfg_bind = engine.bind_session_runtime(root, dir.path()).await.unwrap();
    assert_eq!(
        cfg_bind.configured_agent_model("hya-task"),
        Some(&m("configured"))
    );
    assert_eq!(
        cfg_bind
            .resolve_agent("hya-task")
            .unwrap()
            .model_policy
            .model
            .as_deref(),
        Some("fake/configured")
    );

    engine
        .set_agent_model_override(child, AgentName::new("hya-task"), Some(m("temporary")))
        .await
        .unwrap();
    assert!(engine.replay(root).await.unwrap().iter().any(|e| matches!(
        &e.event,
        Event::SessionAgentModelOverrideSet { session, agent, model: Some(ov) }
            if *session == root && agent.as_str() == "hya-task" && ov == &m("temporary")
    )));
    assert_eq!(
        engine
            .read_projection(root)
            .await
            .unwrap()
            .session
            .agent_model_overrides
            .get("hya-task"),
        Some(&m("temporary"))
    );

    let old_bind = engine.bind_session_runtime(root, dir.path()).await.unwrap();
    let child_bind = engine
        .bind_session_runtime(child, dir.path())
        .await
        .unwrap();
    let other_bind = engine
        .bind_session_runtime(other_root, dir.path())
        .await
        .unwrap();
    assert_eq!(
        child_bind.session_agent_model("hya-task"),
        Some(&m("temporary"))
    );
    assert_eq!(
        child_bind
            .resolve_agent("hya-task")
            .unwrap()
            .model_policy
            .model
            .as_deref(),
        Some("fake/temporary")
    );
    assert_eq!(other_bind.session_agent_model("general"), None);

    let agent = agent_spec("hya-task", "fake/base", dir.path());
    prompt_turn(&engine, root, "omitted", &agent).await.unwrap();
    engine
        .admit_user_prompt(root, "explicit".into())
        .await
        .unwrap();
    engine
        .run_turn_with_external_dirs_and_guidance(
            root,
            &agent,
            CancellationToken::new(),
            &[],
            None,
            Some(m("explicit")),
            None,
        )
        .await
        .unwrap();
    prompt_turn(&engine, child, "child", &agent).await.unwrap();

    engine
        .set_agent_model_override(root, AgentName::new("hya-task"), Some(m("new")))
        .await
        .unwrap();
    assert_eq!(
        old_bind.session_agent_model("hya-task"),
        Some(&m("temporary"))
    );
    assert_eq!(
        engine
            .bind_session_runtime(root, dir.path())
            .await
            .unwrap()
            .session_agent_model("hya-task"),
        Some(&m("new"))
    );

    engine
        .set_agent_model_override(root, AgentName::new("hya-task"), None)
        .await
        .unwrap();
    assert_eq!(
        engine
            .bind_session_runtime(child, dir.path())
            .await
            .unwrap()
            .resolve_agent("hya-task")
            .unwrap()
            .model_policy
            .model
            .as_deref(),
        Some("fake/configured")
    );
    prompt_turn(&engine, root, "configured", &agent)
        .await
        .unwrap();
    assert_eq!(
        provider
            .requests()
            .into_iter()
            .map(|r| r.model)
            .collect::<Vec<_>>(),
        vec![
            m("temporary"),
            m("explicit"),
            m("temporary"),
            m("configured")
        ]
    );

    // switch_model selects model on run_turn.
    let sel_prov = RecordingProvider::new().with_only_model("selected");
    let sel_eng = eng_with(builtin_only_catalog(), sel_prov.clone()).await;
    let sel_sess = create_root_session(&sel_eng, "build", "base", dir.path()).await;
    sel_eng
        .switch_model(sel_sess, ModelRef::new("selected"))
        .await
        .unwrap();
    assert_eq!(
        prompt_turn(
            &sel_eng,
            sel_sess,
            "sel",
            &agent_spec("build", "base", dir.path())
        )
        .await
        .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(
        sel_prov
            .requests()
            .iter()
            .map(|r| r.model.as_str())
            .collect::<Vec<_>>(),
        vec!["selected"]
    );
}

#[test]
fn agent_model_preferences_snapshot_and_resolution_precedence() {
    let dir = TestDir::new("agent-model-prefs");
    let reg = local_runtime(hya_tool_loader::builtins(), builtin_only_catalog());
    let before = reg.bind_turn(dir.path()).unwrap();
    reg.publish_agent_model_preferences(BTreeMap::from([(
        "general".into(),
        ModelRef::new("openai/first"),
    )]));
    let b1 = reg.bind_turn(dir.path()).unwrap();
    reg.publish_agent_model_preferences(BTreeMap::from([(
        "general".into(),
        ModelRef::new("openai/second"),
    )]));
    let b2 = reg.bind_turn(dir.path()).unwrap();
    assert_eq!(before.agent_model_preference("general"), None);
    assert_eq!(
        b1.agent_model_preference("general"),
        Some(&ModelRef::new("openai/first"))
    );
    assert_eq!(
        b2.agent_model_preference("general"),
        Some(&ModelRef::new("openai/second"))
    );

    let mk = |policy| AgentDefinition {
        private: false,
        stable_id: "general",
        description: None,
        role: AgentRole::Subagent,
        color: None,
        prompt: None,
        model_policy: Cow::Owned(policy),
        workdir: None,
        origin: AgentOrigin::Builtin,
    };
    let rem = ModelRef::new("openai/remembered");
    let servable = |m: &ModelRef| m == &rem;
    for (policy, pref, expected) in [
        (ModelPolicy::default(), Some(&rem), "openai/remembered"),
        (
            ModelPolicy::default(),
            Some(&ModelRef::new("missing/model")),
            "openai/base",
        ),
        (
            ModelPolicy {
                model: Some("openai/cfg".into()),
                ..ModelPolicy::default()
            },
            Some(&rem),
            "openai/base",
        ),
        (
            ModelPolicy {
                category: Some("fast".into()),
                ..ModelPolicy::default()
            },
            Some(&rem),
            "openai/base",
        ),
        (
            ModelPolicy {
                reasoning: Some("high".into()),
                ..ModelPolicy::default()
            },
            Some(&rem),
            "openai/remembered",
        ),
    ] {
        assert_eq!(
            apply_agent_model_preference(
                agent_spec("general", "openai/base", "/tmp"),
                &mk(policy),
                pref,
                &servable
            )
            .model
            .as_str(),
            expected
        );
    }

    let cats = CategoryRegistry::from_entries(HashMap::from([(
        "deep".into(),
        CategoryEntry::from_candidates(&["missing/primary".into(), "openai/fallback".into()])
            .unwrap(),
    )]));
    let ok = |m: &ModelRef| m.as_str() == "openai/fallback";
    assert_eq!(
        resolve_configured_agent_model(
            &ModelPolicy {
                category: Some("deep".into()),
                ..ModelPolicy::default()
            },
            &cats,
            &ok
        ),
        Some(ModelRef::new("openai/fallback"))
    );
    assert_eq!(
        resolve_configured_agent_model(
            &ModelPolicy {
                model: Some("openai/direct".into()),
                category: Some("deep".into()),
                reasoning: None
            },
            &cats,
            &ok
        ),
        Some(ModelRef::new("openai/direct"))
    );
}

#[tokio::test]
async fn category_routing_resolution_failover_and_resident_execution() {
    let cats = [
        ("quick", "gw/quick-model"),
        ("deep", "gw/deep-model"),
        ("ultrabrain", "gw/ultra-model"),
        ("writing", "gw/writer-model"),
    ];
    let reg = CategoryRegistry::from_entries(
        cats.iter()
            .map(|(k, v)| {
                (
                    (*k).into(),
                    CategoryEntry::from_candidates(&[(*v).into()]).unwrap(),
                )
            })
            .collect(),
    );
    let distinct: HashSet<_> = cats
        .iter()
        .map(|(c, _)| reg.resolve(c).unwrap().model.to_string())
        .collect();
    assert_eq!(distinct.len(), 4);
    let deep = reg.resolve("deep").unwrap();
    assert_eq!(deep.fallback_chain.first().unwrap(), &deep.model);
    assert!(
        reg.resolve("nonexistent").is_none()
            && CategoryRegistry::new().resolve("anything").is_none()
    );

    let fail_reg = CategoryRegistry::from_entries(HashMap::from([(
        "deep".into(),
        CategoryEntry::from_candidates(&["primary/opus".into(), "backup/sonnet".into()]).unwrap(),
    )]));
    assert_eq!(
        fail_reg.resolve_servable("deep", |_| true).unwrap().model,
        ModelRef::new("primary/opus")
    );
    assert_eq!(
        fail_reg
            .resolve_servable("deep", |m| m.as_str() != "primary/opus")
            .unwrap()
            .model,
        ModelRef::new("backup/sonnet")
    );
    let none = fail_reg.resolve_servable("deep", |_| false).unwrap();
    assert_eq!(
        (none.model, none.fallback_chain.len()),
        (ModelRef::new("primary/opus"), 2)
    );

    let provider = RecordingProvider::new();
    let engine = Arc::new(eng_with(builtin_only_catalog(), provider.clone()).await);
    let dir = TestDir::new("category-residents");
    let lead = create_root_session(&engine, "hya-main", "base", dir.path()).await;
    let base = agent_spec("hya-main", "base", dir.path());
    let binding = engine.bind_runtime(dir.path()).unwrap();
    let specs: Vec<ResidentTurn> = cats
        .iter()
        .map(|(cat, _)| ResidentTurn {
            agent: AgentSpec {
                model: reg.resolve(cat).unwrap().model.clone(),
                ..base.clone()
            },
            binding: binding.clone(),
            agents: Arc::from([]),
            resources: None,
            guidance: None,
            directive: format!("work as {cat}"),
            description: String::new(),
            sidecar_factory: None,
            tool_call: None,
        })
        .collect();
    assert_eq!(
        run_residents(engine, lead, specs, CancellationToken::new())
            .await
            .len(),
        4
    );
    let seen: HashSet<_> = provider
        .requests()
        .into_iter()
        .map(|r| r.model.to_string())
        .collect();
    assert_eq!(seen, cats.iter().map(|(_, m)| (*m).to_string()).collect());
}

// ===========================================================================
// Section 5: Catalog Scope & Session Catalog Scope
// ===========================================================================

fn scope_project(roots: &[&TestDir]) -> CatalogScope {
    CatalogScope::Project {
        id: ProjectId::new(),
        roots: roots.iter().map(|d| d.path().to_path_buf()).collect(),
    }
}

fn scope_overlay(agent: &str) -> ScopeOverlay {
    ScopeOverlay::new(test_catalog(&[(agent, AgentRole::Main, &[])]))
}

fn mcp_lookup_source(server: &str) -> RuntimeSource {
    let name = format!("mcp__{server}__lookup");
    RuntimeSource::new(
        RuntimeSourceId::mcp(server),
        [1; 32],
        Arc::new(()),
        vec![RuntimeSourceExport::tool(
            "lookup",
            name.clone(),
            vec![],
            MarkerTool::new(name),
            ToolPermission::Mcp,
        )],
    )
}

fn plugin_tool_source(id: &str, export: &str) -> RuntimeSource {
    RuntimeSource::new(
        RuntimeSourceId::plugin(id),
        [2; 32],
        Arc::new(()),
        vec![RuntimeSourceExport::tool(
            "tool",
            export,
            vec![],
            MarkerTool::new(export),
            ToolPermission::Tool,
        )],
    )
}

fn empty_hooks() -> Arc<dyn HookDispatcher> {
    Arc::new(HookChain::new(Vec::new()))
}

#[test]
fn catalog_scope_isolation_lazy_rebuild_and_generations() {
    let (root_a, root_b) = (TestDir::new("scope-iso-a"), TestDir::new("scope-iso-b"));
    let registry = RuntimeRegistry::new(hya_tool_loader::builtins(), builtin_only_catalog());
    let (scope_a, scope_b) = (scope_project(&[&root_a]), scope_project(&[&root_b]));

    let base_gen = registry.bind_turn(root_a.path()).unwrap().generation();
    let gen_a = registry
        .publish_scope(scope_a.key(), scope_overlay("alpha-agent"))
        .unwrap();
    let gen_b = registry
        .publish_scope(scope_b.key(), scope_overlay("beta-agent"))
        .unwrap();

    let a = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    let b = registry.bind_scoped(&scope_b, root_b.path()).unwrap();
    assert!(a.resolve_agent("alpha-agent").is_some() && a.resolve_agent("beta-agent").is_none());
    assert!(b.resolve_agent("beta-agent").is_some() && b.resolve_agent("alpha-agent").is_none());
    assert_eq!((a.scope(), b.scope()), (&scope_a, &scope_b));
    assert_eq!(
        registry
            .bind_scoped(&scope_a, root_a.path())
            .unwrap()
            .generation(),
        a.generation()
    );

    let global = registry
        .bind_scoped(&CatalogScope::Global, Path::new(""))
        .unwrap();
    assert!(
        global.resolve_agent("alpha-agent").is_none()
            && global.resolve_agent("beta-agent").is_none()
    );
    assert_eq!(global.scope(), &CatalogScope::Global);

    let plain = registry
        .bind_scoped(
            &CatalogScope::Directory(root_a.path().to_path_buf()),
            root_a.path(),
        )
        .unwrap();
    assert!(plain.resolve_agent("alpha-agent").is_none() && plain.project_bundle_dirs().is_empty());
    assert_eq!(plain.workdir(), root_a.path());
    assert_eq!(
        plain.generation(),
        registry.bind_turn(root_a.path()).unwrap().generation()
    );

    // Base MCP publish lazily rebuilds scopes while keeping overlays and strictly advancing generations.
    let refreshed = registry
        .refresh(|c| c.upsert_sources(vec![mcp_lookup_source("srv")]))
        .unwrap();
    let rebuilt_b = registry.bind_scoped(&scope_b, root_b.path()).unwrap();
    let rebuilt_a = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    assert!(
        binding_tool_names(&rebuilt_a).contains("mcp__srv__lookup")
            && !binding_tool_names(&a).contains("mcp__srv__lookup")
    );
    assert!(
        rebuilt_a.resolve_agent("alpha-agent").is_some()
            && rebuilt_b.resolve_agent("beta-agent").is_some()
    );
    let observed = [
        base_gen,
        gen_a,
        gen_b,
        refreshed,
        rebuilt_b.generation(),
        rebuilt_a.generation(),
    ];
    assert!(
        observed.windows(2).all(|p| p[0] < p[1]),
        "non-monotonic: {observed:?}"
    );
}

#[test]
fn catalog_scope_failures_drop_hooks_and_model_shadowing() {
    struct Owner;
    impl RuntimeSourceOwner for Owner {}

    let (root_a, root_b) = (TestDir::new("scope-ops-a"), TestDir::new("scope-ops-b"));
    let registry = RuntimeRegistry::new(hya_tool_loader::builtins(), builtin_only_catalog());
    let (scope_a, scope_b) = (scope_project(&[&root_a]), scope_project(&[&root_b]));

    // Failing publish leaves base and prior scope intact.
    let base = registry.bind_turn(root_a.path()).unwrap();
    let good = registry
        .publish_scope(scope_a.key(), scope_overlay("alpha-agent"))
        .unwrap();
    let mut bad = scope_overlay("beta-agent");
    bad.plugin_sources = vec![plugin_tool_source("proj", "bare_tool")];
    assert!(matches!(
        registry.publish_scope(scope_a.key(), bad),
        Err(RuntimeRefreshError::ScopeCompose { .. })
    ));
    assert_eq!(
        registry.bind_turn(root_a.path()).unwrap().generation(),
        base.generation()
    );
    let kept = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    assert_eq!(kept.generation(), good);
    assert!(
        kept.resolve_agent("alpha-agent").is_some() && kept.resolve_agent("beta-agent").is_none()
    );

    // Failing lazy rebuild on base collision is typed and recovers once conflict removed.
    let mut colliding = scope_overlay("alpha-agent");
    colliding.plugin_sources = vec![plugin_tool_source("proj", "proj__tool")];
    registry.publish_scope(scope_a.key(), colliding).unwrap();
    let before = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    registry
        .refresh(|c| c.upsert_sources(vec![plugin_tool_source("other", "proj__tool")]))
        .unwrap();
    assert!(matches!(
        registry.bind_scoped(&scope_a, root_a.path()),
        Err(RuntimeRefreshError::ScopeCompose { .. })
    ));
    assert!(
        registry.scope_overlay(&scope_a.key()).is_some()
            && before.resolve_tool("proj__tool").is_some()
    );
    let removed = BTreeSet::from([RuntimeSourceId::plugin("other")]);
    registry
        .refresh(|c| {
            c.remove_sources(&removed);
            Ok(())
        })
        .unwrap();
    let recovered = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    assert!(
        recovered.generation() > before.generation()
            && recovered.resolve_agent("alpha-agent").is_some()
    );

    // Scope plugin hooks stay isolated and drop_scope releases owner once bindings drop.
    let owner = Arc::new(Owner);
    let weak = Arc::downgrade(&owner);
    let mut ov_a = scope_overlay("alpha-agent");
    ov_a.plugin_sources = vec![
        RuntimeSource::new(RuntimeSourceId::plugin("proj-a"), [4; 32], owner, vec![])
            .with_hooks(empty_hooks()),
    ];
    let mut ov_b = scope_overlay("beta-agent");
    ov_b.plugin_sources = vec![
        RuntimeSource::new(
            RuntimeSourceId::plugin("proj-b"),
            [5; 32],
            Arc::new(()),
            vec![],
        )
        .with_hooks(empty_hooks()),
    ];
    registry.publish_scope(scope_a.key(), ov_a).unwrap();
    registry.publish_scope(scope_b.key(), ov_b).unwrap();

    let bind_a = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    let chain_a = bind_a.bundle_hooks_for_agent("hya-main");
    assert_eq!(chain_a.len(), 1);
    assert!(Arc::ptr_eq(
        &registry
            .bind_scoped(&scope_a, root_a.path())
            .unwrap()
            .bundle_hooks_for_agent("hya-main")[0],
        &chain_a[0]
    ));
    assert!(
        bind_a
            .bundle_hooks_for_agent("alpha-agent")
            .iter()
            .any(|h| Arc::ptr_eq(h, &chain_a[0]))
    );
    let bind_b = registry.bind_scoped(&scope_b, root_b.path()).unwrap();
    let chain_b = bind_b.bundle_hooks_for_agent("hya-main");
    assert!(chain_b.len() == 1 && !Arc::ptr_eq(&chain_b[0], &chain_a[0]));
    assert!(
        !bind_b
            .bundle_hooks_for_agent("beta-agent")
            .iter()
            .any(|h| Arc::ptr_eq(h, &chain_a[0]))
    );
    assert!(
        registry
            .bind_turn(root_a.path())
            .unwrap()
            .bundle_hooks_for_agent("hya-main")
            .is_empty()
    );

    registry.drop_scope(&scope_a.key());
    assert!(registry.scope_overlay(&scope_a.key()).is_none() && weak.upgrade().is_some());
    drop((chain_a, bind_a));
    assert!(weak.upgrade().is_none());
    let rebound = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    assert!(
        rebound.resolve_agent("alpha-agent").is_none()
            && rebound.bundle_hooks_for_agent("hya-main").is_empty()
    );

    // Project bundle model leaves shadow user-scope configuration.
    let bid = "hya/test-alpha-agent".to_string();
    registry.publish_agent_model_configuration(AgentModelConfiguration {
        builtin: BTreeMap::from([("hya-main".into(), ModelRef::new("base-build"))]),
        bundles: BTreeMap::from([(
            bid.clone(),
            BTreeMap::from([("alpha-agent".into(), ModelRef::new("user-alpha"))]),
        )]),
    });
    let mut shadowed = scope_overlay("alpha-agent");
    shadowed.project_bundle_dirs =
        BTreeMap::from([(bid.clone(), root_a.path().join(".hya/bundles/alpha"))]);
    registry
        .publish_scope(scope_a.key(), shadowed.clone())
        .unwrap();
    let bind = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    assert!(bind.configured_agent_model("alpha-agent").is_none());
    assert_eq!(
        bind.configured_agent_model("hya-main"),
        Some(&ModelRef::new("base-build"))
    );
    assert_eq!(
        bind.project_bundle_dirs().get(&bid),
        Some(&root_a.path().join(".hya/bundles/alpha"))
    );
    shadowed.bundle_models = BTreeMap::from([(
        bid,
        BTreeMap::from([("alpha-agent".into(), ModelRef::new("project-alpha"))]),
    )]);
    registry.publish_scope(scope_a.key(), shadowed).unwrap();
    assert_eq!(
        registry
            .bind_scoped(&scope_a, root_a.path())
            .unwrap()
            .configured_agent_model("alpha-agent"),
        Some(&ModelRef::new("project-alpha"))
    );
    assert_eq!(
        registry
            .scope_overlay(&scope_a.key())
            .map(|o| o.fingerprint),
        Some([0; 32])
    );
    assert_eq!(
        ScopeKey::Project(scope_a.project_id().unwrap()),
        scope_a.key()
    );
}

const SCOPE_BUNDLE: &str = "hya/test-scoped";

struct ScopeAnswerApi;
#[async_trait]
impl BundleApiProvider for ScopeAnswerApi {
    async fn request(&self, req: BundleApiRequest) -> Result<BundleApiReply, String> {
        Ok(BundleApiReply {
            status: 200,
            body: json!({ "session": req.session.map(|id| id.to_string()) }),
        })
    }
}

#[derive(Default)]
struct RecordingScopeRefresh(Mutex<Vec<CatalogScope>>);

#[async_trait]
impl RuntimeCatalogRefresh for RecordingScopeRefresh {
    async fn refresh_if_changed(&self, _: &RuntimeRegistry) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        self.0.lock().unwrap().push(scope.clone());
        if *scope == CatalogScope::Global || runtime.scope_overlay(&scope.key()).is_some() {
            return Ok(false);
        }
        let mut overlay = ScopeOverlay::new(test_catalog(&[("scoped", AgentRole::Main, &[])]));
        overlay.bundle_sources = vec![
            RuntimeSource::new(
                RuntimeSourceId::bundle(SCOPE_BUNDLE),
                [7; 32],
                Arc::new(()),
                vec![],
            )
            .with_apis(
                vec![SourceApi {
                    id: "usage".into(),
                    method: ApiMethod::Get,
                    scope: ApiScope::Session,
                    path: ApiPathTemplate::parse("/usage").unwrap(),
                    description: String::new(),
                    request_schema: None,
                    response_schema: None,
                }],
                Arc::new(ScopeAnswerApi),
            )
            .with_permission_modes(vec![RuntimePermissionMode {
                id: "careful".into(),
                title: "Careful".into(),
                description: String::new(),
            }]),
        ];
        runtime
            .publish_scope(scope.key(), overlay)
            .map_err(CoreError::runtime_refresh)?;
        Ok(true)
    }
}

async fn scoped_engine() -> (SessionEngine, Arc<RecordingScopeRefresh>) {
    let refresh = Arc::new(RecordingScopeRefresh::default());
    let engine = eng_with(
        builtin_only_catalog(),
        Arc::new(FakeProvider::scripted_turns(vec![vec![FakeStep::Finish(
            FinishReason::Stop,
        )]])),
    )
    .await
    .with_catalog_refresh(refresh.clone());
    (engine, refresh)
}

fn subdir(dir: &TestDir, child: &str) -> PathBuf {
    let p = dir.path().join(child);
    std::fs::create_dir_all(&p).unwrap();
    p
}

async fn mk_project(engine: &SessionEngine, roots: &[&Path]) -> ProjectId {
    let r: Vec<_> = roots
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    engine.store().create_project("demo", &r).await.unwrap().id
}

async fn mk_sess(
    engine: &SessionEngine,
    parent: Option<SessionId>,
    wd: &Path,
    proj: Option<ProjectId>,
    kind: SessionKind,
) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: wd.to_string_lossy().into_owned(),
            project: proj,
            kind,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn session_catalog_scope_project_directory_child_and_skills() {
    let dir = TestDir::new("session-scope-resolve");
    let (a, b, c, b_src, out, elsewhere) = (
        subdir(&dir, "a"),
        subdir(&dir, "b"),
        subdir(&dir, "c"),
        subdir(&dir, "b/src"),
        subdir(&dir, "out"),
        subdir(&dir, "elsewhere"),
    );
    let (engine, refresh) = scoped_engine().await;
    let id = mk_project(&engine, &[&a, &b]).await;
    let ab = CatalogScope::Project {
        id,
        roots: vec![a.clone(), b.clone()],
    };

    let proj_sess = mk_sess(&engine, None, &a, Some(id), SessionKind::Project).await;
    assert_eq!(refresh.0.lock().unwrap().last().cloned(), Some(ab.clone()));
    let bind = engine.bind_session_runtime(proj_sess, &a).await.unwrap();
    assert_eq!(*bind.scope(), ab);
    assert!(bind.resolve_agent("scoped").is_some());

    // Directory lookups + root vs global binds.
    assert_eq!(engine.catalog_scope_for_directory(Some(&b_src)).await, ab);
    assert_eq!(
        engine.catalog_scope_for_directory(Some(&out)).await,
        CatalogScope::Directory(out.clone())
    );
    assert_eq!(
        engine.catalog_scope_for_directory(None).await,
        CatalogScope::Global
    );
    assert_eq!(
        engine
            .catalog_scope_for_directory(Some(Path::new(".")))
            .await,
        CatalogScope::Directory(PathBuf::from("."))
    );
    assert_eq!(*engine.bind_root_runtime(&b_src).await.unwrap().scope(), ab);
    let global = engine.bind_global_runtime().await.unwrap();
    assert_eq!(*global.scope(), CatalogScope::Global);
    assert!(global.resolve_agent("scoped").is_none());

    // Root edit takes effect on next bind and run_turn binds that scope.
    engine
        .store()
        .update_project(
            id,
            None,
            Some(&[
                a.to_string_lossy().into_owned(),
                c.to_string_lossy().into_owned(),
            ]),
        )
        .await
        .unwrap();
    let ac = CatalogScope::Project {
        id,
        roots: vec![a.clone(), c.clone()],
    };
    assert_eq!(
        *engine
            .bind_session_runtime(proj_sess, &a)
            .await
            .unwrap()
            .scope(),
        ac
    );
    assert_eq!(
        engine.catalog_scope_for_session(proj_sess).await.unwrap(),
        ac
    );
    refresh.0.lock().unwrap().clear();
    assert_eq!(
        prompt_turn(&engine, proj_sess, "hi", &agent_spec("build", "fake", &a))
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let seen = refresh.0.lock().unwrap().clone();
    assert!(!seen.is_empty() && seen.iter().all(|s| *s == ac));

    // Child inherits parent project scope while keeping its own workdir.
    let child = mk_sess(
        &engine,
        Some(proj_sess),
        &elsewhere,
        None,
        SessionKind::Temporary,
    )
    .await;
    let cb = engine
        .bind_session_runtime(child, &elsewhere)
        .await
        .unwrap();
    assert_eq!((cb.scope(), cb.workdir()), (&ac, elsewhere.as_path()));

    // Temporary, projectless, and deleted-project sessions bind their workdir.
    for sess in [
        mk_sess(&engine, None, &out, None, SessionKind::Temporary).await,
        mk_sess(&engine, None, &out, None, SessionKind::Project).await,
        mk_sess(
            &engine,
            None,
            &out,
            Some(ProjectId::new()),
            SessionKind::Project,
        )
        .await,
    ] {
        assert_eq!(
            *engine
                .bind_session_runtime(sess, &out)
                .await
                .unwrap()
                .scope(),
            CatalogScope::Directory(out.clone())
        );
        assert_eq!(
            engine.catalog_scope_for_session(sess).await.unwrap(),
            CatalogScope::Directory(out.clone())
        );
    }

    // Multi-root project sees skills from every root in order, first root wins.
    let (sk_a, sk_b) = (TestDir::new("scope-sk-a"), TestDir::new("scope-sk-b"));
    sk_a.write_skill("shared");
    sk_a.write_skill("only-a");
    sk_b.write_skill("shared");
    sk_b.write_skill("only-b");
    let sk_id = mk_project(&engine, &[sk_a.path(), sk_b.path()]).await;
    let sk_sess = mk_sess(
        &engine,
        None,
        sk_a.path(),
        Some(sk_id),
        SessionKind::Project,
    )
    .await;
    let sk_bind = engine
        .bind_session_runtime(sk_sess, sk_a.path())
        .await
        .unwrap();
    let sdir = |name| {
        sk_bind
            .skills()
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .dir
            .clone()
    };
    assert!(
        sdir("only-a").starts_with(sk_a.path())
            && sdir("only-b").starts_with(sk_b.path())
            && sdir("shared").starts_with(sk_a.path())
    );
}

#[tokio::test]
async fn session_catalog_scope_apis_modes_invalidation_lru_and_ttl() {
    let dir = TestDir::new("session-scope-lifecycle");
    let (a, x, y, z) = (
        subdir(&dir, "a"),
        subdir(&dir, "x"),
        subdir(&dir, "y"),
        subdir(&dir, "z"),
    );

    let (engine, _) = scoped_engine().await;
    let id = mk_project(&engine, &[&a]).await;
    let mut notices = engine.subscribe_catalog_scope_invalidations();
    let sess = mk_sess(&engine, None, &a, Some(id), SessionKind::Project).await;
    let call = |session| BundleApiCall {
        bundle: SCOPE_BUNDLE.into(),
        method: ApiMethod::Get,
        session,
        path: "/usage".into(),
        query: BTreeMap::new(),
        body: Value::Null,
    };
    let out = engine.invoke_bundle_api(call(Some(sess))).await.unwrap();
    assert_eq!(
        (out.status, out.body),
        (200, json!({ "session": sess.to_string() }))
    );
    assert_eq!(engine.session_bundle_apis(sess).await.unwrap().len(), 1);
    let mode_id = format!("{SCOPE_BUNDLE}/careful");
    assert!(
        engine
            .session_permission_modes(sess)
            .await
            .unwrap()
            .iter()
            .any(|m| m.id == mode_id)
    );
    engine.set_permission_mode(sess, &mode_id).await.unwrap();

    // Global stays base-only; invalidation drops scope and notifies.
    assert!(matches!(
        engine.invoke_bundle_api(call(None)).await,
        Err(BundleApiError::NotFound { .. })
    ));
    assert!(engine.bundle_apis().await.is_empty());
    assert!(
        !engine
            .permission_modes()
            .await
            .iter()
            .any(|m| m.source == SCOPE_BUNDLE)
    );
    assert!(
        engine
            .runtime_registry()
            .scope_keys()
            .contains(&ScopeKey::Project(id))
    );
    engine.invalidate_catalog_scope(id);
    assert!(
        !engine
            .runtime_registry()
            .scope_keys()
            .contains(&ScopeKey::Project(id))
    );
    assert_eq!(notices.try_recv().unwrap(), ScopeKey::Project(id));

    // LRU cap skips in-flight bound scope.
    let (lru, _) = scoped_engine().await;
    lru.set_catalog_scope_cache_config(CatalogScopeCacheConfig {
        max_scopes: 2,
        idle_ttl: Duration::from_secs(3600),
    });
    let key = |p: &Path| ScopeKey::Directory(p.to_path_buf());
    let held = lru.bind_root_runtime(&x).await.unwrap();
    drop(lru.bind_root_runtime(&y).await.unwrap());
    drop(lru.bind_root_runtime(&z).await.unwrap());
    let keys = lru.runtime_registry().scope_keys();
    assert!(keys.contains(&key(&x)) && !keys.contains(&key(&y)) && keys.contains(&key(&z)));
    drop(held);
    drop(lru.bind_root_runtime(&y).await.unwrap());
    assert_eq!(lru.runtime_registry().scope_keys(), vec![key(&y), key(&z)]);

    // Idle TTL sweep skips held bindings and sweeps once idle past TTL.
    let (ttl, _) = scoped_engine().await;
    let ttl = ttl.with_catalog_scope_cache(CatalogScopeCacheConfig {
        max_scopes: 32,
        idle_ttl: Duration::from_millis(20),
    });
    let held = ttl.bind_root_runtime(&x).await.unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;
    ttl.sweep_catalog_scopes();
    assert_eq!(ttl.runtime_registry().scope_keys().len(), 1);
    drop(held);
    tokio::time::sleep(Duration::from_millis(35)).await;
    ttl.sweep_catalog_scopes();
    assert!(ttl.runtime_registry().scope_keys().is_empty());
}

// ===========================================================================
// Section 6: Runtime Registry, Naming/Schemes, Generations, Refresh & Rebind
// ===========================================================================

#[test]
fn runtime_registry_atomic_refresh_noop_and_concurrency() {
    let dir = TestDir::new("rt-atomic");
    dir.write_skill("stable_skill");
    let tools = hya_tool_loader::builtins();
    let stable = MarkerTool::new("stable_tool");
    tools.register(stable.clone()).unwrap();
    let reg = local_runtime(tools, test_catalog(&[("general", AgentRole::Main, &[])]));
    let before = reg.bind_turn(dir.path()).unwrap();
    let before_names = binding_tool_names(&before);

    let failed = reg.refresh(|c| {
        c.remove_tool("stable_tool");
        c.register_tool(MarkerTool::new("read"))
    });
    assert!(matches!(failed, Err(RuntimeRefreshError::DuplicateTool(_))));
    let after_fail = reg.bind_turn(dir.path()).unwrap();
    assert_eq!(
        (after_fail.generation(), binding_tool_names(&after_fail)),
        (before.generation(), before_names)
    );

    let noop = reg
        .refresh(|c| {
            c.remove_tool("stable_tool");
            c.register_tool(stable)?;
            c.refresh_skills(dir.path());
            Ok(())
        })
        .unwrap();
    assert_eq!(noop, before.generation());
    assert_eq!(
        reg.bind_turn(dir.path()).unwrap().generation(),
        before.generation()
    );

    let next = reg
        .refresh(|c| c.register_tool(MarkerTool::new("after_failure")))
        .unwrap();
    assert_eq!(next.get(), before.generation().get() + 1);

    // 8 concurrent threads produce strictly contiguous unique monotonic generations without mixing.
    const N: usize = 8;
    let cdir = Arc::new(TestDir::new("rt-concurrent"));
    let creg = Arc::new(local_runtime(
        hya_tool_loader::builtins(),
        test_catalog(&[("general", AgentRole::Main, &[])]),
    ));
    let first = creg.bind_turn(cdir.path()).unwrap().generation();
    let barrier = Arc::new(Barrier::new(N));
    let mut pubs: Vec<_> = (0..N)
        .map(|i| {
            let (r, b) = (creg.clone(), barrier.clone());
            std::thread::spawn(move || {
                b.wait();
                let generation = r
                    .refresh(|c| {
                        for p in 0..N {
                            c.remove_tool(&format!("candidate_{p}_tool"));
                            c.remove_tool(&format!("mcp__candidate_{p}__lookup"));
                        }
                        c.register_tool(MarkerTool::new(format!("candidate_{i}_tool")))?;
                        c.register_tool_with_permission(
                            MarkerTool::new(format!("mcp__candidate_{i}__lookup")),
                            ToolPermission::Mcp,
                        )
                    })
                    .unwrap();
                (generation, i)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    pubs.sort_by_key(|(g, _)| g.get());
    assert_eq!(
        pubs.iter().map(|(g, _)| g.get()).collect::<Vec<_>>(),
        (1..=N).map(|o| first.get() + o as u64).collect::<Vec<_>>()
    );
    let (_, winner) = pubs.last().unwrap();
    let active = binding_tool_names(&creg.bind_turn(cdir.path()).unwrap());
    for i in 0..N {
        assert_eq!(
            active.contains(&format!("candidate_{i}_tool")),
            i == *winner
        );
        assert_eq!(
            active.contains(&format!("mcp__candidate_{i}__lookup")),
            i == *winner
        );
    }
}

#[test]
fn runtime_registry_source_naming_masks_and_alias_rejection() {
    let dir = TestDir::new("rt-naming");
    let src = |id, digest, decl, canon, aliases| {
        RuntimeSource::new(
            id,
            [digest; 32],
            Arc::new(()),
            vec![RuntimeSourceExport::tool(
                decl,
                canon,
                aliases,
                MarkerTool::new(canon),
                ToolPermission::Tool,
            )],
        )
    };
    let mut a = prep_agent("name-registry");
    a.prompt = Some("prompt".into());
    let reg = local_runtime(hya_tool_loader::builtins(), catalog_from_agents(vec![a]));
    type CandidateMutator =
        Box<dyn FnOnce(&mut RuntimeCandidate) -> Result<(), RuntimeRefreshError>>;
    let check_err = |f: CandidateMutator, needles: &[&str]| {
        let g = reg.bind_turn(dir.path()).unwrap().generation();
        let msg = reg.refresh(f).unwrap_err().to_string();
        for n in needles {
            assert!(msg.contains(n), "missing {n:?} in {msg}");
        }
        assert_eq!(reg.bind_turn(dir.path()).unwrap().generation(), g);
    };

    let dup = RuntimeSourceId::mcp("duplicate");
    check_err(
        Box::new(move |c| {
            c.upsert_sources(vec![
                src(dup.clone(), 1, "one", "source_one", vec![]),
                src(dup, 2, "two", "source_two", vec![]),
            ])
        }),
        &["duplicate runtime source mcp:duplicate"],
    );
    check_err(
        Box::new(move |c| {
            c.upsert_sources(vec![src(
                RuntimeSourceId::plugin("bare-tools"),
                9,
                "read",
                "read",
                vec![],
            )])
        }),
        &[
            "plugin:bare-tools",
            "`read`",
            "must be qualified as `namespace__local`",
        ],
    );
    check_err(
        Box::new(move |c| {
            c.upsert_sources(vec![
                src(
                    RuntimeSourceId::plugin("dup-a"),
                    10,
                    "dup__tool",
                    "dup__tool",
                    vec![],
                ),
                src(
                    RuntimeSourceId::plugin("dup-b"),
                    11,
                    "dup__tool",
                    "dup__tool",
                    vec![],
                ),
            ])
        }),
        &["plugin:dup-a", "plugin:dup-b"],
    );
    check_err(
        Box::new(move |c| {
            c.upsert_sources(vec![src(
                RuntimeSourceId::plugin("read-masker"),
                15,
                "reader",
                "read_masker__reader",
                vec!["read".into()],
            )])
        }),
        &["protected tool `read` cannot be masked"],
    );

    reg.refresh(|c| {
        c.upsert_sources(vec![src(
            RuntimeSourceId::plugin("acc-src"),
            18,
            "acc",
            "acc__tool",
            vec!["solo".into(), "grep".into()],
        )])
    })
    .unwrap();
    let bind = reg.bind_turn(dir.path()).unwrap();
    assert_eq!(
        bind.masks(),
        &BTreeMap::from([("grep".into(), "acc__tool".into())])
    );
    assert!(bind.resolve_tool("solo").is_some());

    let mut allow_a = prep_agent("allow-alias");
    allow_a.resource_view.allow = vec!["patch".into()];
    let mut deny_a = prep_agent("deny-alias");
    deny_a.resource_view.deny = vec!["patch".into()];
    let alias_bind = local_runtime(
        hya_tool_loader::builtins(),
        catalog_from_bundles(&[
            make_bundle("hya/allow-alias", allow_a, vec![]),
            make_bundle("hya/deny-alias", deny_a, vec![]),
        ]),
    )
    .bind_turn(dir.path())
    .unwrap();
    for id in ["allow-alias", "deny-alias"] {
        assert!(
            matches!(alias_bind.agent_resource_policy(id), Err(BundleError::UnknownResourceReference { ref reference, .. }) if reference == "patch")
        );
    }
}

#[test]
fn runtime_registry_plugin_hook_order_and_schema_claims() {
    let dir = TestDir::new("rt-hooks-schemes");

    // Part A: Plugin-kind bundle hooks run in sorted source-id order before agent's own hook_refs.
    let pkg = |name, yaml, extra: Option<(&str, &str)>| {
        let mut files = vec![SourceFile::new("bundle.yaml", yaml)];
        if let Some((p, body)) = extra {
            files.push(SourceFile::new(p, body));
        }
        prepare_package(BundleSource::new(name, files)).unwrap()
    };
    let pb_pkg = pkg(
        "plugin-b",
        "schema_version: 2\nkind: Bundle\nidentity: { id: acme/plugin-b, version: 1.0.0, publisher: acme }\n",
        None,
    );
    let pa_pkg = pkg(
        "plugin-a",
        "schema_version: 2\nkind: Bundle\nidentity: { id: acme/plugin-a, version: 1.0.0, publisher: acme }\n",
        None,
    );
    let ow_pkg = pkg(
        "owner",
        "schema_version: 2\nkind: Bundle\nidentity: { id: acme/owner, version: 1.0.0, publisher: acme }\nruntime: { kind: js, mode: process, command: [fixture] }\nresources:\n  hooks: [{ id: chat.params, path: hook.json }]\nagents:\n  - { id: hooked-agent, role: main, hook_refs: [chat.params] }\n  - { id: plain-agent, role: main }\n",
        Some(("hook.json", "{}")),
    );
    let hreg = local_runtime(
        hya_tool_loader::builtins(),
        catalog_from_verified(&[&pb_pkg, &ow_pkg, &pa_pkg]),
    );
    hreg.refresh(|c| {
        c.upsert_sources(vec![
            RuntimeSource::new(
                RuntimeSourceId::bundle("acme/plugin-b"),
                [4; 32],
                Arc::new(()),
                vec![],
            )
            .with_hooks(empty_hooks()),
            RuntimeSource::new(
                RuntimeSourceId::bundle("acme/owner"),
                [5; 32],
                Arc::new(()),
                vec![],
            )
            .with_hooks(empty_hooks()),
            RuntimeSource::new(
                RuntimeSourceId::bundle("acme/plugin-a"),
                [6; 32],
                Arc::new(()),
                vec![],
            )
            .with_hooks(empty_hooks()),
        ])
    })
    .unwrap();
    let hb = hreg.bind_turn(dir.path()).unwrap();
    let (pa, pb, ow) = (
        hb.bundle_hooks("acme/plugin-a").unwrap(),
        hb.bundle_hooks("acme/plugin-b").unwrap(),
        hb.bundle_hooks("acme/owner").unwrap(),
    );
    let b_hooks = hb.bundle_hooks_for_agent("hya-main");
    assert!(b_hooks.len() == 2 && Arc::ptr_eq(&b_hooks[0], &pa) && Arc::ptr_eq(&b_hooks[1], &pb));
    let h_hooks = hb.bundle_hooks_for_agent("hooked-agent");
    assert!(
        h_hooks.len() == 3
            && Arc::ptr_eq(&h_hooks[0], &pa)
            && Arc::ptr_eq(&h_hooks[1], &pb)
            && !Arc::ptr_eq(&h_hooks[2], &ow)
    );
    let p_hooks = hb.bundle_hooks_for_agent("plain-agent");
    assert!(p_hooks.len() == 2 && Arc::ptr_eq(&p_hooks[0], &pa) && Arc::ptr_eq(&p_hooks[1], &pb));
    assert!(hb.bundle_hooks_for_agent("missing-agent").is_empty());

    // Part B & C: Scheme competition (greater source id wins) + grouped SchemaConflicts error.
    let ssrc = |id, tool: Option<&str>, schemas| {
        let exports = tool
            .map(|t| {
                vec![RuntimeSourceExport::tool(
                    t,
                    t,
                    vec![],
                    MarkerTool::new(t),
                    ToolPermission::ReadOnly,
                )]
            })
            .unwrap_or_default();
        RuntimeSource::new(RuntimeSourceId::plugin(id), [0; 32], Arc::new(()), exports)
            .with_schemas(schemas)
    };
    let sreg = local_runtime(hya_tool_loader::builtins(), builtin_only_catalog());
    sreg.refresh(|c| {
        c.upsert_sources(vec![
            ssrc(
                "aaa-src",
                Some("aaa__query"),
                vec![SourceSchema {
                    scheme: "db".into(),
                    canonical_tool: "aaa__query".into(),
                    writable: false,
                }],
            ),
            ssrc(
                "zzz-src",
                Some("zzz__query"),
                vec![SourceSchema {
                    scheme: "db".into(),
                    canonical_tool: "zzz__query".into(),
                    writable: true,
                }],
            ),
        ])
    })
    .unwrap();
    let sb = sreg.bind_turn(dir.path()).unwrap();
    let w = sb.schemes().get("db").unwrap();
    assert_eq!(
        (w.owner(), w.canonical_tool(), w.writable()),
        ("plugin:zzz-src", "zzz__query", true)
    );
    assert_eq!(
        sb.scheme_chain("db"),
        vec![
            ("plugin:aaa-src".into(), "aaa__query".into()),
            ("plugin:zzz-src".into(), "zzz__query".into())
        ]
    );

    let Err(RuntimeRefreshError::SchemaConflicts(report)) = sreg.refresh(|c| {
        c.upsert_sources(vec![
            ssrc(
                "bad-a",
                Some("bad-a__query"),
                vec![SourceSchema {
                    scheme: "db".into(),
                    canonical_tool: "not-exported__tool".into(),
                    writable: false,
                }],
            ),
            ssrc(
                "bad-b",
                None,
                vec![
                    SourceSchema {
                        scheme: "local".into(),
                        canonical_tool: "bad-b__put".into(),
                        writable: true,
                    },
                    SourceSchema {
                        scheme: "d".into(),
                        canonical_tool: "bad-b__put".into(),
                        writable: true,
                    },
                ],
            ),
        ])
    }) else {
        panic!("expected SchemaConflicts");
    };
    for frag in [
        "plugin:bad-a",
        "not-exported__tool",
        "plugin:bad-b",
        "internal scheme `local`",
        "scheme `d`",
    ] {
        assert!(report.contains(frag), "missing {frag:?} in:\n{report}");
    }

    // Part D: Bundle source schema claim pointing to view-scoped stable-id tool publishes without registry exports.
    let breg = local_runtime(
        hya_tool_loader::builtins(),
        catalog_from_bundles(&[make_bundle("hya/vecdb", prep_agent("vecdb-lead"), vec![])]),
    );
    breg.refresh(|c| {
        c.replace_sources_of_kind(
            RuntimeSourceKind::Bundle,
            vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle("hya/vecdb"),
                    [7; 32],
                    Arc::new(()),
                    vec![],
                )
                .with_schemas(vec![SourceSchema {
                    scheme: "db".into(),
                    canonical_tool: "bundle:hya/vecdb/tool/query".into(),
                    writable: false,
                }]),
            ],
        )
    })
    .unwrap();
    let bb = breg.bind_turn(dir.path()).unwrap();
    let bw = bb.schemes().get("db").unwrap();
    assert_eq!(
        (bw.owner(), bw.canonical_tool()),
        ("bundle:hya/vecdb", "bundle:hya/vecdb/tool/query")
    );
    assert_eq!(
        bb.scheme_chain("db"),
        vec![(
            "bundle:hya/vecdb".into(),
            "bundle:hya/vecdb/tool/query".into()
        )]
    );
}

#[test]
fn runtime_generation_pinning_and_cross_generation_isolation() {
    let dir = TestDir::new("rt-gen-pin");
    dir.write_skill("skill_n");
    let tools = hya_tool_loader::builtins();
    tools.register(MarkerTool::new("tool_n")).unwrap();
    tools
        .register_with_permission(MarkerTool::new("mcp__n__lookup"), ToolPermission::Mcp)
        .unwrap();
    let reg = local_runtime(
        tools,
        test_catalog(&[
            ("pinned-agent", AgentRole::Main, &[]),
            ("lead", AgentRole::Main, &["worker"]),
            ("worker", AgentRole::Main, &[]),
        ]),
    );
    let gen_n = reg.bind_turn(dir.path()).unwrap();
    assert_eq!(
        gen_n.resolve_agent("pinned-agent").unwrap().prompt,
        Some("pinned-agent prompt")
    );
    assert_eq!(
        gen_n
            .spawnable_agents("lead")
            .unwrap()
            .iter()
            .map(|a| a.stable_id)
            .collect::<Vec<_>>(),
        ["worker"]
    );
    assert!(matches!(
        gen_n.resolve_spawn("lead", "hya-title"),
        Err(BundleError::AgentSpawnNotAllowed { .. })
    ));

    dir.remove_skill("skill_n");
    dir.write_skill("skill_n_plus_1");
    let pub_tool = reg
        .refresh(|c| {
            c.remove_tool("tool_n");
            c.remove_tool("mcp__n__lookup");
            c.register_tool(MarkerTool::new("tool_n_plus_1"))?;
            c.register_tool_with_permission(
                MarkerTool::new("mcp__n_plus_1__lookup"),
                ToolPermission::Mcp,
            )?;
            c.refresh_skills(dir.path());
            Ok(())
        })
        .unwrap();
    let gen_n1 = reg.bind_turn(dir.path()).unwrap();
    assert_eq!(
        (pub_tool.get(), gen_n1.generation()),
        (gen_n.generation().get() + 1, pub_tool)
    );
    assert!(std::ptr::eq(gen_n.agent_catalog(), gen_n1.agent_catalog()));

    let (ot, os, nt, ns) = (
        binding_tool_names(&gen_n),
        binding_skills(&gen_n),
        binding_tool_names(&gen_n1),
        binding_skills(&gen_n1),
    );
    assert!(
        ot.contains("tool_n")
            && ot.contains("mcp__n__lookup")
            && !ot.contains("tool_n_plus_1")
            && os.contains("skill_n")
            && !os.contains("skill_n_plus_1")
    );
    assert!(
        !nt.contains("tool_n")
            && !nt.contains("mcp__n__lookup")
            && nt.contains("tool_n_plus_1")
            && nt.contains("mcp__n_plus_1__lookup")
            && !ns.contains("skill_n")
            && ns.contains("skill_n_plus_1")
    );

    let next_cat = test_catalog(&[
        ("pinned-agent", AgentRole::Main, &[]),
        ("installed-agent", AgentRole::Main, &[]),
    ]);
    let pub_cat = reg.publish_catalog(next_cat.clone()).unwrap();
    let gen_n2 = reg.bind_turn(dir.path()).unwrap();
    assert_eq!(
        (pub_cat.get(), gen_n2.generation()),
        (pub_tool.get() + 1, pub_cat)
    );
    assert!(
        gen_n1.resolve_agent("installed-agent").is_none()
            && gen_n2.resolve_agent("installed-agent").is_some()
    );
    assert_eq!(
        (binding_tool_names(&gen_n1), binding_skills(&gen_n1)),
        (binding_tool_names(&gen_n2), binding_skills(&gen_n2))
    );
    assert!(std::ptr::eq(gen_n2.agent_catalog(), next_cat.as_ref()));
}

#[tokio::test]
async fn runtime_catalog_refresh_entry_points_and_loop_reuse() {
    let dir = TestDir::new("rt-refresh-entries");
    let pub_cat = |calls: Arc<AtomicUsize>, cat: Arc<AgentCatalog>| {
        FnRefresh::new(calls, move |_, rt| {
            rt.publish_catalog(Arc::clone(&cat))
                .map_err(CoreError::runtime_refresh)?;
            Ok(true)
        })
    };

    // Part A: plain bind_runtime stays pure; bind_root_runtime refreshes once.
    let c_a = Arc::new(AtomicUsize::new(0));
    let eng_a = eng_with(
        builtin_only_catalog(),
        Arc::new(FakeProvider::scripted(vec![])),
    )
    .await
    .with_catalog_refresh(pub_cat(
        c_a.clone(),
        test_catalog(&[("installed-agent", AgentRole::Main, &[])]),
    ));
    assert!(
        eng_a
            .bind_runtime(dir.path())
            .unwrap()
            .resolve_agent("installed-agent")
            .is_none()
    );
    assert_eq!(c_a.load(Ordering::SeqCst), 0);
    assert!(
        eng_a
            .bind_root_runtime(dir.path())
            .await
            .unwrap()
            .resolve_agent("installed-agent")
            .is_some()
    );
    assert!(
        eng_a
            .bind_runtime(dir.path())
            .unwrap()
            .resolve_agent("installed-agent")
            .is_some()
    );
    assert_eq!(c_a.load(Ordering::SeqCst), 1);

    // Part B: run_turn and run_shell each refresh once at entry.
    let c_turn = Arc::new(AtomicUsize::new(0));
    let eng_t = eng_with(
        builtin_only_catalog(),
        Arc::new(FakeProvider::scripted(vec![FakeStep::Finish(
            FinishReason::Stop,
        )])),
    )
    .await;
    let s_t = create_root_session(&eng_t, "installed-agent", "fake", dir.path()).await;
    eng_t.admit_user_prompt(s_t, "go".into()).await.unwrap();
    let eng_t = eng_t.with_catalog_refresh(pub_cat(
        c_turn.clone(),
        test_catalog(&[("installed-agent", AgentRole::Main, &[])]),
    ));
    assert_eq!(
        eng_t
            .run_turn(
                s_t,
                &agent_spec("hya-task", "fake", dir.path()),
                CancellationToken::new()
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(c_turn.load(Ordering::SeqCst), 1);

    let c_shell = Arc::new(AtomicUsize::new(0));
    let eng_s = eng_with(
        builtin_only_catalog(),
        Arc::new(FakeProvider::scripted(vec![])),
    )
    .await;
    let s_s = create_root_session(&eng_s, "installed-agent", "fake", dir.path()).await;
    let eng_s = eng_s.with_catalog_refresh(pub_cat(
        c_shell.clone(),
        test_catalog(&[("installed-agent", AgentRole::Main, &[])]),
    ));
    let (_, fin) = eng_s
        .run_shell(
            s_s,
            &agent_spec("hya-task", "fake", dir.path()),
            "printf catalog-refresh-shell".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        (fin, c_shell.load(Ordering::SeqCst)),
        (FinishReason::Stop, 1)
    );

    // Part C: run_loop refreshes once at admission and reuses binding across 2 iterations.
    struct TwoIterVerifier(AtomicUsize);
    #[async_trait]
    impl LoopVerifier for TwoIterVerifier {
        async fn grade(&self, _: &str, _: &str) -> Result<VerifierVerdict, CoreError> {
            let sat = self.0.fetch_add(1, Ordering::SeqCst) == 1;
            Ok(VerifierVerdict {
                score: if sat { 100 } else { 0 },
                satisfied: sat,
                evidence_quality: if sat {
                    EvidenceQuality::Verified
                } else {
                    EvidenceQuality::ClaimOnly
                },
                critical_gaps: if sat { vec![] } else { vec!["continue".into()] },
                iteration_summary: "iter".into(),
                reason: String::new(),
            })
        }
    }
    struct StaticPlanner;
    #[async_trait]
    impl LoopPlanner for StaticPlanner {
        async fn plan_next(
            &self,
            _: &str,
            _: &[String],
            _: &VerifierVerdict,
            _: &str,
        ) -> Result<PlannerOutput, CoreError> {
            Ok(PlannerOutput {
                directive: "continue".into(),
                continuity_brief: String::new(),
                planner_notes: String::new(),
                strategy_change: false,
                change_note: String::new(),
            })
        }
    }
    let c_loop = Arc::new(AtomicUsize::new(0));
    let (cat_a, cat_b) = (
        test_catalog(&[("loop-agent", AgentRole::Main, &[])]),
        builtin_only_catalog(),
    );
    let eng_l = eng_with(
        builtin_only_catalog(),
        Arc::new(FakeProvider::scripted_turns(vec![
            vec![FakeStep::Finish(FinishReason::Stop)],
            vec![FakeStep::Finish(FinishReason::Stop)],
        ])),
    )
    .await;
    let lead = create_root_session(&eng_l, "loop-agent", "fake", dir.path()).await;
    let eng_l = Arc::new(eng_l.with_catalog_refresh(FnRefresh::new(
        c_loop.clone(),
        move |call, rt| {
            rt.publish_catalog(Arc::clone(if call == 0 { &cat_a } else { &cat_b }))
                .map_err(CoreError::runtime_refresh)?;
            Ok(true)
        },
    )));
    let out = run_loop(
        eng_l,
        lead,
        agent_spec("loop-agent", "fake", dir.path()),
        "target".into(),
        Arc::new(TwoIterVerifier(AtomicUsize::new(0))),
        Arc::new(StaticPlanner),
        LoopConfig {
            budget: 2,
            ..LoopConfig::default()
        },
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert!(matches!(out, RunOutcome::Achieved { iterations: 2, .. }));
    assert_eq!(c_loop.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn round_rebind_tools_failure_and_prompt_updates() {
    let dir = TestDir::new("round-rebind-main");
    let mk_rt = |cat| {
        let t = hya_tool_loader::builtins();
        t.register(MarkerTool::new("marker")).unwrap();
        Arc::new(local_runtime(t, cat))
    };

    // Case A: new tool published between rounds is advertised in round 2.
    let (pa, ca) = (
        RecordingProvider::new().with_first_tools(false, &[("marker", json!({}))]),
        Arc::new(AtomicUsize::new(0)),
    );
    let ea = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(pa.clone())),
        mk_rt(builtin_only_catalog()),
        allow_all(),
    )
    .await;
    let sa = create_root_session(&ea, "hya-task", "fake", dir.path()).await;
    ea.admit_user_prompt(sa, "marker".into()).await.unwrap();
    let ea = ea.with_catalog_refresh(FnRefresh::new(ca.clone(), |call, rt| {
        if call != 1 {
            return Ok(false);
        }
        rt.refresh(|c| c.register_tool(MarkerTool::new("late_tool")))
            .map_err(CoreError::runtime_refresh)?;
        Ok(true)
    }));
    assert_eq!(
        ea.run_turn(
            sa,
            &agent_spec("hya-task", "fake", dir.path()),
            CancellationToken::new()
        )
        .await
        .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(ca.load(Ordering::SeqCst), 2);
    let ra = pa.requests();
    assert!(
        ra.len() == 2
            && !req_tools(&ra[0]).contains(&"late_tool".into())
            && req_tools(&ra[0]).contains(&"marker".into())
            && req_tools(&ra[1]).contains(&"late_tool".into())
    );

    // Case B: rebind failure is fail-open and keeps activation-time tools.
    let (pb, cb) = (
        RecordingProvider::new().with_first_tools(false, &[("marker", json!({}))]),
        Arc::new(AtomicUsize::new(0)),
    );
    let eb = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(pb.clone())),
        mk_rt(builtin_only_catalog()),
        allow_all(),
    )
    .await;
    let sb = create_root_session(&eb, "hya-task", "fake", dir.path()).await;
    eb.admit_user_prompt(sb, "marker".into()).await.unwrap();
    let eb = eb.with_catalog_refresh(FnRefresh::new(cb.clone(), |call, _| {
        if call == 0 {
            Ok(false)
        } else {
            Err(CoreError::Invalid("simulated refresh failure".into()))
        }
    }));
    assert_eq!(
        eb.run_turn(
            sb,
            &agent_spec("hya-task", "fake", dir.path()),
            CancellationToken::new()
        )
        .await
        .unwrap(),
        FinishReason::Stop
    );
    let rb = pb.requests();
    assert_eq!(
        (cb.load(Ordering::SeqCst), rb.len(), req_tools(&rb[1])),
        (2, 2, req_tools(&rb[0]))
    );

    // Case C: agent prompt update at round boundary replaces v1 with v2 without stacking.
    let mk_heated = |p: &str| {
        let mut a = prep_agent("heated-agent");
        a.prompt = Some(p.into());
        catalog_from_agents(vec![a])
    };
    let (pc, cc) = (
        RecordingProvider::new().with_first_tools(false, &[("marker", json!({}))]),
        Arc::new(AtomicUsize::new(0)),
    );
    let ec = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(pc.clone())),
        mk_rt(mk_heated("round-rebind prompt v1")),
        allow_all(),
    )
    .await;
    let sc = create_root_session(&ec, "heated-agent", "fake", dir.path()).await;
    ec.admit_user_prompt(sc, "marker".into()).await.unwrap();
    let v2 = mk_heated("round-rebind prompt v2");
    let ec = ec.with_catalog_refresh(FnRefresh::new(cc.clone(), move |call, rt| {
        if call != 1 {
            return Ok(false);
        }
        rt.publish_catalog(Arc::clone(&v2))
            .map_err(CoreError::runtime_refresh)?;
        Ok(true)
    }));
    assert_eq!(
        ec.run_turn(
            sc,
            &agent_spec("hya-task", "fake", dir.path()),
            CancellationToken::new()
        )
        .await
        .unwrap(),
        FinishReason::Stop
    );
    let rc = pc.requests();
    assert_eq!((cc.load(Ordering::SeqCst), rc.len()), (2, 2));
    assert!(
        rc[0]
            .system
            .as_deref()
            .unwrap_or("")
            .contains("round-rebind prompt v1")
    );
    let s2 = rc[1].system.as_deref().unwrap_or("");
    assert!(s2.contains("round-rebind prompt v2") && !s2.contains("round-rebind prompt v1"));
}

#[tokio::test]
async fn round_rebind_initial_context_member_isolation_and_hooks() {
    struct GatedTurnProvider {
        call: AtomicUsize,
        requests: mpsc::UnboundedSender<(usize, CompletionRequest)>,
        release_first: Arc<Notify>,
    }
    #[async_trait]
    impl Provider for GatedTurnProvider {
        fn id(&self) -> &str {
            "gated"
        }
        fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
            Some(Capabilities {
                streaming_tool_calls: true,
                ..Capabilities::default()
            })
        }
        async fn stream(
            &self,
            req: CompletionRequest,
            s: SessionId,
            m: MessageId,
        ) -> Result<EventStream, ProviderError> {
            let call = self.call.fetch_add(1, Ordering::Relaxed);
            let _ = self.requests.send((call, req));
            let script = if call == 0 {
                self.release_first.notified().await;
                vec![
                    FakeStep::ToolCall {
                        name: "generation_n".into(),
                        input: json!({}),
                    },
                    FakeStep::ToolCall {
                        name: "skill".into(),
                        input: json!({ "name": "skill_n" }),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ]
            } else {
                vec![FakeStep::Finish(FinishReason::Stop)]
            };
            Ok(Box::pin(stream::iter(
                FakeProvider::materialize(&script, s, m).into_iter().map(Ok),
            )))
        }
    }

    // Part A: mid-round refresh advances round 2 tools & TurnBindingRecorded while InitialContext stays fixed.
    let dir = TestDir::new("turn-binding-rebind");
    dir.write_skill("skill_n");
    let agents_md = dir.path().join("AGENTS.md");
    std::fs::write(&agents_md, "INITIAL_PROJECT_INSTRUCTIONS").unwrap();
    let (old_c, new_c) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let tools = hya_tool_loader::builtins();
    tools
        .register_with_permission(
            Arc::new(CountingTool("generation_n", old_c.clone())),
            ToolPermission::ReadOnly,
        )
        .unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let engine = Arc::new(
        engine_with_router_and_runtime(
            Arc::new(ProviderRouter::new().with(Arc::new(GatedTurnProvider {
                call: AtomicUsize::new(0),
                requests: tx,
                release_first: release.clone(),
            }))),
            test_runtime(Arc::new(tools)),
            PermissionRules::new(vec![Rule::new(Action::Skill, "*", Mode::Allow)]),
        )
        .await,
    );
    let sess = create_root_session(&engine, "build", "gated/model", dir.path()).await;
    let spec = agent_spec("build", "gated/model", dir.path());
    engine
        .admit_user_prompt(sess, "first".into())
        .await
        .unwrap();

    let (ec, sc) = (engine.clone(), spec.clone());
    let turn1 = tokio::spawn(async move { ec.run_turn(sess, &sc, CancellationToken::new()).await });
    let (c0, req0) = rx.recv().await.unwrap();
    assert_eq!(c0, 0);
    let init = engine
        .read_projection(sess)
        .await
        .unwrap()
        .session
        .initial_context
        .unwrap();
    assert!(init.contains("INITIAL_PROJECT_INSTRUCTIONS") && init.contains("skill_n"));

    std::fs::write(&agents_md, "UPDATED_PROJECT_INSTRUCTIONS").unwrap();
    dir.remove_skill("skill_n");
    dir.write_skill("skill_fresh");
    let published = engine
        .refresh_runtime(|c| {
            c.remove_tool("generation_n");
            c.register_tool_with_permission(
                Arc::new(CountingTool("generation_n_plus_1", new_c.clone())),
                ToolPermission::ReadOnly,
            )?;
            c.refresh_skills(dir.path());
            Ok(())
        })
        .unwrap();
    release.notify_one();
    assert_eq!(turn1.await.unwrap().unwrap(), FinishReason::Stop);
    let (c1, req1) = rx.recv().await.unwrap();
    assert_eq!(c1, 1);
    assert!(
        req_tools(&req0).contains(&"generation_n".into())
            && !req_tools(&req0).contains(&"generation_n_plus_1".into())
    );
    assert!(
        req_tools(&req1).contains(&"generation_n_plus_1".into())
            && !req_tools(&req1).contains(&"generation_n".into())
    );
    assert_eq!(
        (
            &req1.system,
            old_c.load(Ordering::Relaxed),
            new_c.load(Ordering::Relaxed)
        ),
        (&req0.system, 1, 0)
    );
    assert!(engine.read_projection(sess).await.unwrap().session.messages.iter().flat_map(|m| &m.parts).any(|p| matches!(
        p, PartProjection::Tool { name, state: ToolPartState::Completed { output, .. }, .. }
            if name.as_str() == "skill" && output["title"].as_str() == Some("Loaded skill: skill_n")
    )));

    assert_eq!(
        prompt_turn(&engine, sess, "second", &spec).await.unwrap(),
        FinishReason::Stop
    );
    let (c2, req2) = rx.recv().await.unwrap();
    assert_eq!(c2, 2);
    assert!(
        req_tools(&req2).contains(&"generation_n_plus_1".into())
            && !req_tools(&req2).contains(&"generation_n".into())
    );
    assert_eq!(req2.system, req0.system);
    let evs = engine.replay(sess).await.unwrap();
    assert_eq!(
        evs.iter()
            .filter(|e| matches!(e.event, Event::InitialContextRecorded { .. }))
            .count(),
        1
    );
    let gens: Vec<_> = evs
        .into_iter()
        .filter_map(|e| match e.event {
            Event::TurnBindingRecorded { generation, .. } => Some(generation),
            _ => None,
        })
        .collect();
    assert!(
        gens.len() == 3
            && gens[0].get() < published.get()
            && (gens[1], gens[2]) == (published, published)
    );

    // Part B: bound resident member rounds do not trigger another refresh.
    let tools_m = hya_tool_loader::builtins();
    tools_m.register(MarkerTool::new("marker")).unwrap();
    let (pm, cm) = (
        RecordingProvider::new().with_first_tools(false, &[("marker", json!({}))]),
        Arc::new(AtomicUsize::new(0)),
    );
    let em = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(pm.clone())),
        Arc::new(local_runtime(tools_m, builtin_only_catalog())),
        allow_all(),
    )
    .await;
    let lead = create_root_session(&em, "hya-task", "fake", dir.path()).await;
    let em = Arc::new(em.with_catalog_refresh(FnRefresh::new(cm.clone(), |_, _| Ok(false))));
    let rspec = ResidentTurn {
        agent: agent_spec("hya-task", "fake", dir.path()),
        binding: em.bind_runtime(dir.path()).unwrap(),
        agents: Arc::from([]),
        resources: None,
        guidance: None,
        directive: "two rounds".into(),
        description: String::new(),
        sidecar_factory: None,
        tool_call: Some(ToolCallId::new()),
    };
    assert_eq!(
        run_residents(em, lead, vec![rspec], CancellationToken::new())
            .await
            .len(),
        1
    );
    assert_eq!((cm.load(Ordering::SeqCst), pm.requests().len()), (1, 2));

    // Part C: root round 2 adopts a newly published bundle hook when round 1 had none.
    let pkg = prepare_package(BundleSource::new(
        "hook",
        vec![SourceFile::new(
            "bundle.yaml",
            "schema_version: 2\nkind: Bundle\nidentity: { id: acme/hot-hook, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let tools_h = hya_tool_loader::builtins();
    tools_h.register(MarkerTool::new("marker")).unwrap();
    let ph = RecordingProvider::new().with_first_tools(false, &[("marker", json!({}))]);
    let eh = engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(ph.clone())),
        Arc::new(local_runtime(tools_h, catalog_from_verified(&[&pkg]))),
        allow_all(),
    )
    .await;
    let sh = create_root_session(&eh, "general", "fake", dir.path()).await;
    eh.admit_user_prompt(sh, "two rounds".into()).await.unwrap();
    let eh = eh.with_catalog_refresh(FnRefresh::new(Arc::new(AtomicUsize::new(0)), |call, rt| {
        if call != 1 {
            return Ok(false);
        }
        rt.refresh(|c| {
            c.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/hot-hook"),
                    [2; 32],
                    Arc::new(()),
                    vec![],
                )
                .with_hooks(Arc::new(HookProbe {
                    temperature: Some(0.9),
                    ..HookProbe::healthy()
                })),
            ])
        })
        .map_err(CoreError::runtime_refresh)?;
        Ok(true)
    }));
    eh.run_turn(
        sh,
        &agent_spec("general", "fake", dir.path()),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        ph.requests()
            .iter()
            .map(|r| r.temperature)
            .collect::<Vec<_>>(),
        vec![None, Some(0.9)]
    );
}

// ===========================================================================
// Section 7: Root Turn Bundle Precedence & Sidecar Lifecycle (Zero $HOME)
// ===========================================================================

const BASE_MARKER: &str = "ROOT_TURN_BASE_PROMPT_MARKER";
const AGENTS_MARKER: &str = "ROOT_TURN_AGENTS_CONTEXT_MARKER";
const BUNDLE_PROMPT: &str = "EXACT_BUNDLE_PROMPT_FOR_EXPLORE";

fn composed_base(workdir: PathBuf) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("base-caller"),
        model: ModelRef::new("base-model"),
        system_prompt: [
            BASE_MARKER,
            "Base agent.",
            "",
            "## Project context: AGENTS.md",
            AGENTS_MARKER,
            "",
            "## Environment",
            "- cwd: /composed",
        ]
        .join("\n"),
        workdir,
        reasoning: Some(ReasoningEffort::Low),
    }
}

fn skill_header_count(s: &str) -> usize {
    s.matches("Skills found at session start; use the skill tool to load a current skill:")
        .count()
}

struct HookProbe {
    events: Arc<Mutex<Vec<Envelope>>>,
    healthy: Arc<AtomicBool>,
    temperature: Option<f32>,
    fail_event: Option<Arc<AtomicUsize>>,
    fail_before: Option<Arc<AtomicUsize>>,
    fail_after: Option<Arc<AtomicUsize>>,
}

impl HookProbe {
    fn healthy() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            healthy: Arc::new(AtomicBool::new(true)),
            temperature: None,
            fail_event: None,
            fail_before: None,
            fail_after: None,
        }
    }
}

#[async_trait]
impl HookDispatcher for HookProbe {
    fn dispatch_event(&self, env: &Envelope) {
        self.events.lock().unwrap().push(env.clone());
        if let Some(c) = &self.fail_event {
            c.fetch_add(1, Ordering::SeqCst);
            self.healthy.store(false, Ordering::SeqCst);
        }
    }
    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }
    async fn command_execute_before(
        &self,
        i: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: i.text }
    }
    async fn text_complete(&self, i: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: i.text }
    }
    async fn message_user_before(&self, i: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: i.text }
    }
    async fn chat_params(&self, mut i: ChatParamsInput) -> ChatParamsOutcome {
        if let Some(t) = self.temperature {
            i.request.temperature = Some(t);
        }
        ChatParamsOutcome::Continue { request: i.request }
    }
    async fn tool_execute_before(&self, i: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        if let Some(c) = &self.fail_before {
            c.fetch_add(1, Ordering::SeqCst);
            self.healthy.store(false, Ordering::SeqCst);
        }
        ToolExecuteBeforeOutcome::Continue { input: i.input }
    }
    async fn tool_execute_after(&self, i: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        if let Some(c) = &self.fail_after {
            c.fetch_add(1, Ordering::SeqCst);
            self.healthy.store(false, Ordering::SeqCst);
        }
        ToolExecuteAfterOutcome::Continue { result: i.result }
    }
}

#[derive(Default)]
struct SidecarSpy {
    resolver_calls: Mutex<Vec<(ConfigGeneration, String)>>,
    starts: Mutex<Vec<SidecarStart>>,
    tool_bindings: AtomicUsize,
    hook_accessors: AtomicUsize,
    shutdowns: AtomicUsize,
    terminates: AtomicUsize,
    handle_calls: AtomicUsize,
    start_gate: Option<(Arc<Notify>, Arc<Notify>)>,
    ready_gate: Option<(Arc<Notify>, Arc<Notify>)>,
    loss_token: Option<CancellationToken>,
    cancel_loss_on_ready: bool,
    hooks: Option<Arc<dyn HookDispatcher>>,
    shutdown_failure: Option<String>,
}

struct SpyEnv(Arc<SidecarSpy>);
struct SpyFactory(Arc<SidecarSpy>);
struct SpyHandle(Arc<SidecarSpy>);

impl SidecarEnvironment for SpyEnv {
    fn factory_for(
        &self,
        binding: &TurnBinding,
        id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        self.0
            .resolver_calls
            .lock()
            .unwrap()
            .push((binding.generation(), id.into()));
        Ok(Some(Arc::new(SpyFactory(self.0.clone()))))
    }
}
#[async_trait]
impl BoundSidecarFactory for SpyFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.0.starts.lock().unwrap().push(start);
        if let Some((e, r)) = &self.0.start_gate {
            e.notify_one();
            r.notified().await;
        }
        Ok(Box::new(SpyHandle(self.0.clone())))
    }
}
#[async_trait]
impl SidecarHandle for SpyHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.0.handle_calls.fetch_add(1, Ordering::SeqCst);
        if let Some((e, r)) = &self.0.ready_gate {
            e.notify_one();
            r.notified().await;
        }
        if self.0.cancel_loss_on_ready
            && let Some(t) = &self.0.loss_token
        {
            t.cancel();
        }
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.0.handle_calls.fetch_add(1, Ordering::SeqCst);
        self.0.shutdowns.fetch_add(1, Ordering::SeqCst);
        if let Some(m) = &self.0.shutdown_failure {
            return Err(CoreError::Invalid(m.clone()));
        }
        Ok(())
    }
    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.0.handle_calls.fetch_add(1, Ordering::SeqCst);
        self.0.terminates.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.0.handle_calls.fetch_add(1, Ordering::SeqCst);
        self.0.tool_bindings.fetch_add(1, Ordering::SeqCst);
        Arc::from([])
    }
    fn hook_dispatcher(&self) -> Option<Arc<dyn HookDispatcher>> {
        self.0.hooks.as_ref().map(|h| {
            self.0.hook_accessors.fetch_add(1, Ordering::SeqCst);
            h.clone()
        })
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        self.0.loss_token.clone()
    }
}

async fn sidecar_engine(provider: Arc<dyn Provider>, spy: Arc<SidecarSpy>) -> Arc<SessionEngine> {
    Arc::new(
        eng_with(builtin_only_catalog(), provider)
            .await
            .with_sidecar_environment(Arc::new(SpyEnv(spy))),
    )
}

#[tokio::test]
async fn root_turn_bundle_prompt_missing_agent_and_workdir_precedence() {
    let dir = TestDir::new("root-prec-prompt");
    dir.write_skill("session-skill");
    let bdir = TestDir::new("root-prec-bdir");
    bdir.write_skill("from-bundle");
    let adir = TestDir::new("root-prec-adir");
    adir.write_skill("from-agent");

    let mut exp = prep_agent("bundle-explorer");
    exp.prompt = Some(BUNDLE_PROMPT.into());
    let mut hm = prep_agent("hya-main");
    hm.workdir = Some(bdir.path().to_string_lossy().into_owned());
    let provider = RecordingProvider::new();
    let engine = eng_with(
        catalog_from_agents(vec![prep_agent("plain-main"), exp, hm]),
        provider.clone(),
    )
    .await;

    // 1. Missing definition continues as hya-task.
    let ghost = create_root_session(&engine, "ghost", "session-model", dir.path()).await;
    prompt_turn(
        &engine,
        ghost,
        "go",
        &composed_base(dir.path().to_path_buf()),
    )
    .await
    .unwrap();
    assert_eq!(
        engine
            .read_projection(ghost)
            .await
            .unwrap()
            .session
            .agent
            .as_ref()
            .map(AgentName::as_str),
        Some("hya-task")
    );
    assert!(
        provider.requests()[0]
            .system
            .as_deref()
            .unwrap()
            .contains("You are hya-task")
    );

    // 2. prompt=None preserves composed base + AGENTS marker and uses session-model.
    let plain = create_root_session(&engine, "plain-main", "session-model", dir.path()).await;
    assert_eq!(
        prompt_turn(
            &engine,
            plain,
            "base",
            &composed_base(PathBuf::from("/agent/base"))
        )
        .await
        .unwrap(),
        FinishReason::Stop
    );
    let r1 = &provider.requests()[1];
    let s1 = r1.system.as_deref().unwrap();
    assert!(
        s1.contains(BASE_MARKER)
            && s1.contains(AGENTS_MARKER)
            && !s1.contains(BUNDLE_PROMPT)
            && r1.model.as_str() == "session-model"
    );

    // 3. Non-empty bundle prompt replaces base once after kernel prompt and sees zero workdir skills.
    let explorer =
        create_root_session(&engine, "bundle-explorer", "session-model", dir.path()).await;
    prompt_turn(
        &engine,
        explorer,
        "replace",
        &composed_base(dir.path().to_path_buf()),
    )
    .await
    .unwrap();
    let s2 = provider.requests()[2]
        .system
        .as_deref()
        .unwrap()
        .to_string();
    assert!(
        s2.split_once("</cwd>\n\n")
            .is_some_and(|(_, a)| a.starts_with(BUNDLE_PROMPT))
    );
    assert!(
        !s2.contains(BASE_MARKER) && !s2.contains(AGENTS_MARKER) && !s2.contains("session-skill")
    );
    assert_eq!(
        (skill_header_count(&s2), s2.matches(BUNDLE_PROMPT).count()),
        (0, 1)
    );

    // 4. Session workdir wins over bundle & base workdir and records exactly one TurnBindingRecorded.
    let ws = create_root_session(&engine, "hya-main", "session-model", dir.path()).await;
    prompt_turn(&engine, ws, "wd", &composed_base(adir.path().to_path_buf()))
        .await
        .unwrap();
    let s3 = provider.requests()[3]
        .system
        .as_deref()
        .unwrap()
        .to_string();
    assert!(
        s3.contains("session-skill")
            && !s3.contains("from-bundle")
            && !s3.contains("from-agent")
            && skill_header_count(&s3) == 1
    );
    assert_eq!(
        engine
            .replay(ws)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(e.event, Event::TurnBindingRecorded { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn root_turn_bundle_model_and_reasoning_precedence() {
    let dir = TestDir::new("root-prec-model-reason");
    let mut hm = prep_agent("hya-main");
    hm.model_policy.model = Some("bundle-default-model".into());
    hm.model_policy.category = Some("ultrabrain".into());
    let mut wr = prep_agent("with-reasoning");
    wr.model_policy.reasoning = Some("high".into());
    let provider = RecordingProvider::new();
    let engine = eng_with(
        catalog_from_agents(vec![hm, wr, prep_agent("no-reasoning")]),
        provider.clone(),
    )
    .await;
    let base = composed_base(dir.path().to_path_buf());

    let s = create_root_session(&engine, "hya-main", "created-session-model", dir.path()).await;
    prompt_turn(&engine, s, "created", &base).await.unwrap();
    engine
        .switch_model(s, ModelRef::new("switched-model"))
        .await
        .unwrap();
    prompt_turn(&engine, s, "switched", &base).await.unwrap();

    let r_over = create_root_session(&engine, "with-reasoning", "session-model", dir.path()).await;
    prompt_turn(&engine, r_over, "over", &base).await.unwrap();
    let r_keep = create_root_session(&engine, "no-reasoning", "session-model", dir.path()).await;
    prompt_turn(&engine, r_keep, "keep", &base).await.unwrap();

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 4);
    assert_eq!(
        (reqs[0].model.as_str(), reqs[1].model.as_str()),
        ("created-session-model", "switched-model")
    );
    assert_eq!(
        (reqs[2].reasoning, reqs[3].reasoning),
        (Some(ReasoningEffort::High), Some(ReasoningEffort::Low))
    );
}

#[tokio::test]
async fn root_sidecar_ack_gate_generation_length_and_shutdown_error() {
    let dir = TestDir::new("root-sidecar-ack");
    let spec = agent_spec("hya-main", "session-model", dir.path());

    // Part A: ACK gate blocks dispatch/model poll/assistant events; second turn picks up N+1 generation.
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let spy = Arc::new(SidecarSpy {
        ready_gate: Some((entered.clone(), release.clone())),
        ..SidecarSpy::default()
    });
    let prov = RecordingProvider::new();
    let eng = sidecar_engine(prov.clone(), spy.clone()).await;
    let sess = create_root_session(&eng, "hya-main", "session-model", dir.path()).await;
    eng.admit_user_prompt(sess, "first".into()).await.unwrap();

    let (e1, s1) = (eng.clone(), spec.clone());
    let t1 = tokio::spawn(async move { e1.run_turn(sess, &s1, CancellationToken::new()).await });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    let cap_gen = spy.resolver_calls.lock().unwrap()[0].0;
    let mut marker = prep_agent("sidecar-gen-marker");
    marker.prompt = Some("gen N+1".into());
    let pub_gen = eng
        .runtime_registry()
        .publish_catalog(catalog_from_agents(vec![marker]))
        .unwrap();
    assert!(
        pub_gen.get() > cap_gen.get()
            && spy.tool_bindings.load(Ordering::SeqCst) == 0
            && prov.requests().is_empty()
    );
    assert!(!eng.replay(sess).await.unwrap().iter().any(|e| matches!(
        e.event,
        Event::MessageStarted {
            role: Role::Assistant,
            ..
        }
    )));
    assert_eq!(
        spy.starts.lock().unwrap()[0].lifecycle,
        SidecarLifecycle::Transient
    );

    release.notify_one();
    assert_eq!(t1.await.unwrap().unwrap(), FinishReason::Stop);
    let rec_gen = eng
        .replay(sess)
        .await
        .unwrap()
        .into_iter()
        .find_map(|e| match e.event {
            Event::TurnBindingRecorded { generation, .. } => Some(generation),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (
            spy.resolver_calls.lock().unwrap()[0].0,
            spy.tool_bindings.load(Ordering::SeqCst),
            spy.shutdowns.load(Ordering::SeqCst),
            spy.terminates.load(Ordering::SeqCst)
        ),
        (rec_gen, 1, 1, 0)
    );

    eng.admit_user_prompt(sess, "second".into()).await.unwrap();
    let (e2, s2) = (eng.clone(), spec.clone());
    let t2 = tokio::spawn(async move { e2.run_turn(sess, &s2, CancellationToken::new()).await });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    assert_eq!(spy.resolver_calls.lock().unwrap()[1].0, pub_gen);
    release.notify_one();
    assert_eq!(t2.await.unwrap().unwrap(), FinishReason::Stop);
    assert_eq!(
        (
            spy.starts.lock().unwrap().len(),
            spy.tool_bindings.load(Ordering::SeqCst),
            spy.shutdowns.load(Ordering::SeqCst),
            spy.terminates.load(Ordering::SeqCst)
        ),
        (2, 2, 2, 0)
    );

    // Part B: FinishReason::Length gracefully shuts down.
    let spy_l = Arc::new(SidecarSpy::default());
    let eng_l = sidecar_engine(
        RecordingProvider::new().with_finish(FinishReason::Length),
        spy_l.clone(),
    )
    .await;
    let s_l = create_root_session(&eng_l, "hya-main", "session-model", dir.path()).await;
    assert_eq!(
        prompt_turn(&eng_l, s_l, "len", &spec).await.unwrap(),
        FinishReason::Length
    );
    assert_eq!(
        (
            spy_l.shutdowns.load(Ordering::SeqCst),
            spy_l.terminates.load(Ordering::SeqCst)
        ),
        (1, 0)
    );

    // Part C: shutdown failure surfaces as turn error instead of Ok.
    let spy_e = Arc::new(SidecarSpy {
        shutdown_failure: Some("root sidecar shutdown failed".into()),
        ..SidecarSpy::default()
    });
    let prov_e = RecordingProvider::new();
    let eng_e = sidecar_engine(prov_e.clone(), spy_e.clone()).await;
    let s_e = create_root_session(&eng_e, "hya-main", "session-model", dir.path()).await;
    assert!(
        matches!(prompt_turn(&eng_e, s_e, "fail", &spec).await, Err(CoreError::Invalid(ref m)) if m == "root sidecar shutdown failed")
    );
    assert_eq!(
        (
            prov_e.requests().len(),
            spy_e.shutdowns.load(Ordering::SeqCst),
            spy_e.terminates.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
}

#[tokio::test]
async fn root_sidecar_transport_loss_after_ack_in_flight_and_pending_start() {
    let dir = TestDir::new("root-sidecar-loss");
    let spec = agent_spec("hya-main", "session-model", dir.path());

    // Part A: transport loss right after ready ACK cancels before dispatch/model poll.
    let spy_a = Arc::new(SidecarSpy {
        loss_token: Some(CancellationToken::new()),
        cancel_loss_on_ready: true,
        ..SidecarSpy::default()
    });
    let prov_a = RecordingProvider::new();
    let eng_a = sidecar_engine(prov_a.clone(), spy_a.clone()).await;
    let s_a = create_root_session(&eng_a, "hya-main", "session-model", dir.path()).await;
    let p_a = eng_a
        .admit_user_prompt(s_a, "loss after ACK".into())
        .await
        .unwrap();
    let err_a = eng_a
        .run_turn_with_external_dirs_and_guidance(
            s_a,
            &spec,
            CancellationToken::new(),
            &[],
            None,
            None,
            Some(p_a),
        )
        .await
        .unwrap_err();
    assert!(matches!(err_a, CoreError::Cancelled) && prov_a.requests().is_empty());
    assert_eq!(
        (
            spy_a.terminates.load(Ordering::SeqCst),
            spy_a.shutdowns.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    let ev_a = eng_a.replay(s_a).await.unwrap();
    let starts: Vec<_> = ev_a
        .iter()
        .filter_map(|e| match e.event {
            Event::MessageStarted {
                message,
                role: Role::Assistant,
                ..
            } => Some(message),
            _ => None,
        })
        .collect();
    let finishes: Vec<_> = ev_a
        .iter()
        .filter_map(|e| match e.event {
            Event::MessageFinished {
                message,
                role: Role::Assistant,
                finish,
                cause,
                ..
            } => Some((message, finish, cause)),
            _ => None,
        })
        .collect();
    assert_eq!(
        (starts.len(), finishes),
        (1, vec![(starts[0], FinishReason::Cancelled, None)])
    );
    assert!(
        !ev_a
            .iter()
            .any(|e| matches!(e.event, Event::TurnBindingRecorded { .. }))
    );

    // Part B: transport loss during in-flight model stream terminates before released output.
    let (me, mr) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let loss_b = CancellationToken::new();
    let spy_b = Arc::new(SidecarSpy {
        loss_token: Some(loss_b.clone()),
        ..SidecarSpy::default()
    });
    let prov_b = RecordingProvider::new().with_gate(me.clone(), mr.clone());
    let eng_b = sidecar_engine(prov_b.clone(), spy_b.clone()).await;
    let s_b = create_root_session(&eng_b, "hya-main", "session-model", dir.path()).await;
    eng_b
        .admit_user_prompt(s_b, "in-flight loss".into())
        .await
        .unwrap();
    let (eb, sb) = (eng_b.clone(), spec.clone());
    let tb = tokio::spawn(async move { eb.run_turn(s_b, &sb, CancellationToken::new()).await });
    me.notified().await;
    loss_b.cancel();
    mr.notify_one();
    assert!(matches!(tb.await.unwrap(), Err(CoreError::Cancelled)));
    assert_eq!(
        (
            prov_b.requests().len(),
            spy_b.terminates.load(Ordering::SeqCst),
            spy_b.shutdowns.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
    let ev_b = eng_b.replay(s_b).await.unwrap();
    assert!(!ev_b.iter().any(|e| matches!(
        e.event,
        Event::MessageFinished {
            role: Role::Assistant,
            finish: FinishReason::Stop,
            ..
        }
    )));
    assert_eq!(
        ev_b.iter()
            .filter(|e| matches!(
                e.event,
                Event::MessageFinished {
                    role: Role::Assistant,
                    finish: FinishReason::Cancelled,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(
        eng_b
            .read_projection(s_b)
            .await
            .unwrap()
            .session
            .messages
            .last()
            .unwrap()
            .cause
            .is_none()
    );

    // Part C: cancel while factory start is pending stops immediately before model poll.
    let (se, sr) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let spy_c = Arc::new(SidecarSpy {
        start_gate: Some((se.clone(), sr)),
        ..SidecarSpy::default()
    });
    let prov_c = RecordingProvider::new();
    let eng_c = sidecar_engine(prov_c.clone(), spy_c.clone()).await;
    let s_c = create_root_session(&eng_c, "hya-main", "session-model", dir.path()).await;
    eng_c
        .admit_user_prompt(s_c, "cancel start".into())
        .await
        .unwrap();
    let cancel_c = CancellationToken::new();
    let (ec, sc, cc) = (eng_c.clone(), spec.clone(), cancel_c.clone());
    let mut tc = tokio::spawn(async move { ec.run_turn(s_c, &sc, cc).await });
    se.notified().await;
    cancel_c.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), &mut tc)
            .await
            .unwrap()
            .unwrap(),
        Err(CoreError::Cancelled)
    ));
    assert!(prov_c.requests().is_empty());
    assert_eq!(
        (
            spy_c.resolver_calls.lock().unwrap().len(),
            spy_c.starts.lock().unwrap().len(),
            spy_c.handle_calls.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
}

#[tokio::test]
async fn root_sidecar_activation_hooks_observe_events_and_fence_transport_loss() {
    let dir = TestDir::new("root-sidecar-hooks");
    let spec = agent_spec("hya-main", "session-model", dir.path());

    // Part A: healthy sidecar hook observes exact post-ACK event sequence (never user_message).
    let probe_ok = HookProbe::healthy();
    let observed = probe_ok.events.clone();
    let spy_ok = Arc::new(SidecarSpy {
        hooks: Some(Arc::new(probe_ok)),
        ..SidecarSpy::default()
    });
    let eng_ok = sidecar_engine(RecordingProvider::new(), spy_ok.clone()).await;
    let s_ok = create_root_session(&eng_ok, "hya-main", "session-model", dir.path()).await;
    assert_eq!(
        prompt_turn(&eng_ok, s_ok, "hooks", &spec).await.unwrap(),
        FinishReason::Stop
    );
    assert_eq!(
        (
            spy_ok.hook_accessors.load(Ordering::SeqCst),
            spy_ok.shutdowns.load(Ordering::SeqCst),
            spy_ok.terminates.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
    let seq: Vec<&str> = observed
        .lock()
        .unwrap()
        .iter()
        .filter_map(|env| match &env.event {
            Event::MessageStarted {
                session,
                role: Role::User,
                ..
            } if *session == s_ok => Some("user_message"),
            Event::MessageStarted {
                session,
                role: Role::Assistant,
                ..
            } if *session == s_ok => Some("assistant_message_started"),
            Event::TurnBindingRecorded { session, .. } if *session == s_ok => {
                Some("turn_binding_recorded")
            }
            Event::StepStarted { session, .. } if *session == s_ok => Some("step_started"),
            Event::StepFinished { session, .. } if *session == s_ok => Some("step_finished"),
            Event::MessageFinished {
                session,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            } if *session == s_ok => Some("assistant_message_finished"),
            _ => None,
        })
        .collect();
    assert_eq!(
        seq,
        vec![
            "assistant_message_started",
            "turn_binding_recorded",
            "step_started",
            "step_finished",
            "assistant_message_finished"
        ]
    );

    // Part B & C: transport loss during tool_execute_before or tool_execute_after fences commit.
    for fail_before in [false, true] {
        let (bc, ac) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let spy = Arc::new(SidecarSpy {
            hooks: Some(Arc::new(HookProbe {
                fail_before: fail_before.then(|| bc.clone()),
                fail_after: Some(ac.clone()),
                ..HookProbe::healthy()
            })),
            ..SidecarSpy::default()
        });
        let prov = Arc::new(FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "echo".into(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![FakeStep::Finish(FinishReason::Stop)],
        ]));
        let eng = sidecar_engine(prov, spy.clone()).await;
        let sess = create_root_session(&eng, "hya-main", "session-model", dir.path()).await;
        assert!(matches!(
            prompt_turn(&eng, sess, "echo", &spec).await,
            Err(CoreError::Cancelled)
        ));
        assert_eq!(
            (bc.load(Ordering::SeqCst), ac.load(Ordering::SeqCst)),
            if fail_before { (1, 0) } else { (0, 1) }
        );
        let evs = eng.replay(sess).await.unwrap();
        assert!(!evs.iter().any(|e| matches!(&e.event, Event::ToolResult { .. }) || matches!(&e.event, Event::ToolError { value, .. } if value.as_ref().and_then(|v| v.get("code")) != Some(&json!("CANCELLED")))));
        assert!(!evs.iter().any(|e| matches!(
            e.event,
            Event::MessageFinished {
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            }
        )));
        assert_eq!(
            (
                spy.terminates.load(Ordering::SeqCst),
                spy.shutdowns.load(Ordering::SeqCst)
            ),
            (1, 0)
        );
    }

    // Part D: transport loss on event dispatch stops before model poll and StepStarted.
    let ec = Arc::new(AtomicUsize::new(0));
    let spy_ev = Arc::new(SidecarSpy {
        hooks: Some(Arc::new(HookProbe {
            fail_event: Some(ec.clone()),
            ..HookProbe::healthy()
        })),
        ..SidecarSpy::default()
    });
    let prov_ev = RecordingProvider::new();
    let eng_ev = sidecar_engine(prov_ev.clone(), spy_ev.clone()).await;
    let s_ev = create_root_session(&eng_ev, "hya-main", "session-model", dir.path()).await;
    assert!(matches!(
        prompt_turn(&eng_ev, s_ev, "ev loss", &spec).await,
        Err(CoreError::Cancelled)
    ));
    assert!(ec.load(Ordering::SeqCst) >= 1 && prov_ev.requests().is_empty());
    assert_eq!(
        (
            spy_ev.terminates.load(Ordering::SeqCst),
            spy_ev.shutdowns.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    let evs = eng_ev.replay(s_ev).await.unwrap();
    assert!(!evs.iter().any(|e| matches!(
        e.event,
        Event::StepStarted { .. }
            | Event::MessageFinished {
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            }
    )));
}

// ===========================================================================
// Section 8: Bundle API Routing
// ===========================================================================

struct BundleApiRecorder {
    seen: Mutex<Vec<BundleApiRequest>>,
    reply: Option<BundleApiReply>,
}

#[async_trait]
impl BundleApiProvider for BundleApiRecorder {
    async fn request(&self, req: BundleApiRequest) -> Result<BundleApiReply, String> {
        self.seen.lock().unwrap().push(req);
        self.reply
            .clone()
            .ok_or_else(|| "plugin connection closed".into())
    }
}

fn bundle_api_source(reply: Option<BundleApiReply>) -> (RuntimeSource, Arc<BundleApiRecorder>) {
    let api = |id: &str, method, scope, path| SourceApi {
        id: id.into(),
        method,
        scope,
        path: ApiPathTemplate::parse(path).unwrap(),
        description: String::new(),
        request_schema: None,
        response_schema: None,
    };
    let rec = Arc::new(BundleApiRecorder {
        seen: Mutex::new(Vec::new()),
        reply,
    });
    let src = RuntimeSource::new(
        RuntimeSourceId::bundle("acme/api"),
        [1; 32],
        Arc::new(()),
        vec![],
    )
    .with_apis(
        vec![
            api("usage", ApiMethod::Get, ApiScope::Session, "/usage"),
            api("get-item", ApiMethod::Get, ApiScope::Global, "/items/{id}"),
            api("put-item", ApiMethod::Put, ApiScope::Global, "/items/{id}"),
            api("latest", ApiMethod::Get, ApiScope::Global, "/latest"),
        ],
        rec.clone(),
    );
    (src, rec)
}

fn api_call(
    method: ApiMethod,
    session: Option<SessionId>,
    path: &str,
    body: Value,
) -> BundleApiCall {
    BundleApiCall {
        bundle: "acme/api".into(),
        method,
        session,
        path: path.into(),
        query: BTreeMap::from([("q".into(), "1".into())]),
        body,
    }
}

fn api_reply(status: u16, body: Value) -> Option<BundleApiReply> {
    Some(BundleApiReply { status, body })
}

#[tokio::test]
async fn bundle_api_routing_params_scopes_and_method_matching() {
    let (src, rec) = bundle_api_source(api_reply(201, json!({"stored": true})));
    let apis: &SourceApis = src.apis().unwrap();
    let out = apis
        .invoke(api_call(
            ApiMethod::Put,
            None,
            "/items/a%2Fb",
            json!({"value": 1}),
        ))
        .await
        .unwrap();
    assert_eq!(
        (out.api.as_str(), out.status, out.body),
        ("put-item", 201, json!({"stored": true}))
    );
    {
        let s = &rec.seen.lock().unwrap()[0];
        assert_eq!(
            (
                s.api.as_str(),
                s.method,
                s.path.as_str(),
                s.query["q"].as_str(),
                s.session
            ),
            ("put-item", ApiMethod::Put, "/items/a%2Fb", "1", None)
        );
        assert_eq!(
            (&s.path_params, &s.body),
            (
                &BTreeMap::from([("id".into(), "a/b".into())]),
                &json!({"value": 1})
            )
        );
    }

    let session = SessionId::new();
    let (s2, rec2) = bundle_api_source(api_reply(200, json!([])));
    s2.apis()
        .unwrap()
        .invoke(api_call(
            ApiMethod::Get,
            Some(session),
            "/usage",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(rec2.seen.lock().unwrap()[0].session, Some(session));
    for (sess, path) in [(None, "/usage"), (Some(session), "/latest")] {
        assert!(matches!(
            s2.apis()
                .unwrap()
                .invoke(api_call(ApiMethod::Get, sess, path, Value::Null))
                .await
                .unwrap_err(),
            BundleApiError::NotFound { .. }
        ));
    }

    let err = apis
        .invoke(api_call(ApiMethod::Delete, None, "/items/7", Value::Null))
        .await
        .unwrap_err();
    assert!(
        matches!(err, BundleApiError::MethodNotAllowed { ref allow, .. } if allow == &[ApiMethod::Get, ApiMethod::Put])
    );
    for path in ["/items/", "/items/7/x", "/nothing"] {
        assert!(matches!(
            apis.invoke(api_call(ApiMethod::Get, None, path, Value::Null))
                .await
                .unwrap_err(),
            BundleApiError::NotFound { .. }
        ));
    }
}

#[tokio::test]
async fn bundle_api_routing_validation_and_reply_failures() {
    let (src, rec) = bundle_api_source(api_reply(200, Value::Null));
    let apis = src.apis().unwrap();
    for (method, path, body) in [
        (ApiMethod::Get, "/items/%zz", Value::Null),
        (
            ApiMethod::Put,
            "/items/1",
            Value::String("x".repeat(MAX_BUNDLE_API_BODY_BYTES)),
        ),
    ] {
        assert!(matches!(
            apis.invoke(api_call(method, None, path, body))
                .await
                .unwrap_err(),
            BundleApiError::BadRequest(_)
        ));
    }
    assert!(rec.seen.lock().unwrap().is_empty());

    for reply in [
        None,
        api_reply(199, Value::Null),
        api_reply(600, Value::Null),
        api_reply(204, json!({"unexpected": true})),
    ] {
        let (s, _) = bundle_api_source(reply);
        assert!(
            matches!(s.apis().unwrap().invoke(api_call(ApiMethod::Get, None, "/latest", Value::Null)).await.unwrap_err(), BundleApiError::Failed { ref api, .. } if api == "latest")
        );
    }

    let (ok_src, _) = bundle_api_source(api_reply(204, Value::Null));
    let out = ok_src
        .apis()
        .unwrap()
        .invoke(api_call(ApiMethod::Get, None, "/latest", Value::Null))
        .await
        .unwrap();
    assert_eq!((out.status, out.body), (204, Value::Null));
}
