//! Integration tests for `hya-core`: ADR-0026 scope of an "allow always"
//! grant. A Project session (and its subagents) remember grants for the
//! Project; a temporary, Project-less, or deleted-Project session remembers
//! them for itself only.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, FinishReason, ModelRef, ProjectId, SessionId, SessionKind, ToolName, ToolSchema,
};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    Action, GrantScope, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError,
    ToolRegistry,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

type Seen = Arc<Mutex<Vec<Option<GrantScope>>>>;

/// Records the grant scope of every call's permission plane.
struct ScopeProbe {
    seen: Seen,
}

#[async_trait]
impl Tool for ScopeProbe {
    fn name(&self) -> &str {
        "scope_probe"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name()),
            description: "records the permission grant scope".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        self.seen.lock().unwrap().push(ctx.permission.grant_scope());
        Ok(json!({ "ok": true }))
    }
}

struct Harness {
    engine: SessionEngine,
    seen: Seen,
}

async fn harness() -> Harness {
    let scripts = vec![
        vec![
            FakeStep::ToolCall {
                name: "scope_probe".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ];
    let router =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(scripts))));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let tools = ToolRegistry::builtins();
    tools
        .register(Arc::new(ScopeProbe {
            seen: Arc::clone(&seen),
        }))
        .unwrap();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "scope_probe",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::test_runtime(Arc::new(tools)),
        permission,
        EventBus::default(),
    );
    Harness { engine, seen }
}

fn dir(test: &support::TestDir, child: &str) -> PathBuf {
    let path = test.path().join(child);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

async fn session(
    engine: &SessionEngine,
    parent: Option<SessionId>,
    workdir: &Path,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: text(workdir),
            project,
            kind,
        })
        .await
        .unwrap()
}

async fn turn_scope(harness: &Harness, session: SessionId, workdir: &Path) -> Option<GrantScope> {
    harness
        .engine
        .admit_user_prompt(session, "probe".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: workdir.to_path_buf(),
        reasoning: None,
    };
    let finish = harness
        .engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    harness
        .seen
        .lock()
        .unwrap()
        .pop()
        .expect("scope_probe was called")
}

#[tokio::test]
async fn project_session_remembers_for_its_project() {
    let test = support::TestDir::new("grant-scope-project");
    let a = dir(&test, "a");
    let harness = harness().await;
    let project = harness
        .engine
        .store()
        .create_project("demo", &[text(&a)])
        .await
        .unwrap();
    let id = session(
        &harness.engine,
        None,
        &a,
        Some(project.id),
        SessionKind::Project,
    )
    .await;

    assert_eq!(
        turn_scope(&harness, id, &a).await,
        Some(GrantScope::Project(project.id.to_string()))
    );
}

#[tokio::test]
async fn subagent_remembers_for_its_inherited_project() {
    let test = support::TestDir::new("grant-scope-subagent");
    let a = dir(&test, "a");
    let harness = harness().await;
    let project = harness
        .engine
        .store()
        .create_project("demo", &[text(&a)])
        .await
        .unwrap();
    let root = session(
        &harness.engine,
        None,
        &a,
        Some(project.id),
        SessionKind::Project,
    )
    .await;
    let child = session(&harness.engine, Some(root), &a, None, SessionKind::Project).await;

    assert_eq!(
        turn_scope(&harness, child, &a).await,
        Some(GrantScope::Project(project.id.to_string()))
    );
}

#[tokio::test]
async fn temporary_session_remembers_for_itself() {
    let test = support::TestDir::new("grant-scope-temporary");
    let scratch = dir(&test, "scratch");
    let harness = harness().await;
    let id = session(
        &harness.engine,
        None,
        &scratch,
        None,
        SessionKind::Temporary,
    )
    .await;

    assert_eq!(
        turn_scope(&harness, id, &scratch).await,
        Some(GrantScope::Session(id))
    );
}

#[tokio::test]
async fn session_without_a_project_remembers_for_itself() {
    let test = support::TestDir::new("grant-scope-none");
    let w = dir(&test, "w");
    let harness = harness().await;
    let id = session(&harness.engine, None, &w, None, SessionKind::Project).await;

    assert_eq!(
        turn_scope(&harness, id, &w).await,
        Some(GrantScope::Session(id))
    );
}

#[tokio::test]
async fn deleted_project_session_remembers_for_itself() {
    let test = support::TestDir::new("grant-scope-deleted");
    let a = dir(&test, "a");
    let harness = harness().await;
    let store = harness.engine.store();
    let project = store.create_project("demo", &[text(&a)]).await.unwrap();
    assert!(store.delete_project(project.id).await.unwrap());
    let id = session(
        &harness.engine,
        None,
        &a,
        Some(project.id),
        SessionKind::Project,
    )
    .await;

    assert_eq!(
        turn_scope(&harness, id, &a).await,
        Some(GrantScope::Session(id))
    );
}
