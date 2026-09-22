//! Integration tests for `hya-app`: project bundle sources (`.hya/bundles`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{EventBus, RuntimeRegistry, SessionEngine};
use hya_provider::ProviderRouter;
use hya_store::{BundleInstallCandidate, BundleRegistry, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch");
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "hya-project-bundles-{}-{}-{id}-{suffix}",
        elapsed.as_nanos(),
        std::process::id()
    ))
}

/// Write one AgentBundle source directory under `<root>/.hya/bundles/<name>/`.
fn write_project_bundle(root: &Path, name: &str, bundle_id: &str, prompt: &str) {
    let dir = root.join(".hya/bundles").join(name);
    std::fs::create_dir_all(&dir).expect("create project bundle dir");
    let manifest = format!(
        "---\nkind: AgentBundle\nidentity:\n  id: {bundle_id}\n  version: 1.0.0\n  publisher: hya\nresources:\n  skills:\n    - id: {name}-skill\n      path: resources/skills/{name}-skill.md\nagent:\n  id: {name}-agent\n  role: main\n  spawn_lifecycle: transient\n---\n{prompt}\n"
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

async fn build_engine(registry_path: &Path, project_dir: Option<PathBuf>) -> Arc<SessionEngine> {
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().expect("builtin agent catalog"),
    ));
    let mut refresh = hya_app::InstalledBundleRefresh::new(registry_path.to_path_buf());
    if let Some(dir) = project_dir {
        refresh = refresh.with_project_dir(Some(dir));
    }
    let refresh = Arc::new(refresh);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.expect("connect store"),
            Arc::new(ProviderRouter::new()),
            Arc::clone(&runtime),
            permission,
            EventBus::default(),
        )
        .with_catalog_refresh(refresh),
    )
}

#[tokio::test]
async fn project_bundle_becomes_visible_through_root_bind() {
    let root = temp_path("project");
    write_project_bundle(
        &root,
        "local-tools",
        "hya/local-tools",
        "You are the local agent.",
    );
    let registry_path = root.join("registry.db");
    let engine = build_engine(&registry_path, Some(root.join(".hya/bundles"))).await;
    let workdir = temp_path("workdir-a");
    std::fs::create_dir_all(&workdir).expect("create workdir");

    let binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind project catalog");
    assert!(
        binding.resolve_agent("local-tools-agent").is_some(),
        "project bundle agent must be visible"
    );
    let policy = binding
        .agent_resource_policy("local-tools-agent")
        .expect("compile project agent policy");
    assert_eq!(
        policy.selected_bundle_skill_ids(),
        &["bundle:hya/local-tools/skill/local-tools-skill".to_string()]
    );
}

#[tokio::test]
async fn project_bundle_wins_id_conflict_over_installed() {
    let root = temp_path("project-id");
    write_project_bundle(&root, "shared", "hya/shared", "You are the PROJECT agent.");
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().expect("registry path UTF-8"))
        .await
        .expect("connect registry");
    let installed = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "installed",
        vec![hya_bundle::SourceFile::new(
            "bundle.hya.md",
            br#"---
kind: AgentBundle
identity:
  id: hya/shared
  version: 1.0.0
  publisher: hya
agent:
  id: shared-agent
  role: main
  spawn_lifecycle: transient
---
You are the INSTALLED agent.
"#
            .to_vec(),
        )],
    ))
    .expect("prepare installed");
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x61; 32],
                prepared_digest: installed.digest().to_owned(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .expect("install installed bundle");
    drop(registry);

    let engine = build_engine(&registry_path, Some(root.join(".hya/bundles"))).await;
    let workdir = temp_path("workdir-b");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    let binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind with conflict");
    let agent = binding
        .resolve_agent("shared-agent")
        .expect("agent must exist");
    assert!(
        agent
            .prompt
            .is_some_and(|prompt| prompt.contains("PROJECT")),
        "project definition must shadow the installed one: {:?}",
        agent.prompt
    );
}

#[tokio::test]
async fn project_bundle_wins_namespace_conflict_over_installed() {
    let root = temp_path("project-ns");
    // Project namespace defaults to the identity name segment: `proj-ns`.
    write_project_bundle(
        &root,
        "proj-ns",
        "hya/proj-ns",
        "You are the PROJECT agent.",
    );
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().expect("registry path UTF-8"))
        .await
        .expect("connect registry");
    let installed = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "installed-ns",
        vec![hya_bundle::SourceFile::new(
            "bundle.hya.md",
            br#"---
kind: AgentBundle
identity:
  id: hya/other-thing
  version: 1.0.0
  publisher: hya
namespace: proj-ns
agent:
  id: other-agent
  role: main
  spawn_lifecycle: transient
---
You are the INSTALLED agent.
"#
            .to_vec(),
        )],
    ))
    .expect("prepare installed namespace claimant");
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x62; 32],
                prepared_digest: installed.digest().to_owned(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .expect("install namespace claimant");
    drop(registry);

    let engine = build_engine(&registry_path, Some(root.join(".hya/bundles"))).await;
    let workdir = temp_path("workdir-c");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    let binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind with namespace conflict");
    assert!(
        binding.resolve_agent("other-agent").is_none(),
        "the installed namespace claimant must be shadowed by the project bundle"
    );
    assert!(binding.resolve_agent("proj-ns-agent").is_some());
}

#[tokio::test]
async fn project_bundle_content_change_republishes() {
    let root = temp_path("project-fp");
    write_project_bundle(&root, "live", "hya/live", "Version one prompt.");
    let registry_path = root.join("registry.db");
    let engine = build_engine(&registry_path, Some(root.join(".hya/bundles"))).await;
    let workdir = temp_path("workdir-d");
    std::fs::create_dir_all(&workdir).expect("create workdir");

    let first = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("first bind");
    let first_generation = first.generation();
    let first_agent = first.resolve_agent("live-agent").expect("live agent");
    assert!(
        first_agent
            .prompt
            .is_some_and(|p| p.contains("Version one"))
    );

    // Rewrite the prompt (same identity) and rebind: the fingerprint must
    // trigger a republish even though the registry generation is unchanged.
    write_project_bundle(&root, "live", "hya/live", "Version two prompt.");
    let second = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("second bind");
    assert_ne!(
        second.generation(),
        first_generation,
        "content change must advance the runtime generation"
    );
    let second_agent = second.resolve_agent("live-agent").expect("live agent");
    assert!(
        second_agent
            .prompt
            .is_some_and(|p| p.contains("Version two"))
    );
}

#[tokio::test]
async fn project_plugin_publishes_static_skills_without_an_agent() {
    let root = temp_path("project-plugin");
    write_project_plugin(
        &root,
        "local-plugin",
        "hya/local-plugin",
        "PLUGIN_SKILL_BODY",
    );
    let registry_path = root.join("registry.db");
    let engine = build_engine(&registry_path, Some(root.join(".hya/bundles"))).await;
    let workdir = temp_path("workdir-plugin");
    std::fs::create_dir_all(&workdir).expect("create workdir");

    let binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind project plugin catalog");
    assert!(binding.resolve_agent("local-plugin-agent").is_none());
    assert!(
        binding
            .bundle_catalog()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == "hya/local-plugin" && bundle.agents().is_empty())
    );
    let manifest = engine.runtime_registry().effective_manifest();
    let source = manifest
        .sources
        .get(&hya_core::RuntimeSourceId::bundle("hya/local-plugin"))
        .expect("plugin skill source must publish at root bind");
    assert!(
        source
            .skill_entries
            .iter()
            .any(|skill| skill.name == "local-plugin-skill"
                && skill.content.contains("PLUGIN_SKILL_BODY"))
    );
}
