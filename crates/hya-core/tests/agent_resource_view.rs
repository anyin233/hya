//! Integration tests for `hya-core`: agent resource view.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent,
    PreparedBundle, PreparedResource, ResourceView, SpawnLifecycle,
};
use hya_core::{AgentCatalog, AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{AgentName, FinishReason, MessageId, ModelRef, SessionId, ToolName, ToolSchema};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError, ToolRegistry,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct CapturingProvider {
    seen: Mutex<HashSet<SessionId>>,
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
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
        let first = self.seen.lock().unwrap().insert(session);
        let script = if first {
            vec![
                FakeStep::ToolCall {
                    name: "dynamic_marker".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![FakeStep::Finish(FinishReason::Stop)]
        };
        let events = FakeProvider::materialize(&script, session, message);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

struct CountingTool {
    calls: Arc<AtomicUsize>,
}

struct AliasProvider {
    turn: AtomicUsize,
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait]
impl Provider for AliasProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
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
        let script = if self.turn.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                FakeStep::ToolCall {
                    name: "marker".to_string(),
                    input: json!({}),
                },
                FakeStep::ToolCall {
                    name: "dynamic_marker".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![FakeStep::Finish(FinishReason::Stop)]
        };
        let events = FakeProvider::materialize(&script, session, message);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "dynamic_marker"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name()),
            description: "dynamic marker".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    }
}

/// Two installed bundle agents, both on the clamped plane.
///
/// `narrowed-agent` allows only its own bundle skill, so nothing Harness-owned
/// enters its view at all. `clamped-agent` takes the default view, so it sees
/// the internal public tool snapshot plus its own bundle resources. The full
/// plane is reachable only through a built-in, which is the point of the split.
/// Two installed bundle agents, both on the clamped plane.
///
/// `narrowed-agent` allows only its own bundle skill, so nothing Harness-owned
/// enters its view at all. `clamped-agent` takes the default view, so it sees
/// the internal public tool snapshot plus its own bundle resources. The full
/// plane is reachable only through a built-in, which is the point of the split.
///
/// Each agent gets its own bundle, and therefore its own copy of the skill.
fn catalog() -> Arc<AgentCatalog> {
    let bundles = [
        ("narrowed", "narrowed-agent", true),
        ("clamped", "clamped-agent", false),
    ]
    .into_iter()
    .map(|(bundle_slug, stable_id, narrow)| {
        let bundle_id = format!("hya/resource-view-{bundle_slug}");
        let skill_id = format!("bundle:{bundle_id}/skill/bundle-skill");
        PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: bundle_id.clone(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: format!("test-only-{bundle_slug}"),
            agent: PreparedAgent {
                id: AgentName::new(stable_id),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                resource_view: if narrow {
                    ResourceView {
                        allow: vec![skill_id.clone()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    }
                } else {
                    ResourceView::default()
                },
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
            },
            tools: Vec::new(),
            skills: vec![PreparedResource {
                local_id: "bundle-skill".to_string(),
                stable_id: skill_id,
                source_path: "resources/skills/bundle-skill.md".to_string(),
                digest: "test-only".to_string(),
                content:
                    "---\nname: bundle-skill\ndescription: embedded bundle skill\n---\nbundle body\n"
                        .to_string(),
                aliases: Vec::new(),
            }],
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        }
    })
    .collect::<Vec<_>>();
    let bundles = BundleCatalog::from_prepared(&bundles).expect("valid bundle catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog"))
}

#[tokio::test]
async fn agent_origin_decides_the_visible_tool_skill_and_mcp_plane() {
    let workdir = support::TestDir::new("agent-resource-access");
    workdir.write_skill("workdir-skill");
    let provider = Arc::new(CapturingProvider {
        seen: Mutex::new(HashSet::new()),
        requests: Mutex::new(Vec::new()),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog()));
    runtime
        .refresh(|candidate| {
            candidate.register_tool(Arc::new(CountingTool {
                calls: calls.clone(),
            }))
        })
        .unwrap();
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "*",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        runtime,
        permission,
        EventBus::default(),
    );
    let base = AgentSpec {
        name: AgentName::new("general"),
        model: ModelRef::new("fake"),
        system_prompt: "base".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };

    // `general` is a built-in, so it is the only one of the three on the full
    // plane. The other two are installed bundle agents and are clamped.
    for stable_id in ["narrowed-agent", "clamped-agent", "general"] {
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new(stable_id),
                model: ModelRef::new("fake"),
                workdir: workdir.path().to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        engine
            .admit_user_prompt(session, "exercise resources".to_string())
            .await
            .unwrap();
        engine
            .run_turn(session, &base, CancellationToken::new())
            .await
            .unwrap();
    }

    let first_requests = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .step_by(2)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(first_requests.len(), 3);
    let tool_names = |request: &CompletionRequest| {
        request
            .tools
            .iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<Vec<_>>()
    };
    // narrowed: allow-list admits only the bundle skill, so no Harness tool.
    assert!(!tool_names(&first_requests[0]).contains(&"read".to_string()));
    assert!(!tool_names(&first_requests[0]).contains(&"dynamic_marker".to_string()));
    assert!(!tool_names(&first_requests[0]).contains(&"skill".to_string()));
    // clamped: the internal public snapshot, but never a later-registered tool.
    assert!(tool_names(&first_requests[1]).contains(&"read".to_string()));
    assert!(
        !tool_names(&first_requests[1]).contains(&"dynamic_marker".to_string()),
        "a bundle agent must not see a tool registered after the registry was built"
    );
    // builtin: the live snapshot, including the later-registered tool.
    assert!(tool_names(&first_requests[2]).contains(&"read".to_string()));
    assert!(tool_names(&first_requests[2]).contains(&"dynamic_marker".to_string()));

    for request in &first_requests[..2] {
        assert!(
            request
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("bundle-skill"),
            "a bundle agent always sees its own bundle skill"
        );
    }
    let narrowed_system = first_requests[0].system.as_deref().unwrap_or_default();
    assert!(
        narrowed_system.contains("bundle body"),
        "with no skill facade the local static skill body must be inlined, got: {narrowed_system}"
    );

    // Project/user skills discovered from the workdir belong to the Harness
    // plane, so only the built-in sees them.
    assert!(!narrowed_system.contains("workdir-skill"));
    assert!(
        !first_requests[1]
            .system
            .as_deref()
            .unwrap_or_default()
            .contains("workdir-skill"),
        "a bundle agent must not see project or user skills"
    );
    let builtin_system = first_requests[2].system.as_deref().unwrap_or_default();
    assert!(builtin_system.contains("workdir-skill"));
    assert!(
        !builtin_system.contains("bundle-skill"),
        "a builtin owns no bundle, so it must not see bundle-local skills"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn canonical_allow_deny_and_alias_share_schema_and_dispatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let tools = ToolRegistry::builtins();
    tools
        .register(Arc::new(CountingTool {
            calls: calls.clone(),
        }))
        .unwrap();
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/alias-test".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        digest: "test-only".to_string(),
        agent: PreparedAgent {
            id: AgentName::new("alias-agent"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("alias prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            resource_view: ResourceView {
                allow: vec![
                    "harness:tool/dynamic_marker".to_string(),
                    "harness:tool/read".to_string(),
                ],
                deny: vec!["harness:tool/read".to_string()],
                aliases: BTreeMap::from([(
                    "marker".to_string(),
                    "harness:tool/dynamic_marker".to_string(),
                )]),
                namespace: None,
            },
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        },
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    let catalog = agent_catalog(bundle);
    let provider = Arc::new(AliasProvider {
        turn: AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "dynamic_marker",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        Arc::new(RuntimeRegistry::new(tools, catalog)),
        permission,
        EventBus::default(),
    );
    let workdir = support::TestDir::new("agent-resource-alias");
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("alias-agent"),
            model: ModelRef::new("fake"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "use the marker".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("alias-agent"),
                model: ModelRef::new("fake"),
                system_prompt: String::new(),
                workdir: workdir.path().to_path_buf(),
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    let names = requests[0]
        .tools
        .iter()
        .map(|schema| schema.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"marker"));
    assert!(!names.contains(&"dynamic_marker"));
    assert!(!names.contains(&"read"), "deny must win over allow");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "only the selected public alias may dispatch the canonical tool"
    );
}

#[tokio::test]
async fn mcp_selected_public_name_dispatches_once_with_canonical_permission() {
    struct McpProvider {
        requests: Mutex<Vec<CompletionRequest>>,
        turn: AtomicUsize,
    }

    #[async_trait]
    impl Provider for McpProvider {
        fn id(&self) -> &str {
            "fake"
        }

        fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
            Some(Capabilities {
                streaming_tool_calls: true,
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
            let script = if self.turn.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    FakeStep::ToolCall {
                        name: "mcp__fixture__ping".to_string(),
                        input: json!({}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ]
            } else {
                vec![FakeStep::Finish(FinishReason::Stop)]
            };
            let events = FakeProvider::materialize(&script, session, message);
            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
        }
    }

    struct McpPing {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for McpPing {
        fn name(&self) -> &str {
            "mcp__fixture__ping"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new(self.name()),
                description: "mcp ping".to_string(),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "pong": true }))
        }
    }

    let calls = Arc::new(AtomicUsize::new(0));
    // Harness MCP exports live on the full plane, so only a built-in can select
    // one. `general` takes the default (unfiltered) view.
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        support::builtin_only_catalog(),
    ));
    runtime
        .refresh(|candidate| {
            candidate.upsert_sources(vec![hya_core::RuntimeSource::new(
                hya_core::RuntimeSourceId::mcp("fixture"),
                [9; 32],
                Arc::new(()),
                vec![hya_core::RuntimeSourceExport::tool(
                    "ping",
                    "mcp__fixture__ping",
                    Vec::new(),
                    Arc::new(McpPing {
                        calls: calls.clone(),
                    }),
                    hya_tool::ToolPermission::Mcp,
                )],
            )])
        })
        .unwrap();

    let provider = Arc::new(McpProvider {
        requests: Mutex::new(Vec::new()),
        turn: AtomicUsize::new(0),
    });
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    // Authorize by the source MCP canonical identity, not a fabricated public spelling.
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Mcp,
        "mcp__fixture__ping",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        runtime,
        permission,
        EventBus::default(),
    );
    let workdir = support::TestDir::new("agent-resource-mcp");
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("general"),
            model: ModelRef::new("fake"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "ping mcp".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("general"),
                model: ModelRef::new("fake"),
                system_prompt: String::new(),
                workdir: workdir.path().to_path_buf(),
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    let names = requests[0]
        .tools
        .iter()
        .map(|schema| schema.name.as_str())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"mcp__fixture__ping"),
        "provider schema must include selected MCP public name: {names:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "selected MCP public name must dispatch the source tool exactly once"
    );
}

#[tokio::test]
async fn a_bundle_agent_cannot_select_a_harness_mcp_export_or_skill() {
    // The clamp is fail-closed and says why: an out-of-plane reference is not
    // reported as "unknown", which would send an author hunting a typo.
    let workdir = support::TestDir::new("agent-resource-plane-refusal");
    for (slug, reference) in [
        ("mcp", "harness:mcp/mcp__fixture__ping"),
        ("skill", "harness:skill/anything"),
    ] {
        let bundle_id = format!("hya/plane-refusal-{slug}");
        let bundle = PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: bundle_id.clone(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: format!("test-only-{slug}"),
            agent: PreparedAgent {
                id: AgentName::new(format!("plane-refusal-{slug}")),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: Some("plane refusal prompt".to_string()),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                resource_view: ResourceView {
                    allow: vec![reference.to_string()],
                    deny: Vec::new(),
                    aliases: BTreeMap::new(),
                    namespace: None,
                },
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
            },
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        };
        let runtime = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            agent_catalog(bundle),
        ));
        let binding = runtime.bind_turn(workdir.path()).expect("bind turn");
        // A public path that compiles the effective view and propagates the
        // refusal, rather than swallowing it.
        let compiled =
            binding.has_selected_bundle_sidecar_capability(&format!("plane-refusal-{slug}"));
        let Err(hya_bundle::BundleError::ResourceNotInPlane { plane, .. }) = compiled else {
            panic!("`{reference}` must be refused as out-of-plane, got {compiled:?}");
        };
        assert_eq!(plane, "internal-public");
    }
}

/// Wrap one prepared bundle as an agent catalog alongside the compiled-in built-ins.
fn agent_catalog(bundle: PreparedBundle) -> Arc<AgentCatalog> {
    let bundles = BundleCatalog::from_prepared(&[bundle]).expect("valid bundle catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog"))
}
