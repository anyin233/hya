//! App-level Workflow list, info, selection, state, and revision contracts.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::{
    InvocationPolicy, WebSearchConfig, WorkflowCatalog, WorkflowCatalogOwner, WorkflowCatalogRoots,
    WorkflowControlError, WorkflowInvocation, agent_with_model, build_session_engine,
    offline_router,
};
use hya_bundle::{BundleCatalog, BundleSource, PreparedCatalog, SourceFile, prepare_package};
use hya_core::{AgentCatalog, CreateSession, RuntimeRegistry};
use hya_proto::{
    ToolCallId, WorkflowAvailability, WorkflowCommand, WorkflowCommandResult, WorkflowRevision,
    WorkflowRunId, WorkflowRunStatus,
};
use hya_store::SessionStore;
use hya_tool::{ToolOperation, ToolRegistry};
use tokio_util::sync::CancellationToken;

/// Create one process-unique project root.
fn project_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "hya-workflow-control-{}-{nonce}",
        std::process::id()
    ))
}

/// Create one process-unique temporary path for catalog fixtures.
fn temp_path(suffix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "hya-workflow-catalog-{}-{nonce}-{suffix}",
        std::process::id()
    ))
}

/// Build a prepared WorkflowBundle whose prompt bytes can change independently.
fn workflow_bundle_source_with_prompt(
    bundle_id: &str,
    workflow_id: &str,
    prompt: &str,
) -> hya_bundle::BundleSource {
    let manifest = format!(
        r#"kind: WorkflowBundle
identity:
  id: {bundle_id}
  version: 1.0.0
  publisher: hya
workflow:
  id: {workflow_id}
  path: workflows/{workflow_id}.hya.md
agents:
  - id: catalog-worker
    role: subagent
    prompt: prompts/catalog-worker.md
    spawn_lifecycle: transient
"#
    );
    let workflow = format!(
        r#"---
kind: Workflow
name: {workflow_id}
description: Installed catalog precedence fixture.
nodes:
  run:
    agent: catalog-worker
    directive: Run the installed catalog fixture.
---
flowchart TD
  run
"#
    );
    BundleSource::new(
        bundle_id,
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new(
                format!("workflows/{workflow_id}.hya.md"),
                workflow.into_bytes(),
            ),
            SourceFile::new("prompts/catalog-worker.md", prompt.as_bytes().to_vec()),
        ],
    )
}

/// Write one simple compiled Workflow under the project discovery root.
fn write_workflow(root: &Path, name: &str, directive: &str) {
    let directory = root.join(".hya/workflows");
    std::fs::create_dir_all(&directory).expect("create Workflow directory");
    std::fs::write(
        directory.join(format!("{name}.hya.md")),
        format!(
            r#"---
kind: Workflow
name: {name}
description: {name} description.
inputs:
  request: Request to process.
nodes:
  execute:
    agent: general
    directive: {directive} {{{{input.request}}}}
---
flowchart TD
  execute
"#
        ),
    )
    .expect("write Workflow");
}

/// List/info expose the compiler revision, and selection is durable with an
/// optimistic revision fence rather than process-local state.
#[tokio::test]
async fn list_info_select_state_and_stale_revision_share_one_catalog_contract() {
    let root = project_root();
    write_workflow(&root, "alpha", "ALPHA");
    write_workflow(&root, "beta", "BETA");
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.expect("store"),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (WebSearchConfig::default(), InvocationPolicy::default()),
    )
    .await
    .expect("build engine");
    let engine = built.engine();
    let control = built.workflow_control();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: root.display().to_string(),
        })
        .await
        .expect("create Session");

    let list = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::List,
            CancellationToken::new(),
        )
        .await
        .expect("list Workflows")
    {
        WorkflowCommandResult::List { workflows } => workflows,
        result => panic!("unexpected list result: {result:?}"),
    };
    assert_eq!(
        list.iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta", "plan-impl-review"]
    );
    assert!(list.iter().all(|item| item.revision.is_some()));
    let alpha = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Info {
                name: "alpha".to_string(),
            },
            CancellationToken::new(),
        )
        .await
        .expect("alpha info")
    {
        WorkflowCommandResult::Info { workflow } => workflow,
        result => panic!("unexpected info result: {result:?}"),
    };
    assert_eq!(
        alpha.inputs.get("request").map(String::as_str),
        Some("Request to process.")
    );
    assert_eq!(alpha.stages.len(), 1);
    assert_eq!(alpha.stages[0].id, "execute");

    let selected = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "alpha".to_string(),
                expected_revision: Some(alpha.identity.revision),
            },
            CancellationToken::new(),
        )
        .await
        .expect("select alpha")
    {
        WorkflowCommandResult::Selected { state } => state,
        result => panic!("unexpected select result: {result:?}"),
    };
    assert_eq!(
        selected.selection.as_ref().map(|item| item.name.as_str()),
        Some("alpha")
    );
    assert_eq!(
        selected.availability,
        Some(WorkflowAvailability::Available),
        "selection is available while its exact source and revision match",
    );

    let current = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .expect("state")
    {
        WorkflowCommandResult::State { state } => state,
        result => panic!("unexpected state result: {result:?}"),
    };
    assert_eq!(current, selected);

    write_workflow(&root, "alpha", "CHANGED");
    let stale = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "alpha".to_string(),
                expected_revision: Some(alpha.identity.revision),
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("old revision must fail closed");
    assert!(matches!(stale, WorkflowControlError::StaleRevision { .. }));
    assert_eq!(stale.code(), "WORKFLOW_STALE_REVISION");
    let unchanged = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .expect("unchanged state")
    {
        WorkflowCommandResult::State { state } => state,
        result => panic!("unexpected state result: {result:?}"),
    };
    assert_eq!(
        unchanged.selection.expect("selection").revision,
        alpha.identity.revision
    );
    assert_eq!(
        unchanged.availability,
        Some(WorkflowAvailability::Stale),
        "a changed exact source is stale",
    );
    let explicit_stale = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Run {
                name: Some("alpha".to_string()),
                expected_revision: None,
                inputs: BTreeMap::from([("request".to_string(), "value".to_string())]),
                run: Some(WorkflowRunId::new()),
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("explicit same-name run must retain selected revision fence");
    assert!(matches!(
        explicit_stale,
        WorkflowControlError::StaleRevision { .. }
    ));

    std::fs::write(
        root.join(".hya/workflows/alpha.hya.md"),
        "not a Workflow source",
    )
    .expect("invalidate selected source");
    let invalid = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .expect("state for invalid source")
    {
        WorkflowCommandResult::State { state } => state,
        result => panic!("unexpected state result: {result:?}"),
    };
    assert_eq!(invalid.availability, Some(WorkflowAvailability::Stale));

    write_workflow(&root, "alpha", "CHANGED_AGAIN");
    let replacement = root.join(".hya/workflows/replacement.hya.md");
    std::fs::copy(root.join(".hya/workflows/alpha.hya.md"), &replacement)
        .expect("copy same-name replacement source");
    std::fs::remove_file(root.join(".hya/workflows/alpha.hya.md")).expect("remove selected source");
    let missing = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .expect("state for missing source")
    {
        WorkflowCommandResult::State { state } => state,
        result => panic!("unexpected state result: {result:?}"),
    };
    assert_eq!(
        missing.availability,
        Some(WorkflowAvailability::Unavailable)
    );

    let beta = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Info {
                name: "beta".to_string(),
            },
            CancellationToken::new(),
        )
        .await
        .expect("beta info")
    {
        WorkflowCommandResult::Info { workflow } => workflow,
        result => panic!("unexpected info result: {result:?}"),
    };
    let switched = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "beta".to_string(),
                expected_revision: Some(beta.identity.revision),
            },
            CancellationToken::new(),
        )
        .await
        .expect("switch to beta")
    {
        WorkflowCommandResult::Selected { state } => state,
        result => panic!("unexpected select result: {result:?}"),
    };
    assert_eq!(switched.selection.expect("beta selection").name, "beta");

    built.shutdown().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(root);
}

/// A selected source identity cannot silently move to another same-name file.
#[tokio::test]
async fn selected_run_rejects_same_name_source_substitution() {
    let root = project_root();
    write_workflow(&root, "alpha", "ALPHA");
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.expect("store"),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (WebSearchConfig::default(), InvocationPolicy::default()),
    )
    .await
    .expect("build engine");
    let engine = built.engine();
    let control = built.workflow_control();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: root.display().to_string(),
        })
        .await
        .expect("create Session");
    control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "alpha".to_string(),
                expected_revision: None,
            },
            CancellationToken::new(),
        )
        .await
        .expect("select alpha");
    std::fs::rename(
        root.join(".hya/workflows/alpha.hya.md"),
        root.join(".hya/workflows/replacement.hya.md"),
    )
    .expect("replace selected source path");

    for name in [None, Some("alpha".to_string())] {
        let error = control
            .execute(
                session,
                WorkflowInvocation::default(),
                WorkflowCommand::Run {
                    name,
                    expected_revision: None,
                    inputs: BTreeMap::from([("request".to_string(), "value".to_string())]),
                    run: Some(WorkflowRunId::new()),
                },
                CancellationToken::new(),
            )
            .await
            .expect_err("selected source substitution must fail closed");
        assert!(matches!(error, WorkflowControlError::NotFound { .. }));
    }

    built.shutdown().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(root);
}

/// A Tool operation admits one durable run, retains the operation id, and
/// returns the terminal projection without another provider call on replay.
#[tokio::test]
async fn run_is_idempotent_by_tool_operation_and_rejects_changed_inputs() {
    let root = project_root();
    write_workflow(&root, "deliver", "DELIVER");
    let provider = hya_provider::FakeProvider::scripted(vec![hya_provider::FakeStep::Text(
        "DELIVERED".to_string(),
    )]);
    let router = hya_provider::ProviderRouter::new().with(std::sync::Arc::new(provider));
    let agent = agent_with_model("fake/model", None);
    let store = SessionStore::connect_memory().await.expect("store");
    let mut built = build_session_engine(
        store,
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (WebSearchConfig::default(), InvocationPolicy::default()),
    )
    .await
    .expect("build engine");
    let engine = built.engine();
    let control = built.workflow_control();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: root.display().to_string(),
        })
        .await
        .expect("create Session");
    let operation = ToolOperation::from_tool_call(ToolCallId::new());
    let invocation = WorkflowInvocation {
        operation: Some(operation),
        binding: Some(engine.bind_runtime(&root).expect("binding")),
        ..WorkflowInvocation::default()
    };
    let command = WorkflowCommand::Run {
        name: Some("deliver".to_string()),
        expected_revision: None,
        inputs: BTreeMap::from([("request".to_string(), "one".to_string())]),
        run: None,
    };

    let first = match control
        .execute(
            session,
            invocation.clone(),
            command.clone(),
            CancellationToken::new(),
        )
        .await
        .expect("first run")
    {
        WorkflowCommandResult::Run { result } => result,
        result => panic!("unexpected run result: {result:?}"),
    };
    assert!(!first.replayed);
    assert_eq!(first.run.status, WorkflowRunStatus::Completed);
    assert_eq!(
        first.run.id,
        WorkflowRunId::from_operation(operation.operation_id())
    );
    assert_eq!(first.run.stages.len(), 1);
    assert_eq!(first.run.stages[0].members.len(), 1);

    let linked_member = first.run.stages[0].members[0].member;
    let events = engine
        .replay(session)
        .await
        .expect("replay Workflow lifecycle");
    let link_index = events
        .iter()
        .position(|envelope| matches!(
            &envelope.event,
            hya_proto::Event::WorkflowStageMemberLinked { member, .. } if *member == linked_member
        ))
        .expect("Workflow member link");
    let spawn_index = events
        .iter()
        .position(|envelope| {
            matches!(
                &envelope.event,
                hya_proto::Event::MemberSpawned { member, .. } if *member == linked_member
            )
        })
        .expect("canonical member spawn");
    assert!(
        link_index < spawn_index,
        "Workflow activity must reference the member before its governed turn starts"
    );
    let replay = match control
        .execute(
            session,
            invocation.clone(),
            command,
            CancellationToken::new(),
        )
        .await
        .expect("idempotent replay")
    {
        WorkflowCommandResult::Run { result } => result,
        result => panic!("unexpected replay result: {result:?}"),
    };
    assert!(replay.replayed);
    assert_eq!(replay.run.id, first.run.id);

    let conflict = control
        .execute(
            session,
            invocation,
            WorkflowCommand::Run {
                name: Some("deliver".to_string()),
                expected_revision: None,
                inputs: BTreeMap::from([("request".to_string(), "changed".to_string())]),
                run: None,
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("changed operation request must conflict");
    assert!(matches!(
        conflict,
        WorkflowControlError::OperationConflict { .. }
    ));
    assert_eq!(conflict.code(), "WORKFLOW_OPERATION_CONFLICT");

    built.shutdown().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(root);
}

/// Write one valid Workflow source directly into a catalog discovery directory.
fn write_catalog_workflow(directory: &Path, name: &str, directive: &str) {
    std::fs::create_dir_all(directory).expect("create catalog Workflow directory");
    std::fs::write(
        directory.join(format!("{name}.hya.md")),
        format!(
            r#"---
kind: Workflow
name: {name}
description: {name} precedence fixture.
nodes:
  run:
    agent: general
    directive: {directive}
---
flowchart TD
  run
"#
        ),
    )
    .expect("write catalog Workflow");
}

/// Build the same first-party prepared payload used by the production catalog.
fn first_party_source() -> BundleSource {
    BundleSource::new(
        "bundles/first-party/plan-impl-review",
        vec![
            SourceFile::new(
                "bundle.yaml",
                include_bytes!("../../../bundles/first-party/plan-impl-review/bundle.yaml"),
            ),
            SourceFile::new(
                "workflows/plan-impl-review.hya.md",
                include_bytes!(
                    "../../../bundles/first-party/plan-impl-review/workflows/plan-impl-review.hya.md"
                ),
            ),
            SourceFile::new(
                "prompts/planner.md",
                include_bytes!("../../../bundles/first-party/plan-impl-review/prompts/planner.md"),
            ),
            SourceFile::new(
                "prompts/implementer.md",
                include_bytes!(
                    "../../../bundles/first-party/plan-impl-review/prompts/implementer.md"
                ),
            ),
            SourceFile::new(
                "prompts/reviewer.md",
                include_bytes!("../../../bundles/first-party/plan-impl-review/prompts/reviewer.md"),
            ),
        ],
    )
}

/// Build a runtime snapshot containing one installed and one first-party catalog.
fn runtime_for_catalogs(
    installed: &PreparedCatalog,
    first_party: &PreparedCatalog,
) -> Arc<RuntimeRegistry> {
    let bundles = BundleCatalog::from_verified_catalogs(&[installed, first_party])
        .expect("verified Workflow catalogs");
    let agents = AgentCatalog::new(Arc::new(bundles)).expect("Workflow Agent catalog");
    Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        Arc::new(agents),
    ))
}

/// Build explicit project/user roots for the immutable Workflow catalog seam.
fn catalog_roots(project: &Path, user: &Path) -> WorkflowCatalogRoots {
    WorkflowCatalogRoots::new(project.join(".hya/workflows"), Some(user.to_path_buf()))
}

/// Catalog resolution honors all source tiers, folds bundle content into
/// revisions, and keeps an admitted binding pinned across publication.
#[tokio::test]
async fn workflow_catalog_precedence_revision_and_binding_contract() {
    let project = project_root();
    let project_workflows = project.join(".hya/workflows");
    let user_workflows = temp_path("catalog-user-workflows");
    write_catalog_workflow(&project_workflows, "collision", "PROJECT");
    write_catalog_workflow(&user_workflows, "collision", "USER");

    let installed_old = prepare_package(workflow_bundle_source_with_prompt(
        "hya/catalog-installed",
        "collision",
        "OLD PROMPT",
    ))
    .expect("prepare installed WorkflowBundle");
    let first_party = prepare_package(first_party_source()).expect("prepare first-party bundle");
    let runtime = runtime_for_catalogs(&installed_old, &first_party);
    let old_binding = runtime
        .bind_turn(&project)
        .expect("bind old runtime snapshot");

    let project_catalog =
        WorkflowCatalog::build(catalog_roots(&project, &user_workflows), &old_binding)
            .expect("build project/user/installed/first-party catalog");
    let project_entry = project_catalog
        .resolve("collision")
        .expect("project bare name must resolve");
    assert!(matches!(
        project_entry.owner(),
        WorkflowCatalogOwner::Project
    ));
    assert_eq!(project_entry.identity().name, "collision");
    assert_eq!(project_entry.workflow().definition().name(), "collision");
    assert!(project_entry.display_path().ends_with("collision.hya.md"));
    assert_eq!(project_entry.workflow().plan().stages().len(), 1);

    let installed_reference = "bundle:hya/catalog-installed/workflow/collision";
    let installed_entry = project_catalog
        .resolve(installed_reference)
        .expect("qualified installed Workflow must bypass bare shadowing");
    assert!(matches!(
        installed_entry.owner(),
        WorkflowCatalogOwner::Installed { bundle_id } if bundle_id == "hya/catalog-installed"
    ));

    let first_party_reference = "bundle:hya/plan-impl-review/workflow/plan-impl-review";
    let first_party_entry = project_catalog
        .resolve("plan-impl-review")
        .expect("first-party bare Workflow must resolve when unambiguous");
    assert!(matches!(
        first_party_entry.owner(),
        WorkflowCatalogOwner::FirstParty { bundle_id } if bundle_id == "hya/plan-impl-review"
    ));
    assert_eq!(
        first_party_entry.identity().source.as_str(),
        first_party_reference
    );
    assert_eq!(
        project_catalog
            .resolve(first_party_reference)
            .expect("qualified first-party Workflow")
            .identity(),
        first_party_entry.identity()
    );

    let prepared_workflow = installed_old.bundles()[0]
        .workflow()
        .expect("installed payload has one Workflow");
    let compiled = hya_workflow::compile(hya_workflow::WorkflowSource::new(
        &prepared_workflow.source_path,
        &prepared_workflow.source,
    ))
    .expect("prepared Workflow source compiles");
    let compiler_only_revision = WorkflowRevision::from_bytes(compiled.revision().as_bytes());
    assert_ne!(
        project_catalog
            .resolve(installed_reference)
            .expect("installed Workflow")
            .identity()
            .revision,
        compiler_only_revision,
        "bundle Workflow revisions must fold the prepared bundle digest"
    );

    std::fs::remove_file(project_workflows.join("collision.hya.md"))
        .expect("remove project precedence fixture");
    let user_catalog =
        WorkflowCatalog::build(catalog_roots(&project, &user_workflows), &old_binding)
            .expect("rebuild catalog after project removal");
    assert!(matches!(
        user_catalog
            .resolve("collision")
            .expect("user bare name must resolve"),
        entry if matches!(entry.owner(), WorkflowCatalogOwner::User)
    ));

    std::fs::remove_file(user_workflows.join("collision.hya.md"))
        .expect("remove user precedence fixture");
    let installed_catalog =
        WorkflowCatalog::build(catalog_roots(&project, &user_workflows), &old_binding)
            .expect("rebuild catalog after user removal");
    assert!(matches!(
        installed_catalog
            .resolve("collision")
            .expect("installed bare name must resolve"),
        entry if matches!(
            entry.owner(),
            WorkflowCatalogOwner::Installed { bundle_id } if bundle_id == "hya/catalog-installed"
        )
    ));

    let installed_new = prepare_package(workflow_bundle_source_with_prompt(
        "hya/catalog-installed",
        "collision",
        "NEW PROMPT",
    ))
    .expect("prepare changed installed WorkflowBundle");
    let new_bundles = BundleCatalog::from_verified_catalogs(&[&installed_new, &first_party])
        .expect("verified replacement Workflow catalogs");
    let new_agents = AgentCatalog::new(Arc::new(new_bundles)).expect("replacement Agent catalog");
    runtime
        .publish_catalog(Arc::new(new_agents))
        .expect("publish replacement catalog");
    let fresh_binding = runtime
        .bind_turn(&project)
        .expect("bind replacement snapshot");
    let pinned_catalog =
        WorkflowCatalog::build(catalog_roots(&project, &user_workflows), &old_binding)
            .expect("build catalog from pinned binding");
    let fresh_catalog =
        WorkflowCatalog::build(catalog_roots(&project, &user_workflows), &fresh_binding)
            .expect("build catalog from replacement binding");
    let pinned_revision = pinned_catalog
        .resolve(installed_reference)
        .expect("old binding keeps installed Workflow")
        .identity()
        .revision;
    let fresh_revision = fresh_catalog
        .resolve(installed_reference)
        .expect("fresh binding sees replacement Workflow")
        .identity()
        .revision;
    assert_ne!(
        pinned_revision, fresh_revision,
        "changed Agent closure must change the folded Workflow revision"
    );
    assert_eq!(
        old_binding
            .bundle_catalog()
            .resolve_workflow(installed_reference)
            .expect("old binding remains pinned")
            .source_path,
        prepared_workflow.source_path
    );

    let control_root = temp_path("first-party-control");
    std::fs::create_dir_all(&control_root).expect("create control project");
    let (router, model) = offline_router(None);
    let agent = agent_with_model(&model, None);
    let mut built = build_session_engine(
        SessionStore::connect_memory().await.expect("control store"),
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (WebSearchConfig::default(), InvocationPolicy::default()),
    )
    .await
    .expect("build first-party control engine");
    let engine = built.engine();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: control_root.display().to_string(),
        })
        .await
        .expect("create first-party control Session");
    let control = built.workflow_control();
    let before = engine
        .replay(session)
        .await
        .expect("replay before catalog load");
    let listed = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::List,
            CancellationToken::new(),
        )
        .await
        .expect("list first-party Workflow")
    {
        WorkflowCommandResult::List { workflows } => workflows,
        result => panic!("unexpected list result: {result:?}"),
    };
    assert!(
        listed
            .iter()
            .any(|workflow| workflow.name == "plan-impl-review")
    );
    let _ = control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Info {
                name: "plan-impl-review".to_string(),
            },
            CancellationToken::new(),
        )
        .await
        .expect("inspect first-party Workflow");
    let state_before_select = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::State,
            CancellationToken::new(),
        )
        .await
        .expect("read state before first-party selection")
    {
        WorkflowCommandResult::State { state } => state,
        result => panic!("unexpected state result: {result:?}"),
    };
    assert!(state_before_select.selection.is_none());
    let after_catalog_load = engine
        .replay(session)
        .await
        .expect("replay after catalog load");
    assert!(
        after_catalog_load.iter().all(|envelope| !matches!(
            &envelope.event,
            hya_proto::Event::WorkflowSelected { .. } | hya_proto::Event::WorkflowRunStarted { .. }
        )),
        "catalog list/info/state must not select or run a Workflow"
    );
    assert_eq!(before.len(), after_catalog_load.len());

    let selected = match control
        .execute(
            session,
            WorkflowInvocation::default(),
            WorkflowCommand::Select {
                name: "plan-impl-review".to_string(),
                expected_revision: None,
            },
            CancellationToken::new(),
        )
        .await
        .expect("select first-party Workflow explicitly")
    {
        WorkflowCommandResult::Selected { state } => state,
        result => panic!("unexpected selection result: {result:?}"),
    };
    let selection = selected.selection.expect("explicit selection");
    assert_eq!(selection.name, "plan-impl-review");
    assert_eq!(selection.source.as_str(), first_party_reference);
    let events = engine
        .replay(session)
        .await
        .expect("replay explicit selection");
    assert_eq!(
        events
            .iter()
            .filter(|envelope| matches!(&envelope.event, hya_proto::Event::WorkflowSelected { .. }))
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|envelope| matches!(&envelope.event, hya_proto::Event::WorkflowRunStarted { .. }))
    );

    built.shutdown().await.expect("shutdown control engine");
    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(user_workflows);
    let _ = std::fs::remove_dir_all(control_root);
}
