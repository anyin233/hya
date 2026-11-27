use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::BundleCatalog;
use hya_engine::{AgentCatalog, RuntimeRegistry};
use hya_mcp::McpServerConfig;
use hya_proto::AgentName;
use hya_server::McpControl as _;
use serde_json::{Value, json};

use crate::runtime_mcp_control::RuntimeMcpControl;
use crate::runtime_reconcile::{DesiredSource, RuntimeReconciler, SourceId};
use crate::test_env::StableEnvGuard;

struct RefreshHarness {
    root: std::path::PathBuf,
    engine: hya_engine::SessionEngine,
    reconciler: Arc<RuntimeReconciler>,
    refresh: Arc<super::RuntimeExtensionsRefresh>,
}

impl RefreshHarness {
    async fn new() -> anyhow::Result<Self> {
        let root =
            std::env::temp_dir().join(format!("hya-refresh-{}", hya_proto::SessionId::new()));
        std::fs::create_dir_all(&root)?;
        std::fs::write(root.join("config.yaml"), "schema_version: 2\nplugins: {}\n")?;
        let registry = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            crate::runtime::builtin_agent_catalog()?,
        ));
        let reconciler = Arc::new(RuntimeReconciler::new(registry.clone()));
        let refresh = Arc::new(super::RuntimeExtensionsRefresh {
            catalogs: crate::ProjectScopeRefresh::new(Arc::new(
                crate::InstalledBundleRefresh::new(root.join("registry.db")),
            )),
            reconciler: reconciler.clone(),
            config_file: root.join("config.yaml"),
            applied: tokio::sync::Mutex::new(Some("schema_version: 2\nplugins: {}\n".into())),
            pure: false,
        });
        let (permission, _) = hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
        let engine = hya_engine::SessionEngine::new(
            hya_store::SessionStore::connect_memory().await?,
            Arc::new(hya_provider::ProviderRouter::new()),
            registry,
            permission,
            hya_engine::EventBus::default(),
        )
        .with_catalog_refresh(refresh.clone());
        Ok(Self {
            root,
            engine,
            reconciler,
            refresh,
        })
    }

    fn config(&self, ids: &[&str]) -> anyhow::Result<()> {
        let plugins: BTreeMap<_, _> = ids
            .iter()
            .map(|id| {
                (
                    (*id).to_string(),
                    json!({"kind": "rust", "command": ["python3", self.root.join(format!("{id}.py"))], "timeout_ms": 1000}),
                )
            })
            .collect();
        std::fs::write(
            self.root.join("config.yaml"),
            serde_json::to_string(&json!({"schema_version": 2, "plugins": plugins}))?,
        )?;
        Ok(())
    }

    fn script(&self, id: &str, works: bool) -> anyhow::Result<()> {
        let log = serde_json::to_string(&self.root.join(format!("{id}.starts")))?;
        let body = if works {
            r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' in r:
  result={'protocol_version':1,'plugin':{'id':'ID','version':'1.0.0','kind':'rust'},'hooks':[{'name':'message.user.before'}],'tools':[{'name':'echo','inputSchema':{'type':'object'}}]} if r.get('method')=='initialize' else {}
  print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#
            .replace("ID", id)
        } else {
            "raise SystemExit(1)\n".into()
        };
        std::fs::write(
            self.root.join(format!("{id}.py")),
            format!(
                "import os\nwith open({log}, 'a') as f: f.write(str(os.getpid())+'\\n')\n{body}"
            ),
        )?;
        Ok(())
    }

    fn starts(&self, id: &str) -> usize {
        std::fs::read_to_string(self.root.join(format!("{id}.starts")))
            .unwrap_or_default()
            .lines()
            .count()
    }

    async fn bind(&self) -> anyhow::Result<hya_engine::TurnBinding> {
        Ok(self
            .engine
            .bind_scope_runtime(
                &hya_engine::CatalogScope::Directory(self.root.clone()),
                &self.root,
            )
            .await?)
    }
}

impl Drop for RefreshHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn failed_config_does_not_repeat_on_bind_and_explicit_refresh_retries() -> anyhow::Result<()>
{
    let _env = StableEnvGuard::new();
    let h = RefreshHarness::new().await?;
    h.script("bad", false)?;
    h.script("good", true)?;
    h.config(&["bad", "good"])?;
    assert!(h.bind().await?.resolve_tool("good__echo").is_some());
    for _ in 0..2 {
        h.bind().await?;
    }
    assert_eq!(h.starts("bad"), 1);

    // Malformed config keeps prior good plugin alive; fixing bad + explicit refresh retries it.
    std::fs::write(h.root.join("config.yaml"), "plugins: [invalid")?;
    assert!(h.bind().await?.resolve_tool("good__echo").is_some());

    // Repair config & script; explicit refresh retries previously failed plugin even without config edit.
    h.config(&["bad", "good"])?;
    *h.refresh.applied.lock().await = super::read_plugin_config(&h.root.join("config.yaml"))?;
    let specs =
        crate::config::parse_plugin_config(&std::fs::read_to_string(h.root.join("config.yaml"))?)?;
    let _ = h
        .reconciler
        .reconcile_plan(
            h.reconciler.replace_desired(
                specs
                    .into_iter()
                    .map(|s| DesiredSource::plugin(SourceId::plugin(s.id.clone()), s))
                    .collect(),
            )?,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    h.script("bad", true)?;
    let refreshed = h
        .engine
        .refresh_bundles(
            &hya_engine::CatalogScope::Directory(h.root.clone()),
            &h.root,
        )
        .await?;
    assert!(refreshed.errors.is_empty(), "{:?}", refreshed.errors);
    assert!(refreshed.binding.resolve_tool("bad__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_semantic_defaults_secret_resolution_and_plugin_config_isolation() -> anyhow::Result<()>
{
    let _env = StableEnvGuard::new();
    let h = RefreshHarness::new().await?;
    let control = RuntimeMcpControl::new(h.reconciler.clone());

    // 1. Semantic comparison avoids generation bumps; enabling disabled MCP resolves {env:PATH}.
    let script = r#"import json,os,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 result={'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'test','version':'1'}} if r['method']=='initialize' else {'tools':[{'name':'echo','description':os.environ['TOKEN'],'inputSchema':{'type':'object'}}]} if r['method']=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let cfg = McpServerConfig {
        enabled: Some(false),
        command: vec!["python3".into(), "-c".into(), script.into()],
        env: Some(BTreeMap::from([("TOKEN".into(), "{env:PATH}".into())])),
        ..Default::default()
    };
    control
        .upsert("secret".into(), cfg.clone())
        .await
        .map_err(anyhow::Error::msg)?;
    control
        .set_enabled("secret".into(), true)
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        control.status().await["secret"],
        hya_mcp::McpStatus::Connected
    );
    assert_eq!(
        h.bind()
            .await?
            .resolve_tool("mcp__secret__echo")
            .ok_or_else(|| anyhow::anyhow!("missing mcp__secret__echo"))?
            .tool
            .schema()
            .description,
        std::env::var("PATH")?
    );
    let gen_before = h.engine.runtime_registry().effective_manifest().generation;
    control
        .set_enabled("secret".into(), true)
        .await
        .map_err(anyhow::Error::msg)?;
    control
        .upsert(
            "secret".into(),
            McpServerConfig {
                enabled: Some(true),
                transport: Some("stdio".into()),
                timeout_ms: Some(30_000),
                ..cfg
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        h.engine.runtime_registry().effective_manifest().generation,
        gen_before
    );

    // 2. Unrelated config edit does not retry failed MCP, while explicit refresh does even if plugin YAML is invalid.
    let log = h.root.join("mcp.starts");
    control
        .upsert(
            "bad".into(),
            McpServerConfig {
                command: vec![
                    "python3".into(),
                    "-c".into(),
                    format!(
                        "with open({}, 'a') as f: f.write('start\\n')",
                        serde_json::to_string(&log)?
                    ),
                ],
                ..Default::default()
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    std::fs::write(h.root.join("config.yaml"), "plugins: {}\n# comment\n")?;
    h.bind().await?;
    assert_eq!(std::fs::read_to_string(&log)?.lines().count(), 1);
    std::fs::write(h.root.join("config.yaml"), "plugins: [invalid")?;
    let refreshed = h
        .engine
        .refresh_bundles(
            &hya_engine::CatalogScope::Directory(h.root.clone()),
            &h.root,
        )
        .await?;
    assert!(!refreshed.errors.is_empty());
    assert_eq!(std::fs::read_to_string(&log)?.lines().count(), 2);
    Ok(())
}

struct HookedSidecar;
#[async_trait]
impl hya_engine::BoundSidecarFactory for HookedSidecar {
    async fn start(
        &self,
        _: hya_engine::SidecarStart,
    ) -> Result<Box<dyn hya_engine::SidecarHandle>, hya_engine::CoreError> {
        Ok(Box::new(HookedSidecar))
    }
}
#[async_trait]
impl hya_engine::SidecarHandle for HookedSidecar {
    async fn ready(&mut self) -> Result<(), hya_engine::CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), hya_engine::CoreError> {
        Ok(())
    }
    fn hook_dispatcher(&self) -> Option<Arc<dyn hya_engine::HookDispatcher>> {
        Some(Arc::new(hya_engine::hooks::NoopHookHost))
    }
}

#[tokio::test]
async fn global_events_survive_eviction_and_include_resident_sidecar_preturn() -> anyhow::Result<()>
{
    let _env = StableEnvGuard::new();
    use hya_engine::{AgentSpec, CreateSession, ResidentSupervisor, SessionEngine};
    use hya_proto::{Event, FinishCause, ModelRef, Role, SessionKind};
    use tokio_util::sync::CancellationToken;

    let h = RefreshHarness::new().await?;
    let events = h.root.join("events.log");
    std::fs::write(
        h.root.join("global.py"),
        format!(
            r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 with open({}, 'a') as f: f.write(json.dumps(r)+'\n')
 if 'id' in r:
  res={{'protocol_version':1,'plugin':{{'id':'global','version':'1.0.0','kind':'rust'}},'hooks':[{{'name':'event'}},{{'name':'session.start'}},{{'name':'session.end'}}]}} if r.get('method')=='initialize' else {{}}
  print(json.dumps({{'jsonrpc':'2.0','id':r['id'],'result':res}}),flush=True)
"#,
            serde_json::to_string(&events)?
        ),
    )?;
    h.config(&["global"])?;
    h.bind().await?;

    // 1. Events & lifecycle hooks survive catalog scope eviction.
    let s1 = h
        .engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("hya/offline"),
            workdir: h.root.display().to_string(),
            project: None,
            kind: SessionKind::Temporary,
        })
        .await?;
    h.engine
        .set_catalog_scope_cache_config(hya_engine::CatalogScopeCacheConfig {
            max_scopes: 0,
            idle_ttl: Duration::ZERO,
        });
    h.engine.sweep_catalog_scopes();
    h.engine.set_title(s1, "after eviction".into()).await?;
    h.engine.archive_session(s1).await?;

    // 2. Resident child with HookedSidecar still delivers every event envelope exactly once to global hooks.
    let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "child",
        vec![hya_bundle::SourceFile::new(
            "bundle.hya.md",
            "---\nkind: Bundle\nschema_version: 2\nidentity: { id: acme/child, version: 1.0.0, publisher: acme }\nagents: [{ id: hooked-child, role: subagent }]\n---\nChild.\n",
        )],
    ))?;
    let runtime = h.engine.runtime_registry().clone();
    let catalog = Arc::new(AgentCatalog::new(Arc::new(
        BundleCatalog::from_verified_catalogs(&[&prepared])?,
    ))?);
    runtime.refresh(|c| {
        c.replace_catalog(catalog);
        Ok(())
    })?;
    let (permission, _) = hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        hya_store::SessionStore::connect_memory().await?,
        Arc::new(
            hya_provider::ProviderRouter::new()
                .with(Arc::new(hya_provider::FakeProvider::scripted(Vec::new()))),
        ),
        runtime,
        permission,
        hya_engine::EventBus::default(),
    ));
    let mut live = engine.bus().subscribe();
    let root_agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: h.root.clone(),
        reasoning: None,
    };
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: root_agent.name.clone(),
            model: root_agent.model.clone(),
            workdir: h.root.display().to_string(),
            project: None,
            kind: SessionKind::Temporary,
        })
        .await?;
    engine.admit_user_prompt(root, "root prompt".into()).await?;
    engine
        .run_turn(root, &root_agent, CancellationToken::new())
        .await?;

    let binding = engine.bind_runtime(&h.root)?;
    let agents = engine.agent_roster_for_binding(&binding, "hooked-child")?;
    let resources = engine.agent_resource_policy_for_binding(&binding, "hooked-child")?;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, _) = supervisor
        .spawn_resident(
            root,
            AgentSpec {
                name: AgentName::new("hooked-child"),
                ..root_agent
            },
            (binding, agents, resources, Some(Arc::new(HookedSidecar))),
            "child prompt".into(),
            None,
            None,
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let proj = engine.read_projection(child).await?;
            if proj
                .session
                .messages
                .iter()
                .any(|m| m.role == Role::Assistant)
                && !engine.turn_active(child)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(2))
        .await;
    engine.set_title(root, "event barrier".into()).await?;

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let log = std::fs::read_to_string(&events).unwrap_or_default();
            if log.contains("session.end")
                && log.contains("after eviction")
                && log.contains("event barrier")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let lines: Vec<Value> = std::fs::read_to_string(&events)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let delivered: Vec<&Value> = lines
        .iter()
        .filter(|r| r.get("method").and_then(Value::as_str) == Some("event"))
        .filter_map(|r| r.pointer("/params/envelope"))
        .collect();
    let mut expected = Vec::new();
    while let Ok(env) = live.try_recv() {
        expected.push(env);
    }
    assert!(expected.iter().any(|e| matches!(e.event, Event::MessageStarted { session, role: Role::Assistant, .. } if session == child)));
    for env in expected {
        let v = serde_json::to_value(&env)?;
        assert_eq!(delivered.iter().filter(|d| ***d == v).count(), 1, "{v}");
    }
    Ok(())
}

#[tokio::test]
async fn removed_global_plugin_stops_despite_idle_sessions() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    let h = RefreshHarness::new().await?;
    h.script("global", true)?;
    h.config(&["global"])?;
    h.bind().await?;
    let project = h
        .engine
        .store()
        .create_project("idle", &[h.root.display().to_string()])
        .await?;
    let session = h
        .engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("hya/offline"),
            workdir: h.root.display().to_string(),
            project: Some(project.id),
            kind: hya_proto::SessionKind::Project,
        })
        .await?;
    let retained = h.engine.bind_session_runtime(session, &h.root).await?;
    let pid = std::fs::read_to_string(h.root.join("global.starts"))?
        .trim()
        .to_string();
    h.config(&[])?;
    assert!(
        h.engine
            .bind_global_runtime()
            .await?
            .resolve_tool("global__echo")
            .is_none()
    );
    assert!(retained.resolve_tool("global__echo").is_some());
    drop(retained);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let out = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid])
                .output()?;
            let s = String::from_utf8_lossy(&out.stdout);
            if s.trim().is_empty() || s.trim().starts_with('Z') {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, std::io::Error>(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn http_refresh_reports_partial_publication_and_retries() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    use axum::body::Body;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    let h = RefreshHarness::new().await?;
    h.script("bad", false)?;
    h.script("good", true)?;
    h.config(&["bad", "good"])?;
    let (_, model) = crate::runtime::offline_router(None);
    let state = hya_server::AppState::new(
        Arc::new(h.engine.clone()),
        Arc::new(crate::runtime::agent_with_model(&model, None)),
    );
    let mut notices = state.subscribe_catalog_updates();
    let app = hya_server::router(state);
    let req = || {
        axum::http::Request::post("/v2/bundles:refresh")
            .header("content-type", "application/json")
            .body(Body::from("{\"directory\":\"\"}"))
    };
    let resp = app.clone().oneshot(req()?).await?;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body: Value = serde_json::from_slice(&resp.into_body().collect().await?.to_bytes())?;
    assert!(body["errors"].as_array().is_some_and(|e| !e.is_empty()));
    assert!(notices.try_recv().is_ok());
    assert!(h.bind().await?.resolve_tool("good__echo").is_some());

    h.script("bad", true)?;
    let resp = app.oneshot(req()?).await?;
    let body: Value = serde_json::from_slice(&resp.into_body().collect().await?.to_bytes())?;
    assert!(body.get("errors").is_none());
    assert!(h.bind().await?.resolve_tool("bad__echo").is_some());
    Ok(())
}
