//! App-level Workflow list, info, selection, idempotency, precedence, and scope contracts
//! driven through `hya/workflow`'s native BundleApi and `workflow` tool surfaces.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::{
    InvocationPolicy, WorkflowCatalog, WorkflowCatalogOwner, WorkflowCatalogRoots,
    WorkflowControlError, WorkflowInfo, WorkflowSummary, agent_with_model, build_session_engine,
    offline_router,
};
use hya_bundle::{BundleCatalog, BundleSource, SourceFile, prepare_package};
use hya_engine::{
    AgentCatalog, ApiMethod, BundleApiCall, CreateSession, RuntimeRegistry, SessionEngine,
};
use hya_proto::{SessionId, ToolCallId, WorkflowAvailability, WorkflowRunId};
use hya_store::SessionStore;
use hya_tool::extension::ExtensionEvent;
use hya_tool::handle::ArtifactPlane;
use hya_tool::{
    FormatterPlane, InteractionPlane, LifecyclePlane, LspPlane, MailboxPlane, NestedToolPlane,
    PermissionModel, PermissionPlane, PermissionRules, ProjectActivityPlane, SkillPlane,
    SpawnerPlane, ToolCtx, ToolOperation, WorkflowPlane, WorkflowProjection, WorkflowRunStatus,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "hya-workflow-{label}-{}-{nonce}-{seq}",
        std::process::id()
    ))
}

fn write_workflow(dir: &Path, stem: &str, name: &str, directive: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(format!("{stem}.hya.md")),
        format!(
            "---\nkind: Workflow\nname: {name}\ndescription: {name} fixture.\ninputs:\n  request: Request.\nnodes:\n  execute:\n    agent: hya-task\n    directive: {directive} {{{{input.request}}}}\n---\nflowchart TD\n  execute\n"
        ),
    )
    .unwrap();
}

fn write_project_workflow(root: &Path, name: &str, directive: &str) {
    write_workflow(&root.join(".hya/workflows"), name, name, directive);
}

async fn offline_built() -> (hya_app::BuiltSessionEngine, hya_engine::AgentSpec) {
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            hya_app::config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    (built, agent)
}

async fn wf_command(engine: &SessionEngine, session: SessionId, body: Value) -> (u16, Value) {
    let outcome = engine
        .invoke_bundle_api(BundleApiCall {
            bundle: "hya/workflow".to_owned(),
            method: ApiMethod::Post,
            session: Some(session),
            path: "/command".to_owned(),
            query: BTreeMap::new(),
            body,
        })
        .await
        .unwrap();
    (outcome.status, outcome.body)
}

async fn wf_list_global(engine: &SessionEngine, directory: Option<&Path>) -> Vec<WorkflowSummary> {
    let mut query = BTreeMap::new();
    if let Some(dir) = directory {
        query.insert("directory".to_owned(), dir.display().to_string());
    }
    let outcome = engine
        .invoke_bundle_api(BundleApiCall {
            bundle: "hya/workflow".to_owned(),
            method: ApiMethod::Get,
            session: None,
            path: "/workflows".to_owned(),
            query,
            body: Value::Null,
        })
        .await
        .unwrap();
    assert_eq!(outcome.status, 200, "{}", outcome.body);
    serde_json::from_value(outcome.body["workflows"].clone()).unwrap()
}

#[tokio::test]
async fn list_info_select_state_stale_revision_and_source_substitution() {
    let _env = crate::support::env_read_lock().await;
    let root = temp_dir("control");
    write_project_workflow(&root, "alpha", "ALPHA");
    write_project_workflow(&root, "beta", "BETA");
    let (mut built, agent) = offline_built().await;
    let engine = built.engine();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: root.display().to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let (status, body) = wf_command(&engine, session, json!({ "list": {} })).await;
    assert_eq!(status, 200, "{body}");
    let workflows: Vec<WorkflowSummary> =
        serde_json::from_value(body["list"]["workflows"].clone()).unwrap();
    assert_eq!(
        workflows
            .iter()
            .map(|w| w.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "plan-impl-review"]
    );
    assert!(workflows.iter().all(|w| w.revision.is_some()));

    let (status, body) = wf_command(&engine, session, json!({ "info": { "name": "alpha" } })).await;
    assert_eq!(status, 200, "{body}");
    let alpha: WorkflowInfo = serde_json::from_value(body["info"].clone()).unwrap();
    assert_eq!(alpha.stages.len(), 1);

    let (status, body) = wf_command(
        &engine,
        session,
        json!({
            "select": {
                "name": "alpha",
                "expectedRevision": alpha.identity.revision.to_string(),
            }
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let selected: WorkflowProjection = serde_json::from_value(body["selected"].clone()).unwrap();
    assert_eq!(selected.availability, Some(WorkflowAvailability::Available));

    // Editing alpha invalidates revision fence -> 409 failed_precondition.
    write_project_workflow(&root, "alpha", "CHANGED");
    let (status, body) = wf_command(
        &engine,
        session,
        json!({
            "select": {
                "name": "alpha",
                "expectedRevision": alpha.identity.revision.to_string(),
            }
        }),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "failed_precondition");

    let (status, body) = wf_command(
        &engine,
        session,
        json!({
            "run": {
                "name": "alpha",
                "inputs": { "request": "v" },
                "run": WorkflowRunId::new().to_string(),
                "delivery": "finished",
            }
        }),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "failed_precondition");

    // Moving alpha to a different filename fails closed against source substitution.
    std::fs::rename(
        root.join(".hya/workflows/alpha.hya.md"),
        root.join(".hya/workflows/replacement.hya.md"),
    )
    .unwrap();
    let (status, body) = wf_command(&engine, session, json!({ "state": {} })).await;
    assert_eq!(status, 200, "{body}");
    let missing: WorkflowProjection = serde_json::from_value(body["state"].clone()).unwrap();
    assert_eq!(
        missing.availability,
        Some(WorkflowAvailability::Unavailable)
    );
    for name in [Value::Null, json!("alpha")] {
        let mut run = serde_json::Map::new();
        if !name.is_null() {
            run.insert("name".to_owned(), name);
        }
        run.insert("inputs".to_owned(), json!({ "request": "v" }));
        run.insert("run".to_owned(), json!(WorkflowRunId::new().to_string()));
        run.insert("delivery".to_owned(), json!("finished"));
        let (status, body) = wf_command(&engine, session, json!({ "run": run })).await;
        assert_eq!(status, 404, "{body}");
        assert_eq!(body["error"]["code"], "not_found");
    }

    // Switching selection to beta succeeds cleanly.
    let (status, body) =
        wf_command(&engine, session, json!({ "select": { "name": "beta" } })).await;
    assert_eq!(status, 200, "{body}");
    let switched: WorkflowProjection = serde_json::from_value(body["selected"].clone()).unwrap();
    assert_eq!(switched.selection.unwrap().name, "beta");

    built.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn run_is_idempotent_by_tool_operation_and_rejects_changed_inputs() {
    let _env = crate::support::env_read_lock().await;
    let root = temp_dir("idempotent");
    write_project_workflow(&root, "deliver", "DELIVER");
    let path = root.join(".hya/workflows/deliver.hya.md");
    let src = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        src.replace(
            "    agent: hya-task",
            "    agent: hya-task\n    model: { id: fake/model }",
        ),
    )
    .unwrap();

    let provider = hya_provider::FakeProvider::scripted(vec![hya_provider::FakeStep::Text(
        "DELIVERED".into(),
    )]);
    let live_router = hya_provider::ProviderRouter::new().with(Arc::new(provider));
    let agent = agent_with_model("fake/model", None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.unwrap(),
        hya_provider::ProviderRouter::new(),
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (
            InvocationPolicy::default(),
            hya_app::config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    engine.publish_provider_catalog(Arc::new(live_router), engine.provider_catalog_snapshot());
    engine
        .runtime_registry()
        .publish_agent_model_preferences(BTreeMap::from([(
            "hya-task".into(),
            hya_proto::ModelRef::new("fake/model"),
        )]));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: root.display().to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let binding = engine.bind_runtime(&root).unwrap();
    let workflow_tool = binding
        .resolve_tool("workflow")
        .expect("hya/workflow registers workflow tool");
    let op = ToolOperation::from_tool_call(ToolCallId::new());
    let make_ctx = || {
        let (permission, _permission_rx) = PermissionPlane::new_with_policy(
            PermissionRules::default(),
            InvocationPolicy::default().with_model(PermissionModel::Danger),
        );
        let (interaction, _interaction_rx) = InteractionPlane::new();
        let (spawner, _spawner_rx) = SpawnerPlane::new();
        ToolCtx {
            permission: permission.for_session(session),
            interaction: interaction.for_session(session),
            spawner,
            workflows: WorkflowPlane::disconnected(),
            operation: op,
            mailbox: MailboxPlane::disconnected(),
            project_activity: ProjectActivityPlane::disconnected(),
            lifecycle: LifecyclePlane::disconnected(),
            session: Some(session),
            parent_session: None,
            skills: SkillPlane::default(),
            artifacts: ArtifactPlane::default(),
            agents: Default::default(),
            lsp: LspPlane::default(),
            formatter: FormatterPlane::default(),
            workdir: root.clone(),
            roots: vec![root.clone()],
            cancel: CancellationToken::new(),
            nested: NestedToolPlane::disconnected(),
        }
    };

    let input = json!({
        "action": "run",
        "name": "deliver",
        "inputs": { "request": "one" },
    });
    let first_out = workflow_tool
        .tool
        .execute(&make_ctx(), input.clone())
        .await
        .unwrap();
    let hya_app::WorkflowCommandResult::Run { result: first } =
        serde_json::from_value(first_out).unwrap()
    else {
        panic!("expected Run");
    };
    assert!(!first.replayed);
    assert_eq!(first.run.status, WorkflowRunStatus::Completed);
    assert_eq!(
        first.run.id,
        WorkflowRunId::from_operation(op.operation_id())
    );

    // Member link precedes governed resident turn start.
    let linked = first.run.stages[0].members[0].member;
    let events = engine.replay(session).await.unwrap();
    let link_idx = events
        .iter()
        .position(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::WorkflowStageMemberLinked { member, .. }) if member == linked))
        .unwrap();
    let start_idx = events
        .iter()
        .position(|e| {
            matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::ResidentWorkStarted { .. } | ExtensionEvent::MailSent { .. })
            )
        })
        .unwrap();
    assert!(link_idx < start_idx);

    // Same operation replays without another provider call; changed inputs fail with OperationConflict.
    let replay_out = workflow_tool
        .tool
        .execute(&make_ctx(), input)
        .await
        .unwrap();
    let hya_app::WorkflowCommandResult::Run { result: replay } =
        serde_json::from_value(replay_out).unwrap()
    else {
        panic!("expected Run");
    };
    assert!(replay.replayed);
    assert_eq!(replay.run.id, first.run.id);

    let conflict_err = workflow_tool
        .tool
        .execute(
            &make_ctx(),
            json!({
                "action": "run",
                "name": "deliver",
                "inputs": { "request": "changed" },
            }),
        )
        .await
        .unwrap_err();
    assert!(
        conflict_err
            .to_string()
            .contains("WORKFLOW_OPERATION_CONFLICT"),
        "{conflict_err}"
    );

    built.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}

fn workflow_bundle(bundle_id: &str, workflow_id: &str, prompt: &str) -> BundleSource {
    BundleSource::new(
        bundle_id,
        vec![
            SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nschema_version: 2\nidentity: {{ id: {bundle_id}, version: 1.0.0, publisher: hya }}\nworkflow: {{ id: {workflow_id}, path: workflows/{workflow_id}.hya.md }}\nagents: [{{ id: catalog-worker, role: subagent, prompt: prompts/w.md }}]\n"
                ),
            ),
            SourceFile::new(
                format!("workflows/{workflow_id}.hya.md"),
                format!(
                    "---\nkind: Workflow\nname: {workflow_id}\ndescription: Installed.\nnodes:\n  run: {{ agent: catalog-worker, directive: Run. }}\n---\nflowchart TD\n  run\n"
                ),
            ),
            SourceFile::new("prompts/w.md", prompt),
        ],
    )
}

#[tokio::test]
async fn catalog_precedence_folded_revisions_and_first_party_selection() {
    let _env = crate::support::env_read_lock().await;
    let project = temp_dir("prec-proj");
    let proj_wf = project.join(".hya/workflows");
    let user_wf = temp_dir("prec-user");
    write_workflow(&proj_wf, "collision", "collision", "PROJECT");
    write_workflow(&user_wf, "collision", "collision", "USER");

    let installed_old =
        prepare_package(workflow_bundle("hya/cat-inst", "collision", "OLD")).unwrap();
    let first_party = hya_bundle::first_party_bundle("hya/workflow").unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(
            AgentCatalog::new(Arc::new(
                BundleCatalog::from_verified_catalogs(&[&installed_old, first_party]).unwrap(),
            ))
            .unwrap(),
        ),
    ));
    let old_binding = runtime.bind_turn(&project).unwrap();
    let old_bundles: Vec<_> = old_binding
        .bundle_catalog()
        .bundles()
        .iter()
        .map(|b| (**b).clone())
        .collect();
    let roots = || WorkflowCatalogRoots::new(vec![proj_wf.clone()], Some(user_wf.clone()));

    // Precedence: Project > User > Installed, while qualified refs bypass bare shadowing.
    let cat = WorkflowCatalog::from_bundles(roots(), &old_bundles).unwrap();
    assert!(matches!(
        cat.resolve("collision").unwrap().owner(),
        WorkflowCatalogOwner::Project
    ));
    let inst_ref = "bundle:hya/cat-inst/workflow/collision";
    assert!(matches!(
        cat.resolve(inst_ref).unwrap().owner(),
        WorkflowCatalogOwner::Installed { bundle_id } if bundle_id == "hya/cat-inst"
    ));
    assert!(matches!(
        cat.resolve("plan-impl-review").unwrap().owner(),
        WorkflowCatalogOwner::FirstParty { bundle_id } if bundle_id == "hya/workflow"
    ));

    std::fs::remove_file(proj_wf.join("collision.hya.md")).unwrap();
    assert!(matches!(
        WorkflowCatalog::from_bundles(roots(), &old_bundles)
            .unwrap()
            .resolve("collision")
            .unwrap()
            .owner(),
        WorkflowCatalogOwner::User
    ));
    std::fs::remove_file(user_wf.join("collision.hya.md")).unwrap();
    assert!(matches!(
        WorkflowCatalog::from_bundles(roots(), &old_bundles)
            .unwrap()
            .resolve("collision")
            .unwrap()
            .owner(),
        WorkflowCatalogOwner::Installed { .. }
    ));

    // Bundle agent prompt change folds into revision; old binding stays pinned.
    let installed_new =
        prepare_package(workflow_bundle("hya/cat-inst", "collision", "NEW")).unwrap();
    runtime
        .publish_catalog(Arc::new(
            AgentCatalog::new(Arc::new(
                BundleCatalog::from_verified_catalogs(&[&installed_new, first_party]).unwrap(),
            ))
            .unwrap(),
        ))
        .unwrap();
    let fresh_binding = runtime.bind_turn(&project).unwrap();
    let fresh_bundles: Vec<_> = fresh_binding
        .bundle_catalog()
        .bundles()
        .iter()
        .map(|b| (**b).clone())
        .collect();
    let old_rev = WorkflowCatalog::from_bundles(roots(), &old_bundles)
        .unwrap()
        .resolve(inst_ref)
        .unwrap()
        .identity()
        .revision;
    let new_rev = WorkflowCatalog::from_bundles(roots(), &fresh_bundles)
        .unwrap()
        .resolve(inst_ref)
        .unwrap()
        .identity()
        .revision;
    assert_ne!(old_rev, new_rev);

    // Listing/info/state never auto-selects first-party workflows; explicit select persists once.
    let (mut built, agent) = offline_built().await;
    let engine = built.engine();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: project.display().to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let before_len = engine.replay(session).await.unwrap().len();
    let (status, _) = wf_command(&engine, session, json!({ "list": {} })).await;
    assert_eq!(status, 200);
    let (status, _) = wf_command(&engine, session, json!({ "state": {} })).await;
    assert_eq!(status, 200);
    assert_eq!(engine.replay(session).await.unwrap().len(), before_len);
    let (status, body) = wf_command(
        &engine,
        session,
        json!({ "select": { "name": "plan-impl-review" } }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let state: WorkflowProjection = serde_json::from_value(body["selected"].clone()).unwrap();
    assert_eq!(
        state.selection.unwrap().source.as_str(),
        "bundle:hya/workflow/workflow/plan-impl-review"
    );

    built.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(user_wf);
}

#[tokio::test]
async fn directory_global_and_multi_root_project_workflow_scopes() {
    let _env = crate::support::env_read_lock().await;
    let root_a = temp_dir("scope-a");
    let root_b = temp_dir("scope-b");
    let wf_a = root_a.join(".hya/workflows");
    let wf_b = root_b.join(".hya/workflows");
    write_workflow(&wf_a, "same", "root-a-same", "A SAME");
    write_workflow(&wf_b, "same", "root-b-same", "B SAME");
    write_workflow(&wf_a, "unique-a", "shared-name", "A SHARED");
    write_workflow(&wf_b, "unique-b", "shared-name", "B SHARED");
    write_workflow(&wf_b, "gamma", "gamma", "GAMMA");
    let nested = root_a.join("nested");
    write_project_workflow(&nested, "local", "LOCAL");

    // 1. Multi-root dedupe by both source ID and declared name (first root wins).
    let cat = WorkflowCatalog::from_bundles(
        WorkflowCatalogRoots::new(vec![wf_a.clone(), wf_b.clone()], None),
        &[],
    )
    .unwrap();
    assert_eq!(
        cat.list().into_iter().map(|i| i.name).collect::<Vec<_>>(),
        ["root-a-same", "shared-name", "gamma"]
    );
    assert!(
        cat.resolve("root-a-same")
            .unwrap()
            .display_path()
            .starts_with(&wf_a.display().to_string())
    );
    assert!(
        cat.resolve("shared-name")
            .unwrap()
            .display_path()
            .starts_with(&wf_a.display().to_string())
    );
    assert!(matches!(
        cat.resolve("root-b-same"),
        Err(WorkflowControlError::NotFound { .. })
    ));

    // 2. Standalone `list(Some(dir))` vs `list(None)` and multi-root Project session selection.
    let (mut built, agent) = offline_built().await;
    let engine = built.engine();
    assert_eq!(
        wf_list_global(&engine, Some(&root_a))
            .await
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        ["root-a-same", "shared-name", "plan-impl-review"]
    );
    assert_eq!(
        wf_list_global(&engine, None)
            .await
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        ["plan-impl-review"]
    );

    let proj = engine
        .store()
        .create_project(
            "multi",
            &[root_a.display().to_string(), root_b.display().to_string()],
        )
        .await
        .unwrap();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: nested.display().to_string(),
            project: Some(proj.id),
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let (status, body) = wf_command(&engine, session, json!({ "list": {} })).await;
    assert_eq!(status, 200, "{body}");
    let workflows: Vec<WorkflowSummary> =
        serde_json::from_value(body["list"]["workflows"].clone()).unwrap();
    assert_eq!(
        workflows
            .iter()
            .map(|w| w.name.as_str())
            .collect::<Vec<_>>(),
        [
            "local",
            "root-a-same",
            "shared-name",
            "gamma",
            "plan-impl-review"
        ]
    );
    let (status, body) = wf_command(
        &engine,
        session,
        json!({ "select": { "name": "root-a-same" } }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let state: WorkflowProjection = serde_json::from_value(body["selected"].clone()).unwrap();
    assert_eq!(state.availability, Some(WorkflowAvailability::Available));
    let (status, body) = wf_command(&engine, session, json!({ "state": {} })).await;
    assert_eq!(status, 200, "{body}");
    let fresh: WorkflowProjection = serde_json::from_value(body["state"].clone()).unwrap();
    assert_eq!(fresh.availability, Some(WorkflowAvailability::Available));

    built.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root_a);
    let _ = std::fs::remove_dir_all(root_b);
}
