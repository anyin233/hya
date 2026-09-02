//! Integration tests for `hya-app`: installed bundle refresh.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{BundleSource, SourceFile, prepare_package};
use hya_core::{CreateSession, EventBus, RuntimeRegistry, RuntimeSourceId, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::ProviderRouter;
use hya_store::{BundleInstallCandidate, BundleInstallOutcome, BundleRegistry, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch");
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "hya-installed-bundle-refresh-{}-{}-{id}-{suffix}",
        elapsed.as_nanos(),
        std::process::id()
    ))
}

fn installed_source() -> BundleSource {
    BundleSource::new(
        "installed",
        vec![
            SourceFile::new(
                "bundle.hya.md",
                br#"---
kind: AgentBundle
identity:
  id: hya/installed-test
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: installed-skill
      path: resources/skills/installed-skill.md
agent:
  id: installed-agent
  role: main
  spawn_lifecycle: transient
---
You are the installed agent.
"#,
            ),
            SourceFile::new(
                "resources/skills/installed-skill.md",
                b"---\nname: installed-skill\ndescription: Installed Skill fixture.\n---\nINSTALLED_SKILL_BODY\n",
            ),
        ],
    )
}

/// Build a minimal WorkflowBundle with one directly referenced Agent.
fn installed_workflow_source() -> BundleSource {
    BundleSource::new(
        "installed-workflow",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: WorkflowBundle
identity:
  id: hya/installed-workflow-test
  version: 1.0.0
  publisher: hya
workflow:
  id: refresh-flow
  path: workflows/refresh-flow.hya.md
agents:
  - id: refresh-worker
    description: refresh worker
    role: subagent
    spawn_lifecycle: transient
    prompt: prompts/refresh-worker.md
"#,
            ),
            SourceFile::new(
                "workflows/refresh-flow.hya.md",
                br#"---
kind: Workflow
name: refresh-flow
description: Refresh regression workflow.
nodes:
  work:
    agent: refresh-worker
    directive: Run the refresh regression.
---
flowchart TD
  work
"#,
            ),
            SourceFile::new(
                "prompts/refresh-worker.md",
                b"You are the refresh regression worker.\n",
            ),
        ],
    )
}

#[tokio::test]
async fn installed_generation_refresh_publishes_only_for_new_root_bindings() {
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().expect("builtin agent catalog"),
    ));
    let registry_path = temp_path("registry.db");
    let registry =
        BundleRegistry::connect(registry_path.to_str().expect("registry path must be UTF-8"))
            .await
            .expect("connect bundle registry");
    let refresh = Arc::new(hya_app::InstalledBundleRefresh::new(registry_path));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("connect store"),
        Arc::new(ProviderRouter::new()),
        Arc::clone(&runtime),
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh.clone());
    let workdir = temp_path("workdir");
    std::fs::create_dir_all(&workdir).expect("create test workdir");

    let old_binding = engine.bind_runtime(&workdir).expect("bind old catalog");
    let old_generation = old_binding.generation();
    assert!(old_binding.resolve_agent("general").is_some());
    assert!(old_binding.resolve_agent("installed-agent").is_none());

    let installed = prepare_package(installed_source()).expect("prepare installed bundle");
    let outcome = registry
        .install(
            &[],
            BundleInstallCandidate {
                source_digest: [0x42; 32],
                prepared_digest: installed.digest().to_owned(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1_725_000_010,
            },
        )
        .await
        .expect("install bundle");
    assert_eq!(outcome, BundleInstallOutcome::Installed { generation: 1 });

    let before_refresh = engine
        .bind_runtime(&workdir)
        .expect("bind before installed refresh");
    assert_eq!(before_refresh.generation(), old_generation);
    assert!(before_refresh.resolve_agent("installed-agent").is_none());

    let fresh_binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind installed catalog");
    assert_eq!(fresh_binding.generation().get(), old_generation.get() + 1);
    assert!(fresh_binding.resolve_agent("installed-agent").is_some());
    let installed_policy = fresh_binding
        .agent_resource_policy("installed-agent")
        .expect("compile installed agent resource policy");
    assert_eq!(
        installed_policy.selected_bundle_skill_ids(),
        &["bundle:hya/installed-test/skill/installed-skill".to_string()]
    );
    let installed_source = runtime
        .effective_manifest()
        .sources
        .remove(&RuntimeSourceId::bundle("hya/installed-test"))
        .expect("installed Skill source must publish with the catalog");
    assert!(
        installed_source
            .skill_entries
            .iter()
            .any(|skill| skill.name == "installed-skill"
                && skill.content.contains("INSTALLED_SKILL_BODY")),
        "the catalog and its prepared Skill source must publish in one generation"
    );
    assert!(old_binding.resolve_agent("installed-agent").is_none());
    assert!(
        fresh_binding
            .agent_catalog()
            .semantic_identity_v1()
            .is_some_and(|identity| !identity.is_empty())
    );

    let unchanged_binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind unchanged catalog");
    assert_eq!(unchanged_binding.generation(), fresh_binding.generation());
}

#[tokio::test]
async fn installed_workflow_refresh_publishes_workflow_and_agent_atomically_and_pins_bindings() {
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().expect("builtin agent catalog"),
    ));
    let registry_path = temp_path("workflow-registry.db");
    let registry =
        BundleRegistry::connect(registry_path.to_str().expect("registry path must be UTF-8"))
            .await
            .expect("connect bundle registry");
    let refresh = Arc::new(hya_app::InstalledBundleRefresh::new(registry_path));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("connect store"),
        Arc::new(ProviderRouter::new()),
        Arc::clone(&runtime),
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh);
    let workdir = temp_path("workflow-workdir");
    std::fs::create_dir_all(&workdir).expect("create test workdir");
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("general"),
            model: ModelRef::new("hya/offline"),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .expect("create root session");

    let old_binding = engine.bind_runtime(&workdir).expect("bind old catalog");
    let old_generation = old_binding.generation();
    assert!(old_binding.resolve_agent("refresh-worker").is_none());
    assert!(
        old_binding
            .bundle_catalog()
            .resolve_workflow("refresh-flow")
            .is_none()
    );

    let installed = prepare_package(installed_workflow_source()).expect("prepare WorkflowBundle");
    registry
        .install(
            &[],
            BundleInstallCandidate {
                source_digest: [0x43; 32],
                prepared_digest: installed.digest().to_owned(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1_725_000_011,
            },
        )
        .await
        .expect("install WorkflowBundle");

    let fresh_binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind installed WorkflowBundle catalog");
    assert_eq!(fresh_binding.generation().get(), old_generation.get() + 1);
    assert_eq!(
        fresh_binding
            .resolve_agent("refresh-worker")
            .expect("WorkflowBundle Agent published")
            .stable_id,
        "refresh-worker"
    );
    assert!(
        fresh_binding
            .bundle_catalog()
            .resolve_workflow("refresh-flow")
            .is_some(),
        "Workflow published with its Agent closure"
    );
    assert!(
        fresh_binding
            .bundle_catalog()
            .resolve_workflow("bundle:hya/installed-workflow-test/workflow/refresh-flow")
            .is_some(),
        "qualified Workflow resolution remains exact"
    );
    assert!(old_binding.resolve_agent("refresh-worker").is_none());
    assert!(
        old_binding
            .bundle_catalog()
            .resolve_workflow("refresh-flow")
            .is_none()
    );

    let projection = engine
        .read_projection(session)
        .await
        .expect("read root projection");
    assert!(projection.session.workflow.is_none());
}
