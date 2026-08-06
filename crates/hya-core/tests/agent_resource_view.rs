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
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, PreparedResource, ResourceView, SpawnLifecycle,
};
use hya_core::{AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine};
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

fn catalog() -> Arc<BundleCatalog> {
    let agents = [
        ("none-agent", HarnessAccess::None),
        ("basic-agent", HarnessAccess::Basic),
        ("full-agent", HarnessAccess::Full),
    ]
    .into_iter()
    .map(|(stable_id, harness_access)| PreparedAgent {
        local_id: stable_id.to_string(),
        stable_id: AgentName::new(stable_id),
        description: None,
        role: AgentRole::Main,
        color: None,
        prompt: Some(format!("{stable_id} prompt")),
        prompt_source: None,
        prompt_digest: None,
        model_policy: ModelPolicy::default(),
        workdir: None,
        spawn_lifecycle: SpawnLifecycle::Transient,
        harness_access,
        resource_view: ResourceView::default(),
        can_spawn: Vec::new(),
        hook_refs: Vec::new(),
    })
    .collect();
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/resource-view-test".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents,
        tools: Vec::new(),
        skills: vec![PreparedResource {
            local_id: "bundle-skill".to_string(),
            stable_id: "bundle:hya/resource-view-test/skill/bundle-skill".to_string(),
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
    };
    Arc::new(BundleCatalog::from_prepared(&[bundle]).unwrap())
}

#[tokio::test]
async fn harness_access_filters_schema_dispatch_and_skill_prompt() {
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

    for stable_id in ["none-agent", "basic-agent", "full-agent"] {
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
    assert!(!tool_names(&first_requests[0]).contains(&"read".to_string()));
    assert!(!tool_names(&first_requests[0]).contains(&"dynamic_marker".to_string()));
    assert!(!tool_names(&first_requests[0]).contains(&"skill".to_string()));
    assert!(tool_names(&first_requests[1]).contains(&"read".to_string()));
    assert!(!tool_names(&first_requests[1]).contains(&"dynamic_marker".to_string()));
    assert!(tool_names(&first_requests[2]).contains(&"read".to_string()));
    assert!(tool_names(&first_requests[2]).contains(&"dynamic_marker".to_string()));

    for request in &first_requests {
        assert!(
            request
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("bundle-skill"),
            "bundle-local skill must be independent of harness access"
        );
    }
    let none_system = first_requests[0].system.as_deref().unwrap_or_default();
    assert!(
        none_system.contains("bundle body"),
        "None must inline selected local static skill body into the prompt, got: {none_system}"
    );
    assert!(!none_system.contains("workdir-skill"));
    assert!(
        !first_requests[1]
            .system
            .as_deref()
            .unwrap_or_default()
            .contains("workdir-skill")
    );
    assert!(
        first_requests[2]
            .system
            .as_deref()
            .unwrap_or_default()
            .contains("workdir-skill")
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
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: vec![PreparedAgent {
            local_id: "alias-agent".to_string(),
            stable_id: AgentName::new("alias-agent"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("alias prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            harness_access: HarnessAccess::Full,
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
        }],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    let catalog = Arc::new(BundleCatalog::from_prepared(&[bundle]).unwrap());
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
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/mcp-engine".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: vec![PreparedAgent {
            local_id: "mcp-agent".to_string(),
            stable_id: AgentName::new("mcp-agent"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("mcp prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            harness_access: HarnessAccess::Full,
            resource_view: ResourceView {
                allow: vec!["harness:mcp/mcp__fixture__ping".to_string()],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        }],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    let catalog = Arc::new(BundleCatalog::from_prepared(&[bundle]).unwrap());
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
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
            agent: AgentName::new("mcp-agent"),
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
                name: AgentName::new("mcp-agent"),
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
