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
use hya_core::hooks::{HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome};
use hya_core::{
    CatalogScope, CatalogScopeCacheConfig, CreateSession, EventBus, HookChain, RuntimeRegistry,
    SessionEngine, TurnBinding,
};
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::PluginKindWire;
use hya_proto::{ProjectId, SessionId};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{Action, Decision, PermissionPlane, PermissionRules, Resource, ToolRegistry};

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch");
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "hya-project-plugins-{}-{}-{id}-{suffix}",
        elapsed.as_nanos(),
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    // Plugins report their cwd canonicalized (macOS /var -> /private/var).
    std::fs::canonicalize(&path).expect("canonicalize temp dir")
}

/// A plugin whose `echo` tool description reports `origin=<origin>;pid=<pid>;cwd=<cwd>`,
/// that (when `log_starts`) appends its pid to `plugin-starts.log` in its cwd
/// when it starts,
/// prefixes user text with `[<id>]` (`message.user.before`), and rejects every
/// `permission.ask`.
fn plugin_script(id: &str, origin: &str, log_starts: bool) -> String {
    r#"import json,os,sys
if 'LOG_STARTS' == 'yes':
 with open('plugin-starts.log','a') as log: log.write(str(os.getpid()) + '\n')
for line in sys.stdin:
 r=json.loads(line)
 m=r.get('method')
 p=r.get('params') or {}
 if m == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'PLUGIN_ID','version':'1.0.0','kind':'rust'},'hooks':[{'name':'message.user.before'},{'name':'permission.ask'}],'tools':[{'name':'echo','description':'origin=ORIGIN;pid=' + str(os.getpid()) + ';cwd=' + os.getcwd(),'inputSchema':{'type':'object'}}]}
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

/// Write `<root>/.hya/plugins/<dir>/{plugin.toml,plugin.py}` with a command
/// relative to the root.
fn write_plugin(root: &Path, dir: &str, id: &str, origin: &str) {
    let plugin_dir = root.join(".hya/plugins").join(dir);
    std::fs::create_dir_all(&plugin_dir).expect("create plugin dir");
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
        .map(|line| line.parse().unwrap())
        .collect()
}

struct Echo {
    origin: String,
    pid: u32,
    cwd: PathBuf,
}

fn echo(binding: &TurnBinding, tool: &str) -> Echo {
    let description = binding
        .resolve_tool(tool)
        .unwrap_or_else(|| panic!("tool `{tool}` must publish"))
        .tool
        .schema()
        .description;
    let fields = description
        .split(';')
        .filter_map(|field| field.split_once('='))
        .collect::<BTreeMap<_, _>>();
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
        .map(|output| {
            let stat = String::from_utf8_lossy(&output.stdout);
            let stat = stat.trim();
            output.status.success() && !stat.is_empty() && !stat.starts_with('Z')
        })
        .unwrap_or(false)
}

async fn wait_until_exited(pid: u32) -> bool {
    for _ in 0..100 {
        if !running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

struct Harness {
    engine: Arc<SessionEngine>,
    runtime: Arc<RuntimeRegistry>,
}

async fn harness(state: &Path) -> Harness {
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().expect("builtin agent catalog"),
    ));
    let installed = Arc::new(
        hya_app::InstalledBundleRefresh::new(state.join("registry.db"))
            .with_config_file(state.join("config-home/hya/config.yaml")),
    );
    let refresh = Arc::new(hya_app::ProjectScopeRefresh::new(installed));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.expect("connect store"),
            Arc::new(ProviderRouter::new()),
            Arc::clone(&runtime),
            permission,
            EventBus::default(),
        )
        .with_catalog_refresh(refresh),
    );
    Harness { engine, runtime }
}

fn project(roots: &[&Path]) -> CatalogScope {
    CatalogScope::Project {
        id: ProjectId::new(),
        roots: roots.iter().map(|root| root.to_path_buf()).collect(),
    }
}

impl Harness {
    async fn bind(&self, scope: &CatalogScope) -> TurnBinding {
        let workdir = scope
            .roots()
            .first()
            .cloned()
            .unwrap_or_else(std::env::temp_dir);
        self.engine
            .bind_scope_runtime(scope, &workdir)
            .await
            .expect("bind scope")
    }
}

async fn user_text(binding: &TurnBinding, session: SessionId, text: &str) -> String {
    let chain = HookChain::new(binding.bundle_hooks_for_agent("build"));
    let MessageUserBeforeOutcome::Continue { text } = chain
        .message_user_before(MessageUserBeforeInput {
            session,
            text: text.to_string(),
        })
        .await;
    text
}

async fn permission(binding: &TurnBinding, session: SessionId) -> Option<Decision> {
    HookChain::new(binding.bundle_hooks_for_agent("build"))
        .permission_ask(
            Some(session),
            Action::Bash,
            &Resource::Command("ls".to_string()),
        )
        .await
}

#[tokio::test]
async fn a_project_plugin_serves_only_its_project_and_starts_at_its_first_bind() {
    let state = temp_path("state");
    let root_a = temp_path("root-a");
    let root_b = temp_path("root-b");
    write_plugin(&root_a, "alpha", "alpha", "a");
    write_plugin(&root_b, "beta", "beta", "b");
    let harness = harness(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);

    // Directory and Global scopes never start project code.
    let directory = harness.bind(&CatalogScope::Directory(root_a.clone())).await;
    let global = harness.engine.bind_global_runtime().await.unwrap();
    for binding in [&directory, &global] {
        assert!(binding.resolve_tool("alpha__echo").is_none());
        assert!(binding.resolve_tool("beta__echo").is_none());
    }
    assert!(
        starts(&root_a).is_empty(),
        "no process before the first bind"
    );
    assert!(starts(&root_b).is_empty());

    let a = harness.bind(&scope_a).await;
    let alpha = echo(&a, "alpha__echo");
    assert!(a.resolve_tool("beta__echo").is_none());
    assert_eq!(starts(&root_a), vec![alpha.pid]);
    assert!(starts(&root_b).is_empty(), "Project B stays lazy");

    let b = harness.bind(&scope_b).await;
    assert!(b.resolve_tool("alpha__echo").is_none());
    assert_eq!(echo(&b, "beta__echo").origin, "b");

    // Neither the base snapshot nor other scopes picked up the plugins.
    let global = harness.engine.bind_global_runtime().await.unwrap();
    assert!(global.resolve_tool("alpha__echo").is_none());
    assert!(global.resolve_tool("beta__echo").is_none());
    let directory = harness.bind(&CatalogScope::Directory(root_a.clone())).await;
    assert!(directory.resolve_tool("alpha__echo").is_none());

    // Rebinding keeps the running process.
    assert_eq!(
        echo(&harness.bind(&scope_a).await, "alpha__echo").pid,
        alpha.pid
    );
    assert_eq!(starts(&root_a).len(), 1);
}

#[tokio::test]
async fn a_project_plugins_hooks_reach_only_its_project() {
    let state = temp_path("state");
    let root_a = temp_path("hooks-a");
    let root_b = temp_path("hooks-b");
    write_plugin(&root_a, "alpha", "alpha", "a");
    let harness = harness(&state).await;
    let a = harness.bind(&project(&[&root_a])).await;
    let b = harness.bind(&project(&[&root_b])).await;
    let directory = harness.bind(&CatalogScope::Directory(root_a.clone())).await;
    let global = harness.engine.bind_global_runtime().await.unwrap();
    let session = SessionId::new();

    assert_eq!(user_text(&a, session, "hi").await, "[alpha] hi");
    assert!(matches!(
        permission(&a, session).await,
        Some(Decision::Reject { feedback: Some(feedback) }) if feedback == "alpha says no"
    ));
    for other in [&b, &directory, &global] {
        assert_eq!(user_text(other, session, "hi").await, "hi");
        assert_eq!(permission(other, session).await, None);
    }
}

#[tokio::test]
async fn project_sessions_bind_their_projects_plugins_and_config_plugins_win() {
    let root = temp_path("sessions");
    let other = temp_path("sessions-other");
    write_plugin(&root, "alpha", "alpha", "project");
    write_plugin(&root, "configured", "configured", "project");
    // The config.yaml plugin with the manifest's id: one spec, started process-wide.
    let config_script = plugin_script("configured", "config", false);
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.expect("store"),
        router,
        &agent,
        BTreeMap::new(),
        vec![PluginSpec {
            id: "configured".to_string(),
            kind: PluginKindWire::Rust,
            command: vec!["python3".to_string(), "-c".to_string(), config_script],
            timeout_ms: Some(5_000),
            env: BTreeMap::new(),
            posture_overrides: BTreeMap::new(),
            plugin_dir: None,
        }],
        (WebSearchConfig::default(), InvocationPolicy::default()),
    )
    .await
    .expect("build engine");
    let engine = built.engine();
    let store = engine.store();
    let with_plugins = store
        .create_project("with-plugins", &[root.display().to_string()])
        .await
        .unwrap();
    let without = store
        .create_project("without", &[other.display().to_string()])
        .await
        .unwrap();
    let create = |workdir: &Path, project: ProjectId| CreateSession {
        parent: None,
        agent: agent.name.clone(),
        model: agent.model.clone(),
        workdir: workdir.display().to_string(),
        project: Some(project),
        kind: hya_proto::SessionKind::Project,
    };
    let inside = engine.create(create(&root, with_plugins.id)).await.unwrap();
    let outside = engine.create(create(&other, without.id)).await.unwrap();

    let bound = engine.bind_session_runtime(inside, &root).await.unwrap();
    assert_eq!(echo(&bound, "alpha__echo").origin, "project");
    assert_eq!(
        echo(&bound, "configured__echo").origin,
        "config",
        "the config.yaml plugin beats the project manifest"
    );
    assert_eq!(
        starts(&root).len(),
        1,
        "only the project's own plugin started from the manifests"
    );
    assert_eq!(user_text(&bound, inside, "hi").await, "[alpha] hi");

    let unbound = engine.bind_session_runtime(outside, &other).await.unwrap();
    assert!(unbound.resolve_tool("alpha__echo").is_none());
    assert_eq!(echo(&unbound, "configured__echo").origin, "config");
    assert_eq!(user_text(&unbound, outside, "hi").await, "hi");
    drop((bound, unbound));
    built.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn editing_plugin_toml_respawns_the_projects_plugins_at_the_next_bind() {
    let state = temp_path("state");
    let root = temp_path("edit");
    write_plugin(&root, "alpha", "alpha", "a");
    let harness = harness(&state).await;
    let scope = project(&[&root]);

    let first = harness.bind(&scope).await;
    let before = echo(&first, "alpha__echo").pid;
    drop(first);

    // A project bundle change republishes the Project but keeps its plugins.
    let bundle = root.join(".hya/bundles/tools");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(
        bundle.join("bundle.hya.md"),
        "---\nkind: AgentBundle\nidentity:\n  id: hya/tools\n  version: 1.0.0\n  publisher: hya\nagent:\n  id: tools-agent\n  role: main\n---\nTools.\n",
    )
    .unwrap();
    let with_bundle = harness.bind(&scope).await;
    assert!(with_bundle.resolve_agent("tools-agent").is_some());
    assert_eq!(echo(&with_bundle, "alpha__echo").pid, before, "no respawn");
    drop(with_bundle);

    write_manifest(&root, "alpha", "alpha", "timeout_ms = 4000\n");
    let edited = harness.bind(&scope).await;
    let after = echo(&edited, "alpha__echo").pid;
    assert_ne!(
        after, before,
        "a plugin.toml edit respawns at the next bind"
    );
    assert_eq!(starts(&root), vec![before, after]);
    assert!(
        wait_until_exited(before).await,
        "the replaced process {before} must exit"
    );
    assert!(running(after));
}

#[tokio::test]
async fn the_first_root_wins_by_plugin_id_and_each_plugin_runs_in_its_root() {
    let state = temp_path("state");
    let first = temp_path("first");
    let second = temp_path("second");
    write_plugin(&first, "shared", "shared", "first");
    write_plugin(&second, "shared", "shared", "second");
    write_plugin(&second, "only", "only", "second");
    let harness = harness(&state).await;

    let binding = harness.bind(&project(&[&first, &second])).await;
    let shared = echo(&binding, "shared__echo");
    assert_eq!(shared.origin, "first");
    assert_eq!(shared.cwd, first);
    let only = echo(&binding, "only__echo");
    assert_eq!(only.cwd, second, "a plugin runs in the root holding it");
    assert_eq!(starts(&first), vec![shared.pid]);
    assert_eq!(
        starts(&second),
        vec![only.pid],
        "the shadowed plugin never starts"
    );
}

#[tokio::test]
async fn dropping_or_evicting_the_project_scope_stops_its_plugins() {
    let state = temp_path("state");
    let root_a = temp_path("drop-a");
    let root_b = temp_path("drop-b");
    write_plugin(&root_a, "alpha", "alpha", "a");
    write_plugin(&root_b, "beta", "beta", "b");
    let harness = harness(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);

    let a = harness.bind(&scope_a).await;
    let alpha = echo(&a, "alpha__echo").pid;
    harness.runtime.drop_scope(&scope_a.key());
    assert!(running(alpha), "a live binding retains the process");
    drop(a);
    assert!(
        wait_until_exited(alpha).await,
        "the dropped Project's process {alpha} must exit"
    );

    let b = harness.bind(&scope_b).await;
    let beta = echo(&b, "beta__echo").pid;
    drop(b);
    harness
        .engine
        .set_catalog_scope_cache_config(CatalogScopeCacheConfig {
            max_scopes: 32,
            idle_ttl: Duration::ZERO,
        });
    harness.engine.sweep_catalog_scopes();
    assert!(harness.runtime.scope_overlay(&scope_b.key()).is_none());
    assert!(
        wait_until_exited(beta).await,
        "the evicted Project's process {beta} must exit"
    );
}

/// A plugin that appends `<pid> start <session>` for `session.start`,
/// `<pid> end <session>` for `session.end`, and `<pid> event <envelope json>`
/// for each live event to `hooks.log` in its cwd.
const LIFECYCLE_SCRIPT: &str = r#"import json,os,sys
def log(line):
 with open('hooks.log','a') as out: out.write(str(os.getpid()) + ' ' + line + '\n')
for line in sys.stdin:
 r=json.loads(line)
 m=r.get('method')
 p=r.get('params') or {}
 if m == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'watcher','version':'1.0.0','kind':'rust'},'hooks':[{'name':'session.start'},{'name':'session.end'},{'name':'event'}],'tools':[{'name':'echo','description':'origin=watcher;pid=' + str(os.getpid()) + ';cwd=' + os.getcwd(),'inputSchema':{'type':'object'}}]}
 elif m == 'hook/session.start':
  log('start ' + str(p.get('session'))); result={}
 elif m == 'hook/session.end':
  log('end ' + str(p.get('session'))); result={}
 elif m == 'event':
  log('event ' + json.dumps(p.get('envelope'))); result={}
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

/// Pids that logged an event whose envelope mentions `marker`, once the
/// asynchronous event notifications have landed.
async fn event_pids(root: &Path, marker: &str) -> Vec<u32> {
    for _ in 0..100 {
        let pids = hook_log(root)
            .into_iter()
            .filter(|(_, line)| line.starts_with("event ") && line.contains(marker))
            .map(|(pid, _)| pid)
            .collect::<Vec<_>>();
        if !pids.is_empty() {
            return pids;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Vec::new()
}

fn lifecycle_count(root: &Path, pid: u32, kind: &str, session: SessionId) -> usize {
    let wanted = format!("{kind} {session}");
    hook_log(root)
        .into_iter()
        .filter(|(logged, line)| *logged == pid && *line == wanted)
        .count()
}

#[tokio::test]
async fn session_hooks_follow_a_respawn_and_release_on_invalidation() {
    let state = temp_path("state");
    let root = temp_path("follow");
    let plugin_dir = root.join(".hya/plugins/watcher");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.py"), LIFECYCLE_SCRIPT).unwrap();
    write_manifest(&root, "watcher", "watcher", "");
    let harness = harness(&state).await;
    let engine = &harness.engine;
    let project = engine
        .store()
        .create_project("follow", &[root.display().to_string()])
        .await
        .unwrap()
        .id;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("build"),
            model: hya_proto::ModelRef::new("fake"),
            workdir: root.display().to_string(),
            project: Some(project),
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let scope = CatalogScope::Project {
        id: project,
        roots: vec![root.clone()],
    };
    let before = echo(&harness.bind(&scope).await, "watcher__echo").pid;
    assert_eq!(lifecycle_count(&root, before, "start", session), 1);
    engine
        .admit_user_prompt(session, "marker-before-edit".to_string())
        .await
        .unwrap();
    assert_eq!(event_pids(&root, "marker-before-edit").await[0], before);

    // plugin.toml edit: the session's next bind (this admission) swaps its
    // captured hooks to the respawned process before publishing anything.
    write_manifest(&root, "watcher", "watcher", "timeout_ms = 4000\n");
    engine
        .admit_user_prompt(session, "marker-after-edit".to_string())
        .await
        .unwrap();
    let after = echo(&harness.bind(&scope).await, "watcher__echo").pid;
    assert_ne!(after, before, "the manifest edit respawned");
    assert!(
        wait_until_exited(before).await,
        "the session must not keep the replaced process {before} alive"
    );
    let pids = event_pids(&root, "marker-after-edit").await;
    assert!(
        !pids.is_empty() && pids.iter().all(|pid| *pid == after),
        "post-swap events reach only the new process: {pids:?}"
    );
    assert!(
        event_pids(&root, "marker-before-edit")
            .await
            .iter()
            .all(|pid| *pid == before),
        "pre-swap events stay with the old process"
    );
    assert_eq!(lifecycle_count(&root, after, "start", session), 1);
    assert_eq!(lifecycle_count(&root, before, "end", session), 0);

    // Invalidation releases the idle session's hooks at once: no bind needed.
    engine.invalidate_catalog_scope(project);
    assert!(
        wait_until_exited(after).await,
        "an idle session must not keep the invalidated Project's process {after} alive"
    );
}
