//! Integration tests for `hya-app`: installed bundle refresh, generation
//! pinning, schema claims, process tool/hook retention, and config restarts.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::bundle_registry::{
    BundleInstallCandidate, BundleInstallOutcome, BundleRegistry, NamespaceInstallPolicy,
};
use hya_bundle::{BundleSource, SourceFile, prepare_package};
use hya_engine::{
    EventBus, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSourceId, SessionEngine,
};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolCtx};
use sha2::{Digest, Sha256};
use sqlx::Connection;

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "hya-installed-bundle-refresh-{nanos}-{}-{id}-{suffix}",
        std::process::id()
    ))
}

fn process_tool_ctx(workdir: &std::path::Path) -> ToolCtx {
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        permission: PermissionPlane::new(PermissionRules::default()).0,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        workflows: hya_tool::WorkflowPlane::disconnected(),
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(hya_proto::SessionId::new()),
        parent_session: None,
        skills: hya_tool::SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: workdir.to_path_buf(),
        roots: vec![workdir.to_path_buf()],
        cancel: tokio_util::sync::CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

async fn install(
    registry: &BundleRegistry,
    source: BundleSource,
    digest_byte: u8,
) -> BundleInstallOutcome {
    let prepared = prepare_package(source).unwrap();
    registry
        .install(
            &[],
            NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [digest_byte; 32],
                prepared_digest: prepared.digest().to_owned(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: i64::from(digest_byte),
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn installed_agent_workflow_and_schema_refresh_pins_old_bindings() {
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let registry_path = temp_path("catalog-registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let refresh = Arc::new(hya_app::InstalledBundleRefresh::new(registry_path));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new()),
        Arc::clone(&runtime),
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh.clone());
    let workdir = temp_path("catalog-workdir");
    std::fs::create_dir_all(&workdir).unwrap();

    let old_binding = engine.bind_runtime(&workdir).unwrap();
    let old_gen = old_binding.generation();
    assert!(old_binding.resolve_agent("hya-task").is_some());
    assert!(old_binding.resolve_agent("installed-agent").is_none());

    // Install Agent+Skill bundle, WorkflowBundle, and Schema bundle.
    install(
        &registry,
        BundleSource::new(
            "installed",
            vec![
                SourceFile::new(
                    "bundle.hya.md",
                    "---\nkind: Bundle\nschema_version: 2\nidentity: { id: hya/installed-test, version: 1.0.0, publisher: hya }\nresources:\n  skills: [{ id: installed-skill, path: resources/skills/installed-skill.md }]\nagents: [{ id: installed-agent, role: main }]\n---\nYou are the installed agent.\n",
                ),
                SourceFile::new(
                    "resources/skills/installed-skill.md",
                    "---\nname: installed-skill\ndescription: Installed Skill.\n---\nINSTALLED_SKILL_BODY\n",
                ),
            ],
        ),
        0x42,
    )
    .await;
    install(
        &registry,
        BundleSource::new(
            "installed-workflow",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    "kind: Bundle\nschema_version: 2\nidentity: { id: hya/installed-workflow-test, version: 1.0.0, publisher: hya }\nworkflow: { id: refresh-flow, path: workflows/refresh-flow.hya.md }\nagents: [{ id: refresh-worker, role: subagent, prompt: prompts/w.md }]\n",
                ),
                SourceFile::new(
                    "workflows/refresh-flow.hya.md",
                    "---\nkind: Workflow\nname: refresh-flow\ndescription: Refresh test.\nnodes:\n  work: { agent: refresh-worker, directive: Run. }\n---\nflowchart TD\n  work\n",
                ),
                SourceFile::new("prompts/w.md", "Worker.\n"),
            ],
        ),
        0x43,
    )
    .await;
    install(
        &registry,
        BundleSource::new(
            "schema-refresh",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    "kind: Bundle\nschema_version: 2\nidentity: { id: hya/schema-refresh, version: 1.0.0, publisher: hya }\nruntime: { kind: js, mode: managed, entry: extensions/runtime.js }\nschemas: [{ scheme: db, tool: query, writable: true }]\nresources:\n  tools: [{ id: query, path: extensions/runtime.js }]\nagents:\n  - id: schema-lead\n    role: main\n    resource_view: { allow: [query] }\n",
                ),
                SourceFile::new("extensions/runtime.js", "export default {}"),
            ],
        ),
        0x44,
    )
    .await;

    // Non-root bind_runtime keeps old generation until bind_root_runtime refreshes.
    assert_eq!(engine.bind_runtime(&workdir).unwrap().generation(), old_gen);
    let fresh = engine.bind_root_runtime(&workdir).await.unwrap();
    assert_eq!(fresh.generation().get(), old_gen.get() + 1);
    assert!(fresh.resolve_agent("installed-agent").is_some());
    assert!(fresh.resolve_agent("refresh-worker").is_some());
    assert!(
        fresh
            .bundle_catalog()
            .resolve_workflow("refresh-flow")
            .is_some()
    );
    assert!(
        fresh
            .bundle_catalog()
            .resolve_workflow("bundle:hya/installed-workflow-test/workflow/refresh-flow")
            .is_some()
    );
    assert_eq!(
        fresh
            .agent_resource_policy("installed-agent")
            .unwrap()
            .selected_bundle_skill_ids(),
        &["bundle:hya/installed-test/skill/installed-skill".to_string()]
    );
    assert!(
        runtime
            .effective_manifest()
            .sources
            .get(&RuntimeSourceId::bundle("hya/installed-test"))
            .is_some_and(|s| s
                .skill_entries
                .iter()
                .any(|sk| sk.content.contains("INSTALLED_SKILL_BODY")))
    );
    let scheme = runtime
        .effective_schemes()
        .schemes
        .get("db")
        .unwrap()
        .clone();
    assert_eq!(scheme.owner(), "bundle:hya/schema-refresh");
    assert_eq!(
        scheme.canonical_tool(),
        "bundle:hya/schema-refresh/tool/query"
    );
    assert!(scheme.writable());
    assert!(old_binding.resolve_agent("installed-agent").is_none());
    assert!(
        old_binding
            .bundle_catalog()
            .resolve_workflow("refresh-flow")
            .is_none()
    );

    // Steady refresh is a no-op; uninstall advances generation while preserving `fresh`.
    assert_eq!(
        engine
            .bind_root_runtime(&workdir)
            .await
            .unwrap()
            .generation(),
        fresh.generation()
    );
    registry.uninstall("hya/installed-test").await.unwrap();
    let after = engine.bind_root_runtime(&workdir).await.unwrap();
    assert!(after.resolve_agent("installed-agent").is_none());
    assert!(fresh.resolve_agent("installed-agent").is_some());
}

#[tokio::test]
async fn installed_goal_loop_override_uninstall_restores_first_party_prompt() {
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let registry_path = temp_path("goal-loop-registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path);
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let workdir = temp_path("goal-loop-workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    let binding = runtime.bind_turn(&workdir).unwrap();
    let builtin_prompt = binding
        .bundle_skill_content("hya/goal-loop", "evaluator-prompt")
        .unwrap();
    assert!(!builtin_prompt.contains("OVERRIDE_EVALUATOR_PROMPT"));

    install(
        &registry,
        BundleSource::new(
            "goal-loop-override",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    "kind: Bundle\nschema_version: 2\nidentity: { id: hya/goal-loop, version: 9.0.0, publisher: test }\nnamespace: goal-loop\nresources:\n  skills: [{ id: evaluator-prompt, path: evaluator.md }]\n",
                ),
                SourceFile::new(
                    "evaluator.md",
                    "---\nname: evaluator-prompt\ndescription: override\n---\nOVERRIDE_EVALUATOR_PROMPT\n",
                ),
            ],
        ),
        0x77,
    )
    .await;
    refresh.refresh_if_changed(&runtime).await.unwrap();
    assert!(
        runtime
            .bind_turn(&workdir)
            .unwrap()
            .bundle_skill_content("hya/goal-loop", "evaluator-prompt")
            .unwrap()
            .contains("OVERRIDE_EVALUATOR_PROMPT")
    );

    registry.uninstall("hya/goal-loop").await.unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    assert_eq!(
        runtime
            .bind_turn(&workdir)
            .unwrap()
            .bundle_skill_content("hya/goal-loop", "evaluator-prompt"),
        Some(builtin_prompt)
    );
}

#[tokio::test]
async fn process_tools_and_retained_hooks_survive_uninstall() {
    let root = temp_path("process-root");
    std::fs::create_dir_all(&root).unwrap();
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path);
    let script = r#"import json,sys
for line in sys.stdin:
 r=json.loads(line)
 m=r.get('method')
 if m=='initialize':
  res={'protocol_version':1,'plugin':{'id':'process-fixture','version':'1.0.0','kind':'rust'},
       'hooks':[{'name':'tool.execute.before'}],
       'tools':[{'name':'echo','description':'echo fixture','inputSchema':{'type':'object'}}]}
 elif m=='tool/call': res={'ok':True,'output':{'echo':r['params']['input']}}
 elif m=='hook/tool.execute.before': res={'outcome':'continue','input':{'marker':open('payload.txt').read()}}
 else: res={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':res}),flush=True)
"#;
    install(
        &registry,
        BundleSource::new(
            "process",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    "kind: Bundle\nschema_version: 2\nidentity: { id: acme/process-fixture, version: 1.0.0, publisher: acme }\nruntime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }\nassets:\n  - { id: runtime, path: runtime.py }\n  - { id: payload, path: payload.txt }\nresources:\n  tools: [{ id: echo, path: tool.json }]\n  hooks: [{ id: tool.execute.before, path: hook.json }]\n",
                ),
                SourceFile::new("runtime.py", script),
                SourceFile::new("tool.json", "{}"),
                SourceFile::new("hook.json", "{}"),
                SourceFile::new("payload.txt", "RETAINED_HOOK_FILES"),
            ],
        ),
        0x61,
    )
    .await;

    refresh.refresh_if_changed(&runtime).await.unwrap();
    let old = runtime.bind_turn(&root).unwrap();
    assert!(old.resolve_tool("process-fixture__echo").is_some());
    let retained_hooks = old.bundle_hooks_for_agent("hya-main");
    assert_eq!(retained_hooks.len(), 1);

    registry.uninstall("acme/process-fixture").await.unwrap();
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let new = runtime.bind_turn(&root).unwrap();
    assert!(new.resolve_tool("process-fixture__echo").is_none());
    assert!(old.resolve_tool("process-fixture__echo").is_some());

    match retained_hooks[0]
        .tool_execute_before(hya_engine::hooks::ToolExecuteBeforeInput {
            session: hya_proto::SessionId::new(),
            message: hya_proto::MessageId::new(),
            call: hya_proto::ToolCallId::new(),
            tool: "read".into(),
            input: serde_json::json!({}),
        })
        .await
    {
        hya_engine::hooks::ToolExecuteBeforeOutcome::Continue { input } => {
            assert_eq!(input["marker"], "RETAINED_HOOK_FILES");
        }
        hya_engine::hooks::ToolExecuteBeforeOutcome::Veto { reason } => {
            panic!("retained hook lost packaged files: {reason}");
        }
    }
}

#[tokio::test]
async fn failed_process_start_and_old_format_retain_healthy_sources() {
    // 1. Failed replacement process retains prior healthy process source while publishing peers.
    let root = temp_path("failed-process-root");
    std::fs::create_dir_all(&root).unwrap();
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path.clone());
    let good_source = || {
        BundleSource::new(
            "old-process",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    "kind: Bundle\nschema_version: 2\nidentity: { id: acme/old-process, version: 1.0.0, publisher: acme }\nruntime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }\nassets: [{ id: runtime, path: runtime.py }]\nresources:\n  tools: [{ id: echo, path: tool.json }]\n",
                ),
                SourceFile::new("tool.json", "{}"),
                SourceFile::new(
                    "runtime.py",
                    "import json,sys\nfor line in sys.stdin:\n r=json.loads(line)\n if r.get('method')=='initialize': res={'protocol_version':1,'plugin':{'id':'old-process','version':'1.0.0','kind':'rust'},'hooks':[],'tools':[{'name':'echo','description':'old','inputSchema':{'type':'object'}}]}\n elif r.get('method')=='tool/call': res={'ok':True,'output':{'generation':'OLD_PROCESS','input':r['params']['input']}}\n else: res={}\n if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':res}),flush=True)\n",
                ),
            ],
        )
    };
    install(&registry, good_source(), 0x71).await;
    refresh.refresh_if_changed(&runtime).await.unwrap();
    let before = runtime.bind_turn(&root).unwrap();
    assert_eq!(
        before
            .resolve_tool("old-process__echo")
            .unwrap()
            .tool
            .execute(
                &process_tool_ctx(&root),
                serde_json::json!({"phase": "before"})
            )
            .await
            .unwrap()["generation"],
        "OLD_PROCESS"
    );
    assert!(!refresh.refresh_if_changed(&runtime).await.unwrap());

    registry.uninstall("acme/old-process").await.unwrap();
    install(
        &registry,
        BundleSource::new(
            "bad-process",
            vec![SourceFile::new(
                "bundle.yaml",
                "kind: Bundle\nschema_version: 2\nidentity: { id: acme/old-process, version: 2.0.0, publisher: acme }\nruntime: { kind: js, mode: process, command: [/nonexistent/hya-bundle-test-process] }\n",
            )],
        ),
        0x72,
    )
    .await;
    install(
        &registry,
        BundleSource::new(
            "healthy",
            vec![SourceFile::new(
                "bundle.yaml",
                "kind: Bundle\nschema_version: 2\nidentity: { id: acme/healthy, version: 1.0.0, publisher: acme }\n",
            )],
        ),
        0x75,
    )
    .await;

    let error = refresh.refresh_if_changed(&runtime).await.unwrap_err();
    assert!(matches!(
        error,
        hya_engine::CoreError::BundleRefresh(ref errs) if errs.len() == 1 && errs[0].0 == "acme/old-process"
    ));
    assert!(!refresh.refresh_if_changed(&runtime).await.unwrap());
    assert!(refresh.refresh_explicit(&runtime).await.is_err());
    let after = runtime.bind_turn(&root).unwrap();
    assert!(after.generation() > before.generation());
    assert!(
        after
            .bundle_catalog()
            .bundles()
            .iter()
            .any(|b| b.identity().id == "acme/healthy")
    );
    assert_eq!(
        after
            .resolve_tool("old-process__echo")
            .unwrap()
            .tool
            .execute(
                &process_tool_ctx(&root),
                serde_json::json!({"phase": "fresh"})
            )
            .await
            .unwrap()["generation"],
        "OLD_PROCESS"
    );

    // 2. Unreadable old format_version row reports error after publishing built-in sources.
    let old_reg_path = temp_path("old-format.db");
    let old_reg = BundleRegistry::connect(old_reg_path.to_str().unwrap())
        .await
        .unwrap();
    let prepared = prepare_package(BundleSource::new(
        "old-fmt",
        vec![SourceFile::new(
            "bundle.hya.md",
            "---\nkind: Bundle\nschema_version: 2\nidentity: { id: hya/old-fmt, version: 1.0.0, publisher: hya }\nagents: [{ id: old-fmt-agent, role: main }]\n---\nBody.\n",
        )],
    ))
    .unwrap();
    old_reg
        .install(
            &[],
            NamespaceInstallPolicy::DenyConflicts,
            BundleInstallCandidate {
                source_digest: [0x42; 32],
                prepared_digest: prepared.digest().to_owned(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
    let mut old_json: serde_json::Value = serde_json::from_slice(prepared.bytes()).unwrap();
    old_json["format_version"] = serde_json::json!(3);
    old_json["agents"][0]["spawn_lifecycle"] = serde_json::json!("transient");
    let bytes = serde_json::to_vec(&old_json).unwrap();
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut conn = sqlx::SqliteConnection::connect(old_reg_path.to_str().unwrap())
        .await
        .unwrap();
    sqlx::query("UPDATE installed_bundle SET prepared_bytes = ?, prepared_digest = ?")
        .bind(&bytes)
        .bind(&digest)
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();

    let rt2 = RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    );
    let rf2 = hya_app::InstalledBundleRefresh::new(old_reg_path);
    let err = rf2.refresh_if_changed(&rt2).await.unwrap_err().to_string();
    assert!(
        err.contains("please reinstall") && err.contains("hya/old-fmt"),
        "{err}"
    );
    let binding = rt2.bind_turn(&std::env::temp_dir()).unwrap();
    assert!(binding.resolve_agent("hya-goal-guide").is_some());
    assert!(binding.resolve_agent("old-fmt-agent").is_none());
    assert!(!rf2.refresh_if_changed(&rt2).await.unwrap());
}

#[tokio::test]
async fn bundle_config_edit_restarts_process_on_explicit_refresh() {
    let root = temp_path("config-restart-root");
    std::fs::create_dir_all(&root).unwrap();
    let registry_path = root.join("registry.db");
    let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
        .await
        .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let config_file = root.join("config-home/hya/config.yaml");
    let refresh = hya_app::InstalledBundleRefresh::new(registry_path).with_config_file(config_file);
    let script = r#"import json,os,sys
path = os.environ.get('HYA_BUNDLE_CONFIG_FILE', '')
content = open(path).read().strip() if path and os.path.exists(path) else 'absent'
desc = 'file=' + path + '|dir=' + os.environ.get('HYA_BUNDLE_CONFIG_DIR', '') + '|content=' + content
for line in sys.stdin:
 r=json.loads(line)
 if r.get('method')=='initialize':
  res={'protocol_version':1,'plugin':{'id':'config-fixture','version':'1.0.0','kind':'rust'},'hooks':[],
       'tools':[{'name':'echo','description':desc,'inputSchema':{'type':'object'}}]}
 else: res={}
 if 'id' in r: print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':res}),flush=True)
"#;
    install(
        &registry,
        BundleSource::new(
            "config-process",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    "kind: Bundle\nschema_version: 2\nidentity: { id: acme/config-fixture, version: 1.0.0, publisher: acme }\nruntime: { kind: js, mode: process, command: [python3, '${BUNDLE_ROOT}/runtime.py'] }\nassets: [{ id: runtime, path: runtime.py }]\nresources:\n  tools: [{ id: echo, path: tool.json }]\n",
                ),
                SourceFile::new("runtime.py", script),
                SourceFile::new("tool.json", "{}"),
            ],
        ),
        0x63,
    )
    .await;

    let describe = || {
        runtime
            .bind_turn(&root)
            .unwrap()
            .resolve_tool("config-fixture__echo")
            .unwrap()
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
    assert!(!refresh.refresh_if_changed(&runtime).await.unwrap());

    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.yml"), "mode: first\n").unwrap();
    assert!(!refresh.refresh_if_changed(&runtime).await.unwrap());
    assert_eq!(describe(), expected("absent"));
    assert!(refresh.refresh_explicit(&runtime).await.unwrap());
    assert_eq!(describe(), expected("mode: first"));

    std::fs::write(config_dir.join("config.yml"), "mode: second\n").unwrap();
    assert!(refresh.refresh_explicit(&runtime).await.unwrap());
    assert_eq!(describe(), expected("mode: second"));
    drop(refresh);
    let _ = std::fs::remove_dir_all(&root);
}
