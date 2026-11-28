//! App-level Workflow list, info, selection, idempotency, and scope contracts
//! driven through `hya/workflow`'s native BundleApi and `workflow` tool
//! surfaces, read as raw JSON (core has no Workflow types).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::{InvocationPolicy, agent_with_model, build_session_engine, offline_router};
use hya_engine::{ApiMethod, BundleApiCall, CreateSession, SessionEngine};
use hya_proto::{SessionId, ToolCallId};
use hya_store::SessionStore;
use hya_tool::extension::ExtensionEvent;
use hya_tool::{
    InteractionPlane, PermissionModel, PermissionPlane, PermissionRules, ToolCtx, ToolOperation,
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

/// A fresh run id in the bundle's wire form.
fn run_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// The `name`s of a `workflows` list.
fn names(workflows: &Value) -> Vec<&str> {
    workflows
        .as_array()
        .unwrap()
        .iter()
        .map(|workflow| workflow["name"].as_str().unwrap())
        .collect()
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

async fn wf_list_global(engine: &SessionEngine, directory: Option<&Path>) -> Value {
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
    outcome.body["workflows"].clone()
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
    let workflows = &body["list"]["workflows"];
    assert_eq!(names(workflows), ["alpha", "beta", "plan-impl-review"]);
    assert!(
        workflows
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["revision"].is_string())
    );

    let (status, body) = wf_command(&engine, session, json!({ "info": { "name": "alpha" } })).await;
    assert_eq!(status, 200, "{body}");
    let alpha = body["info"].clone();
    assert_eq!(alpha["stages"].as_array().unwrap().len(), 1);
    let alpha_revision = alpha["identity"]["revision"].as_str().unwrap().to_owned();

    let (status, body) = wf_command(
        &engine,
        session,
        json!({
            "select": {
                "name": "alpha",
                "expectedRevision": alpha_revision,
            }
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["selected"]["availability"], "available");

    // Editing alpha invalidates revision fence -> 409 failed_precondition.
    write_project_workflow(&root, "alpha", "CHANGED");
    let (status, body) = wf_command(
        &engine,
        session,
        json!({
            "select": {
                "name": "alpha",
                "expectedRevision": alpha_revision,
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
                "run": run_id(),
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
    assert_eq!(body["state"]["availability"], "unavailable");
    for name in [Value::Null, json!("alpha")] {
        let mut run = serde_json::Map::new();
        if !name.is_null() {
            run.insert("name".to_owned(), name);
        }
        run.insert("inputs".to_owned(), json!({ "request": "v" }));
        run.insert("run".to_owned(), json!(run_id()));
        run.insert("delivery".to_owned(), json!("finished"));
        let (status, body) = wf_command(&engine, session, json!({ "run": run })).await;
        assert_eq!(status, 404, "{body}");
        assert_eq!(body["error"]["code"], "not_found");
    }

    // Switching selection to beta succeeds cleanly.
    let (status, body) =
        wf_command(&engine, session, json!({ "select": { "name": "beta" } })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["selected"]["selection"]["name"], "beta");

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
        let mut ctx = ToolCtx::new(
            root.clone(),
            permission.for_session(session),
            CancellationToken::new(),
        );
        ctx.interaction = interaction.for_session(session);
        ctx.operation = op;
        ctx.session = Some(session);
        ctx
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
    let first = &first_out["result"];
    assert_eq!(first_out["kind"], "run", "{first_out}");
    assert_eq!(first["replayed"], false);
    assert_eq!(first["run"]["status"], "completed");
    // The run id is the operation's UUIDv5 under the Workflow run namespace.
    let namespace = uuid::Uuid::from_u128(0x19aa_6124_335d_54fb_b7e1_c271_82d6_6638);
    assert_eq!(
        first["run"]["id"],
        uuid::Uuid::new_v5(&namespace, op.operation_id().as_uuid().as_bytes()).to_string()
    );

    // Member link precedes governed resident turn start.
    let linked = &first["run"]["stages"][0]["members"][0]["member"];
    let events = engine.replay(session).await.unwrap();
    let link_idx = events
        .iter()
        .position(|e| {
            matches!(&e.event, hya_proto::Event::Extension { bundle, kind, payload, .. }
                if bundle == "hya/workflow"
                    && kind == "workflow_stage_member_linked"
                    && &payload["member"] == linked)
        })
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
    assert_eq!(replay_out["result"]["replayed"], true);
    assert_eq!(replay_out["result"]["run"]["id"], first["run"]["id"]);

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

#[tokio::test]
async fn listing_never_selects_a_first_party_workflow_and_select_persists_it() {
    let _env = crate::support::env_read_lock().await;
    let project = temp_dir("first-party");
    std::fs::create_dir_all(&project).unwrap();
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
    assert_eq!(
        body["selected"]["selection"]["source"],
        "bundle:hya/workflow/workflow/plan-impl-review"
    );

    built.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(project);
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

    // Standalone `list(Some(dir))` vs `list(None)` and multi-root Project
    // session selection (catalog dedupe rules: `native/core/tests/catalog.rs`).
    let (mut built, agent) = offline_built().await;
    let engine = built.engine();
    assert_eq!(
        names(&wf_list_global(&engine, Some(&root_a)).await),
        ["root-a-same", "shared-name", "plan-impl-review"]
    );
    assert_eq!(
        names(&wf_list_global(&engine, None).await),
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
    assert_eq!(
        names(&body["list"]["workflows"]),
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
    assert_eq!(body["selected"]["availability"], "available");
    let (status, body) = wf_command(&engine, session, json!({ "state": {} })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"]["availability"], "available");

    built.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(root_a);
    let _ = std::fs::remove_dir_all(root_b);
}
