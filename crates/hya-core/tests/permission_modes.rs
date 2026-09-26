//! Session permission modes: `yolo`/`manual` per-call planes, `--yolo`
//! process defaults, subagent inheritance from the root, bundle approvers,
//! and mode switches taking effect inside a running turn.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, BundleSource, SourceFile, prepare_package};
use hya_core::hooks::PermissionApproveInput;
use hya_core::{
    AgentCatalog, AgentSpec, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CreateSession, EventBus, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, RuntimePermissionMode, RuntimeRegistry, RuntimeSource,
    RuntimeSourceId, SessionEngine, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_proto::{AgentName, Envelope, FinishReason, ModelRef, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    AskRequest, Decision, InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, ToolRegistry,
};
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

fn workdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hya-permission-modes-{}", SessionId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn agent(dir: &std::path::Path) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "you are build".to_string(),
        workdir: dir.to_path_buf(),
        reasoning: None,
    }
}

/// One scripted turn: a `bash` call of `command`, then a final text round.
fn bash_turn(command: &str) -> [Vec<FakeStep>; 2] {
    [
        vec![
            FakeStep::ToolCall {
                name: "bash".to_string(),
                input: json!({ "command": command }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]
}

fn provider(commands: &[&str]) -> Arc<ProviderRouter> {
    let turns = commands
        .iter()
        .flat_map(|command| bash_turn(command))
        .collect();
    Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(turns))))
}

/// A process plane whose every `printf` command asks under `model`.
fn process_plane(model: PermissionModel) -> (PermissionPlane, UnboundedReceiver<AskRequest>) {
    let policy = InvocationPolicy::compile(
        model,
        vec![InvocationRule::new(
            PermissionTarget::Command,
            "^printf ",
            Mode::Ask,
        )],
    )
    .unwrap();
    PermissionPlane::new_with_policy(PermissionRules::default(), policy)
}

async fn engine_with(
    commands: &[&str],
    model: PermissionModel,
    runtime: Arc<RuntimeRegistry>,
) -> (Arc<SessionEngine>, UnboundedReceiver<AskRequest>) {
    let (permission, asks) = process_plane(model);
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        provider(commands),
        runtime,
        permission,
        EventBus::default(),
    );
    (Arc::new(engine), asks)
}

async fn create(
    engine: &SessionEngine,
    parent: Option<SessionId>,
    dir: &std::path::Path,
) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap()
}

/// Admit a prompt and run one turn to completion in the background.
fn spawn_turn(
    engine: &Arc<SessionEngine>,
    session: SessionId,
    dir: &std::path::Path,
) -> tokio::task::JoinHandle<FinishReason> {
    let engine = Arc::clone(engine);
    let agent = agent(dir);
    tokio::spawn(async move {
        engine
            .admit_user_prompt(session, "run".to_string())
            .await
            .unwrap();
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap()
    })
}

async fn next_ask(asks: &mut UnboundedReceiver<AskRequest>) -> AskRequest {
    tokio::time::timeout(Duration::from_secs(5), asks.recv())
        .await
        .expect("permission ask timeout")
        .expect("permission ask")
}

async fn finished(task: tokio::task::JoinHandle<FinishReason>) -> FinishReason {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("turn timeout")
        .unwrap()
}

fn builtin_runtime() -> Arc<RuntimeRegistry> {
    support::test_runtime(Arc::new(ToolRegistry::builtins()))
}

#[tokio::test]
async fn yolo_skips_asks_and_manual_asks_again_without_restart() {
    let dir = workdir();
    let (engine, mut asks) = engine_with(
        &["printf one", "printf two"],
        PermissionModel::Default,
        builtin_runtime(),
    )
    .await;
    let session = create(&engine, None, &dir).await;
    assert_eq!(engine.permission_mode(session).await.unwrap(), "manual");

    engine.set_permission_mode(session, "yolo").await.unwrap();
    assert_eq!(engine.permission_mode(session).await.unwrap(), "yolo");
    let projection = engine.read_projection(session).await.unwrap();
    assert_eq!(projection.session.permission_mode.as_deref(), Some("yolo"));
    assert_eq!(
        finished(spawn_turn(&engine, session, &dir)).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err(), "yolo must not ask");

    engine.set_permission_mode(session, "manual").await.unwrap();
    let task = spawn_turn(&engine, session, &dir);
    let ask = next_ask(&mut asks).await;
    assert_eq!(ask.session, Some(session));
    ask.reply.send(Decision::AllowOnce).unwrap();
    assert_eq!(finished(task).await, FinishReason::Stop);
}

#[tokio::test]
async fn danger_process_defaults_to_yolo_and_manual_really_asks() {
    let dir = workdir();
    let (engine, mut asks) = engine_with(
        &["printf one", "printf two"],
        PermissionModel::Danger,
        builtin_runtime(),
    )
    .await;
    let session = create(&engine, None, &dir).await;
    assert_eq!(engine.permission_mode(session).await.unwrap(), "yolo");
    assert_eq!(
        finished(spawn_turn(&engine, session, &dir)).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err(), "--yolo default must not ask");

    engine.set_permission_mode(session, "manual").await.unwrap();
    let task = spawn_turn(&engine, session, &dir);
    next_ask(&mut asks)
        .await
        .reply
        .send(Decision::AllowOnce)
        .unwrap();
    assert_eq!(finished(task).await, FinishReason::Stop);
}

#[tokio::test]
async fn subagent_sessions_inherit_the_root_mode() {
    let dir = workdir();
    let (engine, mut asks) = engine_with(
        &["printf child"],
        PermissionModel::Default,
        builtin_runtime(),
    )
    .await;
    let root = create(&engine, None, &dir).await;
    let child = create(&engine, Some(root), &dir).await;

    // Setting the mode through the child records it on the root.
    engine.set_permission_mode(child, "yolo").await.unwrap();
    let root_projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        root_projection.session.permission_mode.as_deref(),
        Some("yolo")
    );
    let child_projection = engine.read_projection(child).await.unwrap();
    assert_eq!(child_projection.session.permission_mode, None);
    assert_eq!(engine.permission_mode(child).await.unwrap(), "yolo");

    assert_eq!(
        finished(spawn_turn(&engine, child, &dir)).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err(), "the child inherits yolo");
}

#[tokio::test]
async fn switching_mid_turn_applies_to_the_next_permission_check() {
    let dir = workdir();
    // One turn with two tool rounds.
    let turns = vec![
        vec![
            FakeStep::ToolCall {
                name: "bash".to_string(),
                input: json!({ "command": "printf first" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::ToolCall {
                name: "bash".to_string(),
                input: json!({ "command": "printf second" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ];
    let (permission, mut asks) = process_plane(PermissionModel::Default);
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(turns)))),
        builtin_runtime(),
        permission,
        EventBus::default(),
    ));
    let session = create(&engine, None, &dir).await;
    let task = spawn_turn(&engine, session, &dir);
    let first = next_ask(&mut asks).await;
    // Switch while the first call waits, then answer it.
    engine.set_permission_mode(session, "yolo").await.unwrap();
    first.reply.send(Decision::AllowOnce).unwrap();
    assert_eq!(finished(task).await, FinishReason::Stop);
    assert!(
        asks.try_recv().is_err(),
        "the second call in the same turn runs under yolo"
    );
}

#[tokio::test]
async fn unknown_modes_are_rejected() {
    let dir = workdir();
    let (engine, _asks) = engine_with(&[], PermissionModel::Default, builtin_runtime()).await;
    let session = create(&engine, None, &dir).await;
    for mode in ["", "danger", "acme/approver/careful", "careful"] {
        assert!(
            matches!(
                engine.set_permission_mode(session, mode).await,
                Err(hya_core::CoreError::Invalid(_))
            ),
            "{mode:?} must be rejected"
        );
    }
    assert!(
        engine
            .set_permission_mode(SessionId::new(), "yolo")
            .await
            .is_err(),
        "a missing session is rejected"
    );
    let ids = engine
        .permission_modes()
        .await
        .into_iter()
        .map(|mode| (mode.id, mode.source))
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        [
            ("manual".to_string(), "builtin".to_string()),
            ("yolo".to_string(), "builtin".to_string())
        ]
    );
}

/// Approves `printf ok`, defers everything else, and records each input.
struct Approver {
    seen: Arc<Mutex<Vec<PermissionApproveInput>>>,
}

#[async_trait]
impl HookDispatcher for Approver {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn permission_approve(&self, input: PermissionApproveInput) -> Option<Decision> {
        let allow = input.resource == hya_tool::Resource::Command("printf ok".to_string());
        self.seen.lock().unwrap().push(input);
        allow.then_some(Decision::AllowOnce)
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

fn approver_runtime(seen: Arc<Mutex<Vec<PermissionApproveInput>>>) -> Arc<RuntimeRegistry> {
    let prepared = prepare_package(BundleSource::new(
        "approver",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Plugin\nidentity: { id: acme/approver, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .expect("prepare Plugin");
    let bundles = BundleCatalog::from_verified_catalogs(&[&prepared]).expect("bundle catalog");
    let catalog = Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("agent catalog"));
    let registry = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog));
    registry
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/approver"),
                    [9; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(Arc::new(Approver { seen }))
                .with_permission_modes(vec![RuntimePermissionMode {
                    id: "careful".to_string(),
                    title: "Careful".to_string(),
                    description: "Approves printf ok".to_string(),
                }]),
            ])
        })
        .expect("publish approver source");
    registry
}

#[tokio::test]
async fn bundle_mode_approver_decides_and_defer_falls_back_to_the_user() {
    let dir = workdir();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, mut asks) = engine_with(
        &["printf ok", "printf other"],
        PermissionModel::Default,
        approver_runtime(Arc::clone(&seen)),
    )
    .await;
    let root = create(&engine, None, &dir).await;
    let child = create(&engine, Some(root), &dir).await;

    let modes = engine.permission_modes().await;
    let careful = modes
        .iter()
        .find(|mode| mode.id == "acme/approver/careful")
        .expect("bundle mode is listed");
    assert_eq!(careful.source, "acme/approver");
    assert_eq!(careful.title, "Careful");

    engine
        .set_permission_mode(root, "acme/approver/careful")
        .await
        .unwrap();
    assert!(
        matches!(
            engine
                .set_permission_mode(root, "acme/approver/missing")
                .await,
            Err(hya_core::CoreError::Invalid(_))
        ),
        "an undeclared bundle mode is rejected"
    );

    // The approver allows `printf ok` in the child session: nobody is asked.
    assert_eq!(
        finished(spawn_turn(&engine, child, &dir)).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err(), "the approver answered");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].session, child);
        assert_eq!(seen[0].root_session, root);
        assert_eq!(seen[0].mode, "careful");
        assert_eq!(seen[0].agent, Some(AgentName::new("build")));
        assert_eq!(seen[0].action, hya_tool::Action::Bash);
    }

    // It defers on `printf other`, so the user is asked.
    let task = spawn_turn(&engine, child, &dir);
    let ask = next_ask(&mut asks).await;
    assert_eq!(
        ask.resource,
        hya_tool::Resource::Command("printf other".to_string())
    );
    ask.reply.send(Decision::AllowOnce).unwrap();
    assert_eq!(finished(task).await, FinishReason::Stop);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

/// Run the user's own `command` as a direct shell turn, failing if it waits
/// for an ask.
async fn user_shell(
    engine: &SessionEngine,
    session: SessionId,
    dir: &std::path::Path,
    command: &str,
) -> FinishReason {
    let (_message, finish) = tokio::time::timeout(
        Duration::from_secs(5),
        engine.run_shell(
            session,
            &agent(dir),
            command.to_string(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the user's own shell command must not wait for an ask")
    .unwrap();
    finish
}

#[tokio::test]
async fn direct_shell_turns_never_ask_in_any_tree_mode() {
    let dir = workdir();
    let (engine, mut asks) = engine_with(&[], PermissionModel::Default, builtin_runtime()).await;
    let root = create(&engine, None, &dir).await;
    let child = create(&engine, Some(root), &dir).await;

    engine.set_permission_mode(root, "yolo").await.unwrap();
    assert_eq!(
        user_shell(&engine, child, &dir, "printf direct-yolo").await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err(), "yolo shell must not ask");

    engine.set_permission_mode(root, "manual").await.unwrap();
    assert_eq!(
        user_shell(&engine, child, &dir, "printf direct-manual").await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err(), "manual shell must not ask");
}

#[tokio::test]
async fn direct_shell_turns_skip_the_bundle_mode_approver() {
    let dir = workdir();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, mut asks) = engine_with(
        &[],
        PermissionModel::Default,
        approver_runtime(Arc::clone(&seen)),
    )
    .await;
    let root = create(&engine, None, &dir).await;
    engine
        .set_permission_mode(root, "acme/approver/careful")
        .await
        .unwrap();

    // The approver would defer on `printf other`; the user typed it, so it runs.
    assert_eq!(
        user_shell(&engine, root, &dir, "printf other").await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err(), "bundle-mode shell must not ask");
    assert!(
        seen.lock().unwrap().is_empty(),
        "the user's own command short-circuits before the mode approver"
    );
}
