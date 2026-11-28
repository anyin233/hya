//! Integration tests for `hya-app`: project bundle sources (`.hya/bundles`)
//! loaded per registered Project from every Project root.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_app::bundle_registry::{BundleInstallCandidate, BundleRegistry, NamespaceInstallPolicy};
use hya_engine::{
    CatalogScope, EventBus, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSourceId, SessionEngine,
    TurnBinding,
};
use hya_proto::{ModelRef, ProjectId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter, ReasoningEffort,
};
use hya_server::AgentModelControl as _;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use sha2::{Digest, Sha256};
use sqlx::Connection;

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "hya-project-bundles-{nanos}-{}-{id}-{suffix}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write_project_bundle_in(
    root: &Path,
    name: &str,
    bundle_id: &str,
    namespace: Option<&str>,
    prompt: &str,
) {
    let dir = root.join(".hya/bundles").join(name);
    std::fs::create_dir_all(dir.join("resources/skills")).unwrap();
    let ns = namespace.map_or(String::new(), |ns| format!("namespace: {ns}\n"));
    std::fs::write(
        dir.join("bundle.hya.md"),
        format!(
            "---\nkind: Bundle\nschema_version: 2\nidentity: {{ id: {bundle_id}, version: 1.0.0, publisher: hya }}\n{ns}resources:\n  skills: [{{ id: {name}-skill, path: resources/skills/{name}-skill.md }}]\nagents: [{{ id: {name}-agent, role: main }}]\n---\n{prompt}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("resources/skills")
            .join(format!("{name}-skill.md")),
        format!(
            "---\nname: {name}-skill\ndescription: Project Skill.\n---\n{}_SKILL_BODY\n",
            name.to_uppercase()
        ),
    )
    .unwrap();
}

fn write_project_bundle(root: &Path, name: &str, bundle_id: &str, prompt: &str) {
    write_project_bundle_in(root, name, bundle_id, None, prompt);
}

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
        "kind: Bundle\nschema_version: 2\nidentity: {{ id: {bundle_id}, version: 1.0.0, publisher: acme }}\nruntime: {{ kind: js, mode: process, command: [python3, '${{BUNDLE_ROOT}}/runtime.py'] }}\nassets: [{{ id: runtime, path: runtime.py }}]\nresources:\n  tools: [{{ id: echo, path: tool.json }}]\n"
    )
}

fn write_project_process(root: &Path, name: &str, bundle_id: &str) {
    let dir = root.join(".hya/bundles").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bundle.yaml"), process_manifest(bundle_id)).unwrap();
    let ns = bundle_id.rsplit('/').next().unwrap();
    std::fs::write(dir.join("runtime.py"), pid_script(ns)).unwrap();
    std::fs::write(dir.join("tool.json"), "{}").unwrap();
}

async fn install(registry_path: &Path, source: hya_bundle::BundleSource, digest: u8) {
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let prepared = hya_bundle::prepare_package(source).unwrap();
    registry
        .install(
            &[],
            NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [digest; 32],
                prepared_digest: prepared.digest().to_owned(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
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
                "---\nkind: Bundle\nschema_version: 2\nidentity: {{ id: {bundle_id}, version: 1.0.0, publisher: hya }}\nnamespace: {namespace}\nagents: [{{ id: {agent}, role: main }}]\n---\n{prompt}\n"
            ),
        )],
    )
}

struct Harness {
    engine: Arc<SessionEngine>,
    runtime: Arc<RuntimeRegistry>,
    refresh: Arc<hya_app::ProjectScopeRefresh>,
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
            .with_catalog_refresh(refresh.clone()),
        );
        Self {
            engine,
            runtime,
            refresh,
        }
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

fn prompt_of(binding: &TurnBinding, agent: &str) -> String {
    binding
        .resolve_agent(agent)
        .unwrap_or_else(|| panic!("agent `{agent}` must resolve"))
        .prompt
        .unwrap_or_default()
        .to_string()
}

fn tool_pid(binding: &TurnBinding, tool: &str) -> u32 {
    binding
        .resolve_tool(tool)
        .unwrap_or_else(|| panic!("tool `{tool}` must publish"))
        .tool
        .schema()
        .description
        .strip_prefix("pid=")
        .and_then(|p| p.parse().ok())
        .unwrap()
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

#[tokio::test]
async fn project_scope_isolation_precedence_first_root_and_static_plugin() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("iso-state");
    let root_a = temp_path("iso-a");
    let root_b = temp_path("iso-b");

    write_project_bundle(&root_a, "alpha", "hya/alpha", "ALPHA");
    write_project_bundle_in(&root_a, "ns-one", "hya/ns-one", Some("team"), "FIRST ns");
    write_project_bundle(&root_a, "shared", "hya/shared", "PROJECT shared");
    write_project_bundle(&root_a, "proj-ns", "hya/proj-ns", "PROJECT ns");
    // Agentless static plugin bundle in root_a.
    let plugin_dir = root_a.join(".hya/bundles/local-plugin");
    std::fs::create_dir_all(plugin_dir.join("resources/skills")).unwrap();
    std::fs::write(
        plugin_dir.join("bundle.yaml"),
        "kind: Bundle\nschema_version: 2\nidentity: { id: hya/local-plugin, version: 1.0.0, publisher: hya }\nresources:\n  skills: [{ id: local-plugin-skill, path: resources/skills/local-plugin-skill.md }]\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("resources/skills/local-plugin-skill.md"),
        "---\nname: local-plugin-skill\ndescription: Static.\n---\nPLUGIN_SKILL_BODY\n",
    )
    .unwrap();

    write_project_bundle(&root_b, "beta", "hya/beta", "BETA");
    write_project_bundle(&root_b, "shared-two", "hya/shared", "SECOND shared");
    write_project_bundle_in(&root_b, "ns-two", "acme/ns-two", Some("team"), "SECOND ns");

    install(
        &state.join("registry.db"),
        installed_agent_source("hya/shared", "shared", "shared-agent", "INSTALLED shared"),
        0x61,
    )
    .await;
    install(
        &state.join("registry.db"),
        installed_agent_source("hya/other-thing", "proj-ns", "other-agent", "INSTALLED ns"),
        0x62,
    )
    .await;

    let h = Harness::new(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);
    let a = h.bind(&scope_a).await;
    let b = h.bind(&scope_b).await;
    let global = h.engine.bind_global_runtime().await.unwrap();
    let dir_scope = h.bind(&CatalogScope::Directory(root_a.clone())).await;

    // Isolation: project A vs B vs global vs directory.
    assert!(a.resolve_agent("alpha-agent").is_some());
    assert!(a.resolve_agent("beta-agent").is_none());
    assert_eq!(
        a.agent_resource_policy("alpha-agent")
            .unwrap()
            .selected_bundle_skill_ids(),
        &["bundle:hya/alpha/skill/alpha-skill".to_string()]
    );
    assert!(
        a.bundle_skill_content("hya/local-plugin", "local-plugin-skill")
            .is_some_and(|c| c.contains("PLUGIN_SKILL_BODY"))
    );
    assert!(
        !h.runtime
            .effective_manifest()
            .sources
            .contains_key(&RuntimeSourceId::bundle("hya/local-plugin"))
    );
    assert!(b.resolve_agent("beta-agent").is_some());
    assert!(b.resolve_agent("alpha-agent").is_none());
    assert!(global.resolve_agent("alpha-agent").is_none());
    assert!(dir_scope.resolve_agent("alpha-agent").is_none());

    // Project overrides installed on both bundle ID and namespace; global retains installed.
    assert!(prompt_of(&a, "shared-agent").contains("PROJECT"));
    assert!(prompt_of(&global, "shared-agent").contains("INSTALLED"));
    assert!(a.resolve_agent("other-agent").is_none());
    assert!(a.resolve_agent("proj-ns-agent").is_some());
    assert!(global.resolve_agent("other-agent").is_some());

    // Multi-root: first root wins by bundle ID and namespace.
    let forward = h.bind(&project(&[&root_a, &root_b])).await;
    assert!(prompt_of(&forward, "shared-agent").contains("PROJECT"));
    assert!(forward.resolve_agent("shared-two-agent").is_none());
    assert!(forward.resolve_agent("ns-one-agent").is_some());
    assert!(forward.resolve_agent("ns-two-agent").is_none());
    assert!(forward.resolve_agent("beta-agent").is_some());
    assert_eq!(
        forward.project_bundle_dirs().get("hya/shared"),
        Some(&root_a.join(".hya/bundles/shared"))
    );
    let reversed = h.bind(&project(&[&root_b, &root_a])).await;
    assert!(prompt_of(&reversed, "shared-two-agent").contains("SECOND"));
    assert!(reversed.resolve_agent("ns-two-agent").is_some());
    assert!(reversed.resolve_agent("ns-one-agent").is_none());
}

#[tokio::test]
async fn editing_evicting_and_zero_read_caching_stay_scoped() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("cache-state");
    let root_a = temp_path("cache-a");
    let root_b = temp_path("cache-b");
    let empty_root = temp_path("cache-empty");
    write_project_bundle(&root_a, "live", "hya/live", "Version one");
    write_project_bundle(&root_b, "other", "hya/other", "Other");
    std::fs::write(root_a.join(".hya/bundles/live/config.yml"), "agents: {}\n").unwrap();

    let h = Harness::new(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);
    let scope_empty = project(&[&empty_root]);

    let first_a = h.bind(&scope_a).await;
    let first_b = h.bind(&scope_b).await;
    let first_empty = h.bind(&scope_empty).await;
    assert!(prompt_of(&first_a, "live-agent").contains("Version one"));
    assert!(!h.refresh.refresh_scope(&h.runtime, &scope_a).await.unwrap());

    // On Linux, steady re-binds perform zero filesystem reads on bundle files.
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::io::Read;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        let fd = unsafe {
            OwnedFd::from_raw_fd(libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC))
        };
        for rel in [
            "bundle.hya.md",
            "resources/skills/live-skill.md",
            "config.yml",
        ] {
            let path = CString::new(
                root_a
                    .join(".hya/bundles/live")
                    .join(rel)
                    .as_os_str()
                    .as_bytes(),
            )
            .unwrap();
            assert!(
                unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), libc::IN_ACCESS) }
                    >= 0
            );
        }
        assert_eq!(h.bind(&scope_a).await.generation(), first_a.generation());
        let mut file = std::fs::File::from(fd);
        assert_eq!(
            file.read(&mut [0u8; 4096]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    // Edits are invisible until explicit refresh, and refresh touches only Project A.
    write_project_bundle(&root_a, "live", "hya/live", "Version two");
    write_project_bundle(&empty_root, "added", "hya/added", "EXPLICIT_ONLY");
    assert_eq!(
        prompt_of(&h.bind(&scope_a).await, "live-agent"),
        "Version one"
    );
    assert!(
        h.bind(&scope_empty)
            .await
            .resolve_agent("added-agent")
            .is_none()
    );
    assert_eq!(
        h.bind(&scope_empty).await.generation(),
        first_empty.generation()
    );

    let second_a = h
        .engine
        .refresh_bundles(&scope_a, &root_a)
        .await
        .unwrap()
        .binding;
    assert_ne!(second_a.generation(), first_a.generation());
    assert!(prompt_of(&second_a, "live-agent").contains("Version two"));
    assert!(prompt_of(&first_a, "live-agent").contains("Version one"));
    assert_eq!(h.bind(&scope_b).await.generation(), first_b.generation());
    assert!(
        h.engine
            .refresh_bundles(&scope_empty, &empty_root)
            .await
            .unwrap()
            .binding
            .resolve_agent("added-agent")
            .is_some()
    );

    // Dropping a scope evicts it; next bind republishes it.
    h.runtime.drop_scope(&scope_a.key());
    assert!(h.runtime.scope_overlay(&scope_a.key()).is_none());
    assert!(h.bind(&scope_a).await.resolve_agent("live-agent").is_some());
}

#[tokio::test]
async fn project_bundle_process_drop_share_and_disable_lifecycle() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("proc-state");
    let root_a = temp_path("proc-a");
    let root_b = temp_path("proc-b");
    write_project_process(&root_a, "proc", "acme/proj-proc");
    let starts = root_a.join("starts");
    std::fs::write(
        root_a.join(".hya/bundles/proc/runtime.py"),
        format!(
            "with open({}, 'a') as f: f.write('start\\n')\n{}",
            serde_json::to_string(&starts).unwrap(),
            pid_script("proj-proc")
        ),
    )
    .unwrap();
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
    let registry = BundleRegistry::connect(state.join("registry.db").to_str().unwrap())
        .await
        .unwrap();
    registry
        .set_bundle_enabled("acme/proj-proc", false)
        .await
        .unwrap();

    let h = Harness::new(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);

    // Directory scope and disabled project bundle never start project code.
    assert!(
        h.bind(&CatalogScope::Directory(root_a.clone()))
            .await
            .resolve_tool("proj-proc__echo")
            .is_none()
    );
    assert!(
        h.bind(&scope_a)
            .await
            .resolve_tool("proj-proc__echo")
            .is_none()
    );
    assert!(!starts.exists());

    // Re-enable -> starts once; installed bundle process is shared across scopes.
    registry
        .set_bundle_enabled("acme/proj-proc", true)
        .await
        .unwrap();
    let a = h.bind(&scope_a).await;
    let b = h.bind(&scope_b).await;
    let global = h.engine.bind_global_runtime().await.unwrap();
    let proj_pid = tool_pid(&a, "proj-proc__echo");
    let shared_pid = tool_pid(&global, "shared-proc__echo");
    assert!(running(proj_pid));
    assert_eq!(
        tool_pid(&h.bind(&scope_a).await, "proj-proc__echo"),
        proj_pid
    );
    assert_eq!(tool_pid(&a, "shared-proc__echo"), shared_pid);
    assert_eq!(tool_pid(&b, "shared-proc__echo"), shared_pid);

    // Dropping scope stops project process but preserves shared installed process.
    h.runtime.drop_scope(&scope_a.key());
    drop(a);
    let _ = h.engine.bind_global_runtime().await.unwrap();
    assert!(wait_until_exited(proj_pid).await);
    assert!(running(shared_pid));

    // Disabling a running project process also stops it.
    let a2 = h.bind(&scope_a).await;
    let pid2 = tool_pid(&a2, "proj-proc__echo");
    drop(a2);
    registry
        .set_bundle_enabled("acme/proj-proc", false)
        .await
        .unwrap();
    assert!(
        h.bind(&scope_a)
            .await
            .resolve_tool("proj-proc__echo")
            .is_none()
    );
    assert!(wait_until_exited(pid2).await);
}

#[tokio::test]
async fn shadowed_project_bundle_processes_never_start_and_release_when_shadowed() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("shadow-state");
    let first = temp_path("shadow-first");
    let second = temp_path("shadow-second");

    // 1. Already shadowed by ID and by namespace: never starts even if broken.
    write_project_bundle_in(&first, "winner", "acme/shared", Some("shared"), "FIRST");
    write_project_process(&second, "loser", "other/shared");
    let marker = second.join("started");
    std::fs::write(
        second.join(".hya/bundles/loser/runtime.py"),
        format!(
            "from pathlib import Path\nPath({:?}).touch()\nraise SystemExit(1)\n",
            marker.to_str().unwrap()
        ),
    )
    .unwrap();
    let h = Harness::new(&state).await;
    let scope = project(&[&first, &second]);
    assert_eq!(prompt_of(&h.bind(&scope).await, "winner-agent"), "FIRST");
    assert!(
        h.engine
            .refresh_bundles(&scope, &first)
            .await
            .unwrap()
            .errors
            .is_empty()
    );
    assert!(!marker.exists());

    // 2. Becoming shadowed releases a running process; removing shadow starts a fresh one.
    let state2 = temp_path("new-shadow-state");
    let first2 = temp_path("new-shadow-first");
    let second2 = temp_path("new-shadow-second");
    write_project_process(&second2, "loser", "acme/shared");
    let h2 = Harness::new(&state2).await;
    let scope2 = project(&[&first2, &second2]);
    let b1 = h2.bind(&scope2).await;
    let pid = tool_pid(&b1, "shared__echo");
    drop(b1);

    write_project_bundle(&first2, "winner", "acme/shared", "FIRST");
    let refreshed = h2.engine.refresh_bundles(&scope2, &first2).await.unwrap();
    assert!(refreshed.errors.is_empty());
    assert_eq!(prompt_of(&refreshed.binding, "winner-agent"), "FIRST");
    assert!(wait_until_exited(pid).await);

    std::fs::remove_dir_all(first2.join(".hya/bundles/winner")).unwrap();
    let unshadowed = h2.engine.refresh_bundles(&scope2, &first2).await.unwrap();
    assert_ne!(tool_pid(&unshadowed.binding, "shared__echo"), pid);
}

#[tokio::test]
async fn hot_reload_fault_isolation_for_registry_manifest_config_and_root() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("fault-state");
    let bad = temp_path("fault-bad");
    let good = temp_path("fault-good");
    let registry_path = state.join("registry.db");

    // Corrupt one installed registry row; readable installed + project + plugin still load.
    install(
        &registry_path,
        installed_agent_source("hya/readable", "r", "readable-agent", "R"),
        1,
    )
    .await;
    install(
        &registry_path,
        installed_agent_source("hya/old", "o", "old-agent", "O"),
        2,
    )
    .await;
    let mut conn = sqlx::SqliteConnection::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let (bytes,): (Vec<u8>,) =
        sqlx::query_as("SELECT prepared_bytes FROM installed_bundle WHERE bundle_id = 'hya/old'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    let mut doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    doc["format_version"] = 3.into();
    let bytes = serde_json::to_vec(&doc).unwrap();
    sqlx::query("UPDATE installed_bundle SET prepared_bytes = ?, prepared_digest = ? WHERE bundle_id = 'hya/old'")
        .bind(&bytes)
        .bind(format!("{:x}", Sha256::digest(&bytes)))
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();

    write_project_bundle(&bad, "kept", "acme/kept", "ORIGINAL");
    write_project_bundle(&good, "other", "acme/other", "BEFORE");
    let h = Harness::new(&state).await;
    let scope = project(&[&bad, &good]);
    let before = h.bind(&scope).await;
    assert_eq!(prompt_of(&before, "readable-agent"), "R");
    assert!(before.resolve_agent("old-agent").is_none());

    // Invalid manifest + unreadable config.yml retain `kept` while updating `other`.
    std::fs::write(
        bad.join(".hya/bundles/kept/bundle.hya.md"),
        "---\nkind: invalid\n---\n",
    )
    .unwrap();
    std::fs::create_dir(bad.join(".hya/bundles/kept/config.yml")).unwrap();
    write_project_bundle(&good, "other", "acme/other", "AFTER-1");
    assert!(
        h.refresh
            .refresh_scope_explicit(&h.runtime, &scope)
            .await
            .is_err()
    );
    let after1 = h.engine.refresh_bundles(&scope, &bad).await.unwrap();
    assert!(
        after1
            .errors
            .iter()
            .any(|e| e.to_string().contains("hya/old"))
    );
    assert_eq!(prompt_of(&after1.binding, "kept-agent"), "ORIGINAL");
    assert!(prompt_of(&after1.binding, "other-agent").contains("AFTER-1"));

    // Unreadable `.hya/bundles` root keeps prior `kept` while updating `other`.
    std::fs::rename(bad.join(".hya/bundles"), bad.join(".hya/saved-bundles")).unwrap();
    std::fs::write(bad.join(".hya/bundles"), "not a directory").unwrap();
    write_project_bundle(&good, "other", "acme/other", "AFTER-2");
    let after2 = h
        .engine
        .refresh_bundles(&scope, &bad)
        .await
        .unwrap()
        .binding;
    assert_eq!(prompt_of(&after2, "kept-agent"), "ORIGINAL");
    assert!(prompt_of(&after2, "other-agent").contains("AFTER-2"));

    // Removing the broken bundle directory cleanly removes its agent.
    std::fs::remove_file(bad.join(".hya/bundles")).unwrap();
    std::fs::create_dir_all(bad.join(".hya/bundles")).unwrap();
    let _ = h.refresh.refresh_scope_explicit(&h.runtime, &scope).await;
    assert!(
        h.runtime
            .bind_scoped(&scope, &bad)
            .unwrap()
            .resolve_agent("kept-agent")
            .is_none()
    );
}

#[tokio::test]
async fn hot_reload_failed_bundle_process_keeps_source_and_explicit_refresh_retries() {
    let _env = crate::support::env_read_lock().await;
    let state = temp_path("fail-proc-state");
    let root = temp_path("fail-proc-root");
    write_project_process(&root, "proc", "acme/proj-proc");
    write_project_bundle(&root, "other", "acme/other", "BEFORE");
    let gate = root.join("ready");
    let log = root.join("starts");
    std::fs::write(&gate, "ready").unwrap();
    let script = format!(
        "import os\nwith open({}, 'a') as f: f.write('start\\n')\nif not os.path.exists({}): raise SystemExit(1)\n{}",
        serde_json::to_string(&log).unwrap(),
        serde_json::to_string(&gate).unwrap(),
        pid_script("proj-proc")
    );
    std::fs::write(root.join(".hya/bundles/proc/runtime.py"), script).unwrap();

    let h = Harness::new(&state).await;
    let scope = project(&[&root]);
    let first = h.bind(&scope).await;
    let original = tool_pid(&first, "proj-proc__echo");
    drop(first);

    std::fs::remove_file(&gate).unwrap();
    std::fs::write(
        root.join(".hya/bundles/proc/bundle.yaml"),
        process_manifest("acme/proj-proc").replace("1.0.0", "1.0.1"),
    )
    .unwrap();
    let failed = h.engine.refresh_bundles(&scope, &root).await.unwrap();
    assert!(!failed.errors.is_empty());
    assert_eq!(tool_pid(&failed.binding, "proj-proc__echo"), original);
    drop(failed);

    write_project_bundle(&root, "other", "acme/other", "AFTER");
    let partial = h
        .engine
        .refresh_bundles(&scope, &root)
        .await
        .unwrap()
        .binding;
    assert_eq!(tool_pid(&partial, "proj-proc__echo"), original);
    assert!(prompt_of(&partial, "other-agent").contains("AFTER"));
    drop(partial);

    // Repair external dependency without changing bundle bytes -> explicit refresh retries.
    std::fs::write(&gate, "ready").unwrap();
    let repaired = h.engine.refresh_bundles(&scope, &root).await.unwrap();
    assert!(repaired.errors.is_empty(), "{:?}", repaired.errors);
    assert!(repaired.changed);
    assert_ne!(tool_pid(&repaired.binding, "proj-proc__echo"), original);
}

#[tokio::test]
async fn project_bundle_model_config_isolation_save_and_request_routing() {
    let _env = crate::support::env_read_lock().await;
    struct Recording(std::sync::Mutex<Vec<CompletionRequest>>);
    #[async_trait::async_trait]
    impl Provider for Recording {
        fn id(&self) -> &str {
            "p"
        }
        fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
            Some(Capabilities {
                reasoning_request: true,
                streaming_tool_calls: true,
                max_context: 200_000,
                ..Capabilities::default()
            })
        }
        async fn stream(
            &self,
            request: CompletionRequest,
            session: hya_proto::SessionId,
            message: hya_proto::MessageId,
        ) -> Result<EventStream, ProviderError> {
            self.0.lock().unwrap().push(request);
            Provider::stream(
                &FakeProvider::scripted(vec![FakeStep::Text("done".into())]),
                CompletionRequest {
                    model: ModelRef::new("unused"),
                    system: None,
                    messages: vec![],
                    tools: vec![],
                    temperature: None,
                    max_output_tokens: None,
                    reasoning: None,
                    headers: Default::default(),
                },
                session,
                message,
            )
            .await
        }
    }

    let state = temp_path("model-state");
    let root_a = temp_path("model-a");
    let root_b = temp_path("model-b");
    write_project_bundle(&root_a, "tools", "acme/tools", "Tools A.");
    write_project_bundle(&root_b, "tools", "acme/tools", "Tools B.");
    let config_a = root_a.join(".hya/bundles/tools/config.yml");
    std::fs::write(
        &config_a,
        "agents:\n  tools-agent:\n    model: p/project-a\n    reasoning: high\n",
    )
    .unwrap();

    let h = Harness::new(&state).await;
    let scope_a = project(&[&root_a]);
    let scope_b = project(&[&root_b]);

    let a = h.bind(&scope_a).await;
    let b = h.bind(&scope_b).await;
    assert_eq!(
        a.configured_agent_model("tools-agent"),
        Some(&ModelRef::new("p/project-a"))
    );
    assert_eq!(b.configured_agent_model("tools-agent"), None);

    // Project model & effort beat user-level defaults on actual provider requests.
    let provider = Arc::new(Recording(Default::default()));
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = hya_proto::OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            store.clone(),
            Arc::new(
                ProviderRouter::new()
                    .with(provider.clone())
                    .with(Arc::new(hya_provider::DevProvider::new())),
            ),
            h.runtime.clone(),
            permission,
            EventBus::default(),
        )
        .with_catalog_refresh(h.refresh.clone()),
    );
    h.runtime
        .publish_agent_effort_configuration(std::collections::BTreeMap::from([(
            "tools-agent".into(),
            ReasoningEffort::Low,
        )]));
    let registered = store
        .create_project("effort", &[root_a.display().to_string()])
        .await
        .unwrap();
    let session = engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("tools-agent"),
            model: ModelRef::new("p/fallback"),
            workdir: root_a.display().to_string(),
            project: Some(registered.id),
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "hello".into())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &hya_app::agent_base_with_model("p/fallback", None),
            Default::default(),
        )
        .await
        .unwrap();
    {
        let captured = provider.0.lock().unwrap();
        assert_eq!(captured[0].model.as_str(), "p/project-a");
        assert_eq!(captured[0].reasoning, Some(ReasoningEffort::High));
    }

    // Saving through AgentModelControl writes only to Project A's config.yml, never user scope or Project B.
    let user_config = state.join("config-home/hya/config.yaml");
    let control =
        hya_app::PersistentAgentModelControl::load(store, owner, Arc::clone(&h.runtime), engine)
            .await
            .unwrap()
            .with_configuration(hya_app::agent_model_config::AgentModelConfigFiles::new(
                user_config.clone(),
            ))
            .await
            .unwrap();
    let project_file = std::path::absolute(&config_a).unwrap();
    let row = control
        .save_configuration(
            a,
            "tools-agent".into(),
            Some(hya_server::AgentModelIdentity::new("hya", "offline")),
            ModelRef::new("fallback"),
        )
        .await
        .unwrap();
    assert_eq!(
        row.configuration_path.as_deref(),
        Some(project_file.to_string_lossy().as_ref())
    );
    assert!(
        std::fs::read_to_string(&project_file)
            .unwrap()
            .contains("hya/offline")
    );
    assert!(!user_config.parent().unwrap().join("bundles").exists());
    assert_eq!(
        h.bind(&scope_a).await.configured_agent_model("tools-agent"),
        Some(&ModelRef::new("hya/offline"))
    );
    assert_eq!(
        h.bind(&scope_b).await.configured_agent_model("tools-agent"),
        None
    );
}
