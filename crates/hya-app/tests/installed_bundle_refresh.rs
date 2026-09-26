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
use hya_tool::{PermissionPlane, PermissionRules, ToolCtx, ToolRegistry};

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

fn process_tool_ctx(workdir: &std::path::Path) -> ToolCtx {
    ToolCtx {
        permission: PermissionPlane::new(PermissionRules::default()).0,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        workflows: hya_tool::WorkflowPlane::disconnected(),
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(hya_proto::SessionId::new()),
        parent_session: None,
        todo: hya_tool::TodoPlane::default(),
        skills: hya_tool::SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: workdir.to_path_buf(),
        roots: vec![workdir.to_path_buf()],
        cancel: tokio_util::sync::CancellationToken::new(),
    }
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

fn installed_plugin_source() -> BundleSource {
    BundleSource::new(
        "installed-plugin",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: hya/installed-plugin, version: 1.0.0, publisher: hya }
resources:
  skills:
    - id: plugin-skill
      path: resources/skills/plugin-skill.md
"#,
            ),
            SourceFile::new(
                "resources/skills/plugin-skill.md",
                b"---\nname: plugin-skill\ndescription: Installed Plugin fixture.\n---\nPLUGIN_SKILL_BODY\n",
            ),
        ],
    )
}

fn goal_loop_override_source() -> BundleSource {
    BundleSource::new(
        "goal-loop-override",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: hya/goal-loop, version: 9.0.0, publisher: test }
namespace: goal-loop
resources:
  skills:
    - id: evaluator-prompt
      path: evaluator.md
"#,
            ),
            SourceFile::new(
                "evaluator.md",
                b"---\nname: evaluator-prompt\ndescription: override\n---\nOVERRIDE_EVALUATOR_PROMPT\n",
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
            hya_store::NamespaceInstallPolicy::DenyConflicts,
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
async fn installed_plugin_refresh_and_uninstall_preserve_old_binding_snapshot() {
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().expect("builtin agent catalog"),
    ));
    let registry_path = temp_path("plugin-registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().expect("registry path UTF-8"))
        .await
        .expect("connect registry");
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
    let workdir = temp_path("plugin-workdir");
    std::fs::create_dir_all(&workdir).expect("create workdir");
    let old_binding = engine.bind_runtime(&workdir).expect("bind old catalog");

    let prepared = prepare_package(installed_plugin_source()).expect("prepare plugin");
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x53; 32],
                prepared_digest: prepared.digest().to_owned(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1_725_000_020,
            },
        )
        .await
        .expect("install plugin");
    let installed_binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("refresh plugin");
    assert!(installed_binding.resolve_agent("plugin-agent").is_none());
    assert!(
        runtime
            .effective_manifest()
            .sources
            .get(&RuntimeSourceId::bundle("hya/installed-plugin"))
            .is_some_and(|source| source
                .skill_entries
                .iter()
                .any(|skill| skill.name == "plugin-skill"))
    );

    registry
        .uninstall("hya/installed-plugin")
        .await
        .expect("uninstall plugin");
    let after_uninstall = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("refresh uninstall");
    assert!(
        after_uninstall
            .bundle_catalog()
            .bundles()
            .iter()
            .all(|bundle| bundle.identity().id != "hya/installed-plugin")
    );
    assert!(
        old_binding
            .bundle_catalog()
            .bundles()
            .iter()
            .all(|bundle| bundle.identity().id != "hya/installed-plugin")
    );
    assert!(
        installed_binding
            .bundle_catalog()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == "hya/installed-plugin")
    );
}

#[tokio::test]
async fn installed_goal_loop_override_uninstall_restores_first_party_prompt() {
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().expect("builtin agent catalog"),
    ));
    let registry_path = temp_path("goal-loop-registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path);

    refresh.refresh_if_changed(&runtime).await.unwrap();
    let workdir = temp_path("goal-loop-workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    let builtin = runtime.bind_turn(&workdir).unwrap();
    let builtin_prompt = builtin
        .bundle_skill_content("hya/goal-loop", "evaluator-prompt")
        .unwrap();
    assert!(!builtin_prompt.contains("OVERRIDE_EVALUATOR_PROMPT"));

    let prepared = prepare_package(goal_loop_override_source()).unwrap();
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x77; 32],
                prepared_digest: prepared.digest().to_string(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let upgraded = runtime.bind_turn(&workdir).unwrap();
    assert!(
        upgraded
            .bundle_skill_content("hya/goal-loop", "evaluator-prompt")
            .unwrap()
            .contains("OVERRIDE_EVALUATOR_PROMPT")
    );

    registry.uninstall("hya/goal-loop").await.unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let restored = runtime.bind_turn(&workdir).unwrap();
    assert_eq!(
        restored.bundle_skill_content("hya/goal-loop", "evaluator-prompt"),
        Some(builtin_prompt)
    );
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
            project: None,
            kind: hya_proto::SessionKind::Project,
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
            hya_store::NamespaceInstallPolicy::DenyConflicts,
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

fn schema_bundle_source() -> BundleSource {
    BundleSource::new(
        "schema-refresh",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity:
  id: hya/schema-refresh
  version: 1.0.0
  publisher: hya
schemas:
  - scheme: db
    tool: query
    writable: true
resources:
  tools:
    - id: query
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agent:
  id: schema-lead
  role: main
  resource_view:
    allow:
      - query
"#,
            ),
            SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
        ],
    )
}

/// Installed bundle `schemas:` declarations publish as Bundle runtime-source
/// scheme claims: the winning binding names the bundle and the owning tool by
/// its view-scoped `bundle:{id}/tool/{local}` stable id.
#[tokio::test]
async fn installed_bundle_schema_declarations_publish_scheme_claims() {
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().expect("builtin agent catalog"),
    ));
    let registry_path = temp_path("schema-registry.db");
    let registry =
        BundleRegistry::connect(registry_path.to_str().expect("registry path must be UTF-8"))
            .await
            .expect("connect bundle registry");
    let refresh = Arc::new(hya_app::InstalledBundleRefresh::new(registry_path));
    refresh
        .refresh_if_changed(&runtime)
        .await
        .expect("initial preset refresh");
    assert!(
        !refresh
            .refresh_if_changed(&runtime)
            .await
            .expect("steady preset refresh"),
        "prepared preset resources publish once even with an empty installed registry"
    );

    let installed = prepare_package(schema_bundle_source()).expect("prepare schema bundle");
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x44; 32],
                prepared_digest: installed.digest().to_owned(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1_725_000_012,
            },
        )
        .await
        .expect("install schema bundle");

    assert!(
        refresh
            .refresh_if_changed(&runtime)
            .await
            .expect("schema refresh"),
        "an installed schema bundle advances the generation"
    );
    let effective = runtime.effective_schemes();
    let binding = effective.schemes.get("db").expect("db scheme must publish");
    assert_eq!(binding.owner(), "bundle:hya/schema-refresh");
    assert_eq!(
        binding.canonical_tool(),
        "bundle:hya/schema-refresh/tool/query"
    );
    assert!(binding.writable());
    assert_eq!(
        runtime.scheme_chain("db"),
        vec![(
            "bundle:hya/schema-refresh".to_string(),
            "bundle:hya/schema-refresh/tool/query".to_string(),
        )],
        "the scheme chain records the bundle claimant"
    );

    // Republishing an unchanged registry is a no-op.
    assert!(
        !refresh
            .refresh_if_changed(&runtime)
            .await
            .expect("steady refresh"),
        "an unchanged registry must not republish"
    );
}

#[tokio::test]
async fn plugin_process_tools_publish_atomically_and_old_binding_survives_uninstall() {
    let root = temp_path("process-root");
    std::fs::create_dir_all(&root).unwrap();
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path);
    let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'process-fixture','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'echo','description':'echo fixture','inputSchema':{'type':'object'}}]}
 elif r.get('method') == 'tool/call': result={'ok':True,'output':{'echo':r['params']['input']}}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let prepared = prepare_package(BundleSource::new(
        "process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/process-fixture, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files: [{ id: runtime, path: runtime.py }]
resources:
  tools: [{ id: echo, path: tool.json }]
"#,
            ),
            SourceFile::new("runtime.py", script),
            SourceFile::new("tool.json", "{}"),
        ],
    ))
    .unwrap();
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x61; 32],
                prepared_digest: prepared.digest().to_string(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let old = runtime.bind_turn(&root).unwrap();
    assert!(
        old.resolve_tool("process-fixture__echo").is_some(),
        "installed process tool must publish"
    );
    registry.uninstall("acme/process-fixture").await.unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let new = runtime.bind_turn(&root).unwrap();
    assert!(new.resolve_tool("process-fixture__echo").is_none());
    assert!(old.resolve_tool("process-fixture__echo").is_some());
}

#[tokio::test]
async fn failed_bundle_process_start_preserves_the_published_generation() {
    let root = temp_path("failed-process-root");
    std::fs::create_dir_all(&root).unwrap();
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path);
    let prepared = prepare_package(BundleSource::new(
        "old-process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/old-process, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files: [{ id: runtime, path: runtime.py }]
resources:
  tools: [{ id: echo, path: tool.json }]
"#,
            ),
            SourceFile::new("tool.json", "{}"),
            SourceFile::new(
                "runtime.py",
                r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize': result={'protocol_version':1,'plugin':{'id':'old-process','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'echo','description':'old process','inputSchema':{'type':'object'}}]}
 elif r.get('method') == 'tool/call': result={'ok':True,'output':{'generation':'OLD_PROCESS','input':r['params']['input']}}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#,
            ),
        ],
    ))
    .unwrap();
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x71; 32],
                prepared_digest: prepared.digest().into(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let before = runtime.bind_turn(&root).unwrap();
    let before_tool = before
        .resolve_tool("old-process__echo")
        .expect("old process tool must publish");
    let before_ctx = process_tool_ctx(&root);
    let before_output = before_tool
        .tool
        .execute(&before_ctx, serde_json::json!({"phase":"before"}))
        .await
        .expect("old process must be callable before failed refresh");
    assert_eq!(before_output["generation"], "OLD_PROCESS");
    assert!(
        !refresh.refresh_if_changed(&runtime).await.unwrap(),
        "unchanged sources must be reused"
    );
    let invalid = prepare_package(BundleSource::new(
        "bad-process",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: Plugin
identity: { id: acme/bad-process, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [/nonexistent/hya-bundle-test-process] }
"#,
        )],
    ))
    .unwrap();
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x72; 32],
                prepared_digest: invalid.digest().into(),
                prepared_bytes: invalid.bytes().to_vec(),
                installed_at: 2,
            },
        )
        .await
        .unwrap();
    assert!(refresh.refresh_if_changed(&runtime).await.is_err());
    let after = runtime.bind_turn(&root).unwrap();
    assert_eq!(
        before.generation(),
        after.generation(),
        "failed startup must never publish a partial candidate"
    );
    assert!(
        after
            .bundle_catalog()
            .bundles()
            .iter()
            .all(|bundle| bundle.identity().id != "acme/bad-process")
    );
    for (label, binding) in [("retained", &before), ("fresh", &after)] {
        let tool = binding
            .resolve_tool("old-process__echo")
            .unwrap_or_else(|| panic!("{label} binding lost old process tool"));
        let ctx = process_tool_ctx(&root);
        let output = tool
            .tool
            .execute(&ctx, serde_json::json!({"phase":label}))
            .await
            .unwrap_or_else(|error| panic!("{label} binding old process call failed: {error}"));
        assert_eq!(output["generation"], "OLD_PROCESS");
        assert_eq!(output["input"]["phase"], label);
    }
    registry.uninstall("acme/bad-process").await.unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    assert!(
        runtime
            .bind_turn(&root)
            .unwrap()
            .bundle_catalog()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == "acme/old-process")
    );
}

#[tokio::test]
async fn retained_bundle_hook_keeps_materialized_files_after_uninstall() {
    let root = temp_path("hook-lifetime");
    std::fs::create_dir_all(&root).unwrap();
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let runtime = RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    );
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path);
    let prepared = prepare_package(BundleSource::new("hook-files", vec![
        SourceFile::new("bundle.yaml", br#"kind: Plugin
identity: { id: acme/hook-files, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files:
    - { id: runtime, path: runtime.py }
    - { id: payload, path: payload.txt }
resources:
  hooks: [{ id: tool.execute.before, path: hook.json }]
"#),
        SourceFile::new("hook.json", "{}"),
        SourceFile::new("payload.txt", "RETAINED_HOOK_FILES"),
        SourceFile::new("runtime.py", r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize': result={'protocol_version':1,'plugin':{'id':'hook-files','version':'1.0.0','kind':'rust'},'hooks':[{'name':'tool.execute.before','posture':'safe'}],'tools':[]}
 elif r.get('method') == 'hook/tool.execute.before': result={'outcome':'continue','input':{'marker':open('payload.txt').read()}}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#),
    ])).unwrap();
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x73; 32],
                prepared_digest: prepared.digest().into(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let binding = runtime.bind_turn(&root).unwrap();
    let retained_hooks = binding.bundle_hooks_for_agent("build");
    assert_eq!(retained_hooks.len(), 1);
    drop(binding);
    registry.uninstall("acme/hook-files").await.unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let reply = retained_hooks[0]
        .tool_execute_before(hya_core::hooks::ToolExecuteBeforeInput {
            session: hya_proto::SessionId::new(),
            message: hya_proto::MessageId::new(),
            call: hya_proto::ToolCallId::new(),
            tool: "read".into(),
            input: serde_json::json!({}),
        })
        .await;
    match reply {
        hya_core::hooks::ToolExecuteBeforeOutcome::Continue { input } => {
            assert_eq!(input["marker"], "RETAINED_HOOK_FILES")
        }
        hya_core::hooks::ToolExecuteBeforeOutcome::Veto { reason } => {
            panic!("retained hook lost packaged files: {reason}")
        }
    }
}

#[tokio::test]
async fn bundle_config_edit_restarts_the_process_at_the_next_refresh() {
    let root = temp_path("config-restart-root");
    std::fs::create_dir_all(&root).unwrap();
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let config_file = root.join("config-home/hya/config.yaml");
    let refresh =
        hya_app::InstalledBundleRefresh::new(registry_path).with_config_file(config_file.clone());
    // The tool description reports the config location and current content.
    let script = r#"import json,os,sys
path = os.environ.get('HYA_BUNDLE_CONFIG_FILE', '')
content = open(path).read().strip() if path and os.path.exists(path) else 'absent'
description = 'file=' + path + '|dir=' + os.environ.get('HYA_BUNDLE_CONFIG_DIR', '') + '|content=' + content
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method') == 'initialize':
  result={'protocol_version':1,'plugin':{'id':'config-fixture','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'echo','description':description,'inputSchema':{'type':'object'}}]}
 else: result={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#;
    let prepared = prepare_package(BundleSource::new(
        "config-process",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/config-fixture, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }
  files: [{ id: runtime, path: runtime.py }]
resources:
  tools: [{ id: echo, path: tool.json }]
"#,
            ),
            SourceFile::new("runtime.py", script),
            SourceFile::new("tool.json", "{}"),
        ],
    ))
    .unwrap();
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x63; 32],
                prepared_digest: prepared.digest().to_string(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
    let describe = || {
        runtime
            .bind_turn(&root)
            .unwrap()
            .resolve_tool("config-fixture__echo")
            .expect("process tool must publish")
            .tool
            .schema()
            .description
    };
    let config_dir = root.join("config-home/hya/bundles/acme%2Fconfig-fixture");
    let expected = |content: &str| {
        format!(
            "file={}|dir={}|content={content}",
            config_dir.join("config.yml").display(),
            config_dir.display()
        )
    };

    assert!(refresh.refresh_if_changed(&runtime).await.unwrap());
    assert_eq!(describe(), expected("absent"));
    assert!(
        !refresh.refresh_if_changed(&runtime).await.unwrap(),
        "an unchanged config must not republish"
    );

    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.yml"), "mode: first\n").unwrap();
    assert!(refresh.refresh_if_changed(&runtime).await.unwrap());
    assert_eq!(describe(), expected("mode: first"));

    std::fs::write(config_dir.join("config.yml"), "mode: second\n").unwrap();
    assert!(refresh.refresh_if_changed(&runtime).await.unwrap());
    assert_eq!(describe(), expected("mode: second"));
    assert!(!refresh.refresh_if_changed(&runtime).await.unwrap());
    drop(refresh);
    let _ = std::fs::remove_dir_all(&root);
}
