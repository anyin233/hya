//! Integration tests for `hya-core`: ADR-0024 workspace roots resolved at
//! every turn start into `ToolCtx::roots`.

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
    Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError, ToolRegistry,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Every probe call's `(workdir, roots)`.
type Seen = Arc<Mutex<Vec<(PathBuf, Vec<PathBuf>)>>>;

/// Records the workdir and roots of every call it receives.
struct RootsProbe {
    seen: Seen,
}

#[async_trait]
impl Tool for RootsProbe {
    fn name(&self) -> &str {
        "roots_probe"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name()),
            description: "records ToolCtx roots".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        self.seen
            .lock()
            .unwrap()
            .push((ctx.workdir.clone(), ctx.roots.clone()));
        Ok(json!({ "ok": true }))
    }
}

struct Harness {
    engine: SessionEngine,
    seen: Seen,
}

/// An engine whose provider calls `roots_probe` once per turn, for `turns`
/// turns.
async fn harness(turns: usize) -> Harness {
    let mut scripts = Vec::new();
    for _ in 0..turns {
        scripts.push(vec![
            FakeStep::ToolCall {
                name: "roots_probe".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ]);
        scripts.push(vec![FakeStep::Finish(FinishReason::Stop)]);
    }
    let router =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(scripts))));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let tools = ToolRegistry::builtins();
    tools
        .register(Arc::new(RootsProbe {
            seen: Arc::clone(&seen),
        }))
        .unwrap();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Tool,
        "roots_probe",
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

fn path(dir: &support::TestDir, child: &str) -> PathBuf {
    let path = dir.path().join(child);
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

/// Run one turn of `session` and return the roots its probe call saw.
async fn turn_roots(harness: &Harness, session: SessionId, workdir: &Path) -> Vec<PathBuf> {
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
    let (seen_workdir, roots) = harness
        .seen
        .lock()
        .unwrap()
        .pop()
        .expect("roots_probe was called");
    assert_eq!(
        seen_workdir, workdir,
        "ToolCtx.workdir stays the session workdir"
    );
    roots
}

#[tokio::test]
async fn project_session_sees_every_project_root_in_order() {
    let dir = support::TestDir::new("roots-order");
    let (a, b) = (path(&dir, "a"), path(&dir, "b"));
    let harness = harness(1).await;
    let project = harness
        .engine
        .store()
        .create_project("demo", &[text(&a), text(&b)])
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

    assert_eq!(turn_roots(&harness, id, &a).await, vec![a, b]);
}

#[tokio::test]
async fn workdir_inside_a_root_keeps_the_project_roots() {
    let dir = support::TestDir::new("roots-cwd-inside");
    let (a, b) = (path(&dir, "a"), path(&dir, "b"));
    let cwd = path(&dir, "b/src");
    let harness = harness(1).await;
    let project = harness
        .engine
        .store()
        .create_project("demo", &[text(&a), text(&b)])
        .await
        .unwrap();
    let id = session(
        &harness.engine,
        None,
        &cwd,
        Some(project.id),
        SessionKind::Project,
    )
    .await;

    assert_eq!(turn_roots(&harness, id, &cwd).await, vec![a, b]);
}

#[tokio::test]
async fn workdir_outside_every_root_is_prepended() {
    let dir = support::TestDir::new("roots-outside");
    let (a, b) = (path(&dir, "a"), path(&dir, "b"));
    let outside = path(&dir, "elsewhere");
    let harness = harness(1).await;
    let project = harness
        .engine
        .store()
        .create_project("demo", &[text(&a), text(&b)])
        .await
        .unwrap();
    let id = session(
        &harness.engine,
        None,
        &outside,
        Some(project.id),
        SessionKind::Project,
    )
    .await;

    assert_eq!(
        turn_roots(&harness, id, &outside).await,
        vec![outside, a, b]
    );
}

#[tokio::test]
async fn deleted_project_falls_back_to_the_workdir() {
    let dir = support::TestDir::new("roots-deleted");
    let (a, b) = (path(&dir, "a"), path(&dir, "b"));
    let harness = harness(1).await;
    let store = harness.engine.store();
    let project = store
        .create_project("demo", &[text(&a), text(&b)])
        .await
        .unwrap();
    assert!(store.delete_project(project.id).await.unwrap());
    let id = session(
        &harness.engine,
        None,
        &a,
        Some(project.id),
        SessionKind::Project,
    )
    .await;

    assert_eq!(turn_roots(&harness, id, &a).await, vec![a]);
}

#[tokio::test]
async fn session_without_a_project_sees_only_its_workdir() {
    let dir = support::TestDir::new("roots-none");
    let w = path(&dir, "w");
    let harness = harness(1).await;
    let id = session(&harness.engine, None, &w, None, SessionKind::Project).await;

    assert_eq!(turn_roots(&harness, id, &w).await, vec![w]);
}

#[tokio::test]
async fn temporary_session_sees_only_its_workdir() {
    let dir = support::TestDir::new("roots-temporary");
    let scratch = path(&dir, "scratch");
    let harness = harness(1).await;
    let id = session(
        &harness.engine,
        None,
        &scratch,
        None,
        SessionKind::Temporary,
    )
    .await;

    assert_eq!(turn_roots(&harness, id, &scratch).await, vec![scratch]);
}

#[tokio::test]
async fn subagent_session_sees_its_inherited_project_roots() {
    let dir = support::TestDir::new("roots-subagent");
    let (a, b) = (path(&dir, "a"), path(&dir, "b"));
    let harness = harness(1).await;
    let project = harness
        .engine
        .store()
        .create_project("demo", &[text(&a), text(&b)])
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

    assert_eq!(turn_roots(&harness, child, &a).await, vec![a, b]);
}

#[tokio::test]
async fn roots_are_read_fresh_on_the_next_turn() {
    let dir = support::TestDir::new("roots-refresh");
    let (a, b, c) = (path(&dir, "a"), path(&dir, "b"), path(&dir, "c"));
    let harness = harness(2).await;
    let project = harness
        .engine
        .store()
        .create_project("demo", &[text(&a), text(&b)])
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
    assert_eq!(turn_roots(&harness, id, &a).await, vec![a.clone(), b]);

    harness
        .engine
        .store()
        .replace_project_roots(project.id, &[text(&a), text(&c)])
        .await
        .unwrap();

    assert_eq!(turn_roots(&harness, id, &a).await, vec![a, c]);
}
