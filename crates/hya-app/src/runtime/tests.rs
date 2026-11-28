#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::sidecar::{
    bind_bundle_sidecar_tools, materialize_bundle_sidecar_resources, validate_bundle_sidecar_hooks,
};
use super::*;
use crate::bundle_registry::{
    BundleInstallCandidate, BundleInstallOutcome, BundleRegistry, NamespaceInstallPolicy,
};
use crate::bundle_runtime::sidecar::environment::validate_bundle_sidecar_skills;
use crate::test_env::{EnvGuard, StableEnvGuard};
use async_trait::async_trait;
use hya_bundle::{
    AgentRole, Bundle, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent, PreparedResource,
    ResourceView, SourceFile, prepare_package,
};
use hya_engine::CategoryEntry;
use hya_engine::CreateSession;
use hya_engine::SessionStoreExt as _;
use hya_engine::resident::ResolvedSpawnMember;
use hya_engine::{SidecarEnvironment, SidecarLifecycle, SidecarStart};
use hya_plugin::PluginContributionSet;
use hya_plugin::config::ProcessLaunch;
use hya_plugin::messages::{
    ActivationLifecycle, ActivationMetadata, HookName, HookRegistration, METHOD_TOOL_CALL,
    PluginKindWire, ToolCallParams, ToolInfo,
};
use hya_plugin::protocol::Frame;
use hya_plugin::{PluginHost, PreparedPlugin};
use hya_proto::{
    Event, FinishReason, MailEndpoint, MailKind, MessageId, OwnerRunId, RosterStatus, SubagentMode,
    ToolName, ToolSchema, WorkflowIdentity, WorkflowRevision,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
};
use hya_tool::extension::{
    ExtensionEvent, projection_reducers, team_projection, workflow_projection,
};
use hya_tool::{
    AgentDef, ArchiveReason, InlineAgent, InteractionPlane, Mode, PermissionModel, PermissionPlane,
    PermissionRules, Rule, SpawnMember, Tool, ToolCtx, ToolError, ToolOperation, ToolPermission,
    WorkflowRunStatus, WorkflowStagePlan,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio_util::sync::CancellationToken;

fn to_agent_catalog(bundles: BundleCatalog) -> AgentCatalog {
    AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog")
}

fn agent_bundles(bundles: impl IntoIterator<Item = Bundle>) -> Vec<Bundle> {
    bundles.into_iter().collect()
}

struct CountingDevProvider {
    calls: Arc<AtomicUsize>,
    inner: DevProvider,
    gate: Option<Arc<ProviderGate>>,
}

struct ProviderGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl Provider for CountingDevProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        if model.as_str() == "dev" {
            return self.inner.capabilities(&ModelRef::new("hya/offline"));
        }
        self.inner.capabilities(model)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.stream(request, session, message).await
    }
}

struct SessionRecordingProvider {
    sessions: Arc<std::sync::Mutex<Vec<SessionId>>>,
}

#[async_trait]
impl Provider for SessionRecordingProvider {
    fn id(&self) -> &str {
        "hya"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        DevProvider::new().capabilities(model)
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.sessions.lock().unwrap().push(session);
        DevProvider::new().stream(request, session, message).await
    }
}

struct RuntimeMarker(&'static str);

#[async_trait]
impl Tool for RuntimeMarker {
    fn name(&self) -> &str {
        self.0
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: format!("{} runtime marker", self.0),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

struct ToolViewProvider {
    inner: FakeProvider,
    requests: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for ToolViewProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.stream(request, session, message).await
    }
}

fn tempdir() -> PathBuf {
    static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-app-runtime-test-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

fn mcp_fixture() -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}
    else:
        result = {"content":{"ok":True},"isError":False}
    print(json.dumps({"jsonrpc":"2.0","id":req["id"],"result":result}), flush=True)
"#
        .to_string(),
    ]
}

fn plugin_fixture(id: &str) -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        format!(
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {{
            "protocol_version":1,
            "plugin":{{"id":"{id}","version":"1","kind":"rust"}},
            "hooks":[],
            "tools":[{{"name":"plugin_ping","description":"Ping","inputSchema":{{"type":"object"}}}}]
        }}
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":result}}), flush=True)
    elif req.get("method") == "shutdown":
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{}}}}), flush=True)
        sys.exit(0)
    elif "id" in req:
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{"ok":True,"output":{{}}}}}}), flush=True)
"#
        ),
    ]
}

async fn review_runtime() -> (BuiltSessionEngine, AgentSpec, SessionId) {
    let (router, model) = offline_router(None);
    let mut agent = agent_with_model(&model, None);
    agent.workdir = tempdir();
    let built = build_session_engine_pure(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let root = built
        .engine()
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    (built, agent, root)
}

async fn engine_with_catalog(catalog: Arc<AgentCatalog>) -> SessionEngine {
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        hya_tool_loader::builtins().snapshot(),
        catalog,
    ));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
        runtime,
        permission,
        EventBus::default(),
    )
}

fn catalog_with_agents(stable_ids: &[&str]) -> Arc<AgentCatalog> {
    let bundles = stable_ids
        .iter()
        .map(|stable_id| Bundle {
            check: None,
            tui: None,
            format_version: 4,
            identity: BundleIdentity {
                id: format!("hya/recovery-resolution-{stable_id}"),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            overrides: None,
            digest: format!("test-only-{stable_id}"),
            agents: vec![PreparedAgent {
                private: false,
                id: AgentName::new(*stable_id),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: Some(format!("{stable_id} recovery prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                resource_view: ResourceView::default(),
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
                advertise: Vec::new(),
                handoff: hya_bundle::HandoffPolicy::Model,
            }],
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
            workflow: None,
            channels: vec![],
            schemas: vec![],
            process: None,
            apis: vec![],
            commands: Vec::new(),
            runtime: None,
            permission_modes: vec![],
            cli: Vec::new(),
            requires: Vec::new(),
            default_agent: None,
        })
        .collect::<Vec<_>>();
    Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&bundles).expect("valid recovery catalog"),
    ))
}

fn catalog_with_worker_policy(model_policy: ModelPolicy) -> Arc<AgentCatalog> {
    let agent = PreparedAgent {
        private: false,
        id: AgentName::new("worker"),
        description: None,
        role: AgentRole::Subagent,
        color: None,
        prompt: Some("worker prompt".to_string()),
        prompt_source: None,
        prompt_digest: None,
        model_policy,
        workdir: None,
        resource_view: ResourceView::default(),
        can_spawn: Vec::new(),
        hook_refs: Vec::new(),
        advertise: Vec::new(),
        handoff: hya_bundle::HandoffPolicy::Model,
    };
    let bundle = Bundle {
        check: None,
        tui: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/spawn-model-precedence-worker".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        overrides: None,
        digest: "test-only-worker".to_string(),
        agents: vec![agent],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        commands: Vec::new(),
        runtime: None,
        permission_modes: vec![],
        cli: Vec::new(),
        requires: Vec::new(),
        default_agent: None,
    };
    Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&[bundle]).expect("valid precedence bundle catalog"),
    ))
}

fn precedence_categories() -> CategoryRegistry {
    let mut entries = HashMap::new();
    for (name, candidate) in [
        ("bundle-cat", "cat/bundle-model"),
        ("inline-cat", "cat/inline-model"),
        ("spawn-cat", "cat/spawn-model"),
    ] {
        entries.insert(
            name.to_string(),
            CategoryEntry::from_candidates(&[candidate.to_string()]).unwrap(),
        );
    }
    CategoryRegistry::from_entries(entries)
}

fn activation_sidecar_fixture(hooks: &str) -> String {
    r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
            "hooks": __HOOKS__,
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "tool/call":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": {"ok": True, "output": "echo", "time_ms": 0}}), flush=True)
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
"#
        .replace("__HOOKS__", hooks)
}

fn sidecar_test_ctx(root: &Path) -> ToolCtx {
    let session = SessionId::new();
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "*",
        Mode::Allow,
    )]));
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let mut ctx = ToolCtx::new(
        root.to_path_buf(),
        permission.for_session(session),
        Default::default(),
    );
    ctx.interaction = interaction.for_session(session);
    ctx.session = Some(session);
    ctx
}

fn materialized_resource(
    marker: &str,
    kind: &str,
    local_id: &str,
    source_path: &str,
) -> PreparedResource {
    PreparedResource {
        local_id: local_id.to_string(),
        stable_id: format!("bundle:hya/materialized/{kind}/{local_id}"),
        source_path: source_path.to_string(),
        digest: format!("{marker}-{source_path}-digest"),
        content: format!("{marker} {source_path}\n"),
        binary_base64: None,
        aliases: Vec::new(),
        overrides: None,
    }
}

fn materialized_bundle(marker: &str) -> Bundle {
    let mut resource_view = ResourceView::default();
    resource_view.allow.push("echo".to_string());
    Bundle {
        check: None,
        tui: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/materialized".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        overrides: None,
        digest: format!("{marker}-bundle-digest"),
        agents: vec![PreparedAgent {
            private: false,
            id: AgentName::new("worker"),
            description: None,
            role: AgentRole::Subagent,
            color: None,
            prompt: Some(format!("{marker} worker prompt")),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view,
            can_spawn: Vec::new(),
            hook_refs: vec!["bundle:hya/materialized/hook/event".to_string()],
            advertise: Vec::new(),
            handoff: hya_bundle::HandoffPolicy::Model,
        }],
        tools: vec![materialized_resource(
            marker,
            "tool",
            "echo",
            "extensions/runtime.js",
        )],
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: vec![materialized_resource(
            marker,
            "hook",
            "event",
            "extensions/runtime.js",
        )],
        extensions: vec![materialized_resource(
            marker,
            "extension",
            "runtime",
            "extensions/runtime.js",
        )],
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        commands: Vec::new(),
        runtime: None,
        permission_modes: vec![],
        cli: Vec::new(),
        requires: Vec::new(),
        default_agent: None,
    }
}

fn materialized_tool_bundle(marker: &str) -> Bundle {
    let mut bundle = materialized_bundle(marker);
    bundle.hooks.clear();
    bundle.agents[0].hook_refs.clear();
    bundle
}

fn disjoint_materialized_bundle(marker: &str) -> Bundle {
    let alpha_path = "extensions/alpha.js";
    let beta_path = "extensions/beta.js";
    let alpha_tool = materialized_resource(marker, "tool", "echo", alpha_path);
    let alpha_hook = materialized_resource(marker, "hook", "event", alpha_path);
    let alpha_extension = materialized_resource(marker, "extension", "alpha", alpha_path);
    let beta_tool = materialized_resource(marker, "tool", "beta", beta_path);
    let beta_hook = materialized_resource(marker, "hook", "tool.execute.before", beta_path);
    let beta_extension = materialized_resource(marker, "extension", "beta", beta_path);
    Bundle {
        check: None,
        tui: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/materialized".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        overrides: None,
        digest: format!("{marker}-disjoint-bundle-digest"),
        agents: vec![PreparedAgent {
            private: false,
            id: AgentName::new("alpha"),
            description: None,
            role: AgentRole::Subagent,
            color: None,
            prompt: Some(format!("{marker} alpha prompt")),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView {
                allow: vec![alpha_tool.stable_id.clone()],
                ..ResourceView::default()
            },
            can_spawn: Vec::new(),
            hook_refs: vec![alpha_hook.stable_id.clone()],
            advertise: Vec::new(),
            handoff: hya_bundle::HandoffPolicy::Model,
        }],
        tools: vec![alpha_tool, beta_tool],
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: vec![alpha_hook, beta_hook],
        extensions: vec![alpha_extension, beta_extension],
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        commands: Vec::new(),
        runtime: None,
        permission_modes: vec![],
        cli: Vec::new(),
        requires: Vec::new(),
        default_agent: None,
    }
}

fn disjoint_materialized_bundles(marker: &str) -> Vec<Bundle> {
    let alpha = disjoint_materialized_bundle(marker);
    let mut beta = alpha.clone();
    beta.identity.id = "hya/materialized-beta".to_string();
    beta.digest = format!("{marker}-beta-bundle-digest");
    let beta_tool = format!("bundle:{}/tool/beta", beta.identity.id);
    let beta_hook = format!("bundle:{}/hook/tool.execute.before", beta.identity.id);
    beta.tools = vec![PreparedResource {
        stable_id: beta_tool.clone(),
        ..alpha.tools[1].clone()
    }];
    beta.hooks = vec![PreparedResource {
        stable_id: beta_hook.clone(),
        ..alpha.hooks[1].clone()
    }];
    beta.extensions = vec![PreparedResource {
        stable_id: format!("bundle:{}/extension/beta", beta.identity.id),
        ..alpha.extensions[1].clone()
    }];
    beta.agents[0] = PreparedAgent {
        private: false,
        id: AgentName::new("beta"),
        prompt: Some(format!("{marker} beta prompt")),
        resource_view: ResourceView {
            allow: vec![beta_tool],
            ..ResourceView::default()
        },
        hook_refs: vec![beta_hook],
        ..alpha.agents[0].clone()
    };
    let mut alpha_only = alpha;
    alpha_only.tools.truncate(1);
    alpha_only.hooks.truncate(1);
    alpha_only.extensions.truncate(1);
    vec![alpha_only, beta]
}

fn set_materialized_extension_content(bundle: &mut Bundle, content: String) {
    let digest = format!(
        "materialized-content-{:x}",
        Sha256::digest(content.as_bytes())
    );
    for resource in bundle
        .tools
        .iter_mut()
        .chain(bundle.hooks.iter_mut())
        .chain(bundle.extensions.iter_mut())
    {
        resource.content = content.clone();
        resource.digest = digest.clone();
    }
}

fn materialized_bun_bundle(marker: &str) -> Bundle {
    let mut bundle = materialized_tool_bundle(marker);
    set_materialized_extension_content(
        &mut bundle,
        r#"
export default {
  id: "bundle-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("bundle extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "bundle echo",
          execute: async () => "bundle-e2e",
        },
      },
    }
  },
}
"#
        .to_string(),
    );
    bundle
}

async fn prepared_sidecar_plugin(
    tools: &[ToolInfo],
    hooks: &[HookRegistration],
) -> (PluginHost, PreparedPlugin) {
    let declaration = json!({
        "protocol_version": 1,
        "plugin": {"id": "test", "version": "1", "kind": "bun"},
        "tools": tools, "hooks": hooks,
    });
    let script = format!(
        r#"
import json, sys
declaration = json.loads({declaration:?})
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req: continue
    result = declaration if req['method'] == 'initialize' else {{}}
    print(json.dumps({{'jsonrpc':'2.0', 'id':req['id'], 'result':result}}), flush=True)
    if req['method'] == 'shutdown': break
"#,
        declaration = declaration.to_string()
    );
    let host = PluginHost::connect(
        PluginSpec {
            id: "test".into(),
            kind: PluginKindWire::Bun,
            command: vec!["python3".into(), "-c".into(), script],
            timeout_ms: None,
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: ProcessLaunch::Bundle {
                root: std::env::temp_dir(),
                activation: Some(ActivationMetadata {
                    activation_id: "test".into(),
                    lifecycle: ActivationLifecycle::Transient,
                }),
            },
        },
        host_info(),
        None,
    )
    .await
    .unwrap();
    let plugin = host.prepared_plugins().remove(0);
    (host, plugin)
}

#[tokio::test]
async fn workflow_control_cold_run_and_shutdown_invariants() {
    let _env = StableEnvGuard::new();
    let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));
    let router = ProviderRouter::new().with(Arc::new(SessionRecordingProvider {
        sessions: sessions.clone(),
    }));
    let mut agent = agent_with_model("hya/offline", None);
    agent.workdir = tempdir();
    let directory = agent.workdir.join(".hya/workflows");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("cold.hya.md"),
        "---\nkind: Workflow\nname: cold\ndescription: Two stages on a cold root.\nnodes:\n  first:\n    agent: hya-scout\n    directive: First.\n  second:\n    agent: hya-scout\n    directive: Second.\n---\nflowchart TD\n  first --> second\n",
    )
    .unwrap();
    let mut built = build_session_engine_pure(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    assert!(
        engine
            .tool_schemas()
            .iter()
            .any(|s| s.name.as_str() == "bash")
    );
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let outcome = engine
        .invoke_bundle_api(hya_engine::BundleApiCall {
            bundle: "hya/workflow".to_owned(),
            method: hya_engine::ApiMethod::Post,
            session: Some(root),
            path: "/command".to_owned(),
            query: BTreeMap::new(),
            body: serde_json::json!({
                "run": {
                    "name": "cold",
                    "delivery": "finished"
                }
            }),
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, 200, "{}", outcome.body);
    assert!(outcome.body.get("started").is_some(), "{}", outcome.body);

    built.shutdown().await.unwrap();
    assert!(built.lifecycle.tasks.is_empty());
    // Idempotent second shutdown.
    built.shutdown().await.unwrap();

    {
        let recorded = sessions.lock().unwrap();
        assert_eq!(recorded.iter().filter(|s| **s == root).count(), 0);
        assert_eq!(recorded.len(), 2);
    }

    // Operations after shutdown fail cleanly with 503 unavailable.
    let late_cmd = engine
        .invoke_bundle_api(hya_engine::BundleApiCall {
            bundle: "hya/workflow".to_owned(),
            method: hya_engine::ApiMethod::Post,
            session: Some(root),
            path: "/command".to_owned(),
            query: BTreeMap::new(),
            body: serde_json::json!({ "list": {} }),
        })
        .await
        .unwrap();
    assert_eq!(late_cmd.status, 503, "{}", late_cmd.body);
    assert_eq!(late_cmd.body["error"]["code"], "unavailable");
    let late_list = engine
        .invoke_bundle_api(hya_engine::BundleApiCall {
            bundle: "hya/workflow".to_owned(),
            method: hya_engine::ApiMethod::Get,
            session: None,
            path: "/workflows".to_owned(),
            query: BTreeMap::new(),
            body: serde_json::Value::Null,
        })
        .await
        .unwrap();
    assert_eq!(late_list.status, 503, "{}", late_list.body);
    assert_eq!(late_list.body["error"]["code"], "unavailable");
    assert!(built.lifecycle.tasks.is_empty());
}

#[tokio::test]
async fn review_drain_joins_late_registration_before_killing_team() {
    let _env = StableEnvGuard::new();
    let (mut built, agent, root) = review_runtime().await;
    let engine = built.engine();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, agent.name.as_str())
        .unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, agent.name.as_str())
        .unwrap();
    let supervisor = built.resident_supervisor();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding.clone(), agents.clone(), resources.clone()),
            None,
            None,
        )
        .await
        .unwrap();
    let stop = built.lifecycle.stop.clone();
    built.lifecycle.tasks.spawn(async move {
        stop.cancelled().await;
        supervisor
            .spawn_resident_parked(
                root,
                agent,
                (binding, agents, resources, None),
                "late registration".into(),
                None,
                None,
            )
            .await
            .unwrap();
    });
    built.drain(FinishCause::Shutdown).await;
    let registrations_joined = built.lifecycle.tasks.is_empty();
    built.shutdown().await.unwrap();
    assert!(registrations_joined);
    assert!(engine.store().active_actor_ids().await.unwrap().is_empty());
}

#[test]
fn review_workflow_catalog_does_not_block_runtime_worker() {
    let _env = StableEnvGuard::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (mut built, _agent, root) = review_runtime().await;
        let engine = built.engine();
        let (release, held) = std::sync::mpsc::channel();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            held.recv().unwrap();
        });
        ready.await.unwrap();
        // Even with the host's single blocking thread saturated, hya/workflow
        // runs catalog discovery off the host's async worker without blocking.
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            engine.invoke_bundle_api(hya_engine::BundleApiCall {
                bundle: "hya/workflow".to_owned(),
                method: hya_engine::ApiMethod::Post,
                session: Some(root),
                path: "/command".to_owned(),
                query: BTreeMap::new(),
                body: serde_json::json!({ "list": {} }),
            }),
        )
        .await
        .expect("workflow catalog must not block on saturated host worker")
        .unwrap();
        assert_eq!(outcome.status, 200, "{}", outcome.body);
        release.send(()).unwrap();
        blocker.await.unwrap();
        built.shutdown().await.unwrap();
    });
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_stops_sources_retained_by_a_plugin_tool_and_a_retired_mcp_binding() {
    let _env = StableEnvGuard::new();
    let root = tempdir();
    let plugin_pid = root.join("plugin.pid");
    let mcp_pid = root.join("mcp.pid");
    let with_pid = |mut command: Vec<String>, path: &Path| {
        command[2] = format!(
            "import os\nfrom pathlib import Path\nPath({}).write_text(str(os.getpid()))\n{}",
            serde_json::to_string(path).unwrap(),
            command[2]
        );
        command
    };
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::from([(
            "owned".into(),
            McpServerConfig {
                command: with_pid(mcp_fixture(), &mcp_pid),
                ..Default::default()
            },
        )]),
        vec![PluginSpec {
            id: "owned".into(),
            kind: PluginKindWire::Rust,
            command: with_pid(plugin_fixture("owned"), &plugin_pid),
            timeout_ms: Some(1000),
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: Default::default(),
        }],
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: false,
            pure: true,
            project_plugins: true,
        },
    )
    .await
    .unwrap();
    let runtime = built.engine().runtime_registry();
    let plugin_tool = runtime
        .bind_turn(&root)
        .unwrap()
        .resolve_tool("owned__plugin_ping")
        .unwrap();
    runtime
        .refresh(|candidate| {
            candidate.remove_sources(&std::collections::BTreeSet::from([
                hya_engine::RuntimeSourceId::plugin("owned"),
            ]));
            Ok(())
        })
        .unwrap();
    let mcp_binding = runtime.bind_turn(&root).unwrap();
    assert!(mcp_binding.resolve_tool("owned__plugin_ping").is_none());
    assert!(mcp_binding.resolve_tool("mcp__owned__ping").is_some());
    runtime
        .refresh(|candidate| {
            candidate.remove_sources(&std::collections::BTreeSet::from([
                hya_engine::RuntimeSourceId::mcp("owned"),
            ]));
            Ok(())
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), built.shutdown())
        .await
        .unwrap()
        .unwrap();
    for path in [&plugin_pid, &mcp_pid] {
        let pid: i32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }
    let mut ctx = sidecar_test_ctx(&root);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Tool, "*", Mode::Allow),
        Rule::new(Action::Mcp, "*", Mode::Allow),
    ]));
    ctx.permission = permission;
    for tool in [
        plugin_tool,
        mcp_binding.resolve_tool("mcp__owned__ping").unwrap(),
    ] {
        assert!(tool.tool.execute(&ctx, json!({})).await.is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn built_shutdown_cancels_and_joins_deferred_mcp_preparation() {
    let _env = StableEnvGuard::new();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/sse", listener.local_addr().unwrap());
    let (entered, handshake) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut sse, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        assert!(sse.read(&mut buffer).await.unwrap() > 0);
        sse.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: endpoint\ndata: /messages\n\n").await.unwrap();
        let (mut post, _) = listener.accept().await.unwrap();
        assert!(post.read(&mut buffer).await.unwrap() > 0);
        entered.send(()).unwrap();
        assert_eq!(sse.read(&mut buffer).await.unwrap(), 0);
    });
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::from([(
            "gated".into(),
            McpServerConfig {
                url: Some(endpoint),
                transport: Some("sse".into()),
                ..Default::default()
            },
        )]),
        Vec::new(),
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: true,
            pure: true,
            project_plugins: true,
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), handshake)
        .await
        .unwrap()
        .unwrap();
    assert!(!built.lifecycle.io_tasks.is_empty());
    tokio::time::timeout(Duration::from_secs(5), built.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(built.lifecycle.tasks.is_empty());
    assert!(built.lifecycle.io_tasks.is_empty());
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn built_shutdown_cancels_started_workflow_while_reads_stay_unblocked() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let workflow_dir = workdir.join(".hya/workflows");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::write(
        workflow_dir.join("hold.hya.md"),
        "---\nkind: Workflow\nname: hold\ndescription: Hold until shutdown.\nnodes:\n  hold:\n    agent: hya-scout\n    directive: WAIT FOR SHUTDOWN\n---\nflowchart TD\n  hold\n",
    )
    .unwrap();
    let gate = Arc::new(ProviderGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let router = ProviderRouter::new().with(Arc::new(CountingDevProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        inner: DevProvider::new(),
        gate: Some(gate.clone()),
    }));
    let mut agent = agent_with_model("hya/offline", None);
    agent.workdir = workdir.clone();
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::compile(PermissionModel::Danger, Vec::new()).unwrap(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: false,
            pure: true,
            project_plugins: true,
        },
    )
    .await
    .unwrap();
    let engine = built.engine();
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let started = engine
        .invoke_bundle_api(hya_engine::BundleApiCall {
            bundle: "hya/workflow".to_owned(),
            method: hya_engine::ApiMethod::Post,
            session: Some(lead),
            path: "/command".to_owned(),
            query: BTreeMap::new(),
            body: serde_json::json!({
                "run": {
                    "name": "hold",
                    "delivery": "started"
                }
            }),
        })
        .await
        .unwrap();
    assert_eq!(started.status, 200, "{}", started.body);
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    // List/State reads must not block behind a running workflow.
    for body in [
        serde_json::json!({ "list": {} }),
        serde_json::json!({ "state": {} }),
    ] {
        let outcome = tokio::time::timeout(
            Duration::from_millis(250),
            engine.invoke_bundle_api(hya_engine::BundleApiCall {
                bundle: "hya/workflow".to_owned(),
                method: hya_engine::ApiMethod::Post,
                session: Some(lead),
                path: "/command".to_owned(),
                query: BTreeMap::new(),
                body,
            }),
        )
        .await
        .expect("read command must not block")
        .unwrap();
        assert_eq!(outcome.status, 200, "{}", outcome.body);
    }
    tokio::time::timeout(Duration::from_secs(2), built.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(built.lifecycle.tasks.is_empty());
    assert!(!engine.has_active_service_work(lead));
    let state = engine.read_projection(lead).await.unwrap();
    assert_eq!(
        workflow_projection(&state).unwrap().run.unwrap().status,
        WorkflowRunStatus::Cancelled
    );
}

#[tokio::test]
async fn runtime_store_ownership_contention_and_startup_recovery() {
    let _env = StableEnvGuard::new();
    let dir = tempdir();
    let db = dir.join("sessions.db");
    let db_str = db.to_str().unwrap();

    // File-store owner contention in both strict and fallback policies.
    let held = open_runtime_store(db_str).await.unwrap();
    let strict_err = open_runtime_store(db_str).await.err().unwrap();
    let fallback_err = open_runtime_store_or_memory(db_str).await.err().unwrap();
    assert!(strict_err.is_owner_busy());
    assert!(fallback_err.is_owner_busy());
    let legacy_err = open_store(db_str).await.err().unwrap();
    assert!(matches!(
        legacy_err.downcast_ref::<hya_store::StoreError>(),
        Some(hya_store::StoreError::RuntimeOwnerBusy)
    ));
    let invalid_db = dir.join("directory.db");
    std::fs::create_dir(&invalid_db).unwrap();
    assert!(
        !open_runtime_store(invalid_db.to_str().unwrap())
            .await
            .err()
            .unwrap()
            .is_owner_busy()
    );
    drop(held);
    std::fs::remove_dir_all(dir).unwrap();

    // In-memory store: engine claims owner until shutdown, blocks concurrent rebuild,
    // closes crash-left turns & prior workflow runs on rebuild.
    let store = SessionStore::connect_memory().await.unwrap();
    let open_session = SessionId::new();
    store
        .append_event(
            open_session,
            &Event::MessageStarted {
                session: open_session,
                message: MessageId::new(),
                role: hya_proto::Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();

    let build = || {
        let store = store.clone();
        async move {
            let (router, model) = offline_router(None);
            let agent = agent_with_model(&model, None);
            build_session_engine_pure(
                store,
                router,
                &agent,
                BTreeMap::new(),
                Vec::new(),
                (
                    InvocationPolicy::default(),
                    config::EngineSettings::default(),
                ),
            )
            .await
        }
    };
    let mut first = build().await.unwrap();
    let first_owner = first.engine().runtime_owner();
    let recovered_msg = first
        .engine()
        .read_projection(open_session)
        .await
        .unwrap()
        .session
        .messages
        .remove(0);
    assert_eq!(recovered_msg.finish, Some(FinishReason::Cancelled));
    assert_eq!(recovered_msg.cause, Some(FinishCause::Interrupted));

    // Concurrent build on the same store fails with RuntimeOwnerBusy.
    let busy = match build().await {
        Ok(mut second) => {
            second.shutdown().await.unwrap();
            first.shutdown().await.unwrap();
            panic!("concurrent engine must not claim an owned store");
        }
        Err(err) => err,
    };
    assert!(matches!(
        busy.downcast_ref::<hya_store::StoreError>(),
        Some(hya_store::StoreError::RuntimeOwnerBusy)
    ));

    // Seed a workflow run owned by `first_owner`, shut down, and rebuild.
    let wf_session = SessionId::new();
    store
        .append_event(
            wf_session,
            &ExtensionEvent::WorkflowRunStarted {
                session: wf_session,
                run: hya_proto::WorkflowRunId::new(),
                workflow: WorkflowIdentity {
                    source: hya_proto::WorkflowSourceId::new("test:rebuild"),
                    name: "rebuild".into(),
                    revision: WorkflowRevision::from_bytes([1; 32]),
                },
                request_hash: "inputs".into(),
                owner: first_owner,
                stages: vec![WorkflowStagePlan {
                    id: "stage".into(),
                    title: None,
                    agent: AgentName::new("general"),
                    mode: "once".into(),
                    level: 0,
                    worker_model: None,
                    selected_worker_model: None,
                    verifier_model: None,
                    selected_verifier_model: None,
                }],
            }
            .into_event(),
        )
        .await
        .unwrap();
    first.shutdown().await.unwrap();
    let mut next = build().await.unwrap();
    // Repeated shutdown on old owner must not release `next`'s ownership claim.
    first.shutdown().await.unwrap();
    assert!(matches!(
        store.claim_runtime_owner(OwnerRunId::new()),
        Err(hya_store::StoreError::RuntimeOwnerBusy)
    ));
    assert_ne!(next.engine().runtime_owner(), first_owner);
    let wf_proj = hya_proto::Projection::from_events_with(
        &store.replay(wf_session).await.unwrap(),
        &projection_reducers(),
    );
    let wf = workflow_projection(&wf_proj).unwrap().run.unwrap();
    assert_eq!(wf.status, WorkflowRunStatus::Interrupted);
    assert_eq!(wf.error.as_deref(), Some("backend startup recovery"));
    next.shutdown().await.unwrap();
    store.claim_runtime_owner(OwnerRunId::new()).unwrap();
}

#[tokio::test]
async fn resolve_runtime_config_permission_and_tool_exposure() {
    let dir = tempdir();
    let _env = EnvGuard::with_home(&dir, &dir);
    let config_path = dir.join("hya/config.yaml");

    // 1. Missing config -> offline fallback + offline notice data.
    let missing = resolve_runtime(None, RuntimeFlags::default())
        .await
        .unwrap();
    assert_eq!(missing.model, "hya/offline");
    let notice = missing.offline_notice.expect("offline notice");
    assert!(notice.config_path.ends_with("hya/config.yaml"));
    assert!(notice.render().contains("OFFLINE"));

    // 2. Startup snapshot frozen across later config edits; permission + yolo; invalid rule falls back to Strict.
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        "schema_version: 2\nreasoning: high\npermission:\n  model: allow\n",
    )
    .unwrap();
    let rt = resolve_runtime(None, RuntimeFlags::default())
        .await
        .unwrap();
    assert_eq!(rt.permission.model(), PermissionModel::Allow);
    assert_eq!(
        resolve_runtime(None, RuntimeFlags::default())
            .await
            .unwrap()
            .with_yolo(true)
            .permission
            .model(),
        PermissionModel::Danger
    );
    std::fs::write(&config_path, "schema_version: 2\nreasoning: low\n").unwrap();
    let agent = agent_base_with_model(&rt.model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        rt.router,
        &agent,
        rt.mcp,
        rt.plugins,
        (rt.permission, rt.settings),
    )
    .await
    .unwrap();
    assert_eq!(
        built
            .engine()
            .effective_effort(&agent.model, None, None, None)
            .await
            .unwrap()
            .effort,
        Some(ReasoningEffort::High)
    );
    built.shutdown().await.unwrap();

    std::fs::write(
        &config_path,
        "schema_version: 2\npermission:\n  rules:\n    - target: tool\n      selector: '('\n      permission: allow\n",
    )
    .unwrap();
    assert_eq!(
        resolve_runtime(None, RuntimeFlags::default())
            .await
            .unwrap()
            .permission
            .model(),
        PermissionModel::Strict
    );

    // 3. Websearch + webfetch independent exposure via per-bundle `hya/network` config.yml.
    std::fs::write(&config_path, "schema_version: 2\n").unwrap();
    let net_loc = crate::bundle_config::bundle_config_location(
        &config_path,
        "hya/network",
        crate::bundle_config::BundleConfigScope::User,
    )
    .unwrap();
    std::fs::create_dir_all(net_loc.dir()).unwrap();
    std::fs::write(
        net_loc.file(),
        "webfetch:\n  enabled: false\nwebsearch:\n  provider: parallel\n  endpoint: https://search.example.test/mcp\n  key: secret\n  enabled: false\n",
    )
    .unwrap();
    let ws_rt = resolve_runtime(None, RuntimeFlags::default())
        .await
        .unwrap();
    let ws_agent = agent_with_model(&ws_rt.model, None);
    let mut ws_built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        ws_rt.router,
        &ws_agent,
        ws_rt.mcp,
        ws_rt.plugins,
        (ws_rt.permission, ws_rt.settings),
    )
    .await
    .unwrap();
    // Disabled network tools stay registered but hya/network's own
    // `tool.visibility` hook hides them from every turn.
    let outcome = ws_built
        .engine()
        .process_hooks()
        .tool_visibility(hya_runtime_abi::ToolVisibilityInput {
            session: hya_proto::SessionId::new(),
            agent: None,
            candidate_tools: vec!["webfetch".into(), "websearch".into(), "read".into()],
            interactive: true,
            depth: 0,
        })
        .await;
    assert_eq!(
        outcome,
        hya_runtime_abi::ToolVisibilityOutcome::Hide {
            tools: vec!["webfetch".into(), "websearch".into()]
        }
    );
    ws_built.shutdown().await.unwrap();

    // 3b. Air-gapped mode: disabling `hya/network` removes all internet tools and aliases.
    std::fs::write(
        &config_path,
        "schema_version: 2\nbundles:\n  builtin:\n    profile: full\n    disable: [hya/network]\n",
    )
    .unwrap();
    std::fs::write(
        net_loc.file(),
        "webfetch:\n  enabled: true\nwebsearch:\n  enabled: true\n",
    )
    .unwrap();
    let airgap_rt = resolve_runtime(None, RuntimeFlags::default())
        .await
        .unwrap();
    let airgap_agent = agent_with_model(&airgap_rt.model, None);
    let mut airgap_built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        airgap_rt.router,
        &airgap_agent,
        airgap_rt.mcp,
        airgap_rt.plugins,
        (airgap_rt.permission, airgap_rt.settings),
    )
    .await
    .unwrap();
    let airgap_names: Vec<String> = airgap_built
        .engine()
        .tool_schemas()
        .into_iter()
        .map(|s| s.name.to_string())
        .collect();
    assert!(!airgap_names.iter().any(|n| n == "websearch"));
    assert!(!airgap_names.iter().any(|n| n == "webfetch"));
    assert!(!airgap_names.iter().any(|n| n == "search"));
    assert!(!airgap_names.iter().any(|n| n == "fetch"));
    assert!(airgap_names.iter().any(|n| n == "read"));
    airgap_built.shutdown().await.unwrap();

    // 4. Selected model reasoning default stays in catalog, not baked into root Agent.
    std::fs::write(
        &config_path,
        "schema_version: 2\ndefault_model: gateway/gpt-5.6-sol\nproviders:\n  gateway:\n    kind: openai\n    base_url: https://example.test/v1\n    api_key: test\n    models:\n      - id: gpt-5.6-sol\n        reasoning:\n          default: medium\n          variants: [low, medium]\n",
    )
    .unwrap();
    let reasoning_rt = resolve_runtime(None, RuntimeFlags::default())
        .await
        .unwrap();
    assert_eq!(
        reasoning_rt.router.catalog()[0].reasoning_default,
        Some(ReasoningEffort::Medium)
    );
    assert_eq!(agent_with_model(&reasoning_rt.model, None).reasoning, None);
}

#[test]
fn agent_spec_catalog_and_pure_runtime_invariants() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);

    let catalog = builtin_agent_catalog().expect("builtin agent catalog");
    assert!(
        catalog
            .semantic_identity_v1()
            .is_some_and(|identity| !identity.is_empty())
    );

    std::fs::write(workdir.join("AGENTS.md"), "SERVER_BASE_MUST_NOT_BAKE").unwrap();
    write_skill(
        &workdir.join(".hya/skills/baseline"),
        "baseline-skill",
        "Baseline skill",
        "baseline body",
    );

    let server_base = agent_base_with_model("fake", None);
    assert_eq!(server_base.system_prompt, HARNESS_AGENT_BASE);
    assert_eq!(server_base.workdir, PathBuf::new());
    assert!(!server_base.system_prompt.contains("## Environment"));
    assert!(
        !server_base
            .system_prompt
            .contains("SERVER_BASE_MUST_NOT_BAKE")
    );

    for direct in [
        agent_with_model("fake", None),
        agent_with_model_pure("fake", None),
    ] {
        assert!(!direct.system_prompt.contains("baseline-skill"));
        assert!(direct.workdir.is_absolute());
        assert_eq!(
            std::fs::canonicalize(&direct.workdir).unwrap(),
            std::fs::canonicalize(&workdir).unwrap()
        );
    }
}

#[tokio::test]
async fn pure_and_bare_flags_shape_the_resolved_runtime() {
    let home = tempdir();
    let _env = EnvGuard::with_home(&home, &home);
    std::fs::create_dir_all(home.join("hya")).unwrap();
    std::fs::write(
        home.join("hya/config.yaml"),
        "schema_version: 2\ndefault_agent: hya-main\nmcp:\n  external: { command: [echo] }\nproviders:\n  claude:\n    kind: anthropic\n    base_url: https://anthropic.example.test/v1\n    api_key: test\n    models: [sonnet]\n  local:\n    kind: openai\n    base_url: https://local.example.test/v1\n    api_key: test\n    models: [qwen]\n",
    )
    .unwrap();

    let full = resolve_runtime(None, RuntimeFlags::default())
        .await
        .unwrap();
    assert!(!full.pure && full.mcp.contains_key("external"));
    assert!(full.kinds.protocol("anthropic").is_some());
    assert!(
        full.router
            .resolve(&ModelRef::new("claude/sonnet"))
            .is_some()
    );

    let pure = resolve_runtime(
        None,
        RuntimeFlags {
            bare: false,
            pure: true,
        },
    )
    .await
    .unwrap();
    assert!(pure.pure && pure.mcp.is_empty() && pure.plugins.is_empty());

    // Bare registers only the core kinds before the providers resolve: the
    // anthropic provider is unsupported, the openai one still routes.
    let bare = resolve_runtime(
        None,
        RuntimeFlags {
            bare: true,
            pure: false,
        },
    )
    .await
    .unwrap();
    assert!(bare.kinds.protocol("anthropic").is_none());
    assert_eq!(bare.default_agent, None);
    assert_eq!(
        bare.settings.builtin_bundles(),
        &hya_bundle::BuiltinBundleSelection::bare()
    );
    assert!(
        bare.router
            .resolve(&ModelRef::new("claude/sonnet"))
            .is_none()
    );
    assert!(bare.router.resolve(&ModelRef::new("local/qwen")).is_some());
    let claude = bare
        .catalog
        .providers()
        .iter()
        .find(|state| state.provider_id == "claude")
        .unwrap();
    assert_eq!(
        claude.result,
        hya_provider::ProviderCatalogResult::Unsupported
    );
    assert_eq!(claude.kind, "anthropic");
}

#[tokio::test]
async fn built_engine_lazily_refreshes_installed_catalog_at_root_binding() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let registry_path = home.join("hya/bundles/registry.sqlite3");
    assert!(!registry_path.exists());

    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    assert!(!registry_path.exists());
    let old_binding = engine.bind_runtime(&workdir).unwrap();
    assert!(
        old_binding
            .resolve_agent("runtime-installed-agent")
            .is_none()
    );

    std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let installed = prepare_package(BundleSource::new(
        "runtime-installed",
        vec![SourceFile::new(
            "bundle.hya.md",
            b"---\nkind: Bundle\nschema_version: 2\nidentity:\n  id: hya/runtime-installed-test\n  version: 1.0.0\n  publisher: hya\nagents:\n  - id: runtime-installed-agent\n    role: main\n---\nYou are the runtime-installed agent.\n",
        )],
    ))
    .unwrap();
    let outcome = registry
        .install(
            &[],
            NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x52; 32],
                prepared_digest: installed.digest().to_string(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1_725_000_020,
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, BundleInstallOutcome::Installed { generation: 1 });

    let fresh_binding = engine.bind_root_runtime(&workdir).await.unwrap();
    assert!(
        fresh_binding
            .resolve_agent("runtime-installed-agent")
            .is_some()
    );
    assert!(
        old_binding
            .resolve_agent("runtime-installed-agent")
            .is_none()
    );
    built.shutdown().await.unwrap();
}

#[tokio::test]
async fn engine_snapshot_rejects_builder_bypass_and_publishes_deferred_set_atomically() {
    let _env = StableEnvGuard::new();
    let (router, _model) = offline_router(None);
    let builder = Arc::new(hya_tool_loader::builtins());
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(router),
        Arc::new(RuntimeRegistry::from_snapshot(
            builder.snapshot(),
            builtin_agent_catalog().unwrap(),
        )),
        permission,
        EventBus::default(),
    ));
    builder
        .register(Arc::new(RuntimeMarker("builder_bypass")))
        .unwrap();
    assert!(
        engine
            .tool_schemas()
            .iter()
            .all(|s| s.name.as_str() != "builder_bypass")
    );

    let saw_complete_old_view = Arc::new(AtomicBool::new(false));
    let observed = saw_complete_old_view.clone();
    let inspect_engine = engine.clone();
    let mut next = 0;
    let deferred_tools = std::iter::from_fn(move || {
        let item = match next {
            0 => Some(Arc::new(RuntimeMarker("mcp__deferred__first")) as Arc<dyn Tool>),
            1 => {
                let visible: Vec<_> = inspect_engine
                    .tool_schemas()
                    .into_iter()
                    .map(|s| s.name.to_string())
                    .collect();
                observed.store(
                    !visible.iter().any(|n| n == "mcp__deferred__first")
                        && !visible.iter().any(|n| n == "mcp__deferred__second"),
                    Ordering::SeqCst,
                );
                Some(Arc::new(RuntimeMarker("mcp__deferred__second")) as Arc<dyn Tool>)
            }
            _ => None,
        };
        next += 1;
        item
    });
    engine
        .refresh_runtime(|candidate| {
            for tool in deferred_tools {
                candidate.register_tool_with_permission(tool, ToolPermission::Mcp)?;
            }
            Ok(())
        })
        .unwrap();

    assert!(saw_complete_old_view.load(Ordering::SeqCst));
    let visible: Vec<_> = engine
        .tool_schemas()
        .into_iter()
        .map(|s| s.name.to_string())
        .collect();
    assert!(visible.iter().any(|n| n == "mcp__deferred__first"));
    assert!(visible.iter().any(|n| n == "mcp__deferred__second"));
    assert!(!visible.iter().any(|n| n == "builder_bypass"));
}

#[tokio::test]
async fn engine_build_fences_residents_but_defers_queued_work_until_start() {
    let _env = StableEnvGuard::new();
    let database = tempdir().join("resident-recovery.db");
    let store = SessionStore::connect(database.to_str().unwrap())
        .await
        .unwrap();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let queued_root = SessionId::new();
    let queued_actor = SessionId::new();
    let running_root = SessionId::new();
    let running_actor = SessionId::new();

    for (root, actor) in [(queued_root, queued_actor), (running_root, running_actor)] {
        for (session, parent) in [(root, None), (actor, Some(root))] {
            store
                .append_event(
                    session,
                    &Event::SessionCreated {
                        session,
                        parent,
                        agent: agent.name.clone(),
                        model: agent.model.clone(),
                        workdir: agent.workdir.to_string_lossy().into_owned(),
                        project: None,
                        kind: hya_proto::SessionKind::Project,
                    },
                )
                .await
                .unwrap();
        }
    }

    let queued_claim = store
        .try_claim_new(queued_root, queued_actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .commit_resident_mutation(
            &queued_claim,
            queued_root,
            vec![
                ExtensionEvent::AgentRegistered {
                    session: queued_root,
                    agent_session: queued_actor,
                    handle: "queued-1".into(),
                    parent: Some("main".into()),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                }
                .into_event(),
            ],
        )
        .await
        .unwrap();
    store
        .append_event(
            queued_root,
            &ExtensionEvent::ChannelCreated {
                session: queued_root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), "main/queued-1".into()],
            }
            .into_event(),
        )
        .await
        .unwrap();
    store
        .append_event(
            queued_root,
            &ExtensionEvent::MailSent {
                session: queued_root,
                from: "main".into(),
                to: MailEndpoint::Channel("DM-12345678".into()),
                kind: MailKind::Message,
                body: "resume me".into(),
            }
            .into_event(),
        )
        .await
        .unwrap();

    let running_claim = store
        .try_claim_new(running_root, running_actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .commit_resident_mutation(
            &running_claim,
            running_root,
            vec![
                ExtensionEvent::AgentRegistered {
                    session: running_root,
                    agent_session: running_actor,
                    handle: "running-1".into(),
                    parent: Some("main".into()),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                }
                .into_event(),
                ExtensionEvent::ResidentWorkStarted {
                    session: running_root,
                    actor_session: running_actor,
                    handle: "main/running-1".into(),
                    epoch: running_claim.epoch,
                    inbox_through: 0,
                }
                .into_event(),
            ],
        )
        .await
        .unwrap();
    drop(store);

    let store = SessionStore::connect(database.to_str().unwrap())
        .await
        .unwrap();
    let mut built = build_session_engine(
        store.clone(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(built.resident_supervisor().episode_count(), 0);
    let before = built.engine().read_projection(queued_root).await.unwrap();
    let before_team = team_projection(&before);
    assert_eq!(before_team.roster["main/queued-1"].resident_cursor, 0);
    assert!(before_team.roster["main/queued-1"].resident_work.is_none());

    built.resident_supervisor().start_recovered().await.unwrap();
    let running = built.engine().read_projection(running_root).await.unwrap();
    let running_team = team_projection(&running);
    assert_eq!(
        running_team.roster["main/running-1"].status,
        RosterStatus::Failed
    );
    assert!(
        running_team.roster["main/running-1"]
            .resident_work
            .is_none()
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let queued = built.engine().read_projection(queued_root).await.unwrap();
            let team = team_projection(&queued);
            let entry = &team.roster["main/queued-1"];
            if entry.resident_cursor == 1 || entry.resident_work.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    built.shutdown().await.unwrap();
}

#[tokio::test]
async fn deferred_mcp_and_hot_reload_plugin_source_wiring() {
    let _env = StableEnvGuard::new();
    let root = tempdir();
    let log = root.join("bad-plugin-starts");
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut hook_cmd = plugin_fixture("source-hooks");
    hook_cmd[2] = hook_cmd[2].replace(
        "\"hooks\":[]",
        "\"hooks\":[{\"name\":\"message.user.before\"}]",
    );
    let started = std::time::Instant::now();
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::from([
            (
                "slow".into(),
                McpServerConfig {
                    command: vec![
                        "python3".into(),
                        "-c".into(),
                        "import sys; sys.stdin.read()".into(),
                    ],
                    ..Default::default()
                },
            ),
            (
                "ready".into(),
                McpServerConfig {
                    command: mcp_fixture(),
                    ..Default::default()
                },
            ),
        ]),
        vec![
            PluginSpec {
                id: "source-hooks".into(),
                kind: PluginKindWire::Rust,
                command: hook_cmd,
                timeout_ms: Some(1_000),
                env: BTreeMap::new(),
                plugin_dir: None,
                launch: Default::default(),
            },
            PluginSpec {
                id: "bad".into(),
                kind: PluginKindWire::Rust,
                command: vec![
                    "python3".into(),
                    "-c".into(),
                    format!(
                        "with open({}, 'a') as f: f.write('start\\n')",
                        serde_json::to_string(&log).unwrap()
                    ),
                ],
                timeout_ms: Some(1_000),
                env: BTreeMap::new(),
                plugin_dir: None,
                launch: Default::default(),
            },
        ],
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: true,
            pure: false,
            project_plugins: true,
        },
    )
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    let control = built.mcp_control();
    assert_eq!(
        control.status().await.get("slow"),
        Some(&hya_mcp::McpStatus::Connecting)
    );
    let binding = built.engine().runtime_registry().bind_turn(&root).unwrap();
    assert!(binding.resolve_tool("source-hooks__plugin_ping").is_some());
    assert_eq!(binding.bundle_hooks_for_agent(agent.name.as_str()).len(), 1);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if control.status().await.get("ready") == Some(&hya_mcp::McpStatus::Connected) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Deferred MCP resolution must not retry the failed startup plugin.
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    built.shutdown().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn startup_mixed_mcp_plugin_and_compat_mcp_control() {
    let _env = StableEnvGuard::new();
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine_with_mcp_defer(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::from([(
            "mixed".into(),
            McpServerConfig {
                command: mcp_fixture(),
                timeout_ms: Some(1_000),
                ..Default::default()
            },
        )]),
        vec![PluginSpec {
            id: "mixed-plugin".into(),
            kind: PluginKindWire::Rust,
            command: plugin_fixture("mixed-plugin"),
            timeout_ms: Some(1_000),
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: Default::default(),
        }],
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
        EngineBuildOptions {
            defer_mcp: false,
            pure: false,
            project_plugins: true,
        },
    )
    .await
    .unwrap();
    let engine = built.engine();
    let manifest = engine.runtime_registry().effective_manifest();
    assert_eq!(
        manifest.generation.get(),
        hya_proto::ConfigGeneration::INITIAL.get() + 3
    );
    assert!(manifest.sources.contains_key(&SourceId::mcp("mixed")));
    assert!(
        manifest
            .sources
            .contains_key(&SourceId::plugin("mixed-plugin"))
    );
    let names: Vec<_> = engine
        .tool_schemas()
        .into_iter()
        .map(|s| s.name.to_string())
        .collect();
    assert!(names.contains(&"mcp__mixed__ping".to_string()));
    assert!(names.contains(&"mixed-plugin__plugin_ping".to_string()));

    // Dynamic MCP control upsert + disable preserves prior turn bindings.
    let workdir = tempdir();
    let control = built.mcp_control();
    let before = engine.runtime_registry().bind_turn(&workdir).unwrap();
    control
        .upsert(
            "dynamic".into(),
            McpServerConfig {
                command: mcp_fixture(),
                timeout_ms: Some(1_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let connected = engine.runtime_registry().bind_turn(&workdir).unwrap();
    assert!(before.resolve_tool("mcp__dynamic__ping").is_none());
    assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
    assert_eq!(
        control.status().await.get("dynamic"),
        Some(&hya_mcp::McpStatus::Connected)
    );
    assert_eq!(
        control.tools().await.get("dynamic"),
        Some(&vec!["mcp__dynamic__ping".to_string()])
    );

    assert!(control.set_enabled("dynamic".into(), false).await.unwrap());
    let removed = engine.runtime_registry().bind_turn(&workdir).unwrap();
    assert!(removed.resolve_tool("mcp__dynamic__ping").is_none());
    assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
    assert_eq!(
        control.status().await.get("dynamic"),
        Some(&hya_mcp::McpStatus::Disabled)
    );
    assert_eq!(control.tools().await.get("dynamic"), None);
    built.shutdown().await.unwrap();
    std::fs::remove_dir_all(workdir).unwrap();
}

#[tokio::test]
async fn initial_context_backfills_and_survives_runtime_restart() {
    let home = tempdir();
    let workdir = home.join("project");
    let _env = EnvGuard::with_home(&home, &workdir);

    for pure in [false, true] {
        let db = home.join(if pure { "pure.db" } else { "full.db" });
        let agents_path = workdir.join("AGENTS.md");
        std::fs::write(&agents_path, "ORIGINAL_PROJECT_INSTRUCTIONS").unwrap();
        let agent = agent_with_model("fake", None);
        assert_eq!(agent.system_prompt, HARNESS_AGENT_BASE);

        let session = SessionId::new();
        let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent: None,
                    agent: agent.name.clone(),
                    model: agent.model.clone(),
                    workdir: workdir.to_string_lossy().into_owned(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .read_projection_shared(session)
                .await
                .unwrap()
                .session
                .initial_context
                .is_none()
        );

        let mut first_system = None;
        for restart in [false, true] {
            let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
            let provider = Arc::new(FakeProvider::scripted(vec![FakeStep::Finish(
                FinishReason::Stop,
            )]));
            let mut runtime = build_session_engine_with_mcp_defer(
                store,
                ProviderRouter::new().with(provider.clone()),
                &agent,
                BTreeMap::new(),
                Vec::new(),
                (
                    InvocationPolicy::default(),
                    config::EngineSettings::default(),
                ),
                EngineBuildOptions {
                    defer_mcp: false,
                    pure,
                    project_plugins: true,
                },
            )
            .await
            .unwrap();
            let engine = runtime.engine();
            engine
                .admit_user_prompt(session, "continue".into())
                .await
                .unwrap();
            assert_eq!(
                engine
                    .run_turn(session, &agent, CancellationToken::new())
                    .await
                    .unwrap(),
                FinishReason::Stop
            );
            let events = engine.replay(session).await.unwrap();
            let recorded: Vec<_> = events
                .iter()
                .filter_map(|e| match &e.event {
                    Event::InitialContextRecorded { text, .. } => Some((e.seq, text)),
                    _ => None,
                })
                .collect();
            let [(recorded_seq, text)] = recorded.as_slice() else {
                panic!(
                    "expected one InitialContextRecorded, got {}",
                    recorded.len()
                );
            };
            let step = events
                .iter()
                .find_map(|e| match &e.event {
                    Event::StepStarted {
                        view: Some(view), ..
                    } => Some(view),
                    _ => None,
                })
                .unwrap();
            assert!(step.cut.0 >= recorded_seq.0);
            assert_eq!(
                engine
                    .read_projection(session)
                    .await
                    .unwrap()
                    .session
                    .initial_context
                    .as_ref(),
                Some(*text)
            );
            let requests = provider.requests();
            let [request] = requests.as_slice() else {
                panic!("expected one request, got {}", requests.len());
            };
            let system = request.system.as_deref().unwrap();
            assert!(system.contains("## Environment"));
            assert_eq!(system.contains("ORIGINAL_PROJECT_INSTRUCTIONS"), !pure);
            assert!(!system.contains("UPDATED_PROJECT_INSTRUCTIONS"));
            if restart {
                assert_eq!(Some(system), first_system.as_deref());
            } else {
                first_system = Some(system.to_string());
                std::fs::write(&agents_path, "UPDATED_PROJECT_INSTRUCTIONS").unwrap();
            }
            runtime.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn recovered_resident_resolution_and_workdir_invariants() {
    let _env = StableEnvGuard::new();
    let session_dir = tempdir();
    let base_dir = tempdir();
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base-model"),
        system_prompt: "lead base".into(),
        workdir: base_dir.clone(),
        reasoning: None,
    };

    // Missing definition fails closed without synthesizing `general`/`build`.
    let empty_engine = engine_with_catalog(catalog_with_agents(&[])).await;
    let err = match empty_engine.resolve_resident_runtime(
        empty_engine.bind_runtime(&session_dir).unwrap(),
        &base,
        &AgentName::new("legacy-resident-only"),
    ) {
        Ok(_) => panic!("missing resident definition must fail"),
        Err(err) => err,
    };
    assert!(matches!(
        &err,
        CoreError::AgentDefinitionMissing { agent_id } if agent_id == "legacy-resident-only"
    ));
    assert!(err.to_string().contains("AGENT_DEFINITION_MISSING"));
    assert!(!err.to_string().contains("`general`") && !err.to_string().contains("`build`"));

    // Exact lookup preserves recorded id without `can_spawn`, and uses session workdir over base.
    let engine = engine_with_catalog(catalog_with_agents(&["resident-helper"])).await;
    let (resolved, _) = engine
        .resolve_resident_runtime(
            engine.bind_runtime(&session_dir).unwrap(),
            &base,
            &AgentName::new("resident-helper"),
        )
        .unwrap();
    assert_eq!(resolved.name.as_str(), "resident-helper");
    assert_eq!(resolved.workdir, session_dir);
    assert_ne!(resolved.workdir, base_dir);
    assert!(
        resolved
            .system_prompt
            .contains("resident-helper recovery prompt")
    );
}

#[tokio::test]
async fn resolve_spawn_member_model_precedence_effort_and_sidecar_capture() {
    let _env = StableEnvGuard::new();
    let workdir = tempdir();
    let categories = precedence_categories();
    let is_servable = |_: &ModelRef| true;
    let allowed = [AgentDef {
        name: "worker".into(),
        description: None,
        category: None,
        mode: "subagent".into(),
        effort: None,
        effort_source: None,
    }];
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("base/model"),
        system_prompt: "lead base".into(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let sidecar_environment =
        BundleSidecarEnvironment::from_command(vec!["bun".into(), "sidecar.js".into()], tempdir());

    // 1. Precedence ladder (lowest -> highest).
    let cases = [
        ("base", None, None, None, None, None, None, "base/model"),
        (
            "bundle-cat",
            None,
            Some("bundle-cat"),
            None,
            None,
            None,
            None,
            "cat/bundle-model",
        ),
        (
            "inline-cat",
            None,
            Some("bundle-cat"),
            None,
            Some("inline-cat"),
            None,
            None,
            "cat/inline-model",
        ),
        (
            "bundle-model",
            Some("bundle/model"),
            Some("bundle-cat"),
            None,
            Some("inline-cat"),
            None,
            None,
            "bundle/model",
        ),
        (
            "inline-model",
            Some("bundle/model"),
            Some("bundle-cat"),
            Some("hya/offline"),
            Some("inline-cat"),
            None,
            None,
            "hya/offline",
        ),
        (
            "spawn-cat",
            Some("bundle/model"),
            Some("bundle-cat"),
            Some("inline/model"),
            Some("inline-cat"),
            None,
            Some("spawn-cat"),
            "cat/spawn-model",
        ),
        (
            "spawn-model",
            Some("bundle/model"),
            Some("bundle-cat"),
            Some("inline/model"),
            Some("inline-cat"),
            Some("offline"),
            Some("spawn-cat"),
            "hya/offline",
        ),
    ];
    for (label, b_mod, b_cat, i_mod, i_cat, s_mod, s_cat, expected) in cases {
        let engine = engine_with_catalog(catalog_with_worker_policy(ModelPolicy {
            model: b_mod.map(str::to_string),
            category: b_cat.map(str::to_string),
            reasoning: None,
        }))
        .await;
        let binding = engine.bind_runtime(&workdir).unwrap();
        let has_inline = i_mod.is_some() || i_cat.is_some();
        let resolved = resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &sidecar_environment,
            },
            SpawnMember {
                description: label.into(),
                prompt: "resolve".into(),
                subagent_type: "worker".into(),
                model: s_mod.map(str::to_string),
                category: s_cat.map(str::to_string),
                effort: None,
                inline_agent: has_inline.then(|| InlineAgent {
                    name: "overlay".into(),
                    prompt: "overlay".into(),
                    model: i_mod.map(str::to_string),
                    category: i_cat.map(str::to_string),
                    ..InlineAgent::default()
                }),
            },
        )
        .unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(resolved.agent.model.as_str(), expected, "{label}");
        assert_eq!(resolved.authorized_target.as_str(), "worker");
    }

    // 2. Remembered preference + effort suffix + invalid effort rejection.
    let engine = engine_with_catalog(catalog_with_worker_policy(ModelPolicy::default())).await;
    engine.runtime_registry().publish_agent_model_preferences(
        [("worker".into(), ModelRef::new("remembered/model"))]
            .into_iter()
            .collect(),
    );
    let binding = engine.bind_runtime(&workdir).unwrap();
    let resolve = |model: Option<&str>, effort: Option<&str>| {
        resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &CategoryRegistry::default(),
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &sidecar_environment,
            },
            SpawnMember {
                prompt: "effort".into(),
                subagent_type: "worker".into(),
                model: model.map(str::to_string),
                effort: effort.map(str::to_string),
                ..SpawnMember::default()
            },
        )
        .map(|r| r.agent.model.to_string())
    };
    assert_eq!(resolve(None, None).unwrap(), "remembered/model");
    assert_eq!(
        resolve(Some("explicit/model"), None).unwrap(),
        "remembered/model"
    );
    assert_eq!(
        resolve(None, Some("high")).unwrap(),
        "remembered/model#high"
    );
    assert_eq!(
        resolve(Some("offline#low"), None).unwrap(),
        "hya/offline#low"
    );
    assert_eq!(
        resolve(Some("offline#low"), Some("high")).unwrap(),
        "hya/offline#high"
    );
    assert!(matches!(
        resolve(None, Some("ludicrous")),
        Err(SpawnError::InvalidEffort { effort, .. }) if effort == "ludicrous"
    ));

    // 3. Category fallback suffix chains.
    let mut fb_entries = HashMap::new();
    fb_entries.insert(
        "deep".into(),
        CategoryEntry::from_candidates(&[
            "provider/first".into(),
            "provider/second".into(),
            "provider/third".into(),
        ])
        .unwrap(),
    );
    fb_entries.insert(
        "single".into(),
        CategoryEntry::from_candidates(&["provider/only".into()]).unwrap(),
    );
    let fallbacks = category_model_fallbacks(&CategoryRegistry::from_entries(fb_entries));
    assert_eq!(fallbacks.len(), 2);
    assert_eq!(
        fallbacks[&ModelRef::new("provider/first")],
        ["provider/first", "provider/second", "provider/third"].map(ModelRef::new)
    );
    assert_eq!(
        fallbacks[&ModelRef::new("provider/second")],
        ["provider/second", "provider/third"].map(ModelRef::new)
    );
    assert!(!fallbacks.contains_key(&ModelRef::new("provider/only")));

    // 4. Executable spawn captures sidecar factory before admission.
    let exec_engine = engine_with_catalog(Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("exec")])).unwrap(),
    )))
    .await;
    let exec_binding = exec_engine.bind_runtime(&workdir).unwrap();
    let exec_resolved = resolve_spawn_member(
        &ResolveSpawnMemberCtx {
            engine: &exec_engine,
            binding: &exec_binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &CategoryRegistry::default(),
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &sidecar_environment,
        },
        SpawnMember {
            prompt: "pre-admission".into(),
            subagent_type: "worker".into(),
            ..SpawnMember::default()
        },
    )
    .unwrap();
    assert!(exec_resolved.sidecar_factory.is_some());
}

#[test]
fn sidecar_materialization_scoping_and_validation_invariants() {
    let _env = StableEnvGuard::new();

    // 1. Duplicate hook declaration is rejected.
    let empty_rt = RuntimeRegistry::new(hya_tool_loader::builtins(), catalog_with_agents(&[]));
    let empty_binding = empty_rt.bind_turn(&std::env::temp_dir()).unwrap();
    let dup = validate_bundle_sidecar_hooks(
        &empty_binding,
        "build",
        &[
            HookRegistration {
                name: HookName::Event.into(),
            },
            HookRegistration {
                name: HookName::Event.into(),
            },
        ],
    );
    assert!(matches!(
        dup,
        Err(CoreError::Invalid(ref msg)) if msg.contains("duplicate Bundle sidecar hook declaration `event`")
    ));

    // 2. Cross-bundle resource selection is refused.
    let owner = materialized_bundle("owner");
    let mut selector = owner.clone();
    selector.identity.id = "hya/selector".into();
    selector.digest = "selector-digest".into();
    selector.agents[0].id = AgentName::new("selector");
    selector.agents[0].resource_view = ResourceView {
        allow: vec!["bundle:hya/materialized/tool/echo".into()],
        ..ResourceView::default()
    };
    selector.agents[0].hook_refs = vec!["bundle:hya/materialized/hook/event".into()];
    selector.tools.clear();
    selector.hooks.clear();
    selector.extensions.clear();
    let cross_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[selector, owner]).unwrap(),
        )),
    );
    let cross_dir = tempdir();
    let cross_binding = cross_rt.bind_turn(&cross_dir).unwrap();
    assert!(matches!(
        cross_binding.has_selected_bundle_sidecar_capability("selector"),
        Err(hya_bundle::BundleError::ResourceNotInPlane { reference, .. })
            if reference == "bundle:hya/materialized/tool/echo"
    ));

    // 3. Captured turn binding pins materialized resources across catalog publication & isolates agent closures.
    let old_cat = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&disjoint_materialized_bundles("old")).unwrap(),
    ));
    let new_cat = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&disjoint_materialized_bundles("new")).unwrap(),
    ));
    let disjoint_rt = RuntimeRegistry::new(hya_tool_loader::builtins(), old_cat);
    let old_binding = disjoint_rt.bind_turn(&cross_dir).unwrap();
    disjoint_rt.publish_catalog(new_cat).unwrap();
    let alpha_dir = tempdir();
    let alpha_files =
        materialize_bundle_sidecar_resources(&old_binding, "alpha", &alpha_dir).unwrap();
    assert_eq!(alpha_files, vec![alpha_dir.join("extensions/alpha.js")]);
    assert_eq!(
        std::fs::read_to_string(alpha_dir.join("extensions/alpha.js")).unwrap(),
        "old extensions/alpha.js\n"
    );
    assert!(!alpha_dir.join("extensions/beta.js").exists());
    let new_binding = disjoint_rt.bind_turn(&cross_dir).unwrap();
    let new_dir = tempdir();
    materialize_bundle_sidecar_resources(&new_binding, "alpha", &new_dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(new_dir.join("extensions/alpha.js")).unwrap(),
        "new extensions/alpha.js\n"
    );

    // 4. Deduplicated extension materialization orders by canonical identity.
    let mut ordered = disjoint_materialized_bundle("ordered");
    ordered.agents[0]
        .resource_view
        .allow
        .push(ordered.tools[1].stable_id.clone());
    ordered.extensions[0].local_id = "zeta".into();
    ordered.extensions[0].stable_id = "bundle:hya/materialized/extension/zeta".into();
    ordered.extensions[1].local_id = "alpha".into();
    ordered.extensions[1].stable_id = "bundle:hya/materialized/extension/alpha".into();
    let ord_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[ordered]).unwrap(),
        )),
    );
    let ord_dir = tempdir();
    let ord_binding = ord_rt.bind_turn(&cross_dir).unwrap();
    let ord_files = materialize_bundle_sidecar_resources(&ord_binding, "alpha", &ord_dir).unwrap();
    assert_eq!(
        ord_files,
        vec![
            ord_dir.join("extensions/beta.js"),
            ord_dir.join("extensions/alpha.js"),
        ]
    );

    // 5. Factory scoping: static-only / non-selecting agent stays process-free; executable agent gets a factory.
    let worker_bundle = materialized_bundle("scoped");
    let mut lead_bundle = worker_bundle.clone();
    lead_bundle.identity.id = "hya/scoped-lead".into();
    lead_bundle.digest = "scoped-lead-digest".into();
    lead_bundle.tools.clear();
    lead_bundle.hooks.clear();
    lead_bundle.extensions.clear();
    lead_bundle.agents[0].id = AgentName::new("lead");
    lead_bundle.agents[0].role = AgentRole::Main;
    lead_bundle.agents[0].resource_view = ResourceView::default();
    lead_bundle.agents[0].hook_refs.clear();
    let scoped_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[lead_bundle, worker_bundle]).unwrap(),
        )),
    );
    let scoped_binding = scoped_rt.bind_turn(&cross_dir).unwrap();
    let staging = tempdir();
    let env = BundleSidecarEnvironment::from_command(
        vec!["bun".into(), "sidecar.js".into()],
        staging.clone(),
    );
    assert!(env.factory_for(&scoped_binding, "lead").unwrap().is_none());
    assert!(
        env.factory_for(&scoped_binding, "worker")
            .unwrap()
            .is_some()
    );

    // 6. Skill declaration validation matches complete package (or empty for static skills).
    let make_skill = |local_id: &str| {
        let content = format!("---\nname: {local_id}\ndescription: {local_id}\n---\nbody\n");
        PreparedResource {
            local_id: local_id.into(),
            stable_id: format!("bundle:hya/materialized/skill/{local_id}"),
            source_path: format!("resources/skills/{local_id}.md"),
            digest: format!("{:x}", Sha256::digest(content.as_bytes())),
            content,
            binary_base64: None,
            aliases: Vec::new(),
            overrides: None,
        }
    };
    let selected = make_skill("selected");
    let unselected = make_skill("unselected");
    let mut skill_bundle = disjoint_materialized_bundle("skills");
    skill_bundle.agents[0]
        .resource_view
        .allow
        .push(selected.stable_id.clone());
    skill_bundle.skills = vec![selected.clone(), unselected.clone()];
    let skill_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[skill_bundle]).unwrap(),
        )),
    );
    let full = PluginContributionSet {
        skills: [&selected, &unselected]
            .into_iter()
            .map(|r| hya_plugin::SkillContribution {
                id: r.local_id.clone(),
                content: r.content.clone(),
                digest: r.digest.clone(),
            })
            .collect(),
        ..PluginContributionSet::default()
    };
    let src = crate::bundle_runtime::source::prepared_static_bundle_source(
        "hya/materialized",
        &[selected.clone(), unselected],
        &full,
    )
    .unwrap()
    .into_runtime_source();
    skill_rt.refresh(|c| c.upsert_sources(vec![src])).unwrap();
    let skill_binding = skill_rt.bind_turn(&cross_dir).unwrap();
    let partial = PluginContributionSet {
        skills: vec![hya_plugin::SkillContribution {
            id: selected.local_id,
            content: selected.content,
            digest: selected.digest,
        }],
        ..PluginContributionSet::default()
    };
    assert!(validate_bundle_sidecar_skills(&skill_binding, "alpha", &partial).is_err());
    assert!(validate_bundle_sidecar_skills(&skill_binding, "alpha", &full).is_ok());
    assert!(
        validate_bundle_sidecar_skills(&skill_binding, "alpha", &PluginContributionSet::default())
            .is_ok()
    );

    for d in [cross_dir, alpha_dir, new_dir, ord_dir, staging] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[tokio::test]
async fn bundle_sidecar_declaration_validation_and_tool_binding() {
    let _env = StableEnvGuard::new();
    let turn_dir = tempdir();

    // 1. Canonical tool binding preserves captured binding across catalog replacement.
    let old_cat = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&[materialized_bundle("old")]).unwrap(),
    ));
    let mut new_bundle = materialized_bundle("new");
    new_bundle.tools.clear();
    let new_cat = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&[new_bundle]).unwrap(),
    ));
    let rt = RuntimeRegistry::new(hya_tool_loader::builtins(), old_cat);
    let old_binding = rt.bind_turn(&turn_dir).unwrap();
    rt.publish_catalog(new_cat).unwrap();
    let echo = ToolInfo {
        name: "echo".into(),
        description: "sidecar echo".into(),
        input_schema: json!({"type": "object"}),
    };
    let (host, plugin) = prepared_sidecar_plugin(std::slice::from_ref(&echo), &[]).await;
    let bindings = bind_bundle_sidecar_tools(&old_binding, "worker", &plugin).unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].tool.name(), "bundle:hya/materialized/tool/echo");
    assert_eq!(bindings[0].permission, ToolPermission::Tool);
    host.shutdown().await.unwrap();

    // 2. Loaded module tool set must match exact selected set (missing & extra rejected); order ignored.
    let mut multi = disjoint_materialized_bundle("multi");
    let all_tools: Vec<_> = multi.tools.iter().map(|r| r.stable_id.clone()).collect();
    let all_hooks: Vec<_> = multi.hooks.iter().map(|r| r.stable_id.clone()).collect();
    let exact_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[multi.clone()]).unwrap(),
        )),
    );
    let exact_binding = exact_rt.bind_turn(&turn_dir).unwrap();
    assert!(matches!(
        validate_bundle_sidecar_hooks(&exact_binding, "alpha", &[]),
        Err(CoreError::Invalid(_))
    ));
    let beta = ToolInfo {
        name: "beta".into(),
        description: "beta".into(),
        input_schema: json!({"type": "object"}),
    };
    let (h_empty, p_empty) = prepared_sidecar_plugin(&[], &[]).await;
    assert!(matches!(
        bind_bundle_sidecar_tools(&exact_binding, "alpha", &p_empty),
        Err(CoreError::Invalid(_))
    ));
    h_empty.shutdown().await.unwrap();
    let (h_extra, p_extra) = prepared_sidecar_plugin(&[echo.clone(), beta.clone()], &[]).await;
    assert!(matches!(
        bind_bundle_sidecar_tools(&exact_binding, "alpha", &p_extra),
        Err(CoreError::Invalid(_))
    ));
    h_extra.shutdown().await.unwrap();

    // Order-independent matching for both hooks and tools.
    multi.agents[0].resource_view.allow = all_tools;
    multi.agents[0].hook_refs = all_hooks;
    let ord_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[multi]).unwrap(),
        )),
    );
    let ord_binding = ord_rt.bind_turn(&turn_dir).unwrap();
    let regs = [
        HookRegistration {
            name: HookName::ToolExecuteBefore.into(),
        },
        HookRegistration {
            name: HookName::Event.into(),
        },
    ];
    assert!(validate_bundle_sidecar_hooks(&ord_binding, "alpha", &regs).is_ok());
    let (h_ord, p_ord) = prepared_sidecar_plugin(&[echo, beta], &regs).await;
    let names: Vec<_> = bind_bundle_sidecar_tools(&ord_binding, "alpha", &p_ord)
        .unwrap()
        .iter()
        .map(|b| b.tool.name().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "bundle:hya/materialized/tool/beta",
            "bundle:hya/materialized/tool/echo",
        ]
    );
    h_ord.shutdown().await.unwrap();

    // 3. Hook-only activation accepts zero tools; unsupported workspace adapters are ignored.
    let mut hook_only = materialized_bundle("hook-only");
    hook_only.tools.clear();
    hook_only.agents[0].resource_view = ResourceView::default();
    let ho_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[hook_only]).unwrap(),
        )),
    );
    let ho_binding = ho_rt.bind_turn(&turn_dir).unwrap();
    let (h_ho, p_ho) = prepared_sidecar_plugin(
        &[],
        &[HookRegistration {
            name: HookName::Event.into(),
        }],
    )
    .await;
    assert!(
        bind_bundle_sidecar_tools(&ho_binding, "worker", &p_ho)
            .unwrap()
            .is_empty()
    );
    h_ho.shutdown().await.unwrap();

    let staging = tempdir();
    let adapter_rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("ws-adapter")]).unwrap(),
        )),
    );
    let adapter_binding = adapter_rt.bind_turn(&turn_dir).unwrap();
    let fixture = activation_sidecar_fixture("[]").replace(
        "\"workspaceAdapters\": []",
        r#""workspaceAdapters": [{"type":"worktree","name":"unsupported","description":"ignored"}]"#,
    );
    let env = BundleSidecarEnvironment::from_command(
        vec!["python3".into(), "-c".into(), fixture],
        staging.clone(),
    );
    let mut handle = env
        .factory_for(&adapter_binding, "worker")
        .unwrap()
        .unwrap()
        .start(SidecarStart {
            activation_id: "unsupported-adapter".into(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    std::fs::remove_dir_all(staging).unwrap();
    std::fs::remove_dir_all(turn_dir).unwrap();
}

#[tokio::test]
async fn bundle_sidecar_lifecycle_staging_cleanup_and_tool_cancellation() {
    let _env = StableEnvGuard::new();
    let turn_dir = tempdir();
    let staging = tempdir();
    let rt = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("lifecycle")]).unwrap(),
        )),
    );
    let binding = rt.bind_turn(&turn_dir).unwrap();
    let env = BundleSidecarEnvironment::from_command(
        vec![
            "python3".into(),
            "-c".into(),
            activation_sidecar_fixture("[]"),
        ],
        staging.clone(),
    );
    let factory = env.factory_for(&binding, "worker").unwrap().unwrap();
    let ctx = sidecar_test_ctx(&turn_dir);

    // 1. Explicit shutdown closes retained tools and removes activation staging.
    let mut h1 = factory
        .start(SidecarStart {
            activation_id: "act-shutdown".into(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .unwrap();
    h1.ready().await.unwrap();
    assert!(staging.join("act-shutdown").is_dir());
    let tool1 = h1.tool_bindings()[0].tool.clone();
    h1.shutdown().await.unwrap();
    assert!(!staging.join("act-shutdown").exists());
    assert!(matches!(
        tool1.execute(&ctx, json!({})).await,
        Err(ToolError::Other(msg)) if msg.contains("plugin connection closed")
    ));

    // 2. Dropping handle closes connection, triggers loss token, and removes activation staging.
    let mut h2 = factory
        .start(SidecarStart {
            activation_id: "act-drop".into(),
            lifecycle: SidecarLifecycle::Transient,
        })
        .await
        .unwrap();
    h2.ready().await.unwrap();
    let tool2 = h2.tool_bindings()[0].tool.clone();
    tool2.execute(&ctx, json!({})).await.unwrap();
    let loss = h2.loss_token().expect("transport loss token");
    drop(h2);
    assert!(loss.is_cancelled());
    assert!(!staging.join("act-drop").exists());
    assert!(matches!(
        tool2.execute(&ctx, json!({})).await,
        Err(ToolError::Other(msg)) if msg.contains("plugin connection closed")
    ));

    // 3. Aborting pending start cleans activation staging.
    #[cfg(unix)]
    {
        let hang_env = BundleSidecarEnvironment::from_command(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf started > sidecar-started.marker; cat >/dev/null".into(),
            ],
            staging.clone(),
        );
        let hang_factory = hang_env.factory_for(&binding, "worker").unwrap().unwrap();
        let act_dir = staging.join("act-cancel");
        let marker = act_dir.join("sidecar-started.marker");
        let start = tokio::spawn(async move {
            hang_factory
                .start(SidecarStart {
                    activation_id: "act-cancel".into(),
                    lifecycle: SidecarLifecycle::Transient,
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !marker.is_file() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        start.abort();
        assert!(matches!(start.await, Err(err) if err.is_cancelled()));
        assert!(!act_dir.exists());
    }

    // 4. Tool cancellation returns ToolError::Cancelled without waiting for RPC reply.
    let session = SessionId::new();
    let call = hya_proto::ToolCallId::new();
    let canonical = "bundle:hya/materialized/tool/echo".to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        r#"
import json, sys, socket
for line in sys.stdin:
    req = json.loads(line)
    if req['method'] == 'initialize':
        result = {{'protocol_version':1,'plugin':{{'id':'test','version':'1','kind':'bun'}},
                  'tools':[{{'name':'echo','description':'echo','inputSchema':{{'type':'object'}}}}]}}
        print(json.dumps({{'jsonrpc':'2.0','id':req['id'],'result':result}}), flush=True)
    elif req['method'] == 'tool/call':
        with socket.create_connection(('127.0.0.1', {port})) as conn:
            conn.sendall((json.dumps(req)+'\n').encode())
        sys.stdin.read()
"#
    );
    let host = PluginHost::connect(
        PluginSpec {
            id: "test".into(),
            kind: PluginKindWire::Bun,
            command: vec!["python3".into(), "-c".into(), script],
            timeout_ms: None,
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: ProcessLaunch::Bundle {
                root: std::env::temp_dir(),
                activation: Some(ActivationMetadata {
                    activation_id: "cancel".into(),
                    lifecycle: ActivationLifecycle::Transient,
                }),
            },
        },
        host_info(),
        None,
    )
    .await
    .unwrap();
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (io, _) = listener.accept().await.unwrap();
        let line = tokio::io::BufReader::new(io)
            .lines()
            .next_line()
            .await
            .unwrap()
            .unwrap();
        let Frame::Request(req) = Frame::parse(&line).unwrap() else {
            panic!("expected request frame");
        };
        assert_eq!(req.method, METHOD_TOOL_CALL);
        let params: ToolCallParams = serde_json::from_value(req.params).unwrap();
        assert_eq!(params.tool, "echo");
        assert_eq!(params.session, session);
        assert_eq!(params.call, call);
        let _ = observed_tx.send(());
    });
    let mut cancel_ctx = sidecar_test_ctx(&turn_dir);
    cancel_ctx.session = Some(session);
    cancel_ctx.operation = ToolOperation::from_tool_call(call);
    let cancel = cancel_ctx.cancel.clone();
    let tool = host.prepared_plugins()[0].tool("echo", &canonical).unwrap();
    let mut execute = Box::pin(tool.execute(&cancel_ctx, json!({"text": "cancel"})));
    let outcome = tokio::time::timeout(Duration::from_millis(500), async {
        tokio::select! {
            result = &mut execute => (false, result),
            observed = observed_rx => {
                assert!(observed.is_ok());
                cancel.cancel();
                (true, execute.await)
            }
        }
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
    assert!(matches!(outcome, (true, Err(ToolError::Cancelled))));
    host.terminate().await.unwrap();

    std::fs::remove_dir_all(staging).unwrap();
    std::fs::remove_dir_all(turn_dir).unwrap();
}

#[tokio::test]
async fn one_bun_module_enforces_disjoint_views_in_engine_turns() {
    let _env = StableEnvGuard::new();
    disjoint_engine_turns(false).await;
}

#[tokio::test]
async fn one_bundle_process_enforces_disjoint_views_in_engine_turns() {
    let _env = StableEnvGuard::new();
    disjoint_engine_turns(true).await;
}

async fn disjoint_engine_turns(process: bool) {
    let root = tempdir();
    let staging = tempdir();
    let runtime_block = if process {
        "runtime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/main.py'] }\nassets: [{ id: main, path: main.py }]"
    } else {
        "runtime: { kind: js, mode: managed, entry: main.js }"
    };
    let tool_path = if process { "tool.json" } else { "main.js" };
    let manifest = format!(
        "kind: Bundle\nschema_version: 2\nidentity: {{id: test/disjoint-turns, version: 1.0.0, publisher: test}}\n{runtime_block}\nagents:\n  - {{id: alpha, role: main, resource_view: {{allow: [alpha]}}}}\n  - {{id: beta, role: main, resource_view: {{allow: [beta]}}}}\nresources:\n  tools:\n    - {{id: alpha, path: {tool_path}}}\n    - {{id: beta, path: {tool_path}}}\n"
    );
    let prepared = prepare_package(BundleSource::new(
        "disjoint-turns",
        vec![
            SourceFile::new("bundle.yaml", manifest),
            SourceFile::new("tool.json", "{}"),
            SourceFile::new(
                "main.js",
                r#"
export default {server: async () => ({tool: {
  alpha: {description: "alpha", execute: async () => `alpha:${process.pid}`},
  beta: {description: "beta", execute: async () => `beta:${process.pid}`},
}})};
"#,
            ),
            SourceFile::new(
                "main.py",
                r#"
import json, os, sys
for line in sys.stdin:
    req = json.loads(line)
    if req['method'] == 'initialize':
        result = {'protocol_version': 1, 'plugin': {'id': 'disjoint-turns', 'version': '1', 'kind': 'rust'},
                  'tools': [{'name': name, 'description': name, 'inputSchema': {'type': 'object'}} for name in ['alpha', 'beta']]}
    elif req['method'] == 'tool/call':
        result = {'ok': True, 'output': {'title': '', 'output': req['params']['tool'] + ':' + str(os.getpid()), 'metadata': {}}, 'time_ms': 0}
    elif req['method'] == 'shutdown':
        print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': {}}), flush=True)
        break
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': req['id'], 'result': result}), flush=True)
"#,
            ),
        ],
    ))
    .unwrap();
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(prepared.bundles()).unwrap(),
    ));
    let runtime = Arc::new(RuntimeRegistry::new(hya_tool_loader::builtins(), catalog));
    let bundle = &prepared.bundles()[0];
    let process_extension = prepared.bundle_process("test/disjoint-turns");
    let cfg = crate::bundle_config::bundle_config_location(
        &root.join("config.yaml"),
        "test/disjoint-turns",
        crate::bundle_config::BundleConfigScope::User,
    )
    .unwrap();
    let source = crate::bundle_runtime::prepare_source(
        bundle,
        crate::bundle_runtime::BundleRuntimeParts {
            process: process_extension,
            schemas: &[],
            apis: &[],
            permission_modes: &[],
            reads: None,
        },
        &crate::bundle_runtime::BundleRuntimeConfig::capture(bundle, process_extension, cfg),
    )
    .await
    .unwrap();
    runtime
        .refresh(|c| c.upsert_sources(vec![source.source]))
        .unwrap();

    let mut pids = Vec::new();
    for (agent, other) in [("alpha", "beta"), ("beta", "alpha")] {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let scripts = [
            agent.to_string(),
            other.to_string(),
            format!("bundle:test/disjoint-turns/tool/{other}"),
        ]
        .into_iter()
        .map(|name| {
            vec![
                FakeStep::ToolCall {
                    name,
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        })
        .chain([vec![FakeStep::Finish(FinishReason::Stop)]])
        .collect();
        let provider = ToolViewProvider {
            inner: FakeProvider::scripted_turns(scripts),
            requests: requests.clone(),
        };
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Tool,
            "*",
            Mode::Allow,
        )]));
        let environment = BundleSidecarEnvironment::from_command(
            plugins::bundle_sidecar_command().unwrap(),
            staging.clone(),
        );
        let engine = SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            runtime.clone(),
            permission,
            EventBus::default(),
        )
        .with_sidecar_environment(Arc::new(environment));
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new(agent),
                model: ModelRef::new("fake"),
                workdir: root.to_string_lossy().into_owned(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        engine
            .admit_user_prompt(session, "exercise tool view".into())
            .await
            .unwrap();
        engine
            .run_turn(
                session,
                &AgentSpec {
                    name: AgentName::new(agent),
                    model: ModelRef::new("fake"),
                    system_prompt: "tool view".into(),
                    workdir: root.clone(),
                    reasoning: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        for request in requests.lock().unwrap().iter() {
            let names: Vec<_> = request.tools.iter().map(|t| t.name.as_str()).collect();
            assert!(names.contains(&agent));
            assert!(!names.contains(&other));
        }
        let events = engine.store().replay(session).await.unwrap();
        let results: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::ToolResult { output, .. } => Some(output["output"].as_str().unwrap()),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 1);
        let (tool, pid) = results[0].split_once(':').unwrap();
        assert_eq!(tool, agent);
        pids.push(pid.to_string());
        let errors: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::ToolError { message_text, .. } => Some(message_text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            errors,
            [
                format!("unknown tool: {other}"),
                format!("unknown tool: bundle:test/disjoint-turns/tool/{other}")
            ]
        );
    }
    if process {
        assert_eq!(pids[0], pids[1]);
    } else {
        assert_ne!(pids[0], pids[1]);
    }
    drop(runtime);
    assert!(std::fs::read_dir(&staging).unwrap().next().is_none());
    std::fs::remove_dir_all(staging).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn selected_main_bun_bundle_runs_harness_tool_hook_event_loop_and_reaps_sidecar() {
    let _env = StableEnvGuard::new();
    let canonical = "bundle:hya/materialized/tool/echo";
    let mut bundle = materialized_bun_bundle("bun-root-hooks");
    let event_hook =
        materialized_resource("bun-root-hooks", "hook", "event", "extensions/runtime.js");
    let before_hook = materialized_resource(
        "bun-root-hooks",
        "hook",
        "tool.execute.before",
        "extensions/runtime.js",
    );
    let after_hook = materialized_resource(
        "bun-root-hooks",
        "hook",
        "tool.execute.after",
        "extensions/runtime.js",
    );
    bundle.agents[0].hook_refs = vec![
        event_hook.stable_id.clone(),
        before_hook.stable_id.clone(),
        after_hook.stable_id.clone(),
    ];
    bundle.hooks.extend([event_hook, before_hook, after_hook]);
    bundle.agents[0].id = AgentName::new("root-hook-agent");
    bundle.agents[0].role = AgentRole::Main;
    set_materialized_extension_content(
        &mut bundle,
        r#"
let eventCount = 0
export default {
  id: "bundle-extension",
  server: async () => ({
    event: () => { eventCount += 1 },
    "tool.execute.before": async (params) => ({
      input: { ...params.input, hooked: true, eventCount },
    }),
    "tool.execute.after": async (params) => ({
      ...params.result,
      output: { ...params.result.output, output: `${params.result.output.output}:after` },
    }),
    tool: {
      echo: {
        description: "bundle echo",
        execute: async (input) => {
          if (input.hooked !== true || input.eventCount <= 0) throw new Error("missing hook mutation")
          return `bun-root-hooks:${input.eventCount}`
        },
      },
    },
  }),
}
"#
        .to_string(),
    );
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&[bundle]).unwrap(),
    ));
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".into(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("bundle root complete".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let workdir = tempdir();
    let staging_root = tempdir();
    let environment = BundleSidecarEnvironment::from_command(
        plugins::bundle_sidecar_command().expect("Bun must be available"),
        staging_root.clone(),
    );
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            Arc::new(RuntimeRegistry::new(hya_tool_loader::builtins(), catalog)),
            permission,
            EventBus::default(),
        )
        .with_sidecar_environment(Arc::new(environment)),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("root-hook-agent"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "run root bundle hook".into())
        .await
        .unwrap();
    let finish = engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("root-hook-agent"),
                model: ModelRef::new("fake"),
                system_prompt: "root base".into(),
                workdir: workdir.clone(),
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let events = engine.store().replay(session).await.unwrap();
    let outputs: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::ToolResult { output, .. } => output.get("output").and_then(Value::as_str),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 1);
    assert!(outputs[0].starts_with("bun-root-hooks:") && outputs[0].ends_with(":after"));
    assert!(std::fs::read_dir(&staging_root).unwrap().next().is_none());
    std::fs::remove_dir_all(staging_root).unwrap();
    std::fs::remove_dir_all(workdir).unwrap();
}

#[tokio::test]
async fn resident_bun_bundle_reuses_one_sidecar_across_two_mailbox_turns() {
    let _env = StableEnvGuard::new();
    let canonical = "bundle:hya/materialized/tool/echo";
    let mut bundle = materialized_bun_bundle("bun-resident");
    set_materialized_extension_content(
        &mut bundle,
        r#"
let calls = 0
export default {
  id: "bundle-extension",
  server: async () => ({
    tool: {
      echo: {
        description: "bundle echo",
        execute: async () => `${process.pid}:${++calls}`,
      },
    },
  }),
}
"#
        .to_string(),
    );
    let catalog = Arc::new(to_agent_catalog(
        BundleCatalog::from_prepared(&[bundle]).unwrap(),
    ));
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "echo".into(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("first".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::ToolCall {
                name: "echo".into(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("second".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        canonical,
        Mode::Allow,
    )]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(hya_tool_loader::builtins(), catalog)),
        permission,
        EventBus::default(),
    ));
    let workdir = tempdir();
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&workdir).unwrap();
    let staging_root = tempdir();
    let environment = BundleSidecarEnvironment::from_command(
        plugins::bundle_sidecar_command().expect("Bun must be available"),
        staging_root.clone(),
    );
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "lead base".into(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let allowed = [AgentDef {
        name: "worker".into(),
        description: None,
        category: None,
        mode: "subagent".into(),
        effort: None,
        effort_source: None,
    }];
    let categories = CategoryRegistry::default();
    let is_servable = |_: &ModelRef| true;
    let ResolvedSpawnMember {
        agent,
        binding,
        agents,
        resources,
        sidecar_factory,
        ..
    } = resolve_spawn_member(
        &ResolveSpawnMemberCtx {
            engine: &engine,
            binding: &binding,
            base: &base,
            caller: "build",
            allowed_agents: &allowed,
            categories: &categories,
            is_servable: &is_servable,
            guidance: None,
            sidecar_environment: &environment,
        },
        SpawnMember {
            subagent_type: "worker".into(),
            ..SpawnMember::default()
        },
    )
    .unwrap();
    let _lead_turn = engine.try_begin_turn(lead).unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = supervisor
        .spawn_resident(
            lead,
            agent,
            (binding, agents, resources, sidecar_factory),
            String::new(),
            None,
            None,
        )
        .await
        .unwrap();
    let mut bus = engine.bus().subscribe();

    for mail in ["resident mail one", "resident mail two"] {
        engine
            .mail_send(
                lead,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                mail.into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let envelope = bus.recv().await.unwrap();
                if matches!(
                    ExtensionEvent::from_event(&envelope.event),
                    Some(ExtensionEvent::AgentActivityChanged {
                        handle: ref h,
                        status: RosterStatus::Idle,
                        ..
                    }) if h == &handle
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    let events = engine.store().replay(child).await.unwrap();
    let outputs: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::ToolResult { output, .. } => output.get("output").and_then(Value::as_str),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 2);
    let (pid1, c1) = outputs[0].split_once(':').unwrap();
    let (pid2, c2) = outputs[1].split_once(':').unwrap();
    assert_eq!(pid1, pid2);
    assert_eq!((c1, c2), ("1", "2"));

    supervisor.team_cancel(lead).unwrap().cancel();
    std::fs::remove_dir_all(staging_root).unwrap();
    std::fs::remove_dir_all(workdir).unwrap();
}

#[tokio::test]
async fn startup_recovery_rebuilds_executable_resident_sidecar_before_queued_mail() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let registry_path = bundle_registry_path();
    std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
    let stable_id = "runtime-installed-resident-agent";
    let marker_path = home.join("resident-startup-sidecar.marker");
    let marker_literal = serde_json::to_string(&marker_path.to_string_lossy().to_string()).unwrap();
    let installed = prepare_package(BundleSource::new(
        "runtime-installed-resident",
        vec![
            SourceFile::new(
                "bundle.hya.md",
                b"---\nkind: Bundle\nschema_version: 2\nidentity:\n  id: hya/runtime-installed-resident\n  version: 1.0.0\n  publisher: hya\nruntime:\n  kind: js\n  mode: managed\n  entry: extensions/runtime.js\nresources:\n  tools:\n    - id: echo\n      path: extensions/runtime.js\nagents:\n  - id: runtime-installed-resident-agent\n    role: subagent\n    resource_view:\n      allow:\n        - bundle:hya/runtime-installed-resident/tool/echo\n---\nYou are the installed resident agent.\n",
            ),
            SourceFile::new(
                "extensions/runtime.js",
                format!(
                    r#"export default {{
  id: "runtime",
  server: async () => {{
    await Bun.write({marker_literal}, "ready");
    return {{ tool: {{ echo: {{ description: "echo", execute: async () => "ok" }} }} }};
  }},
}}"#
                ),
            ),
        ],
    ))
    .unwrap();
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        registry
            .install(
                &[],
                NamespaceInstallPolicy::DenyConflicts,
                BundleInstallCandidate {
                    source_digest: [0x53; 32],
                    prepared_digest: installed.digest().to_string(),
                    prepared_bytes: installed.bytes().to_vec(),
                    installed_at: 1_725_000_021,
                },
            )
            .await
            .unwrap(),
        BundleInstallOutcome::Installed { generation: 1 }
    );

    let database = home.join("resident-startup-recovery.db");
    let store = SessionStore::connect(database.to_str().unwrap())
        .await
        .unwrap();
    let (router, model) = offline_router(None);
    let base = agent_with_model(&model, None);
    let root = SessionId::new();
    let actor = SessionId::new();
    for (session, parent, agent_name) in [
        (root, None, "removed-root-bundle-agent"),
        (actor, Some(root), stable_id),
    ] {
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent,
                    agent: AgentName::new(agent_name),
                    model: base.model.clone(),
                    workdir: workdir.to_string_lossy().into_owned(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();
    }
    let claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    store
        .commit_resident_mutation(
            &claim,
            root,
            vec![
                ExtensionEvent::AgentRegistered {
                    session: root,
                    agent_session: actor,
                    handle: "installed-resident-1".into(),
                    parent: Some("main".into()),
                    agent_type: AgentName::new(stable_id),
                    mode: SubagentMode::Resident,
                }
                .into_event(),
            ],
        )
        .await
        .unwrap();
    store
        .append_event(
            root,
            &ExtensionEvent::ChannelCreated {
                session: root,
                channel: "DM-12345678".into(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec!["main".into(), "main/installed-resident-1".into()],
            }
            .into_event(),
        )
        .await
        .unwrap();
    store
        .append_event(
            root,
            &ExtensionEvent::MailSent {
                session: root,
                from: "main".into(),
                to: MailEndpoint::Channel("DM-12345678".into()),
                kind: MailKind::Message,
                body: "startup recovery executable mail".into(),
            }
            .into_event(),
        )
        .await
        .unwrap();

    let mut built = build_session_engine(
        store,
        router,
        &base,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(built.resident_supervisor().episode_count(), 0);
    built.resident_supervisor().start_recovered().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = built.engine().read_projection(root).await.unwrap();
            let team = team_projection(&projection);
            if team
                .archived
                .get("main/installed-resident-1")
                .is_some_and(|e| e.reason == ArchiveReason::Reported)
                || team
                    .roster
                    .get("main/installed-resident-1")
                    .is_some_and(|e| e.resident_cursor >= 1)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&marker_path).unwrap_or_default(),
        "ready"
    );
    assert_eq!(
        built
            .engine()
            .bind_runtime(&workdir)
            .unwrap()
            .resolve_agent(stable_id)
            .unwrap()
            .stable_id,
        stable_id
    );
    built.shutdown().await.unwrap();
}

#[tokio::test]
async fn catalog_scope_sweeper_evicts_idle_scopes_until_stopped() {
    let _env = StableEnvGuard::new();
    let (router, _model) = offline_router(None);
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        builtin_agent_catalog().unwrap(),
    ));
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(router),
            Arc::clone(&runtime),
            permission,
            EventBus::default(),
        )
        .with_catalog_scope_cache(hya_engine::CatalogScopeCacheConfig {
            max_scopes: 32,
            idle_ttl: Duration::ZERO,
        }),
    );
    let root = std::env::temp_dir();
    let scope = hya_engine::CatalogScope::Project {
        id: hya_proto::ProjectId::new(),
        roots: vec![root.clone()],
    };
    let binding = engine.bind_scope_runtime(&scope, &root).await.unwrap();
    runtime
        .publish_scope(
            scope.key(),
            hya_engine::ScopeOverlay::new(builtin_agent_catalog().unwrap()),
        )
        .unwrap();
    drop(binding);
    assert!(runtime.scope_overlay(&scope.key()).is_some());

    let stop = CancellationToken::new();
    let sweeper = tokio::spawn(run_catalog_scope_sweeper(
        Arc::downgrade(&engine),
        stop.clone(),
        Duration::from_millis(10),
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.scope_overlay(&scope.key()).is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("sweeper must evict idle scope");
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), sweeper)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn bare_profile_builds_unified_session_engine_with_eight_bare_tools_and_hya_bare_agent() {
    let home = tempdir();
    let _env = EnvGuard::with_home(&home, &home);

    let workdir = tempdir();
    std::fs::write(workdir.join("AGENTS.md"), "# Bare Test\nBARE_APP_MARKER\n").unwrap();
    write_skill(
        &workdir.join(".hya/skills/unit-skill"),
        "unit-skill",
        "Bare runtime unit skill.",
        "Skill body.",
    );

    let bare_settings = || {
        let mut settings = config::EngineSettings::default();
        settings.set_builtin_bundles(hya_bundle::BuiltinBundleSelection::bare());
        settings
    };

    let rt = resolve_runtime(
        None,
        RuntimeFlags {
            bare: true,
            pure: false,
        },
    )
    .await
    .unwrap();
    let resolved_agent = agent_base_for_runtime(&rt, None);
    assert_eq!(resolved_agent.name.as_str(), hya_engine::BARE_AGENT_ID);
    assert!(!resolved_agent.system_prompt.is_empty());
    assert!(!resolved_agent.system_prompt.contains("hya-main"));

    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider_bare = ToolViewProvider {
        inner: FakeProvider::scripted_turns(vec![vec![
            FakeStep::Text("BARE_UNIT_OK".into()),
            FakeStep::Finish(FinishReason::Stop),
        ]]),
        requests: Arc::clone(&requests),
    };
    let agent = AgentSpec {
        name: AgentName::new(hya_engine::BARE_AGENT_ID),
        model: ModelRef::new("fake/model"),
        system_prompt: String::new(),
        workdir: workdir.clone(),
        reasoning: None,
    };

    // 1. Bare mode (non-pure): 8 standard bare tools, `hya-bare` default agent,
    //    AGENTS.md + local skill discovery, zero builtin bundles.
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        ProviderRouter::new().with(Arc::new(provider_bare)),
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (InvocationPolicy::default(), bare_settings()),
    )
    .await
    .unwrap();

    let binding = built.engine().bind_runtime(&workdir).unwrap();
    let registry_names: std::collections::BTreeSet<String> = binding
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_owned())
        .collect();
    assert_eq!(
        registry_names,
        ["bash", "edit", "grep", "read", "skill", "write"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "todo/rewrite must remain kernel-exclusive and absent from ToolRegistry"
    );
    assert!(!registry_names.contains("todo"));
    assert!(!registry_names.contains("rewrite"));

    let session = built
        .engine()
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    built
        .engine()
        .admit_user_prompt(session, "hi bare".into())
        .await
        .unwrap();
    built
        .engine()
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    built.shutdown().await.unwrap();

    // 2. Bare + pure mode: skips AGENTS.md and external disk skills.
    let provider_pure = ToolViewProvider {
        inner: FakeProvider::scripted_turns(vec![vec![
            FakeStep::Text("BARE_PURE_UNIT_OK".into()),
            FakeStep::Finish(FinishReason::Stop),
        ]]),
        requests: Arc::clone(&requests),
    };
    let mut pure_built = build_session_engine_pure(
        SessionStore::connect_memory().await.unwrap(),
        ProviderRouter::new().with(Arc::new(provider_pure)),
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (InvocationPolicy::default(), bare_settings()),
    )
    .await
    .unwrap();
    let pure_session = pure_built
        .engine()
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    pure_built
        .engine()
        .admit_user_prompt(pure_session, "hi bare pure".into())
        .await
        .unwrap();
    pure_built
        .engine()
        .run_turn(pure_session, &agent, CancellationToken::new())
        .await
        .unwrap();
    pure_built.shutdown().await.unwrap();

    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 2);
    let expected_offered: std::collections::BTreeSet<String> = [
        "bash", "edit", "grep", "read", "rewrite", "skill", "todo", "write",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for req in captured.iter() {
        let offered: std::collections::BTreeSet<String> = req
            .tools
            .iter()
            .map(|t| t.name.as_str().to_owned())
            .collect();
        assert_eq!(offered, expected_offered);
    }
    let bare_sys = captured[0].system.as_deref().unwrap_or("");
    let pure_sys = captured[1].system.as_deref().unwrap_or("");
    assert!(bare_sys.contains("BARE_APP_MARKER"), "{bare_sys}");
    assert!(bare_sys.contains("unit-skill"), "{bare_sys}");
    assert!(!pure_sys.contains("BARE_APP_MARKER"), "{pure_sys}");
    assert!(!pure_sys.contains("unit-skill"), "{pure_sys}");
}

/// What a runtime built with `selection` exposes of `hya/agent-channels`:
/// started native service ids, its tools, steer hooks, and the tool result a
/// model `read channel://…` gets.
struct ChannelsSurface {
    services: Vec<String>,
    tools: Vec<String>,
    steer_hooks: usize,
    channel_read: String,
}

async fn channels_surface(selection: hya_bundle::BuiltinBundleSelection) -> ChannelsSurface {
    let workdir = tempdir();
    let agent_id = if core_agents_available(&selection) {
        BUILTIN_DEFAULT_AGENT_ID
    } else {
        hya_engine::BARE_AGENT_ID
    };
    let mut settings = config::EngineSettings::default();
    settings.set_builtin_bundles(selection);
    let agent = AgentSpec {
        name: AgentName::new(agent_id),
        model: ModelRef::new("fake/model"),
        system_prompt: String::new(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".into(),
                input: json!({ "path": "channel://DM-12345678" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        ProviderRouter::new().with(Arc::new(provider)),
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (InvocationPolicy::default(), settings),
    )
    .await
    .unwrap();
    let engine = built.engine();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "read the channel".into())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    let channel_read = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .find_map(|envelope| match envelope.event {
            Event::ToolResult { output, .. } => Some(output.to_string()),
            Event::ToolError { message_text, .. } => Some(message_text),
            _ => None,
        })
        .expect("read tool result");
    let surface = ChannelsSurface {
        services: built
            .native_services
            .iter()
            .map(|service| service.id().to_string())
            .collect(),
        tools: engine
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.to_string())
            .filter(|name| name == "send" || name == "list_channel")
            .collect(),
        steer_hooks: engine
            .process_hooks()
            .entries()
            .iter()
            .filter(|entry| entry.hook().id() == "unread-mail-steer")
            .count(),
        channel_read,
    };
    built.shutdown().await.unwrap();
    surface
}

#[tokio::test]
async fn channels_disabled_runtime_has_no_channels_service_tool_scheme_or_hook() {
    let home = tempdir();
    let _env = EnvGuard::with_home(&home, &home);
    let disabled = hya_bundle::BuiltinBundleSelection {
        disable: vec!["hya/agent-channels".into()],
        ..hya_bundle::BuiltinBundleSelection::full()
    };
    for selection in [hya_bundle::BuiltinBundleSelection::bare(), disabled] {
        let surface = channels_surface(selection.clone()).await;
        assert!(
            !surface.services.iter().any(|id| id == "agent-channels"),
            "{selection:?}: {:?}",
            surface.services
        );
        assert!(
            surface.tools.is_empty(),
            "{selection:?}: {:?}",
            surface.tools
        );
        assert_eq!(surface.steer_hooks, 0, "{selection:?}");
        assert!(
            surface
                .channel_read
                .contains("unknown handle scheme: channel://"),
            "{selection:?}: {}",
            surface.channel_read
        );
    }
}

#[tokio::test]
async fn channels_enabled_registers_exactly_one_channels_service_hook_and_scheme() {
    let home = tempdir();
    let _env = EnvGuard::with_home(&home, &home);
    let surface = channels_surface(hya_bundle::BuiltinBundleSelection::full()).await;
    assert_eq!(
        surface
            .services
            .iter()
            .filter(|id| *id == "agent-channels")
            .count(),
        1,
        "{:?}",
        surface.services
    );
    assert_eq!(surface.tools.len(), 2, "{:?}", surface.tools);
    assert_eq!(surface.steer_hooks, 1);
    assert!(
        !surface.channel_read.contains("unknown handle scheme"),
        "{}",
        surface.channel_read
    );
}

#[tokio::test]
async fn installed_bundle_missing_a_prerequisite_is_left_out_of_the_catalog() {
    let home = tempdir();
    let workdir = tempdir();
    let _env = EnvGuard::with_home(&home, &workdir);
    let registry_path = home.join("hya/bundles/registry.sqlite3");
    std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    for (index, (id, body)) in [
        ("acme/needs", "requires: [acme/absent]\n"),
        ("acme/plain", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let name = id.trim_start_matches("acme/");
        let installed = prepare_package(BundleSource::new(
            name,
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nschema_version: 2\nidentity: {{ id: {id}, version: 1.0.0, publisher: acme }}\n{body}agents:\n  - {{ id: {name}-agent, role: main }}\n"
                ),
            )],
        ))
        .unwrap();
        registry
            .install(
                &[],
                NamespaceInstallPolicy::DenyConflicts,
                BundleInstallCandidate {
                    source_digest: [0x60 + index as u8; 32],
                    prepared_digest: installed.digest().to_string(),
                    prepared_bytes: installed.bytes().to_vec(),
                    installed_at: 1_725_000_030 + index as i64,
                },
            )
            .await
            .unwrap();
    }

    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let binding = built.engine().bind_root_runtime(&workdir).await.unwrap();
    assert!(binding.resolve_agent("plain-agent").is_some());
    assert!(binding.resolve_agent("needs-agent").is_none());
    built.shutdown().await.unwrap();
}
