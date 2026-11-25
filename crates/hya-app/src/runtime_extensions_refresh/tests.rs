use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::BundleCatalog;
use hya_engine::{AgentCatalog, RuntimeRegistry};
use hya_mcp::McpServerConfig;
use hya_proto::AgentName;
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
        std::fs::write(root.join("config.yaml"), "plugins: {}\n")?;
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
            applied: tokio::sync::Mutex::new(Some("plugins: {}\n".into())),
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
        let plugins = ids.iter().map(|id| (id.to_string(), json!({"kind":"rust", "command":["python3", self.root.join(format!("{id}.py"))], "timeout_ms":1000}))).collect::<BTreeMap<_,_>>();
        std::fs::write(
            self.root.join("config.yaml"),
            serde_json::to_string(&json!({"plugins":plugins}))?,
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
"#.replace("ID", id)
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
async fn hot_reload_failed_config_does_not_block_or_repeat_on_bind() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    let h = RefreshHarness::new().await?;
    h.script("bad", false)?;
    h.script("good", true)?;
    h.config(&["bad", "good"])?;
    let binding = h.bind().await?;
    assert!(binding.resolve_tool("good__echo").is_some());
    for _ in 0..3 {
        h.bind().await?;
    }
    assert_eq!(h.starts("bad"), 1);
    std::fs::write(h.root.join("config.yaml"), "plugins: [invalid")?;
    assert!(h.bind().await?.resolve_tool("good__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn hot_reload_explicit_refresh_retries_identical_failed_config() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    let h = RefreshHarness::new().await?;
    h.script("retry", false)?;
    h.config(&["retry"])?;
    // RuntimeExtensionsRefresh snapshots the file before startup reconciliation.
    *h.refresh.applied.lock().await = super::read_plugin_config(&h.root.join("config.yaml"))?;
    let specs =
        crate::config::parse_plugin_config(&std::fs::read_to_string(h.root.join("config.yaml"))?)?;
    let plan = h.reconciler.replace_desired(
        specs
            .into_iter()
            .map(|spec| DesiredSource::plugin(SourceId::plugin(spec.id.clone()), spec))
            .collect(),
    )?;
    assert!(
        h.reconciler
            .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(h.starts("retry"), 1);
    h.script("retry", true)?;
    let refreshed = h
        .engine
        .refresh_bundles(
            &hya_engine::CatalogScope::Directory(h.root.clone()),
            &h.root,
        )
        .await?;
    assert!(refreshed.errors.is_empty(), "{:?}", refreshed.errors);
    assert!(refreshed.binding.resolve_tool("retry__echo").is_some());
    assert_eq!(h.starts("retry"), 2);
    Ok(())
}

#[tokio::test]
async fn hot_reload_config_edit_does_not_retry_unrelated_failed_mcp() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    use hya_server::McpControl as _;
    let h = RefreshHarness::new().await?;
    let log = h.root.join("mcp.starts");
    RuntimeMcpControl::new(h.reconciler.clone())
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
    std::fs::write(
        h.root.join("config.yaml"),
        "plugins: {}\n# unrelated provider edit\n",
    )?;
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
    assert_eq!(
        std::fs::read_to_string(&log)?.lines().count(),
        2,
        "a malformed plugin config must not block explicit retry of the known MCP declaration"
    );
    Ok(())
}

#[tokio::test]
async fn hot_reload_connect_compares_mcp_defaults_semantically() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    use hya_server::McpControl as _;
    let h = RefreshHarness::new().await?;
    let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 result={'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'test','version':'1'}} if r['method']=='initialize' else {'tools':[]} if r['method']=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let control = RuntimeMcpControl::new(h.reconciler.clone());
    let config = McpServerConfig {
        command: vec!["python3".into(), "-c".into(), script.into()],
        ..Default::default()
    };
    control
        .upsert("test".into(), config.clone())
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        control.status().await["test"],
        hya_mcp::McpStatus::Connected
    );
    let generation = h.engine.runtime_registry().effective_manifest().generation;
    control
        .set_enabled("test".into(), true)
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        h.engine.runtime_registry().effective_manifest().generation,
        generation
    );
    control
        .upsert(
            "test".into(),
            McpServerConfig {
                enabled: Some(true),
                env: Some(BTreeMap::new()),
                transport: Some("stdio".into()),
                timeout_ms: Some(30_000),
                ..config
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(
        h.engine.runtime_registry().effective_manifest().generation,
        generation
    );
    Ok(())
}

#[tokio::test]
async fn hot_reload_global_events_survive_scope_eviction() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    let h = RefreshHarness::new().await?;
    let events = h.root.join("events.log");
    let script = format!(
        r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 with open({}, 'a') as f: f.write(json.dumps(r)+'\n')
 if 'id' in r:
  result={{'protocol_version':1,'plugin':{{'id':'global','version':'1.0.0','kind':'rust'}},'hooks':[{{'name':'event'}},{{'name':'session.start'}},{{'name':'session.end'}}]}} if r.get('method')=='initialize' else {{}}
  print(json.dumps({{'jsonrpc':'2.0','id':r['id'],'result':result}}),flush=True)
"#,
        serde_json::to_string(&events)?
    );
    std::fs::write(h.root.join("global.py"), script)?;
    h.config(&["global"])?;
    h.bind().await?;
    let session = h
        .engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("hya/offline"),
            workdir: h.root.display().to_string(),
            project: None,
            kind: hya_proto::SessionKind::Temporary,
        })
        .await?;
    h.engine
        .set_catalog_scope_cache_config(hya_engine::CatalogScopeCacheConfig {
            max_scopes: 0,
            idle_ttl: std::time::Duration::ZERO,
        });
    h.engine.sweep_catalog_scopes();
    h.engine.set_title(session, "after eviction".into()).await?;
    h.engine.archive_session(session).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let log = std::fs::read_to_string(&events).unwrap_or_default();
            if log.contains("session.end") && log.contains("after eviction") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    let log = std::fs::read_to_string(events)?;
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("session_created"))
            .count(),
        1,
        "SessionCreated must reach global event hooks exactly once: {log}"
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("hook/session.start"))
            .count(),
        1
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("hook/session.end"))
            .count(),
        1
    );
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

async fn assert_global_events_with_child_sidecar(resident: bool) -> anyhow::Result<()> {
    use hya_engine::{AgentSpec, CreateSession, ResidentSupervisor, SessionEngine};
    use hya_proto::{Event, FinishCause, ModelRef, Role, SessionKind};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let h = RefreshHarness::new().await?;
    let events = h.root.join("events.log");
    std::fs::write(
        h.root.join("global.py"),
        format!(
            r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method')=='event':
  with open({}, 'a') as f: f.write(json.dumps(r['params']['envelope'])+'\n')
 if 'id' in r:
  result={{'protocol_version':1,'plugin':{{'id':'global','version':'1.0.0','kind':'rust'}},'hooks':[{{'name':'event'}}]}} if r.get('method')=='initialize' else {{}}
  print(json.dumps({{'jsonrpc':'2.0','id':r['id'],'result':result}}),flush=True)
"#,
            serde_json::to_string(&events)?
        ),
    )?;
    h.config(&["global"])?;
    h.bind().await?;
    let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "child",
        vec![hya_bundle::SourceFile::new(
            "bundle.hya.md",
            "---\nkind: Bundle\nidentity: { id: acme/child, version: 1.0.0, publisher: acme }\nagents: [{ id: hooked-child, role: subagent }]\n---\nChild prompt.\n",
        )],
    ))?;
    let runtime = h.engine.runtime_registry().clone();
    let catalog = Arc::new(AgentCatalog::new(Arc::new(
        BundleCatalog::from_verified_catalogs(&[&prepared])?,
    ))?);
    runtime.refresh(|candidate| {
        candidate.replace_catalog(catalog);
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
    let child_agent = AgentSpec {
        name: AgentName::new("hooked-child"),
        ..root_agent
    };
    let binding = engine.bind_runtime(&h.root)?;
    let agents = engine.agent_roster_for_binding(&binding, "hooked-child")?;
    let resources = engine.agent_resource_policy_for_binding(&binding, "hooked-child")?;
    let child = if resident {
        let supervisor = ResidentSupervisor::start(engine.clone());
        let (child, handle) = supervisor
            .spawn_resident(
                root,
                child_agent,
                (binding, agents, resources, Some(Arc::new(HookedSidecar))),
                "child prompt".into(),
                None,
                None,
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let projection = engine.read_projection(child).await?;
                if projection
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
        engine
            .mail_send(
                root,
                hya_proto::MailEndpoint::Handle(handle),
                hya_proto::MailKind::Message,
                "next child prompt".into(),
            )
            .await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let projection = engine.read_projection(child).await?;
                if projection
                    .session
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::Assistant)
                    .count()
                    >= 2
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
        child
    } else {
        crate::resident_fixture::run_residents(
            engine.clone(),
            root,
            vec![crate::resident_fixture::ResidentTurn {
                agent: child_agent,
                binding,
                agents,
                resources: Some(resources),
                guidance: None,
                directive: "child prompt".into(),
                tool_call: None,
                description: String::new(),
                sidecar_factory: Some(Arc::new(HookedSidecar)),
            }],
            CancellationToken::new(),
        )
        .await[0]
    };
    // A final notification is a queue barrier for this plugin's event worker.
    engine.set_title(root, "event barrier".into()).await?;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if std::fs::read_to_string(&events)
                .unwrap_or_default()
                .contains("event barrier")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let delivered = std::fs::read_to_string(&events)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected = Vec::new();
    while let Ok(envelope) = live.try_recv() {
        expected.push(envelope);
    }
    assert!(
        expected
            .iter()
            .any(|e| matches!(e.event, Event::SessionCreated { session, .. } if session == root))
    );
    assert!(expected.iter().any(|e| matches!(e.event, Event::MessageStarted { session, role: Role::User, .. } if session == child)));
    assert!(expected.iter().any(|e| matches!(e.event, Event::MessageStarted { session, role: Role::Assistant, .. } if session == child)));
    for envelope in expected {
        let value = serde_json::to_value(&envelope)?;
        assert_eq!(
            delivered.iter().filter(|e| **e == value).count(),
            1,
            "every envelope exactly once: {value}; delivered={delivered:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn hot_reload_global_events_include_subagent_sidecar_preturn() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    assert_global_events_with_child_sidecar(false).await
}

#[tokio::test]
async fn hot_reload_global_events_include_resident_sidecar_preturn() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    assert_global_events_with_child_sidecar(true).await
}

#[tokio::test]
async fn hot_reload_removed_global_plugin_stops_despite_idle_sessions() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    let h = RefreshHarness::new().await?;
    h.script("global", true)?;
    h.config(&["global"])?;
    h.bind().await?;
    let bundle = h.root.join(".hya/bundles/local");
    std::fs::create_dir_all(&bundle)?;
    std::fs::write(
        bundle.join("bundle.hya.md"),
        "---\nkind: Bundle\nidentity: { id: acme/local, version: 1.0.0, publisher: test }\nagents: [{ id: local, role: main }]\n---\nLocal.\n",
    )?;
    let project = h
        .engine
        .store()
        .create_project("idle", &[h.root.display().to_string()])
        .await?;
    let session = h
        .engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("hya/offline"),
            workdir: h.root.display().to_string(),
            project: Some(project.id),
            kind: hya_proto::SessionKind::Project,
        })
        .await?;
    let retained = h.engine.bind_session_runtime(session, &h.root).await?;
    assert!(
        h.engine
            .runtime_registry()
            .scope_overlay(&hya_engine::ScopeKey::Project(project.id))
            .is_some()
    );
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
    assert!(
        h.engine
            .runtime_registry()
            .scope_in_use(&hya_engine::ScopeKey::Project(project.id))
    );
    drop(retained);
    assert!(
        !h.engine
            .runtime_registry()
            .scope_in_use(&hya_engine::ScopeKey::Project(project.id))
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid])
                .output()?;
            let state = String::from_utf8_lossy(&output.stdout);
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok::<_, std::io::Error>(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn hot_reload_http_refresh_reports_partial_publication_and_retries() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    use axum::body::Body;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    let h = RefreshHarness::new().await?;
    h.script("bad", false)?;
    h.script("good", true)?;
    h.config(&["bad", "good"])?;
    let engine = Arc::new(h.engine.clone());
    let (_, model) = crate::runtime::offline_router(None);
    let state = hya_server::AppState::new(
        engine,
        Arc::new(crate::runtime::agent_with_model(&model, None)),
    );
    let mut notices = state.subscribe_catalog_updates();
    let app = hya_server::router(state);
    let request = || {
        axum::http::Request::post("/v1/bundles:refresh")
            .header("content-type", "application/json")
            .body(Body::from("{\"directory\":\"\"}"))
    };
    let response = app.clone().oneshot(request()?).await?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body: Value = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
    assert!(
        body["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty()),
        "{body}"
    );
    let generation: u64 = serde_json::from_value::<String>(body["generation"].clone())?.parse()?;
    assert!(generation > 1, "{body}");
    assert!(
        notices.try_recv().is_ok(),
        "partial publication must notify catalog clients"
    );
    assert!(h.bind().await?.resolve_tool("good__echo").is_some());
    assert_eq!(h.starts("bad"), 1);
    h.script("bad", true)?;
    let response = app.oneshot(request()?).await?;
    let body: Value = serde_json::from_slice(&response.into_body().collect().await?.to_bytes())?;
    assert!(
        body.get("errors").is_none(),
        "protojson omits empty errors: {body}"
    );
    assert!(h.bind().await?.resolve_tool("bad__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn hot_reload_enabling_disabled_mcp_resolves_its_secret() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    use hya_server::McpControl as _;
    let h = RefreshHarness::new().await?;
    let script = r#"import json,os,sys
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 result={'protocolVersion':'2024-11-05','capabilities':{},'serverInfo':{'name':'test','version':'1'}} if r['method']=='initialize' else {'tools':[{'name':'echo','description':os.environ['TOKEN'],'inputSchema':{'type':'object'}}]} if r['method']=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let control = RuntimeMcpControl::new(h.reconciler.clone());
    control
        .upsert(
            "secret".into(),
            McpServerConfig {
                enabled: Some(false),
                command: vec!["python3".into(), "-c".into(), script.into()],
                env: Some(BTreeMap::from([("TOKEN".into(), "{env:PATH}".into())])),
                ..Default::default()
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    control
        .set_enabled("secret".into(), true)
        .await
        .map_err(anyhow::Error::msg)?;
    let binding = h.bind().await?;
    let tool = binding
        .resolve_tool("mcp__secret__echo")
        .ok_or_else(|| anyhow::anyhow!("connected MCP tool must publish"))?;
    assert_eq!(tool.tool.schema().description, std::env::var("PATH")?);
    Ok(())
}
