//! Integration tests for `hya-app`: project bundle sources (`.hya/bundles`)
//! loaded per registered Project from every Project root.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_core::{
    CatalogScope, EventBus, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSourceId, SessionEngine,
    TurnBinding,
};
use hya_proto::{ModelRef, ProjectId};
use hya_provider::ProviderRouter;
use hya_store::{BundleInstallCandidate, BundleRegistry, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch");
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "hya-project-bundles-{}-{}-{id}-{suffix}",
        elapsed.as_nanos(),
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// Write one AgentBundle source directory under `<root>/.hya/bundles/<name>/`
/// whose Agent is `<name>-agent`.
fn write_project_bundle(root: &Path, name: &str, bundle_id: &str, prompt: &str) {
    write_project_bundle_in(root, name, bundle_id, None, prompt);
}

fn write_project_bundle_in(
    root: &Path,
    name: &str,
    bundle_id: &str,
    namespace: Option<&str>,
    prompt: &str,
) {
    let dir = root.join(".hya/bundles").join(name);
    std::fs::create_dir_all(&dir).expect("create project bundle dir");
    let namespace = namespace.map_or(String::new(), |namespace| {
        format!("namespace: {namespace}\n")
    });
    let manifest = format!(
        "---\nkind: AgentBundle\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\n{namespace}resources:\n  skills:\n    - id: {name}-skill\n      path: resources/skills/{name}-skill.md\nagent:\n  id: {name}-agent\n  role: main\n---\n{prompt}\n"
    );
    std::fs::write(dir.join("bundle.hya.md"), manifest).expect("write project manifest");
    let skills = dir.join("resources/skills");
    std::fs::create_dir_all(&skills).expect("create skills dir");
    std::fs::write(
        skills.join(format!("{name}-skill.md")),
        format!(
            "---\nname: {name}-skill\ndescription: Project Skill fixture.\n---\n{}_SKILL_BODY\n",
            name.to_uppercase()
        ),
    )
    .expect("write project skill");
}

fn write_project_plugin(root: &Path, name: &str, bundle_id: &str, body: &str) {
    let dir = root.join(".hya/bundles").join(name);
    std::fs::create_dir_all(dir.join("resources/skills")).expect("create project plugin dir");
    let manifest = format!(
        "kind: Plugin\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\nresources:\n  skills:\n    - id: {name}-skill\n      path: resources/skills/{name}-skill.md\n"
    );
    std::fs::write(dir.join("bundle.yaml"), manifest).expect("write project plugin manifest");
    std::fs::write(
        dir.join("resources/skills")
            .join(format!("{name}-skill.md")),
        format!("---\nname: {name}-skill\ndescription: Project plugin fixture.\n---\n{body}\n"),
    )
    .expect("write project plugin skill");
}

/// A process whose `echo` tool description reports the process id; its
/// plugin id is the bundle namespace `namespace`.
fn pid_script(namespace: &str) -> String {
    r#"import json,os,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'NAMESPACE','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'echo','description':'pid=' + str(os.getpid()),'inputSchema':{'type':'object'}}]}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#
    .replace("NAMESPACE", namespace)
}

fn process_manifest(bundle_id: &str) -> String {
    format!(
        "kind: Plugin\nidentity: {{ id: {bundle_id}, version: 1.0.0, publisher: acme }}\nextensions:\n  process: {{ kind: rust, command: [python3, '${{BUNDLE_ROOT}}/runtime.py'] }}\n  files: [{{ id: runtime, path: runtime.py }}]\nresources:\n  tools: [{{ id: echo, path: tool.json }}]\n"
    )
}

/// A process-backed project Plugin under `<root>/.hya/bundles/<name>/`.
fn write_project_process(root: &Path, name: &str, bundle_id: &str) {
    let dir = root.join(".hya/bundles").join(name);
    std::fs::create_dir_all(&dir).expect("create project process dir");
    std::fs::write(dir.join("bundle.yaml"), process_manifest(bundle_id)).unwrap();
    let namespace = bundle_id.rsplit('/').next().unwrap();
    std::fs::write(dir.join("runtime.py"), pid_script(namespace)).unwrap();
    std::fs::write(dir.join("tool.json"), "{}").unwrap();
}

async fn install(registry_path: &Path, source: hya_bundle::BundleSource, digest: u8) {
    let registry = BundleRegistry::connect(registry_path.to_str().expect("registry path UTF-8"))
        .await
        .expect("connect registry");
    let prepared = hya_bundle::prepare_package(source).expect("prepare installed");
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [digest; 32],
                prepared_digest: prepared.digest().to_owned(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .expect("install bundle");
}

fn installed_agent_source(
    bundle_id: &str,
    namespace: &str,
    agent: &str,
    prompt: &str,
) -> hya_bundle::BundleSource {
    hya_bundle::BundleSource::new(
        "installed",
        vec![hya_bundle::SourceFile::new(
            "bundle.hya.md",
            format!(
                "---\nkind: AgentBundle\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\nnamespace: {namespace}\nagent:\n  id: {agent}\n  role: main\n---\n{prompt}\n"
            ),
        )],
    )
}

struct Harness {
    engine: Arc<SessionEngine>,
    runtime: Arc<RuntimeRegistry>,
    refresh: Arc<hya_app::ProjectScopeRefresh>,
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
        .with_catalog_refresh(refresh.clone()),
    );
    Harness {
        engine,
        runtime,
        refresh,
    }
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

fn prompt_of(binding: &TurnBinding, agent: &str) -> String {
    binding
        .resolve_agent(agent)
        .unwrap_or_else(|| panic!("agent `{agent}` must resolve"))
        .prompt
        .unwrap_or_default()
        .to_string()
}

fn tool_pid(binding: &TurnBinding, tool: &str) -> u32 {
    let description = binding
        .resolve_tool(tool)
        .unwrap_or_else(|| panic!("tool `{tool}` must publish"))
        .tool
        .schema()
        .description;
    description
        .strip_prefix("pid=")
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("unexpected description {description}"))
}

/// Whether `pid` is a running (non-zombie) process.
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

#[tokio::test]
async fn each_project_sees_only_its_own_bundles_and_global_sees_none() {
    let state = temp_path("state");
    let root_a = temp_path("root-a");
    let root_b = temp_path("root-b");
    write_project_bundle(&root_a, "alpha", "hya/alpha", "You are ALPHA.");
    write_project_bundle(&root_b, "beta", "hya/beta", "You are BETA.");
    let harness = harness(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);

    let a = harness.bind(&scope_a).await;
    assert!(a.resolve_agent("alpha-agent").is_some());
    assert!(a.resolve_agent("beta-agent").is_none());
    assert_eq!(
        a.project_bundle_dirs().keys().collect::<Vec<_>>(),
        vec!["hya/alpha"]
    );
    let policy = a
        .agent_resource_policy("alpha-agent")
        .expect("compile project agent policy");
    assert_eq!(
        policy.selected_bundle_skill_ids(),
        &["bundle:hya/alpha/skill/alpha-skill".to_string()]
    );

    let b = harness.bind(&scope_b).await;
    assert!(b.resolve_agent("beta-agent").is_some());
    assert!(b.resolve_agent("alpha-agent").is_none());

    let global = harness.engine.bind_global_runtime().await.unwrap();
    assert!(global.resolve_agent("alpha-agent").is_none());
    assert!(global.resolve_agent("beta-agent").is_none());

    // A directory outside every Project never loads project bundles.
    let directory = harness.bind(&CatalogScope::Directory(root_a.clone())).await;
    assert!(directory.resolve_agent("alpha-agent").is_none());
    // First-party bundles stay visible everywhere.
    for binding in [&a, &b, &global, &directory] {
        assert!(
            binding
                .bundle_catalog()
                .bundles()
                .iter()
                .any(|bundle| bundle.identity().id == "hya/plan-impl-review")
        );
    }
}

#[tokio::test]
async fn the_first_root_wins_on_bundle_id_and_namespace() {
    let state = temp_path("state");
    let first = temp_path("first");
    let second = temp_path("second");
    write_project_bundle(&first, "shared-one", "hya/shared", "FIRST shared.");
    write_project_bundle(&second, "shared-two", "hya/shared", "SECOND shared.");
    write_project_bundle_in(&first, "ns-one", "hya/ns-one", Some("team"), "FIRST ns.");
    write_project_bundle_in(&second, "ns-two", "acme/ns-two", Some("team"), "SECOND ns.");
    write_project_bundle(&second, "only-second", "hya/only-second", "SECOND only.");
    let harness = harness(&state).await;

    let forward = harness.bind(&project(&[&first, &second])).await;
    assert!(prompt_of(&forward, "shared-one-agent").contains("FIRST"));
    assert!(forward.resolve_agent("shared-two-agent").is_none());
    assert!(forward.resolve_agent("ns-one-agent").is_some());
    assert!(forward.resolve_agent("ns-two-agent").is_none());
    assert!(forward.resolve_agent("only-second-agent").is_some());
    assert_eq!(
        forward.project_bundle_dirs().get("hya/shared"),
        Some(&first.join(".hya/bundles/shared-one"))
    );

    let reversed = harness.bind(&project(&[&second, &first])).await;
    assert!(prompt_of(&reversed, "shared-two-agent").contains("SECOND"));
    assert!(reversed.resolve_agent("shared-one-agent").is_none());
    assert!(reversed.resolve_agent("ns-two-agent").is_some());
    assert!(reversed.resolve_agent("ns-one-agent").is_none());

    let (catalogs, _) =
        hya_app::project_bundles::load_project_bundles_for_roots(&[first.clone(), second.clone()]);
    let mut ids = catalogs
        .iter()
        .map(|catalog| catalog.bundles()[0].identity().id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec!["hya/ns-one", "hya/only-second", "hya/shared"]);
}

#[tokio::test]
async fn project_bundle_wins_id_conflict_over_installed() {
    let state = temp_path("state");
    let root = temp_path("project-id");
    write_project_bundle(&root, "shared", "hya/shared", "You are the PROJECT agent.");
    install(
        &state.join("registry.db"),
        installed_agent_source(
            "hya/shared",
            "shared",
            "shared-agent",
            "You are the INSTALLED agent.",
        ),
        0x61,
    )
    .await;
    let harness = harness(&state).await;

    let binding = harness.bind(&project(&[&root])).await;
    assert!(prompt_of(&binding, "shared-agent").contains("PROJECT"));
    let global = harness.engine.bind_global_runtime().await.unwrap();
    assert!(prompt_of(&global, "shared-agent").contains("INSTALLED"));
}

#[tokio::test]
async fn project_bundle_wins_namespace_conflict_over_installed() {
    let state = temp_path("state");
    let root = temp_path("project-ns");
    // Project namespace defaults to the identity name segment: `proj-ns`.
    write_project_bundle(
        &root,
        "proj-ns",
        "hya/proj-ns",
        "You are the PROJECT agent.",
    );
    install(
        &state.join("registry.db"),
        installed_agent_source("hya/other-thing", "proj-ns", "other-agent", "INSTALLED."),
        0x62,
    )
    .await;
    let harness = harness(&state).await;

    let binding = harness.bind(&project(&[&root])).await;
    assert!(binding.resolve_agent("other-agent").is_none());
    assert!(binding.resolve_agent("proj-ns-agent").is_some());
    let global = harness.engine.bind_global_runtime().await.unwrap();
    assert!(global.resolve_agent("other-agent").is_some());
}

#[tokio::test]
async fn editing_a_bundle_republishes_only_its_project() {
    let state = temp_path("state");
    let root_a = temp_path("edit-a");
    let root_b = temp_path("edit-b");
    write_project_bundle(&root_a, "live", "hya/live", "Version one prompt.");
    write_project_bundle(&root_b, "other", "hya/other", "Other prompt.");
    let harness = harness(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);

    let first_a = harness.bind(&scope_a).await;
    let first_b = harness.bind(&scope_b).await;
    assert!(prompt_of(&first_a, "live-agent").contains("Version one"));
    assert!(
        !harness
            .refresh
            .refresh_scope(&harness.runtime, &scope_a)
            .await
            .unwrap(),
        "an unchanged Project must not republish"
    );

    write_project_bundle(&root_a, "live", "hya/live", "Version two prompt.");
    assert!(
        !harness
            .refresh
            .refresh_scope(&harness.runtime, &scope_b)
            .await
            .unwrap(),
        "editing Project A must not republish Project B"
    );
    let second_b = harness.bind(&scope_b).await;
    assert_eq!(second_b.generation(), first_b.generation());

    let second_a = harness.bind(&scope_a).await;
    assert_ne!(second_a.generation(), first_a.generation());
    assert!(prompt_of(&second_a, "live-agent").contains("Version two"));
    assert!(
        prompt_of(&first_a, "live-agent").contains("Version one"),
        "a retained binding keeps its snapshot"
    );
}

#[tokio::test]
async fn project_plugin_publishes_static_skills_without_an_agent() {
    let state = temp_path("state");
    let root = temp_path("project-plugin");
    write_project_plugin(
        &root,
        "local-plugin",
        "hya/local-plugin",
        "PLUGIN_SKILL_BODY",
    );
    let harness = harness(&state).await;
    let scope = project(&[&root]);

    let binding = harness.bind(&scope).await;
    assert!(binding.resolve_agent("local-plugin-agent").is_none());
    assert!(
        binding
            .bundle_catalog()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == "hya/local-plugin" && bundle.agents().is_empty())
    );
    assert!(
        binding
            .bundle_skill_content("hya/local-plugin", "local-plugin-skill")
            .is_some_and(|content| content.contains("PLUGIN_SKILL_BODY"))
    );
    let overlay = harness
        .runtime
        .scope_overlay(&scope.key())
        .expect("the Project overlay is published");
    assert!(
        overlay
            .bundle_sources
            .iter()
            .any(|source| *source.id() == RuntimeSourceId::bundle("hya/local-plugin")),
        "plugin skill source must publish in the Project scope"
    );
    assert!(
        !harness
            .runtime
            .effective_manifest()
            .sources
            .contains_key(&RuntimeSourceId::bundle("hya/local-plugin")),
        "the base snapshot never carries project bundles"
    );
}

#[tokio::test]
async fn a_project_bundle_process_exits_once_its_scope_is_dropped() {
    let state = temp_path("state");
    let root = temp_path("process-project");
    write_project_process(&root, "proc", "acme/proj-proc");
    let harness = harness(&state).await;
    let scope = project(&[&root]);

    let directory = harness.bind(&CatalogScope::Directory(root.clone())).await;
    assert!(
        directory.resolve_tool("proj-proc__echo").is_none(),
        "a directory outside a Project never starts project code"
    );

    let binding = harness.bind(&scope).await;
    let pid = tool_pid(&binding, "proj-proc__echo");
    assert!(running(pid));
    let again = harness.bind(&scope).await;
    assert_eq!(tool_pid(&again, "proj-proc__echo"), pid, "no respawn");
    drop(again);

    harness.runtime.drop_scope(&scope.key());
    drop(binding);
    // Any later bind collects sources no live scope uses.
    let _global = harness.engine.bind_global_runtime().await.unwrap();
    assert!(
        wait_until_exited(pid).await,
        "the dropped Project's process {pid} must exit"
    );
}

#[tokio::test]
async fn an_installed_bundle_process_is_shared_across_scopes() {
    let state = temp_path("state");
    let root_a = temp_path("shared-a");
    let root_b = temp_path("shared-b");
    write_project_bundle(&root_a, "alpha", "hya/alpha", "ALPHA.");
    write_project_bundle(&root_b, "beta", "hya/beta", "BETA.");
    install(
        &state.join("registry.db"),
        hya_bundle::BundleSource::new(
            "installed-process",
            vec![
                hya_bundle::SourceFile::new("bundle.yaml", process_manifest("acme/shared-proc")),
                hya_bundle::SourceFile::new("runtime.py", pid_script("shared-proc")),
                hya_bundle::SourceFile::new("tool.json", "{}"),
            ],
        ),
        0x63,
    )
    .await;
    let harness = harness(&state).await;
    let scope_a = project(&[&root_a]);

    let a = harness.bind(&scope_a).await;
    let b = harness.bind(&project(&[&root_b])).await;
    let global = harness.engine.bind_global_runtime().await.unwrap();
    let pid = tool_pid(&global, "shared-proc__echo");
    assert_eq!(tool_pid(&a, "shared-proc__echo"), pid);
    assert_eq!(tool_pid(&b, "shared-proc__echo"), pid);

    harness.runtime.drop_scope(&scope_a.key());
    drop(a);
    let _again = harness.engine.bind_global_runtime().await.unwrap();
    assert!(
        running(pid),
        "dropping a Project keeps shared installed processes"
    );
}

#[tokio::test]
async fn a_project_bundle_config_model_applies_only_in_its_project() {
    let state = temp_path("state");
    let root_a = temp_path("model-a");
    let root_b = temp_path("model-b");
    write_project_bundle(&root_a, "tools", "acme/tools", "Tools A.");
    write_project_bundle(&root_b, "tools", "acme/tools", "Tools B.");
    let config_a = root_a.join(".hya/bundles/tools/config.yml");
    std::fs::write(
        &config_a,
        "agents:\n  tools-agent:\n    model: p/project-a\n",
    )
    .unwrap();
    let harness = harness(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);

    let a = harness.bind(&scope_a).await;
    assert_eq!(
        a.configured_agent_model("tools-agent"),
        Some(&ModelRef::new("p/project-a"))
    );
    assert_eq!(
        a.project_bundle_dirs().get("acme/tools"),
        Some(&root_a.join(".hya/bundles/tools"))
    );
    let b = harness.bind(&scope_b).await;
    assert!(b.resolve_agent("tools-agent").is_some());
    assert_eq!(b.configured_agent_model("tools-agent"), None);

    // A config.yml edit republishes that Project at its next bind.
    std::fs::write(&config_a, "agents:\n  tools-agent:\n    model: p/edited\n").unwrap();
    let edited = harness.bind(&scope_a).await;
    assert_eq!(
        edited.configured_agent_model("tools-agent"),
        Some(&ModelRef::new("p/edited"))
    );
    assert_eq!(harness.bind(&scope_b).await.generation(), b.generation());
}

#[tokio::test]
async fn an_evicted_scope_is_republished_at_its_next_bind() {
    let state = temp_path("state");
    let root = temp_path("evicted");
    write_project_bundle(&root, "alpha", "hya/alpha", "ALPHA.");
    let harness = harness(&state).await;
    let scope = project(&[&root]);

    assert!(
        harness
            .bind(&scope)
            .await
            .resolve_agent("alpha-agent")
            .is_some()
    );
    harness.runtime.drop_scope(&scope.key());
    assert!(harness.runtime.scope_overlay(&scope.key()).is_none());

    let again = harness.bind(&scope).await;
    assert!(again.resolve_agent("alpha-agent").is_some());
    assert!(harness.runtime.scope_overlay(&scope.key()).is_some());
}

#[tokio::test]
async fn saving_a_project_bundle_model_writes_only_that_projects_file() {
    use hya_server::AgentModelControl as _;

    let state = temp_path("state");
    let root_a = temp_path("save-a");
    let root_b = temp_path("save-b");
    write_project_bundle(&root_a, "tools", "acme/tools", "Tools A.");
    write_project_bundle(&root_b, "tools", "acme/tools", "Tools B.");
    let harness = harness(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);
    let a = harness.bind(&scope_a).await;

    let store = SessionStore::connect_memory().await.unwrap();
    let owner = hya_proto::OwnerRunId::new();
    store
        .claim_runtime_owner(owner)
        .expect("claim runtime owner");
    let router = Arc::new(ProviderRouter::new().with(Arc::new(hya_provider::DevProvider::new())));
    let config_file = state.join("config-home/hya/config.yaml");
    let control = hya_app::PersistentAgentModelControl::load(
        store,
        owner,
        Arc::clone(&harness.runtime),
        router,
    )
    .await
    .unwrap()
    .with_configuration(hya_app::agent_model_config::AgentModelConfigFiles::new(
        config_file.clone(),
    ))
    .await
    .unwrap();

    let project_file = std::path::absolute(root_a.join(".hya/bundles/tools/config.yml")).unwrap();
    let listed = control
        .list(a.clone(), ModelRef::new("fallback"))
        .await
        .unwrap();
    let listed = listed
        .iter()
        .find(|row| row.agent_id == "tools-agent")
        .expect("the project Agent is listed");
    assert_eq!(
        listed.configuration_path.as_deref(),
        Some(project_file.to_string_lossy().as_ref())
    );

    let row = control
        .save_configuration(
            a,
            "tools-agent".to_string(),
            Some(hya_server::AgentModelIdentity::new("hya", "offline")),
            ModelRef::new("fallback"),
        )
        .await
        .unwrap();
    assert_eq!(
        row.configuration_path.as_deref(),
        Some(project_file.to_string_lossy().as_ref())
    );
    assert_eq!(
        row.configuration
            .map(|identity| format!("{}/{}", identity.provider_id, identity.model_id)),
        Some("hya/offline".to_string())
    );
    assert!(
        std::fs::read_to_string(&project_file)
            .unwrap()
            .contains("hya/offline")
    );
    assert!(
        !config_file.parent().unwrap().join("bundles").exists(),
        "a project bundle model never lands in the user scope"
    );

    assert_eq!(
        harness
            .bind(&scope_a)
            .await
            .configured_agent_model("tools-agent"),
        Some(&ModelRef::new("hya/offline"))
    );
    assert_eq!(
        harness
            .bind(&scope_b)
            .await
            .configured_agent_model("tools-agent"),
        None
    );
}
