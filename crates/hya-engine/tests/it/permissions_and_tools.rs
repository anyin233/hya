//! Permission modes (builtin + bundle approvers), tool filtering/variants,
//! nested tool calls, structured tool error payloads, and compaction/lifecycle
//! hooks.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;
use support::resident::{ResidentTurn, run_residents};

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent,
    ResourceView, SourceFile, prepare_package,
};
use hya_engine::hooks::PermissionApproveInput;
use hya_engine::{
    AgentCatalog, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CompactionAfterInput, CompactionBeforeInput, CompactionConfig,
    CompactionDecision, CreateSession, EventBus, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, RuntimePermissionMode, RuntimeRegistry, RuntimeSource,
    RuntimeSourceId, SessionEngine, SummarizeOptions, Summarizer, TextCompleteInput,
    TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome,
};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MessageId, ModelRef, PartProjection, Role, SessionId,
    ToolName, ToolPartState, ToolSchema,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter, ReasoningEffort,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, AskRequest, Decision, InteractionPlane, InvocationPolicy, InvocationRule, Mode,
    PermissionModel, PermissionPlane, PermissionRules, PermissionTarget, Rule, Tool, ToolCtx,
    ToolError, ToolRegistry,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Permission modes & bundle approvers
// ---------------------------------------------------------------------------

fn bash_turn(command: &str) -> [Vec<FakeStep>; 2] {
    [
        vec![
            FakeStep::ToolCall {
                name: "bash".into(),
                input: json!({ "command": command }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]
}

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

async fn mode_engine(
    commands: &[&str],
    model: PermissionModel,
    runtime: Arc<RuntimeRegistry>,
) -> (Arc<SessionEngine>, UnboundedReceiver<AskRequest>) {
    let turns = commands.iter().flat_map(|c| bash_turn(c)).collect();
    let (permission, asks) = process_plane(model);
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(turns)))),
        runtime,
        permission,
        EventBus::default(),
    );
    (Arc::new(engine), asks)
}

fn spawn_mode_turn(
    engine: &Arc<SessionEngine>,
    session: SessionId,
    dir: &Path,
) -> tokio::task::JoinHandle<FinishReason> {
    let engine = Arc::clone(engine);
    let agent = support::agent_spec("hya-main", "fake", dir);
    tokio::spawn(async move {
        engine
            .admit_user_prompt(session, "run".into())
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

async fn join_turn(task: tokio::task::JoinHandle<FinishReason>) -> FinishReason {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("turn timeout")
        .unwrap()
}

struct Approver {
    seen: Arc<Mutex<Vec<PermissionApproveInput>>>,
}

#[async_trait]
impl HookDispatcher for Approver {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn permission_approve(&self, input: PermissionApproveInput) -> Option<Decision> {
        let allow = input.resource == hya_tool::Resource::Command("printf ok".into());
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
            "kind: Bundle\nidentity: { id: acme/approver, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let bundles = BundleCatalog::from_verified_catalogs(&[&prepared]).unwrap();
    let catalog = Arc::new(AgentCatalog::new(Arc::new(bundles)).unwrap());
    let registry = Arc::new(RuntimeRegistry::new(hya_tool_loader::builtins(), catalog));
    registry
        .refresh(|c| {
            c.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/approver"),
                    [9; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(Arc::new(Approver { seen }))
                .with_permission_modes(vec![RuntimePermissionMode {
                    id: "careful".into(),
                    title: "Careful".into(),
                    description: "Approves printf ok".into(),
                }]),
            ])
        })
        .unwrap();
    registry
}

#[tokio::test]
async fn permission_modes_toggle_danger_default_and_reject_unknown() {
    let dir = support::TestDir::new("perm-modes");
    let rt = || support::test_runtime(Arc::new(hya_tool_loader::builtins()));

    // 1. Default model starts manual; yolo skips asks; switching back to manual asks again.
    let (engine, mut asks) = mode_engine(
        &["printf one", "printf two"],
        PermissionModel::Default,
        rt(),
    )
    .await;
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    assert_eq!(engine.permission_mode(session).await.unwrap(), "manual");
    engine.set_permission_mode(session, "yolo").await.unwrap();
    assert_eq!(
        engine
            .read_projection(session)
            .await
            .unwrap()
            .session
            .permission_mode
            .as_deref(),
        Some("yolo")
    );
    assert_eq!(
        join_turn(spawn_mode_turn(&engine, session, dir.path())).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err());

    engine.set_permission_mode(session, "manual").await.unwrap();
    let task = spawn_mode_turn(&engine, session, dir.path());
    let ask = next_ask(&mut asks).await;
    assert_eq!(ask.session, Some(session));
    ask.reply.send(Decision::AllowOnce).unwrap();
    assert_eq!(join_turn(task).await, FinishReason::Stop);

    // 2. Unknown modes & missing session rejected; builtin modes listed.
    for bad in ["", "danger", "acme/approver/careful", "careful"] {
        assert!(matches!(
            engine.set_permission_mode(session, bad).await,
            Err(hya_engine::CoreError::Invalid(_))
        ));
    }
    assert!(
        engine
            .set_permission_mode(SessionId::new(), "yolo")
            .await
            .is_err()
    );
    let ids: Vec<_> = engine
        .permission_modes()
        .await
        .into_iter()
        .map(|m| (m.id, m.source))
        .collect();
    assert_eq!(
        ids,
        [
            ("manual".into(), "builtin".into()),
            ("yolo".into(), "builtin".into())
        ]
    );

    // 3. Danger process defaults to yolo; manual still asks.
    let (engine, mut asks) =
        mode_engine(&["printf one", "printf two"], PermissionModel::Danger, rt()).await;
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    assert_eq!(engine.permission_mode(session).await.unwrap(), "yolo");
    assert_eq!(
        join_turn(spawn_mode_turn(&engine, session, dir.path())).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err());
    engine.set_permission_mode(session, "manual").await.unwrap();
    let task = spawn_mode_turn(&engine, session, dir.path());
    next_ask(&mut asks)
        .await
        .reply
        .send(Decision::AllowOnce)
        .unwrap();
    assert_eq!(join_turn(task).await, FinishReason::Stop);
}

#[tokio::test]
async fn permission_modes_subagent_inheritance_and_mid_turn_switch() {
    let dir = support::TestDir::new("perm-inherit");
    let rt = || support::test_runtime(Arc::new(hya_tool_loader::builtins()));

    // 1. Subagent mode change writes to root and child inherits it.
    let (engine, mut asks) = mode_engine(&["printf child"], PermissionModel::Default, rt()).await;
    let root = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: dir.path().to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine.set_permission_mode(child, "yolo").await.unwrap();
    assert_eq!(
        engine
            .read_projection(root)
            .await
            .unwrap()
            .session
            .permission_mode
            .as_deref(),
        Some("yolo")
    );
    assert_eq!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .permission_mode,
        None
    );
    assert_eq!(engine.permission_mode(child).await.unwrap(), "yolo");
    assert_eq!(
        join_turn(spawn_mode_turn(&engine, child, dir.path())).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err());

    // 2. Switching mode mid-turn applies to the next tool round immediately.
    let turns = vec![
        vec![
            FakeStep::ToolCall {
                name: "bash".into(),
                input: json!({ "command": "printf first" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::ToolCall {
                name: "bash".into(),
                input: json!({ "command": "printf second" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ];
    let (permission, mut asks) = process_plane(PermissionModel::Default);
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(turns)))),
        rt(),
        permission,
        EventBus::default(),
    ));
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    let task = spawn_mode_turn(&engine, session, dir.path());
    let first = next_ask(&mut asks).await;
    engine.set_permission_mode(session, "yolo").await.unwrap();
    first.reply.send(Decision::AllowOnce).unwrap();
    assert_eq!(join_turn(task).await, FinishReason::Stop);
    assert!(asks.try_recv().is_err());
}

#[tokio::test]
async fn bundle_mode_approver_decides_defers_and_direct_shell_bypasses() {
    let dir = support::TestDir::new("perm-approver");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, mut asks) = mode_engine(
        &["printf ok", "printf other"],
        PermissionModel::Default,
        approver_runtime(Arc::clone(&seen)),
    )
    .await;
    let root = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: dir.path().to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let careful = engine
        .permission_modes()
        .await
        .into_iter()
        .find(|m| m.id == "acme/approver/careful")
        .expect("bundle mode listed");
    assert_eq!(
        (careful.source.as_str(), careful.title.as_str()),
        ("acme/approver", "Careful")
    );

    engine
        .set_permission_mode(root, "acme/approver/careful")
        .await
        .unwrap();
    assert!(matches!(
        engine
            .set_permission_mode(root, "acme/approver/missing")
            .await,
        Err(hya_engine::CoreError::Invalid(_))
    ));

    // Approver allows `printf ok` on child without asking; defers `printf other` to user.
    assert_eq!(
        join_turn(spawn_mode_turn(&engine, child, dir.path())).await,
        FinishReason::Stop
    );
    assert!(asks.try_recv().is_err());
    {
        let s = seen.lock().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(
            (s[0].session, s[0].root_session, s[0].mode.as_str()),
            (child, root, "careful")
        );
        assert_eq!(s[0].action, Action::Bash);
    }
    let task = spawn_mode_turn(&engine, child, dir.path());
    let ask = next_ask(&mut asks).await;
    assert_eq!(
        ask.resource,
        hya_tool::Resource::Command("printf other".into())
    );
    ask.reply.send(Decision::AllowOnce).unwrap();
    assert_eq!(join_turn(task).await, FinishReason::Stop);
    assert_eq!(seen.lock().unwrap().len(), 2);

    // Direct shell bypasses both asks and the bundle mode approver in every mode.
    let agent = support::agent_spec("hya-main", "fake", dir.path());
    let before = seen.lock().unwrap().len();
    for mode in ["acme/approver/careful", "manual", "yolo"] {
        engine.set_permission_mode(root, mode).await.unwrap();
        let (_, finish) = engine
            .run_shell(
                child,
                &agent,
                "printf other".into(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(finish, FinishReason::Stop);
        assert!(asks.try_recv().is_err());
    }
    assert_eq!(seen.lock().unwrap().len(), before);
}

// ---------------------------------------------------------------------------
// Tool filtering, nested tools, and structured tool errors
// ---------------------------------------------------------------------------

struct RequestCaptureProvider {
    id: &'static str,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for RequestCaptureProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

async fn capture_request(
    provider_id: &'static str,
    model: &str,
    reasoning: Option<ReasoningEffort>,
    interactive: bool,
) -> CompletionRequest {
    let dir = support::TestDir::new("tool-filter");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (interaction, questions) = InteractionPlane::new();
    if !interactive {
        drop(questions);
    }
    let engine = support::engine_with_provider(
        Arc::new(RequestCaptureProvider {
            id: provider_id,
            requests: Arc::clone(&requests),
        }),
        PermissionRules::default(),
    )
    .await
    .with_interaction(interaction);
    let session = support::create_root_session(&engine, "build", model, dir.path()).await;
    engine
        .admit_user_prompt(session, "go".into())
        .await
        .unwrap();
    let mut agent = support::agent_spec("build", model, dir.path());
    agent.reasoning = reasoning;
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    requests.lock().unwrap()[0].clone()
}

#[tokio::test]
async fn tool_filtering_base_tools_gpt_patch_websearch_human_and_variants() {
    // 1. Base tools replace kernel tools except kernel-owned `rewrite` and `todo`.
    let registry = hya_tool_loader::builtins();
    for tool in hya_engine::kernel::tools::builtin() {
        let name = tool.name();
        if [
            hya_engine::kernel::tools::REWRITE,
            hya_engine::kernel::tools::TODO,
        ]
        .contains(&name)
        {
            assert!(registry.get(name).is_none(), "{name}");
        } else {
            assert_eq!(
                registry.builtin_bundle_origin(name),
                Some("hya/base-tools"),
                "{name}"
            );
        }
    }

    // 2. `apply_patch` offered to GPT-family models only; `websearch` offered for every provider.
    for (provider_id, model, gpt) in [
        ("recording", "openai/gpt-5.5", true),
        ("recording", "x/y/gpt-6.2-sol#high", true),
        ("compat", "anthropic/claude-opus-5-5", false),
        ("anthropic", "glm-5.3", false),
    ] {
        let req = capture_request(provider_id, model, None, false).await;
        let ids: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(ids.contains(&"edit") && ids.contains(&"write") && ids.contains(&"websearch"));
        assert_eq!(ids.contains(&"apply_patch"), gpt, "{model}");
        assert!(!ids.contains(&"patch"));
    }

    // 3. Human-answer tools offered only with an active question receiver.
    for interactive in [false, true] {
        let req = capture_request("recording", "test", None, interactive).await;
        let names: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names.contains(&"ask_user"), interactive);
        assert_eq!(names.contains(&"plan_exit"), interactive);
        assert!(names.contains(&"read"));
    }

    // 4. Model `#variant` overrides agent reasoning; invalid variant sends None.
    assert_eq!(
        capture_request("recording", "gpt-5#low", Some(ReasoningEffort::High), false)
            .await
            .reasoning,
        Some(ReasoningEffort::Low)
    );
    assert_eq!(
        capture_request(
            "recording",
            "gpt-5#bogus",
            Some(ReasoningEffort::High),
            false
        )
        .await
        .reasoning,
        None
    );
}

struct Nester;
#[async_trait]
impl Tool for Nester {
    fn name(&self) -> &str {
        "nester"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("nester"),
            description: "calls another tool".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let listed: Vec<_> = ctx
            .nested
            .list()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect();
        let outcome = match ctx
            .nested
            .call(
                input["name"].as_str().unwrap_or_default(),
                input["input"].clone(),
            )
            .await
        {
            Ok(output) => json!({ "ok": output }),
            Err(error) => {
                let kind = match &error {
                    ToolError::Permission(_) => "permission",
                    ToolError::Input(_) => "input",
                    ToolError::Other(_) => "other",
                    _ => "unexpected",
                };
                json!({ "error": { "kind": kind, "message": error.to_string() } })
            }
        };
        Ok(json!({ "listed": listed, "outcome": outcome }))
    }
}

struct Probe;
#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        "probe"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("probe"),
            description: "reports nested plane".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "connected": ctx.nested.is_connected() }))
    }
}

struct Stater;
#[async_trait]
impl Tool for Stater {
    fn name(&self) -> &str {
        "stater"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("stater"),
            description: "reads workspace state".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        ctx.nested
            .call(
                "write",
                json!({ "path": input["write"], "content": "nested\n" }),
            )
            .await?;
        let first = ctx.nested.workspace_state().await?;
        let second = ctx.nested.workspace_state().await?;
        std::fs::write(input["outside"].as_str().unwrap(), "outside\n").unwrap();
        let third = ctx.nested.workspace_state().await?;
        Ok(json!([first, second, third]))
    }
}

fn nested_catalog() -> Arc<AgentCatalog> {
    let bundle = Bundle {
        check: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/nested-test".into(),
            version: "0.0.0".into(),
            publisher: "hya-tests".into(),
        },
        namespace: None,
        digest: "test-only".into(),
        agents: vec![PreparedAgent {
            private: false,
            id: AgentName::new("nesting-agent"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("nesting prompt".into()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView {
                allow: vec![
                    "harness:tool/nester".into(),
                    "harness:tool/probe".into(),
                    "harness:tool/stater".into(),
                    "harness:tool/write".into(),
                    "harness:tool/ask_user".into(),
                    "harness:tool/plan_exit".into(),
                ],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
            advertise: Vec::new(),
            handoff: hya_bundle::HandoffPolicy::Model,
        }],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        tui: None,
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        permission_modes: vec![],
    };
    let bundles = BundleCatalog::from_prepared(&[bundle]).unwrap();
    Arc::new(AgentCatalog::new(Arc::new(bundles)).unwrap())
}

fn nested_tools() -> ToolRegistry {
    let tools = hya_tool_loader::builtins();
    tools.register(Arc::new(Nester)).unwrap();
    tools.register(Arc::new(Probe)).unwrap();
    tools.register(Arc::new(Stater)).unwrap();
    tools
}

#[tokio::test]
async fn nested_calls_run_in_callers_view_and_record_against_caller() {
    let workdir = support::TestDir::new("nested-tools");
    let written = workdir.path().join("a.txt");
    let denied = workdir.path().join("denied.txt");
    let nest = |name: &str, input: Value| FakeStep::ToolCall {
        name: "nester".into(),
        input: json!({ "name": name, "input": input }),
    };
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            nest("write", json!({ "path": written, "content": "nested\n" })),
            nest("glob", json!({ "pattern": "*" })),
            nest("nester", json!({})),
            nest("write", json!({ "path": denied, "content": "no\n" })),
            nest("probe", json!({})),
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(nested_tools(), nested_catalog())),
        PermissionRules::new(vec![
            Rule::new(Action::Tool, "nester", Mode::Allow),
            Rule::new(Action::Tool, "probe", Mode::Allow),
            Rule::new(Action::Edit, "**", Mode::Allow),
            Rule::new(Action::Edit, denied.to_string_lossy(), Mode::Deny),
        ]),
    )
    .await;
    let session =
        support::create_root_session(&engine, "nesting-agent", "fake", workdir.path()).await;
    engine
        .admit_user_prompt(session, "nest".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("nesting-agent", "fake", workdir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );

    let events: Vec<_> = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    let calls: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::ToolCallRequested { call, name, .. } => Some((*call, name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 5);
    let results: BTreeMap<_, _> = events
        .iter()
        .filter_map(|e| match e {
            Event::ToolResult { call, output, .. } => Some((*call, output.clone())),
            _ => None,
        })
        .collect();
    let outcome = |i: usize| results[&calls[i].0]["outcome"].clone();
    let listed: Vec<&str> = results[&calls[0].0]["listed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(listed.contains(&"write") && listed.contains(&"probe"));
    for excluded in ["nester", "glob", "invalid", "ask_user", "plan_exit"] {
        assert!(!listed.contains(&excluded), "{listed:?}");
    }
    assert!(outcome(0).get("ok").is_some());
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "nested\n");
    assert!(events.iter().any(|e| matches!(e, Event::FilesChanged { call, files, .. } if *call == Some(calls[0].0) && files[0].path == written.to_string_lossy())));
    assert_eq!(outcome(1)["error"]["message"], "unknown tool: glob");
    assert_eq!(outcome(2)["error"]["kind"], "input");
    assert_eq!(outcome(3)["error"]["kind"], "permission");
    assert!(!denied.exists());
    assert_eq!(outcome(4)["ok"]["connected"], false);
}

#[tokio::test]
async fn nested_workspace_state_is_sessions_file_revision() {
    let workdir = support::TestDir::new("nested-state");
    let written = workdir.path().join("a.txt");
    let outside = workdir.path().join("b.txt");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "stater".into(),
                input: json!({ "write": written, "outside": outside }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(nested_tools(), nested_catalog())),
        PermissionRules::new(vec![
            Rule::new(Action::Tool, "stater", Mode::Allow),
            Rule::new(Action::Edit, "**", Mode::Allow),
        ]),
    )
    .await;
    let session =
        support::create_root_session(&engine, "nesting-agent", "fake", workdir.path()).await;
    engine
        .admit_user_prompt(session, "state".into())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec("nesting-agent", "fake", workdir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let events: Vec<_> = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    let states = events
        .iter()
        .find_map(|e| match e {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    if cfg!(target_os = "linux") {
        let [s0, s1, s2] = [0, 1, 2].map(|i| states[i].as_str().unwrap().to_string());
        assert!(s0.starts_with(&format!("{session}:")));
        assert_eq!(s0, s1);
        assert_ne!(s1, s2);
        let external: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::FilesObserved {
                    external: true,
                    files,
                    ..
                } => Some(files),
                _ => None,
            })
            .collect();
        assert_eq!(external.len(), 1);
        assert_eq!(external[0][0].path, outside.to_string_lossy());
    } else {
        assert_eq!(states, json!([null, null, null]));
    }
}

#[tokio::test]
async fn tool_errors_record_structured_payload_for_provider_replay() {
    let dir = support::TestDir::new("tool-err-payload");
    let blocked = dir.path().join("blocked.txt");
    tokio::fs::write(&blocked, "secret").await.unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".into(),
                input: json!({ "path": blocked }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let engine = support::engine_with_provider(
        Arc::new(provider),
        PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Deny)]),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "read".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let proj = engine.store().read_projection(session).await.unwrap();
    let (msg, val) = proj
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap()
        .parts
        .iter()
        .find_map(|p| match p {
            PartProjection::Tool {
                state: ToolPartState::Error { message, value, .. },
                ..
            } => Some((message, value.as_ref().expect("structured error"))),
            _ => None,
        })
        .expect("tool error");
    assert!(msg.contains("permission denied"));
    assert_eq!(val["error"]["type"], "permission");
    assert!(
        val["error"]["message"]
            .as_str()
            .unwrap()
            .contains("permission denied")
    );
}

// ---------------------------------------------------------------------------
// Compaction & session/agent lifecycle hooks
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CompactionTestHook {
    decisions: Mutex<Vec<CompactionDecision>>,
    calls: Mutex<Vec<String>>,
}

impl CompactionTestHook {
    fn with_decisions(decisions: Vec<CompactionDecision>) -> Arc<Self> {
        Arc::new(Self {
            decisions: Mutex::new(decisions),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl HookDispatcher for CompactionTestHook {
    fn dispatch_event(&self, _envelope: &Envelope) {}

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

    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        self.calls
            .lock()
            .unwrap()
            .push(format!("compaction.before:{:?}", input.trigger));
        self.decisions
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(CompactionDecision::Proceed)
    }

    async fn compaction_after(&self, input: CompactionAfterInput) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("compaction.after:{}", input.summary_tokens));
    }

    async fn session_start(&self, input: hya_engine::SessionLifecycleInput) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("session.start:{}", input.session));
    }

    async fn session_end(&self, input: hya_engine::SessionLifecycleInput) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("session.end:{}", input.session));
    }

    async fn agent_spawn(&self, input: hya_engine::AgentSpawnInput) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("agent.spawn:{}:{}", input.parent, input.child));
    }
}

struct NoSummarizer;

#[async_trait]
impl Summarizer for NoSummarizer {
    async fn summarize(
        &self,
        _messages: &[hya_proto::Message],
        _options: SummarizeOptions,
    ) -> Result<String, hya_engine::CoreError> {
        Err(hya_engine::CoreError::Invalid("no summarizer".into()))
    }
}

async fn compacting_engine(hooks: Arc<dyn HookDispatcher>) -> Arc<SessionEngine> {
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("detail across the earlier turn ".repeat(7_000)),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("second".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ])
    .with_max_context(40_000);
    Arc::new(
        support::engine_with_provider(Arc::new(provider), PermissionRules::default())
            .await
            .with_hooks(hooks),
    )
}

async fn setup_overflow_session(engine: &SessionEngine, dir: &Path) -> SessionId {
    let session = support::create_root_session(engine, "build", "fake", dir).await;
    let agent = support::agent_spec("build", "fake", dir);
    engine
        .admit_user_prompt(session, "PROMPT_0".into())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "PROMPT_1".into())
        .await
        .unwrap();
    session
}

#[tokio::test]
async fn compaction_hooks_surround_rewrite_and_ignore_overflow_skip() {
    for (decision, check_calls) in [
        (
            CompactionDecision::Replace {
                instructions: "SUMMARIZE".into(),
            },
            true,
        ),
        (
            CompactionDecision::Skip {
                reason: "skip overflow".into(),
            },
            false,
        ),
    ] {
        let dir = support::TestDir::new("compact-hook");
        let hooks = CompactionTestHook::with_decisions(vec![decision]);
        let engine = compacting_engine(hooks.clone()).await;
        let session = setup_overflow_session(&engine, dir.path()).await;
        engine
            .run_turn(
                session,
                &support::agent_spec("build", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let rewrites = engine
            .replay(session)
            .await
            .unwrap()
            .iter()
            .filter(|e| {
                matches!(
                    e.event,
                    Event::ContextCompacted {
                        strategy: hya_proto::CompactionStrategy::Rewrite,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(rewrites, 1);
        if check_calls {
            let calls: Vec<_> = hooks
                .calls()
                .into_iter()
                .filter(|c| c.starts_with("compaction."))
                .collect();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0], "compaction.before:Overflow");
            assert!(calls[1].starts_with("compaction.after:"));
        }
    }
}

#[tokio::test]
async fn session_lifecycle_and_agent_spawn_hooks_fire_best_effort() {
    let dir = support::TestDir::new("lifecycle-hooks");
    let hooks = Arc::new(CompactionTestHook::default());
    let engine = Arc::new(
        support::engine_with_provider(
            Arc::new(FakeProvider::scripted(vec![
                FakeStep::Text("MEMBERTEXT".into()),
                FakeStep::Finish(FinishReason::Stop),
            ])),
            PermissionRules::default(),
        )
        .await
        .with_compaction(Arc::new(NoSummarizer), CompactionConfig::default())
        .with_hooks(hooks.clone()),
    );
    let lead = support::create_root_session(&engine, "hya-task", "fake", dir.path()).await;
    assert!(hooks.calls().contains(&format!("session.start:{lead}")));

    let agent = support::agent_spec("hya-task", "fake", dir.path());
    let turn = ResidentTurn {
        agent: agent.clone(),
        binding: engine.bind_runtime(&agent.workdir).unwrap(),
        agents: Arc::from([]),
        resources: None,
        guidance: None,
        directive: "small task".into(),
        description: String::new(),
        sidecar_factory: None,
        tool_call: None,
    };
    run_residents(engine.clone(), lead, vec![turn], CancellationToken::new()).await;
    let child = engine.read_projection(lead).await.unwrap().session.members[0]
        .child
        .unwrap();
    assert!(
        hooks
            .calls()
            .contains(&format!("agent.spawn:{lead}:{child}"))
    );

    engine.archive_session(lead).await.unwrap();
    assert!(hooks.calls().contains(&format!("session.end:{lead}")));
}

#[tokio::test]
async fn compaction_commit_is_atomic_single_transaction() {
    use sqlx::Connection;

    // 1. Failing commit trigger rolls back the marker atomically and fails the turn.
    let dir = support::TestDir::new("compact-atomic");
    let hooks = CompactionTestHook::with_decisions(Vec::new());
    let engine = compacting_engine(hooks).await;
    let session = setup_overflow_session(&engine, dir.path()).await;
    let mut tx = engine.store().begin_transaction().await.unwrap();
    sqlx::query("CREATE TRIGGER reject_compaction BEFORE INSERT ON event_log WHEN NEW.payload LIKE '%context_compacted%' BEGIN SELECT RAISE(ABORT, 'compaction commit rejected'); END")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let err = engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("compaction commit rejected"));
    let events = engine.replay(session).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, Event::ContextCompacted { .. }))
    );
    assert!(!events.iter().any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains(hya_provider::COMPACT_CONTEXT_MARKER))));

    // 2. `/summarize` commits marker + ContextCompacted in one writer transaction.
    let dir2 = support::TestDir::new("compact-tx");
    let db = dir2.path().join("events.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    let mut observer = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        store,
        Arc::new(
            ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![vec![
                FakeStep::Text("answer".into()),
                FakeStep::Finish(FinishReason::Stop),
            ]]))),
        ),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        perm,
        EventBus::default(),
    );
    let session = support::create_root_session(&engine, "build", "fake", dir2.path()).await;
    engine
        .admit_user_prompt(session, "summarize".into())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir2.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("PRAGMA data_version")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    let marker = engine.summarize_session(session).await.unwrap();
    let after: i64 = sqlx::query_scalar("PRAGMA data_version")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert_eq!(after - before, 1);
    assert!(
        engine
            .replay(session)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                e.event,
                Event::ContextCompacted { message, .. } if message == marker
            ))
    );
}
