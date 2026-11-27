//! Integration tests for `hya-app`: project plugins (`.hya/plugins/*/plugin.toml`)
//! loaded per registered Project from every Project root.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_app::{
    InvocationPolicy, WebSearchConfig, agent_with_model, build_session_engine, offline_router,
};
use hya_engine::hooks::{HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome};
use hya_engine::{
    CatalogScope, CatalogScopeCacheConfig, CreateSession, EventBus, HookChain, RuntimeRegistry,
    SessionEngine, TurnBinding,
};
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::PluginKindWire;
use hya_proto::{ProjectId, SessionId};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{Action, Decision, PermissionPlane, PermissionRules, Resource};

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "hya-project-plugins-{nanos}-{}-{id}-{suffix}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(&path).unwrap()
}

fn plugin_script(id: &str, origin: &str, log_starts: bool) -> String {
    r#"import json,os,sys
if 'LOG_STARTS' == 'yes':
 with open('plugin-starts.log','a') as log: log.write(str(os.getpid()) + '\n')
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); p=r.get('params') or {}
 if m == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'PLUGIN_ID','version':'1.0.0','kind':'rust'},
          'hooks':[{'name':'message.user.before'},{'name':'permission.ask'}],
          'tools':[{'name':'echo','description':'origin=ORIGIN;pid=' + str(os.getpid()) + ';cwd=' + os.getcwd(),'inputSchema':{'type':'object'}}]}
 elif m == 'hook/message.user.before':
  result={'outcome':'continue','text':'[PLUGIN_ID] ' + p.get('text','')}
 elif m == 'hook/permission.ask':
  result={'outcome':'reject','feedback':'PLUGIN_ID says no'}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#
    .replace("PLUGIN_ID", id)
    .replace("ORIGIN", origin)
    .replace("LOG_STARTS", if log_starts { "yes" } else { "no" })
}

fn write_plugin(root: &Path, dir: &str, id: &str, origin: &str) {
    let plugin_dir = root.join(".hya/plugins").join(dir);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.py"),
        plugin_script(id, origin, true),
    )
    .unwrap();
    write_manifest(root, dir, id, "");
}

fn write_manifest(root: &Path, dir: &str, id: &str, extra: &str) {
    std::fs::write(
        root.join(".hya/plugins").join(dir).join("plugin.toml"),
        format!(
            "id = \"{id}\"\nkind = \"rust\"\ncommand = [\"python3\", \".hya/plugins/{dir}/plugin.py\"]\n{extra}"
        ),
    )
    .unwrap();
}

fn starts(root: &Path) -> Vec<u32> {
    std::fs::read_to_string(root.join("plugin-starts.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.parse().unwrap())
        .collect()
}

struct Echo {
    origin: String,
    pid: u32,
    cwd: PathBuf,
}

fn echo(binding: &TurnBinding, tool: &str) -> Echo {
    let desc = binding
        .resolve_tool(tool)
        .unwrap()
        .tool
        .schema()
        .description;
    let fields: BTreeMap<_, _> = desc.split(';').filter_map(|f| f.split_once('=')).collect();
    Echo {
        origin: fields["origin"].to_string(),
        pid: fields["pid"].parse().unwrap(),
        cwd: PathBuf::from(fields["cwd"]),
    }
}

fn running(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            let s = s.trim();
            o.status.success() && !s.is_empty() && !s.starts_with('Z')
        })
        .unwrap_or(false)
}

async fn wait_until_exited(pid: u32) -> bool {
    for _ in 0..200 {
        if !running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    false
}

struct Harness {
    engine: Arc<SessionEngine>,
    runtime: Arc<RuntimeRegistry>,
}

impl Harness {
    async fn new(state: &Path) -> Self {
        let runtime = Arc::new(RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            hya_app::builtin_agent_catalog().unwrap(),
        ));
        let installed = Arc::new(hya_app::InstalledBundleRefresh::new(
            state.join("registry.db"),
        ));
        let refresh = Arc::new(hya_app::ProjectScopeRefresh::new(installed));
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::new(ProviderRouter::new()),
                Arc::clone(&runtime),
                permission,
                EventBus::default(),
            )
            .with_catalog_refresh(refresh),
        );
        Self { engine, runtime }
    }

    async fn bind(&self, scope: &CatalogScope) -> TurnBinding {
        let workdir = scope
            .roots()
            .first()
            .cloned()
            .unwrap_or_else(std::env::temp_dir);
        self.engine
            .bind_scope_runtime(scope, &workdir)
            .await
            .unwrap()
    }
}

fn project(roots: &[&Path]) -> CatalogScope {
    CatalogScope::Project {
        id: ProjectId::new(),
        roots: roots.iter().map(|r| r.to_path_buf()).collect(),
    }
}

async fn user_text(binding: &TurnBinding, session: SessionId, text: &str) -> String {
    let MessageUserBeforeOutcome::Continue { text } =
        HookChain::new(binding.bundle_hooks_for_agent("hya-main"))
            .message_user_before(MessageUserBeforeInput {
                session,
                text: text.to_string(),
            })
            .await;
    text
}

async fn permission(binding: &TurnBinding, session: SessionId) -> Option<Decision> {
    HookChain::new(binding.bundle_hooks_for_agent("hya-main"))
        .permission_ask(Some(session), Action::Bash, &Resource::Command("ls".into()))
        .await
}

#[tokio::test]
async fn project_plugins_isolation_hooks_first_root_and_config_precedence() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("iso-state");
    let first = temp_path("iso-first");
    let second = temp_path("iso-second");
    write_plugin(&first, "shared", "shared", "first");
    write_plugin(&second, "shared", "shared", "second");
    write_plugin(&second, "only", "only", "second");

    // 1. Lazy start, scope isolation, hooks, and first-root wins by plugin ID.
    let h = Harness::new(&state).await;
    let dir = h.bind(&CatalogScope::Directory(first.clone())).await;
    let global = h.engine.bind_global_runtime().await.unwrap();
    assert!(dir.resolve_tool("shared__echo").is_none());
    assert!(global.resolve_tool("shared__echo").is_none());
    assert!(starts(&first).is_empty() && starts(&second).is_empty());

    let multi = project(&[&first, &second]);
    let bound = h.bind(&multi).await;
    let shared = echo(&bound, "shared__echo");
    let only = echo(&bound, "only__echo");
    assert_eq!(shared.origin, "first");
    assert_eq!(shared.cwd, first);
    assert_eq!(only.cwd, second);
    assert_eq!(starts(&first), vec![shared.pid]);
    assert_eq!(starts(&second), vec![only.pid]);
    assert_eq!(echo(&h.bind(&multi).await, "shared__echo").pid, shared.pid);

    let session = SessionId::new();
    assert_eq!(user_text(&bound, session, "hi").await, "[shared] [only] hi");
    assert!(matches!(
        permission(&bound, session).await,
        Some(Decision::Reject { feedback: Some(ref f) }) if f.ends_with("says no")
    ));
    assert_eq!(user_text(&global, session, "hi").await, "hi");
    assert_eq!(permission(&global, session).await, None);
    drop(bound);

    // 2. Process-wide config.yaml plugin wins over same-ID project manifest and chains hooks.
    let cfg_root = temp_path("cfg-root");
    let other_root = temp_path("cfg-other");
    write_plugin(&cfg_root, "alpha", "alpha", "project");
    write_plugin(&cfg_root, "configured", "configured", "project");
    let alpha_py = cfg_root.join(".hya/plugins/alpha/plugin.py");
    let patched = std::fs::read_to_string(&alpha_py).unwrap().replace(
        "{'outcome':'reject','feedback':'alpha says no'}",
        "{'outcome':'allow_once'}",
    );
    std::fs::write(alpha_py, patched).unwrap();

    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        vec![PluginSpec {
            id: "configured".into(),
            kind: PluginKindWire::Rust,
            command: vec![
                "python3".into(),
                "-c".into(),
                plugin_script("configured", "config", false),
            ],
            timeout_ms: Some(5_000),
            env: BTreeMap::new(),
            plugin_dir: None,
            launch: Default::default(),
        }],
        (
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            hya_app::config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    let with_p = engine
        .store()
        .create_project("with", &[cfg_root.display().to_string()])
        .await
        .unwrap();
    let without_p = engine
        .store()
        .create_project("without", &[other_root.display().to_string()])
        .await
        .unwrap();
    let inside = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: cfg_root.display().to_string(),
            project: Some(with_p.id),
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let outside = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: other_root.display().to_string(),
            project: Some(without_p.id),
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let b_in = engine
        .bind_session_runtime(inside, &cfg_root)
        .await
        .unwrap();
    assert_eq!(echo(&b_in, "alpha__echo").origin, "project");
    assert_eq!(echo(&b_in, "configured__echo").origin, "config");
    assert_eq!(starts(&cfg_root).len(), 1);
    assert_eq!(
        user_text(&b_in, inside, "hi").await,
        "[alpha] [configured] hi"
    );
    assert!(matches!(
        permission(&b_in, inside).await,
        Some(Decision::Reject { feedback: Some(ref f) }) if f == "configured says no"
    ));
    let b_out = engine
        .bind_session_runtime(outside, &other_root)
        .await
        .unwrap();
    assert!(b_out.resolve_tool("alpha__echo").is_none());
    assert_eq!(user_text(&b_out, outside, "hi").await, "[configured] hi");
    drop((b_in, b_out));
    built.shutdown().await.unwrap();
}

const LIFECYCLE_SCRIPT: &str = r#"import json,os,sys
def log(line):
 with open('hooks.log','a') as out: out.write(str(os.getpid()) + ' ' + line + '\n')
for line in sys.stdin:
 r=json.loads(line); m=r.get('method'); p=r.get('params') or {}
 if m == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'watcher','version':'1.0.0','kind':'rust'},
          'hooks':[{'name':'session.start'},{'name':'session.end'},{'name':'event'}],
          'tools':[{'name':'echo','description':'origin=watcher;pid=' + str(os.getpid()) + ';cwd=' + os.getcwd(),'inputSchema':{'type':'object'}}]}
 elif m == 'hook/session.start': log('start ' + str(p.get('session'))); result={}
 elif m == 'hook/session.end': log('end ' + str(p.get('session'))); result={}
 elif m == 'event': log('event ' + json.dumps(p.get('envelope'))); result={}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;

fn hook_log(root: &Path) -> Vec<(u32, String)> {
    std::fs::read_to_string(root.join("hooks.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let (pid, rest) = line.split_once(' ').unwrap();
            (pid.parse().unwrap(), rest.to_string())
        })
        .collect()
}

async fn event_pids(root: &Path, marker: &str) -> Vec<u32> {
    for _ in 0..200 {
        let pids: Vec<_> = hook_log(root)
            .into_iter()
            .filter(|(_, line)| line.starts_with("event ") && line.contains(marker))
            .map(|(pid, _)| pid)
            .collect();
        if !pids.is_empty() {
            return pids;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    Vec::new()
}

#[tokio::test]
async fn plugin_respawn_session_hooks_invalidation_and_eviction() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("life-state");
    let root = temp_path("life-follow");
    let root_evict = temp_path("life-evict");
    let plugin_dir = root.join(".hya/plugins/watcher");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.py"), LIFECYCLE_SCRIPT).unwrap();
    write_manifest(&root, "watcher", "watcher", "");
    write_plugin(&root_evict, "beta", "beta", "b");

    let h = Harness::new(&state).await;
    let project_id = h
        .engine
        .store()
        .create_project("follow", &[root.display().to_string()])
        .await
        .unwrap()
        .id;
    let session = h
        .engine
        .create(CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("fake"),
            workdir: root.display().to_string(),
            project: Some(project_id),
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let scope = CatalogScope::Project {
        id: project_id,
        roots: vec![root.clone()],
    };

    let before = echo(&h.bind(&scope).await, "watcher__echo").pid;
    // Bundle refresh does not respawn unchanged plugins.
    let bdir = root.join(".hya/bundles/tools");
    std::fs::create_dir_all(&bdir).unwrap();
    std::fs::write(
        bdir.join("bundle.hya.md"),
        "---\nkind: Bundle\nschema_version: 2\nidentity: { id: hya/tools, version: 1.0.0, publisher: hya }\nagents: [{ id: tools-agent, role: main }]\n---\nTools.\n",
    )
    .unwrap();
    assert_eq!(
        echo(
            &h.engine
                .refresh_bundles(&scope, &root)
                .await
                .unwrap()
                .binding,
            "watcher__echo"
        )
        .pid,
        before
    );
    h.engine
        .admit_user_prompt(session, "marker-before".into())
        .await
        .unwrap();
    assert_eq!(event_pids(&root, "marker-before").await[0], before);

    // Editing plugin.toml respawns on next session bind and routes new events only to the new process.
    write_manifest(&root, "watcher", "watcher", "timeout_ms = 4000\n");
    h.engine
        .admit_user_prompt(session, "marker-after".into())
        .await
        .unwrap();
    let after = echo(&h.bind(&scope).await, "watcher__echo").pid;
    assert_ne!(after, before);
    assert!(wait_until_exited(before).await);
    let pids = event_pids(&root, "marker-after").await;
    assert!(!pids.is_empty() && pids.iter().all(|&p| p == after));

    // Invalidation stops the idle session's plugin immediately.
    h.engine.invalidate_catalog_scope(project_id);
    assert!(wait_until_exited(after).await);

    // Cache eviction stops idle project plugins as well.
    let evict_scope = project(&[&root_evict]);
    let b = h.bind(&evict_scope).await;
    let beta = echo(&b, "beta__echo").pid;
    drop(b);
    h.engine
        .set_catalog_scope_cache_config(CatalogScopeCacheConfig {
            max_scopes: 32,
            idle_ttl: Duration::ZERO,
        });
    h.engine.sweep_catalog_scopes();
    assert!(h.runtime.scope_overlay(&evict_scope.key()).is_none());
    assert!(wait_until_exited(beta).await);
}

#[tokio::test]
async fn hot_reload_failed_project_plugin_keeps_source_and_explicit_refresh_retries() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("failed-state");
    let root = temp_path("failed-plugin");
    write_plugin(&root, "alpha", "alpha", "original");
    write_plugin(&root, "beta", "beta", "original");
    let h = Harness::new(&state).await;
    let scope = project(&[&root]);
    let first = h.bind(&scope).await;
    let original = echo(&first, "alpha__echo").pid;
    drop(first);

    std::fs::write(root.join(".hya/plugins/alpha/plugin.toml"), "id = [invalid").unwrap();
    write_manifest(&root, "beta", "beta", "timeout_ms = 4500");
    let binding = h.bind(&scope).await;
    assert_eq!(echo(&binding, "alpha__echo").pid, original);
    assert!(binding.resolve_tool("beta__echo").is_some());
    drop(binding);

    write_manifest(&root, "alpha", "alpha", "timeout_ms = 900");
    std::fs::write(
        root.join(".hya/plugins/alpha/plugin.py"),
        "raise SystemExit(1)",
    )
    .unwrap();
    assert_eq!(echo(&h.bind(&scope).await, "alpha__echo").pid, original);

    std::fs::write(
        root.join(".hya/plugins/alpha/plugin.py"),
        plugin_script("alpha", "repaired", true),
    )
    .unwrap();
    let result = h.engine.refresh_bundles(&scope, &root).await.unwrap();
    assert!(result.changed);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(echo(&result.binding, "alpha__echo").origin, "repaired");
}
