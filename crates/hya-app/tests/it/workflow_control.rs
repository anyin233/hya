//! App-level Workflow list, info, selection, idempotency, precedence, and scope contracts.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::{
    InvocationPolicy, WebSearchConfig, WorkflowCatalog, WorkflowCatalogOwner, WorkflowCatalogRoots,
    WorkflowControlError, WorkflowInvocation, agent_with_model, build_session_engine,
    offline_router,
};
use hya_bundle::{BundleCatalog, BundleSource, SourceFile, prepare_package};
use hya_engine::{AgentCatalog, CreateSession, RuntimeRegistry};
use hya_proto::{
    ToolCallId, WorkflowAvailability, WorkflowCommand, WorkflowCommandResult, WorkflowRunId,
    WorkflowRunStatus,
};
use hya_store::SessionStore;
use hya_tool::ToolOperation;
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
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            hya_app::config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    (built, agent)
}

#[tokio::test]
async fn list_info_select_state_stale_revision_and_source_substitution() {
    let _env = crate::support::env_read_lock().await;
    let root = temp_dir("control");
    write_project_workflow(&root, "alpha", "ALPHA");
    write_project_workflow(&root, "beta", "BETA");
    let (mut built, agent) = offline_built().await;
    let engine = built.engine();
    let control = built.workflow_control();
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

    let WorkflowCommandResult::List { workflows } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::List,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(
        workflows
            .iter()
            .map(|w| w.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "plan-impl-review"]
    );
    assert!(workflows.iter().all(|w| w.revision.is_some()));

    let WorkflowCommandResult::Info { workflow: alpha } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Info {
                name: "alpha".into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected Info");
    };
    assert_eq!(alpha.stages.len(), 1);

    let WorkflowCommandResult::Selected { state: selected } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "alpha".into(),
                expected_revision: Some(alpha.identity.revision),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected Selected");
    };
    assert_eq!(selected.availability, Some(WorkflowAvailability::Available));

    // Editing alpha invalidates revision fence -> StaleRevision + State::Stale.
    write_project_workflow(&root, "alpha", "CHANGED");
    let stale = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "alpha".into(),
                expected_revision: Some(alpha.identity.revision),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code(), "WORKFLOW_STALE_REVISION");
    assert!(matches!(
        control
            .execute(
                session,
                WorkflowInvocation::default(),
                WorkflowCommand::Run {
                    name: Some("alpha".into()),
                    expected_revision: None,
                    inputs: BTreeMap::from([("request".into(), "v".into())]),
                    run: Some(WorkflowRunId::new()),
                },
                CancellationToken::new(),
            )
            .await,
        Err(WorkflowControlError::StaleRevision { .. })
    ));

    // Moving alpha to a different filename fails closed against source substitution.
    std::fs::rename(
        root.join(".hya/workflows/alpha.hya.md"),
        root.join(".hya/workflows/replacement.hya.md"),
    )
    .unwrap();
    let WorkflowCommandResult::State { state: missing } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected State");
    };
    assert_eq!(
        missing.availability,
        Some(WorkflowAvailability::Unavailable)
    );
    for name in [None, Some("alpha".to_string())] {
        assert!(matches!(
            control
                .execute(
                    session,
                    WorkflowInvocation::default(),
                    WorkflowCommand::Run {
                        name,
                        expected_revision: None,
                        inputs: BTreeMap::from([("request".into(), "v".into())]),
                        run: Some(WorkflowRunId::new()),
                    },
                    CancellationToken::new(),
                )
                .await,
            Err(WorkflowControlError::NotFound { .. })
        ));
    }

    // Switching selection to beta succeeds cleanly.
    let WorkflowCommandResult::Selected { state: switched } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "beta".into(),
                expected_revision: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected Selected");
    };
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
            WebSearchConfig::default(),
            InvocationPolicy::default(),
            hya_app::config::EngineSettings::default(),
        ),
    )
    .await
    .unwrap();
    let engine = built.engine();
    let control = built.workflow_control();
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

    let op = ToolOperation::from_tool_call(ToolCallId::new());
    let inv = WorkflowInvocation {
        operation: Some(op),
        binding: Some(engine.bind_runtime(&root).unwrap()),
        ..WorkflowInvocation::default()
    };
    let cmd = WorkflowCommand::Run {
        name: Some("deliver".into()),
        expected_revision: None,
        inputs: BTreeMap::from([("request".into(), "one".into())]),
        run: None,
    };

    let WorkflowCommandResult::Run { result: first } = control
        .execute(session, inv.clone(), cmd.clone(), CancellationToken::new())
        .await
        .unwrap()
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
        .position(|e| matches!(&e.event, hya_proto::Event::WorkflowStageMemberLinked { member, .. } if *member == linked))
        .unwrap();
    let start_idx = events
        .iter()
        .position(|e| {
            matches!(
                &e.event,
                hya_proto::Event::ResidentWorkStarted { .. } | hya_proto::Event::MailSent { .. }
            )
        })
        .unwrap();
    assert!(link_idx < start_idx);

    // Same operation replays without another provider call; changed inputs fail with OperationConflict.
    let WorkflowCommandResult::Run { result: replay } = control
        .execute(session, inv.clone(), cmd, CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("expected Run");
    };
    assert!(replay.replayed);
    assert_eq!(replay.run.id, first.run.id);

    let conflict = control
        .execute(
            session,
            inv,
            WorkflowCommand::Run {
                name: Some("deliver".into()),
                expected_revision: None,
                inputs: BTreeMap::from([("request".into(), "changed".into())]),
                run: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code(), "WORKFLOW_OPERATION_CONFLICT");

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
                    "kind: Bundle\nidentity: {{ id: {bundle_id}, version: 1.0.0, publisher: hya }}\nworkflow: {{ id: {workflow_id}, path: workflows/{workflow_id}.hya.md }}\nagents: [{{ id: catalog-worker, role: subagent, prompt: prompts/w.md }}]\n"
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
    let first_party = prepare_package(BundleSource::new(
        "bundles/first-party/plan-impl-review",
        vec![
            SourceFile::new("bundle.yaml", include_bytes!("../../../../bundles/first-party/plan-impl-review/bundle.yaml")),
            SourceFile::new("workflows/plan-impl-review.hya.md", include_bytes!("../../../../bundles/first-party/plan-impl-review/workflows/plan-impl-review.hya.md")),
            SourceFile::new("prompts/planner.md", include_bytes!("../../../../bundles/first-party/plan-impl-review/prompts/planner.md")),
            SourceFile::new("prompts/implementer.md", include_bytes!("../../../../bundles/first-party/plan-impl-review/prompts/implementer.md")),
            SourceFile::new("prompts/reviewer.md", include_bytes!("../../../../bundles/first-party/plan-impl-review/prompts/reviewer.md")),
        ],
    ))
    .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(
            AgentCatalog::new(Arc::new(
                BundleCatalog::from_verified_catalogs(&[&installed_old, &first_party]).unwrap(),
            ))
            .unwrap(),
        ),
    ));
    let old_binding = runtime.bind_turn(&project).unwrap();
    let roots = || WorkflowCatalogRoots::new(vec![proj_wf.clone()], Some(user_wf.clone()));

    // Precedence: Project > User > Installed, while qualified refs bypass bare shadowing.
    let cat = WorkflowCatalog::build(roots(), &old_binding).unwrap();
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
        WorkflowCatalogOwner::FirstParty { bundle_id } if bundle_id == "hya/plan-impl-review"
    ));

    std::fs::remove_file(proj_wf.join("collision.hya.md")).unwrap();
    assert!(matches!(
        WorkflowCatalog::build(roots(), &old_binding)
            .unwrap()
            .resolve("collision")
            .unwrap()
            .owner(),
        WorkflowCatalogOwner::User
    ));
    std::fs::remove_file(user_wf.join("collision.hya.md")).unwrap();
    assert!(matches!(
        WorkflowCatalog::build(roots(), &old_binding)
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
                BundleCatalog::from_verified_catalogs(&[&installed_new, &first_party]).unwrap(),
            ))
            .unwrap(),
        ))
        .unwrap();
    let fresh_binding = runtime.bind_turn(&project).unwrap();
    let old_rev = WorkflowCatalog::build(roots(), &old_binding)
        .unwrap()
        .resolve(inst_ref)
        .unwrap()
        .identity()
        .revision;
    let new_rev = WorkflowCatalog::build(roots(), &fresh_binding)
        .unwrap()
        .resolve(inst_ref)
        .unwrap()
        .identity()
        .revision;
    assert_ne!(old_rev, new_rev);

    // Listing/info/state never auto-selects first-party workflows; explicit select persists once.
    let (mut built, agent) = offline_built().await;
    let engine = built.engine();
    let control = built.workflow_control();
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
    let _ = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::List,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let _ = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(engine.replay(session).await.unwrap().len(), before_len);
    let WorkflowCommandResult::Selected { state } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "plan-impl-review".into(),
                expected_revision: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected Selected");
    };
    assert_eq!(
        state.selection.unwrap().source.as_str(),
        "bundle:hya/plan-impl-review/workflow/plan-impl-review"
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
    let empty = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(
            AgentCatalog::new(Arc::new(
                BundleCatalog::from_verified_catalogs(&[]).unwrap(),
            ))
            .unwrap(),
        ),
    ));
    let binding = empty.bind_turn(&root_a).unwrap();
    let cat = WorkflowCatalog::build(
        WorkflowCatalogRoots::new(vec![wf_a.clone(), wf_b.clone()], None),
        &binding,
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
    let control = built.workflow_control();
    assert_eq!(
        control
            .list(Some(&root_a))
            .await
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        ["root-a-same", "shared-name", "plan-impl-review"]
    );
    assert_eq!(
        control
            .list(None)
            .await
            .unwrap()
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
    let WorkflowCommandResult::List { workflows } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::List,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
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
    let WorkflowCommandResult::Selected { state } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "root-a-same".into(),
                expected_revision: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected Selected");
    };
    assert_eq!(state.availability, Some(WorkflowAvailability::Available));
    let WorkflowCommandResult::State { state: fresh } = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    else {
        panic!("expected State");
    };
    assert_eq!(fresh.availability, Some(WorkflowAvailability::Available));

    built.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root_a);
    let _ = std::fs::remove_dir_all(root_b);
}
