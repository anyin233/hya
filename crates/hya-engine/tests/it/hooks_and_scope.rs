//! Consolidated integration tests for `hya-engine`: hook seam, command/text
//! hooks, bundle hook generations, scoped session hook lifecycle, grant scope,
//! session project/kind inheritance, workspace roots, and project activity.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support::{self, TestDir};

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::{AgentRole, BundleCatalog, BundleSource, SourceFile, prepare_package};
use hya_engine::{
    AgentCatalog, CatalogScope, CatalogScopeCacheConfig, ChatParamsInput, ChatParamsOutcome,
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, CoreError, CreateSession, EventBus,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, RuntimeCatalogRefresh,
    RuntimeRegistry, RuntimeSource, RuntimeSourceId, ScopeKey, ScopeOverlay, SessionEngine,
    SessionLifecycleInput, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, ModelRef, PartProjection, ProjectId, Role, SessionId,
    SessionKind, ToolName, ToolPartState, ToolSchema,
};
use hya_provider::{DevProvider, FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    Action, Decision, ExactSubject, GrantScope, InvocationPolicy, InvocationRule, Mode,
    PermissionModel, PermissionPlane, PermissionRules, PermissionTarget, RememberScope, Resource,
    Rule, Tool, ToolCtx, ToolError,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Shared hooks, session, and provider helpers
// ---------------------------------------------------------------------------

type Lineage = (SessionId, SessionId, Option<AgentName>);

#[derive(Default)]
struct HookCounts {
    event: AtomicUsize,
    user_before: AtomicUsize,
    chat_params: AtomicUsize,
    tool_before: AtomicUsize,
    tool_after: AtomicUsize,
    starts: AtomicUsize,
    ends: AtomicUsize,
}

#[derive(Default)]
struct ScopeProbe {
    events: Mutex<HashMap<SessionId, usize>>,
    starts: Mutex<HashMap<SessionId, usize>>,
    ends: Mutex<HashMap<SessionId, usize>>,
    dropped: AtomicBool,
}

impl ScopeProbe {
    fn get(map: &Mutex<HashMap<SessionId, usize>>, s: SessionId) -> usize {
        map.lock().unwrap().get(&s).copied().unwrap_or(0)
    }
    fn bump(map: &Mutex<HashMap<SessionId, usize>>, s: SessionId) {
        *map.lock().unwrap().entry(s).or_default() += 1;
    }
    fn events(&self, s: SessionId) -> usize {
        Self::get(&self.events, s)
    }
    fn starts(&self, s: SessionId) -> usize {
        Self::get(&self.starts, s)
    }
    fn ends(&self, s: SessionId) -> usize {
        Self::get(&self.ends, s)
    }
    fn alive(&self) -> bool {
        !self.dropped.load(Ordering::SeqCst)
    }
}

/// Separate drop guard so [`ScriptedHook`] supports `..Default::default()`.
struct ScopeDropGuard(Arc<ScopeProbe>);

impl Drop for ScopeDropGuard {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct ScriptedHook {
    counts: Arc<HookCounts>,
    lineage: Arc<Mutex<Vec<Lineage>>>,
    scope_probe: Option<ScopeDropGuard>,
    rewrite_bash_to: Option<&'static str>,
    command_expect: Option<(&'static str, &'static str)>,
    command_suffix: Option<&'static str>,
    user_suffix: Option<&'static str>,
    text_replace: Option<(&'static str, &'static str)>,
}

impl ScriptedHook {
    fn for_scope(probe: Arc<ScopeProbe>) -> Self {
        Self {
            scope_probe: Some(ScopeDropGuard(probe)),
            ..Default::default()
        }
    }
}

#[async_trait]
impl HookDispatcher for ScriptedHook {
    fn dispatch_event(&self, envelope: &Envelope) {
        self.counts.event.fetch_add(1, Ordering::SeqCst);
        if let (Some(g), Some(session)) = (&self.scope_probe, envelope.event.session()) {
            ScopeProbe::bump(&g.0.events, session);
        }
    }

    async fn command_execute_before(
        &self,
        mut input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        if let Some((cmd, args)) = self.command_expect {
            assert_eq!(input.command, cmd);
            assert_eq!(input.arguments, args);
        }
        if let Some(suffix) = self.command_suffix {
            input.text.push_str(suffix);
        }
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        let text = match self.text_replace {
            Some((from, to)) if input.text == from => to.to_string(),
            _ => input.text,
        };
        TextCompleteOutcome::Continue { text }
    }

    async fn message_user_before(
        &self,
        mut input: MessageUserBeforeInput,
    ) -> MessageUserBeforeOutcome {
        self.counts.user_before.fetch_add(1, Ordering::SeqCst);
        if let Some(suffix) = self.user_suffix {
            input.text.push_str(suffix);
        }
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        self.counts.chat_params.fetch_add(1, Ordering::SeqCst);
        self.lineage
            .lock()
            .unwrap()
            .push((input.session, input.root_session, input.agent.clone()));
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        self.counts.tool_before.fetch_add(1, Ordering::SeqCst);
        let mut value = input.input;
        if let Some(cmd) = self.rewrite_bash_to
            && input.tool == "bash"
        {
            value["command"] = json!(cmd);
        }
        ToolExecuteBeforeOutcome::Continue { input: value }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        self.counts.tool_after.fetch_add(1, Ordering::SeqCst);
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }

    async fn session_start(&self, input: SessionLifecycleInput) {
        self.counts.starts.fetch_add(1, Ordering::SeqCst);
        if let Some(g) = &self.scope_probe {
            ScopeProbe::bump(&g.0.starts, input.session);
        }
    }

    async fn session_end(&self, input: SessionLifecycleInput) {
        self.counts.ends.fetch_add(1, Ordering::SeqCst);
        if let Some(g) = &self.scope_probe {
            ScopeProbe::bump(&g.0.ends, input.session);
        }
    }
}

fn subdir(dir: &TestDir, child: &str) -> PathBuf {
    let path = dir.path().join(child);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn tool_step(name: &str, input: Value) -> FakeStep {
    FakeStep::ToolCall {
        name: name.to_string(),
        input,
    }
}

fn tool_then_stop(name: &str, input: Value, final_text: Option<&str>) -> Vec<Vec<FakeStep>> {
    let mut second = final_text
        .map(|t| vec![FakeStep::Text(t.to_string())])
        .unwrap_or_default();
    second.push(FakeStep::Finish(FinishReason::Stop));
    vec![
        vec![
            tool_step(name, input),
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        second,
    ]
}

async fn make_project(engine: &SessionEngine, name: &str, roots: &[&Path]) -> ProjectId {
    let paths: Vec<String> = roots.iter().map(|p| path_text(p)).collect();
    engine
        .store()
        .create_project(name, &paths)
        .await
        .unwrap()
        .id
}

async fn create_sess(
    engine: &SessionEngine,
    parent: Option<SessionId>,
    agent: &str,
    workdir: &Path,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new(agent),
            model: ModelRef::new("fake"),
            workdir: path_text(workdir),
            project,
            kind,
        })
        .await
        .expect("create session")
}

async fn proj_sess(
    engine: &SessionEngine,
    agent: &str,
    dir: &Path,
    project: ProjectId,
) -> SessionId {
    create_sess(
        engine,
        None,
        agent,
        dir,
        Some(project),
        SessionKind::Project,
    )
    .await
}

async fn child_sess(
    engine: &SessionEngine,
    parent: SessionId,
    agent: &str,
    dir: &Path,
) -> SessionId {
    create_sess(engine, Some(parent), agent, dir, None, SessionKind::Project).await
}

async fn run_prompt(
    engine: &SessionEngine,
    session: SessionId,
    agent: &str,
    workdir: &Path,
    prompt: &str,
) -> FinishReason {
    engine
        .admit_user_prompt(session, prompt.to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec(agent, "fake", workdir),
            CancellationToken::new(),
        )
        .await
        .unwrap()
}

async fn completed_tool_output(engine: &SessionEngine, session: SessionId, name: &str) -> Value {
    let proj = engine.store().read_projection(session).await.unwrap();
    proj.session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| &m.parts)
        .find_map(|p| match p {
            PartProjection::Tool {
                name: tool,
                state: ToolPartState::Completed { output, .. },
                ..
            } if tool.as_str() == name => Some(output.clone()),
            _ => None,
        })
        .expect("completed tool output")
}

// ---------------------------------------------------------------------------
// Section 1: Hooks seam, command hooks, text_complete, bundle hook generation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_tool_authorizes_after_lookup_and_before_hook_with_call_correlation() {
    let dir = TestDir::new("hooks-auth-order");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            tool_step("missing_tool", json!({})),
            tool_step("bash", json!({ "command": "printf pre-hook" })),
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let policy = InvocationPolicy::compile(
        PermissionModel::Default,
        vec![
            InvocationRule::new(PermissionTarget::Command, "^printf pre-hook$", Mode::Deny),
            InvocationRule::new(PermissionTarget::Command, "^printf post-hook$", Mode::Ask),
        ],
    )
    .unwrap();
    let (permission, mut asks) =
        PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            permission,
            EventBus::default(),
        )
        .with_round_retry_waits(&support::ZERO_RETRY_WAITS)
        .with_hooks(Arc::new(ScriptedHook {
            rewrite_bash_to: Some("printf post-hook"),
            ..Default::default()
        })),
    );
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "run shell".into())
        .await
        .unwrap();

    let runner = Arc::clone(&engine);
    let task_agent = support::agent_spec("hya-main", "fake", dir.path());
    let task = tokio::spawn(async move {
        runner
            .run_turn(session, &task_agent, CancellationToken::new())
            .await
    });
    let req = tokio::time::timeout(Duration::from_secs(1), asks.recv())
        .await
        .expect("permission request timeout")
        .expect("permission request");
    let (corr_sess, corr_msg, corr_call) = (req.session, req.message_id, req.call_id);
    let (remember, resource) = (req.remember.clone(), req.resource.clone());
    req.reply.send(Decision::AllowOnce).unwrap();
    assert_eq!(task.await.unwrap().unwrap(), FinishReason::Stop);

    assert_eq!(corr_sess, Some(session));
    assert!(corr_msg.is_some() && corr_call.is_some());
    assert_eq!(resource, Resource::Command("printf post-hook".into()));
    assert_eq!(
        remember,
        RememberScope::Exact(ExactSubject::new(
            PermissionTarget::Command,
            "printf post-hook"
        ))
    );
    assert!(asks.try_recv().is_err(), "one invocation must prompt once");
}

#[tokio::test]
async fn hooks_fire_once_and_carry_lineage_across_child_sessions() {
    let dir = TestDir::new("hooks-passthrough-lineage");
    let file = dir.path().join("foo.txt");
    tokio::fs::write(&file, "42 lines").await.unwrap();
    let mut turns = tool_then_stop("read", json!({ "path": path_text(&file) }), Some("done"));
    turns.push(vec![
        FakeStep::Text("child".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let counts = Arc::new(HookCounts::default());
    let lineage = Arc::new(Mutex::new(Vec::new()));
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(turns)),
        PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]),
    )
    .await
    .with_hooks(Arc::new(ScriptedHook {
        counts: Arc::clone(&counts),
        lineage: Arc::clone(&lineage),
        ..Default::default()
    }));

    let root = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    let child = child_sess(&engine, root, "hya-scout", dir.path()).await;
    let grandchild = child_sess(&engine, child, "hya-scout", dir.path()).await;

    assert_eq!(
        run_prompt(&engine, root, "hya-main", dir.path(), "read foo.txt").await,
        FinishReason::Stop
    );
    assert_eq!(counts.user_before.load(Ordering::SeqCst), 1);
    assert_eq!(
        counts.chat_params.load(Ordering::SeqCst),
        2,
        "one per provider round"
    );
    assert_eq!(counts.tool_before.load(Ordering::SeqCst), 1);
    assert_eq!(counts.tool_after.load(Ordering::SeqCst), 1);
    assert!(counts.event.load(Ordering::SeqCst) > 0);
    assert_eq!(
        completed_tool_output(&engine, root, "read").await["content"],
        "42 lines"
    );

    assert_eq!(
        run_prompt(&engine, grandchild, "hya-scout", dir.path(), "child turn").await,
        FinishReason::Stop
    );
    assert_eq!(
        lineage.lock().unwrap().as_slice(),
        &[
            (root, root, Some(AgentName::new("hya-main"))),
            (root, root, Some(AgentName::new("hya-main"))),
            (grandchild, root, Some(AgentName::new("hya-scout"))),
        ]
    );
}

#[tokio::test]
async fn tool_after_hook_cannot_mask_denial_and_cleans_rewritten_bash_artifact() {
    // 1. A plugin after-hook must not mask a permission denial into a result.
    let deny_dir = TestDir::new("hooks-after-deny");
    let secret = deny_dir.path().join("foo.txt");
    tokio::fs::write(&secret, "secret").await.unwrap();
    let deny_engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(tool_then_stop(
            "read",
            json!({ "path": path_text(&secret) }),
            Some("done"),
        ))),
        PermissionRules::new(vec![Rule::new(Action::Read, "*", Mode::Deny)]),
    )
    .await
    .with_hooks(Arc::new(support::MaskFirstResult::default()));
    let deny_session =
        support::create_root_session(&deny_engine, "hya-main", "fake", deny_dir.path()).await;
    run_prompt(
        &deny_engine,
        deny_session,
        "hya-main",
        deny_dir.path(),
        "read foo.txt",
    )
    .await;
    let proj = deny_engine
        .store()
        .read_projection(deny_session)
        .await
        .unwrap();
    let assistant = proj
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert!(
        !assistant.parts.iter().any(|p| matches!(
            p,
            PartProjection::Tool { state: ToolPartState::Completed { output, .. }, .. }
                if output.get("masked").is_some()
        )),
        "a plugin must not mask a permission denial into a result"
    );
    assert!(assistant.parts.iter().any(|p| matches!(
        p,
        PartProjection::Tool { name, state: ToolPartState::Error { .. }, .. }
            if name.as_str() == "read"
    )));

    // 2. Rewriting a completed Bash output drops outputPath and removes the pre-hook artifact.
    let bash_dir = TestDir::new("hooks-after-bash-artifact");
    let bash_engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(tool_then_stop(
            "bash",
            json!({ "command": "python3 -c 'import sys; sys.stdout.write(\"A\" * 51201)'" }),
            None,
        ))),
        PermissionRules::new(vec![Rule::new(Action::Bash, "*", Mode::Allow)]),
    )
    .await
    .with_hooks(Arc::new(support::MaskFirstResult::default()));
    let bash_sess =
        support::create_root_session(&bash_engine, "hya-main", "fake", bash_dir.path()).await;
    assert_eq!(
        run_prompt(
            &bash_engine,
            bash_sess,
            "hya-main",
            bash_dir.path(),
            "run Bash"
        )
        .await,
        FinishReason::Stop
    );
    let bash_out = completed_tool_output(&bash_engine, bash_sess, "bash").await;
    assert!(bash_out["metadata"].get("outputPath").is_none());
    let mut artifacts = tokio::fs::read_dir(bash_dir.path().join(".hya/tool-output"))
        .await
        .unwrap();
    assert!(artifacts.next_entry().await.unwrap().is_none());
}

#[tokio::test]
async fn command_execute_before_mutates_text_before_user_message_is_admitted() {
    let engine = support::engine_with_provider(
        Arc::new(DevProvider::new()),
        PermissionRules::new(vec![Rule::new(Action::Read, "*", Mode::Allow)]),
    )
    .await
    .with_hooks(Arc::new(ScriptedHook {
        command_expect: Some(("review", "commit")),
        command_suffix: Some(" with hook context"),
        ..Default::default()
    }));
    let session =
        support::create_root_session(&engine, "build", "hya/offline", Path::new(".")).await;
    engine
        .admit_command_prompt(
            session,
            "review".into(),
            "commit".into(),
            "Review the diff".into(),
        )
        .await
        .unwrap();

    let proj = engine.read_projection(session).await.unwrap();
    let user = proj
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .unwrap();
    assert!(user.parts.iter().any(|p| matches!(
        p,
        PartProjection::Text { text, .. } if text == "Review the diff with hook context"
    )));
    assert!(engine.replay(session).await.unwrap().iter().any(|env| matches!(
        &env.event,
        Event::CommandExecuted { session: s, command, arguments, message }
            if *s == session && command == "review" && arguments == "commit" && *message == user.id
    )));
}

#[tokio::test]
async fn text_complete_replaces_assistant_text_before_projection_finishes() {
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("draft".into()),
            tool_step("bash", json!({"command": "true"})),
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let engine = support::engine_with_provider(
        Arc::new(provider),
        PermissionRules::new(vec![Rule::new(Action::Bash, "**", Mode::Allow)]),
    )
    .await
    .with_hooks(Arc::new(ScriptedHook {
        text_replace: Some(("draft", "final")),
        ..Default::default()
    }));
    let session = support::create_root_session(&engine, "build", "fake", Path::new(".")).await;
    run_prompt(&engine, session, "build", Path::new("."), "say it").await;

    let proj = engine.read_projection(session).await.unwrap();
    let assistant = proj
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert!(
        matches!(assistant.parts.first(), Some(PartProjection::Text { text, .. }) if text == "final")
    );
    assert!(
        assistant
            .parts
            .iter()
            .any(|p| matches!(p, PartProjection::Tool { .. }))
    );
    assert!(
        engine
            .replay(session)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(e.event, Event::TextReplace { .. }))
    );
}

#[tokio::test]
async fn removed_plugin_hooks_leave_fresh_bindings_but_remain_live_on_old_bindings() {
    let prepared = prepare_package(BundleSource::new(
        "plugin",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: Bundle\nidentity: { id: acme/rewrite, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(
            BundleCatalog::from_verified_catalogs(&[&prepared]).unwrap(),
        ))
        .unwrap(),
    );
    let registry =
        support::test_runtime_with_catalog(Arc::new(hya_tool_loader::builtins()), catalog);
    let source_id = RuntimeSourceId::bundle("acme/rewrite");
    let counts = Arc::new(HookCounts::default());
    registry
        .refresh(|c| {
            c.upsert_sources(vec![
                RuntimeSource::new(source_id.clone(), [7; 32], Arc::new(()), Vec::new())
                    .with_hooks(Arc::new(ScriptedHook {
                        counts: Arc::clone(&counts),
                        command_suffix: Some("/old"),
                        user_suffix: Some("/old"),
                        ..Default::default()
                    })),
            ])
        })
        .unwrap();

    let workdir = TestDir::new("bundle-hook-gen");
    let engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new()),
        Arc::clone(&registry),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "hya-main", "fake", workdir.path()).await;
    assert_eq!(counts.starts.load(Ordering::SeqCst), 1);

    let text_of = |proj: &hya_proto::Projection, id| -> String {
        proj.session
            .messages
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .parts
            .iter()
            .filter_map(|p| match p {
                PartProjection::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    };
    let before_events = counts.event.load(Ordering::SeqCst);
    let msg = engine
        .admit_command_prompt(session, "test".into(), String::new(), "body".into())
        .await
        .unwrap();
    assert_eq!(
        text_of(&engine.read_projection(session).await.unwrap(), msg),
        "body/old/old"
    );
    assert!(counts.event.load(Ordering::SeqCst) > before_events);

    let old = registry.bind_turn(workdir.path()).unwrap();
    let old_hooks = old.bundle_hooks_for_agent("hya-main");
    assert_eq!(old_hooks.len(), 1);
    assert!(old.bundle_hooks_for_agent("acme-bundle-agent").is_empty());

    registry
        .refresh(|c| {
            c.remove_sources(&BTreeSet::from([source_id.clone()]));
            Ok(())
        })
        .unwrap();
    assert!(
        registry
            .bind_turn(workdir.path())
            .unwrap()
            .bundle_hooks_for_agent("hya-main")
            .is_empty()
    );

    let fresh_msg = engine
        .admit_user_prompt(session, "fresh".into())
        .await
        .unwrap();
    assert_eq!(
        text_of(&engine.read_projection(session).await.unwrap(), fresh_msg),
        "fresh"
    );

    let MessageUserBeforeOutcome::Continue { text } = old_hooks[0]
        .message_user_before(MessageUserBeforeInput {
            session: SessionId::new(),
            text: "body".into(),
        })
        .await;
    assert_eq!(text, "body/old");
    assert!(engine.delete_session(session).await.unwrap());
    // Fresh admission's bind released the retired source; it receives no `session.end`.
    assert_eq!(counts.ends.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// Section 2: Scoped session hook lifecycle (session_hook_scope)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PluginRefresh {
    version: AtomicUsize,
    published: Mutex<HashMap<ScopeKey, usize>>,
    processes: Mutex<Vec<Arc<ScopeProbe>>>,
}

impl PluginRefresh {
    fn edit_manifest(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }
    fn process(&self, index: usize) -> Arc<ScopeProbe> {
        Arc::clone(&self.processes.lock().unwrap()[index])
    }
    fn spawned(&self) -> usize {
        self.processes.lock().unwrap().len()
    }
}

#[async_trait]
impl RuntimeCatalogRefresh for PluginRefresh {
    async fn refresh_if_changed(&self, _runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        if *scope == CatalogScope::Global {
            return Ok(false);
        }
        let key = scope.key();
        let version = self.version.load(Ordering::SeqCst);
        if self.published.lock().unwrap().get(&key).copied() == Some(version)
            && runtime.scope_overlay(&key).is_some()
        {
            return Ok(false);
        }
        let probe = Arc::new(ScopeProbe::default());
        self.processes.lock().unwrap().push(Arc::clone(&probe));
        let mut overlay =
            ScopeOverlay::new(support::test_catalog(&[("scoped", AgentRole::Main, &[])]));
        overlay.plugin_sources = vec![
            RuntimeSource::new(
                RuntimeSourceId::plugin("project-plugin"),
                [9; 32],
                Arc::new(()),
                Vec::new(),
            )
            .with_hooks(Arc::new(ScriptedHook::for_scope(probe))),
        ];
        runtime
            .publish_scope(key.clone(), overlay)
            .map_err(CoreError::runtime_refresh)?;
        self.published.lock().unwrap().insert(key, version);
        Ok(true)
    }
}

struct ScopeHookHarness {
    engine: SessionEngine,
    refresh: Arc<PluginRefresh>,
    config: Arc<ScopeProbe>,
}

async fn scope_hook_harness(cache: Option<CatalogScopeCacheConfig>) -> ScopeHookHarness {
    let refresh = Arc::new(PluginRefresh::default());
    let config = Arc::new(ScopeProbe::default());
    let mut engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        PermissionRules::default(),
    )
    .await
    .with_hooks(Arc::new(ScriptedHook::for_scope(Arc::clone(&config))))
    .with_catalog_refresh(refresh.clone());
    if let Some(cache) = cache {
        engine = engine.with_catalog_scope_cache(cache);
    }
    ScopeHookHarness {
        engine,
        refresh,
        config,
    }
}

async fn admit_hi(engine: &SessionEngine, session: SessionId) {
    engine
        .admit_user_prompt(session, "hi".into())
        .await
        .unwrap();
}

#[tokio::test]
async fn scoped_plugin_respawn_swaps_active_and_releases_idle_sessions() {
    let dir = TestDir::new("hook-follow-respawn");

    // Part A: Single session swaps cleanly on respawn and emits session.end only on delete.
    let root = subdir(&dir, "a");
    let h = scope_hook_harness(None).await;
    let project_id = make_project(&h.engine, "demo", &[&root]).await;
    let session = proj_sess(&h.engine, "hya-main", &root, project_id).await;
    let old = h.refresh.process(0);
    assert_eq!(old.starts(session), 1);
    admit_hi(&h.engine, session).await;
    assert!(old.events(session) > 0);

    // Unchanged scope never respawns or re-starts.
    admit_hi(&h.engine, session).await;
    assert_eq!(h.refresh.spawned(), 1);
    assert_eq!(old.starts(session), 1);

    h.refresh.edit_manifest();
    let old_events = old.events(session);
    let config_events = h.config.events(session);
    admit_hi(&h.engine, session).await;
    assert_eq!(h.refresh.spawned(), 2);
    let new = h.refresh.process(1);
    assert!(!old.alive(), "retired process exits at swap");
    assert_eq!(old.events(session), old_events);
    assert_eq!(new.starts(session), 1);
    assert_eq!(
        new.events(session),
        h.config.events(session) - config_events
    );
    assert_eq!(old.ends(session), 0, "swap is not a session end");

    admit_hi(&h.engine, session).await;
    assert_eq!(new.starts(session), 1);
    assert_eq!(h.config.starts(session), 1);
    assert!(h.engine.delete_session(session).await.unwrap());
    assert_eq!(new.ends(session), 1);
    assert_eq!(h.config.ends(session), 1);

    // Part B: Another session's bind releases the retired process held by an idle sibling.
    let root_b = subdir(&dir, "b");
    let h2 = scope_hook_harness(None).await;
    let project_b = make_project(&h2.engine, "demo-b", &[&root_b]).await;
    let idle = proj_sess(&h2.engine, "hya-main", &root_b, project_b).await;
    let busy = proj_sess(&h2.engine, "hya-main", &root_b, project_b).await;
    let old_b = h2.refresh.process(0);
    assert!(old_b.starts(idle) == 1 && old_b.starts(busy) == 1);

    h2.refresh.edit_manifest();
    admit_hi(&h2.engine, busy).await;
    let new_b = h2.refresh.process(1);
    assert!(
        !old_b.alive(),
        "idle session must not keep retired process alive"
    );
    assert_eq!(new_b.starts(busy), 1);
    assert_eq!(new_b.starts(idle), 0, "idle session rebinds lazily");

    admit_hi(&h2.engine, idle).await;
    assert_eq!(new_b.starts(idle), 1);
    assert!(new_b.events(idle) > 0);
    admit_hi(&h2.engine, idle).await;
    assert_eq!(new_b.starts(idle), 1);
    assert_eq!(new_b.starts(busy), 1);
}

#[tokio::test]
async fn scope_invalidation_and_idle_eviction_release_captured_session_hooks() {
    let dir = TestDir::new("hook-follow-release");

    // Part A: Explicit invalidation releases idle hooks immediately without waiting for a bind.
    let root_inv = subdir(&dir, "invalidate");
    let h_inv = scope_hook_harness(None).await;
    let id_inv = make_project(&h_inv.engine, "inv", &[&root_inv]).await;
    let sess_inv = proj_sess(&h_inv.engine, "hya-main", &root_inv, id_inv).await;
    let old_inv = h_inv.refresh.process(0);
    assert!(old_inv.alive());

    h_inv.engine.invalidate_catalog_scope(id_inv);
    assert!(!old_inv.alive(), "released without waiting for a bind");

    admit_hi(&h_inv.engine, sess_inv).await;
    let new_inv = h_inv.refresh.process(1);
    assert_eq!(new_inv.starts(sess_inv), 1);
    assert!(new_inv.events(sess_inv) > 0);
    assert_eq!(old_inv.ends(sess_inv), 0);
    assert_eq!(h_inv.config.starts(sess_inv), 1);

    // Part B: Idle TTL eviction releases captured hooks and respawns on next bind.
    let root_evict = subdir(&dir, "evict");
    let h_evict = scope_hook_harness(Some(CatalogScopeCacheConfig {
        max_scopes: 32,
        idle_ttl: Duration::from_millis(35),
    }))
    .await;
    let id_evict = make_project(&h_evict.engine, "evict", &[&root_evict]).await;
    let sess_evict = proj_sess(&h_evict.engine, "hya-main", &root_evict, id_evict).await;
    let old_evict = h_evict.refresh.process(0);
    tokio::time::sleep(Duration::from_millis(70)).await;
    h_evict.engine.sweep_catalog_scopes();
    assert!(
        !h_evict
            .engine
            .runtime_registry()
            .scope_keys()
            .contains(&ScopeKey::Project(id_evict))
    );
    assert!(
        !old_evict.alive(),
        "idle session does not pin evicted scope"
    );

    admit_hi(&h_evict.engine, sess_evict).await;
    assert_eq!(h_evict.refresh.process(1).starts(sess_evict), 1);
}

// ---------------------------------------------------------------------------
// Section 3 & 5 shared ToolCtx probe (grant scope + workspace roots)
// ---------------------------------------------------------------------------

struct ContextSample {
    workdir: PathBuf,
    roots: Vec<PathBuf>,
    grant_scope: Option<GrantScope>,
}

struct ContextProbe(Arc<Mutex<Vec<ContextSample>>>);

#[async_trait]
impl Tool for ContextProbe {
    fn name(&self) -> &str {
        "context_probe"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name()),
            description: "records ToolCtx workdir, roots, and permission grant scope".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        self.0.lock().unwrap().push(ContextSample {
            workdir: ctx.workdir.clone(),
            roots: ctx.roots.clone(),
            grant_scope: ctx.permission.grant_scope(),
        });
        Ok(json!({ "ok": true }))
    }
}

struct ProbeHarness {
    engine: SessionEngine,
    seen: Arc<Mutex<Vec<ContextSample>>>,
}

async fn probe_harness(turns: usize) -> ProbeHarness {
    let scripts: Vec<Vec<FakeStep>> = (0..turns)
        .flat_map(|_| tool_then_stop("context_probe", json!({}), None))
        .collect();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let tools = hya_tool_loader::builtins();
    tools
        .register(Arc::new(ContextProbe(Arc::clone(&seen))))
        .unwrap();
    let engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(scripts)))),
        support::test_runtime(Arc::new(tools)),
        PermissionRules::new(vec![Rule::new(Action::Tool, "context_probe", Mode::Allow)]),
    )
    .await;
    ProbeHarness { engine, seen }
}

async fn run_probe(harness: &ProbeHarness, session: SessionId, workdir: &Path) -> ContextSample {
    assert_eq!(
        run_prompt(&harness.engine, session, "build", workdir, "probe").await,
        FinishReason::Stop
    );
    let sample = harness
        .seen
        .lock()
        .unwrap()
        .pop()
        .expect("context_probe called");
    assert_eq!(
        sample.workdir, workdir,
        "ToolCtx.workdir stays the session workdir"
    );
    sample
}

// ---------------------------------------------------------------------------
// Section 3: Session grant scope (ADR-0026)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grant_scope_uses_project_for_project_roots_and_subagents() {
    let dir = TestDir::new("grant-scope-project");
    let a = subdir(&dir, "a");
    let h = probe_harness(2).await;
    let project_id = make_project(&h.engine, "demo", &[&a]).await;
    let root = proj_sess(&h.engine, "build", &a, project_id).await;
    let child = child_sess(&h.engine, root, "build", &a).await;

    let expected = Some(GrantScope::Project(project_id.to_string()));
    for (label, id) in [("root", root), ("subagent", child)] {
        assert_eq!(run_probe(&h, id, &a).await.grant_scope, expected, "{label}");
    }
}

#[tokio::test]
async fn grant_scope_falls_back_to_session_for_temporary_unassigned_and_deleted_projects() {
    let dir = TestDir::new("grant-scope-fallback");
    let h = probe_harness(3).await;
    let (scratch, unassigned, deleted_dir) = (
        subdir(&dir, "scratch"),
        subdir(&dir, "unassigned"),
        subdir(&dir, "deleted"),
    );
    let deleted_id = make_project(&h.engine, "gone", &[&deleted_dir]).await;
    assert!(h.engine.store().delete_project(deleted_id).await.unwrap());

    for (label, workdir, project, kind) in [
        ("temporary", &scratch, None, SessionKind::Temporary),
        ("no-project", &unassigned, None, SessionKind::Project),
        (
            "deleted-project",
            &deleted_dir,
            Some(deleted_id),
            SessionKind::Project,
        ),
    ] {
        let id = create_sess(&h.engine, None, "build", workdir, project, kind).await;
        assert_eq!(
            run_probe(&h, id, workdir).await.grant_scope,
            Some(GrantScope::Session(id)),
            "{label}"
        );
    }
}

// ---------------------------------------------------------------------------
// Section 4: Session project & kind inheritance (ADR-0024)
// ---------------------------------------------------------------------------

async fn empty_engine() -> SessionEngine {
    support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        PermissionRules::default(),
    )
    .await
}

fn session_spec(
    parent: Option<SessionId>,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> CreateSession {
    CreateSession {
        parent,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: "/tmp/hya-core-session-project".into(),
        project,
        kind,
    }
}

async fn append_root_event(
    engine: &SessionEngine,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> SessionId {
    let root = SessionId::new();
    engine
        .store()
        .append_event(
            root,
            &Event::SessionCreated {
                session: root,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp/hya-core-session-project".into(),
                project,
                kind,
            },
        )
        .await
        .unwrap();
    root
}

#[tokio::test]
async fn root_session_project_and_kind_matrix() {
    let engine = empty_engine().await;
    let project = ProjectId::new();

    for (label, req_project, req_kind) in [
        ("without-project", None, SessionKind::Project),
        ("in-project", Some(project), SessionKind::Project),
        ("temporary", None, SessionKind::Temporary),
    ] {
        let id = engine
            .create(session_spec(None, req_project, req_kind))
            .await
            .unwrap();
        let proj = engine.read_projection(id).await.unwrap();
        assert_eq!(proj.session.project, req_project, "{label}");
        assert_eq!(proj.session.kind, req_kind, "{label}");
    }

    let err = engine
        .create(session_spec(
            None,
            Some(ProjectId::new()),
            SessionKind::Temporary,
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::Invalid(_)),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn subagent_sessions_inherit_parent_project_and_kind() {
    let engine = empty_engine().await;

    let project = ProjectId::new();
    let project_root = append_root_event(&engine, Some(project), SessionKind::Project).await;
    let child = engine
        .create(session_spec(
            Some(project_root),
            Some(ProjectId::new()),
            SessionKind::Project,
        ))
        .await
        .unwrap();
    let grandchild = engine
        .create(session_spec(Some(child), None, SessionKind::Temporary))
        .await
        .unwrap();
    for session in [child, grandchild] {
        let proj = engine.read_projection(session).await.unwrap();
        assert_eq!(proj.session.project, Some(project));
        assert_eq!(proj.session.kind, SessionKind::Project);
    }

    let temp_root = append_root_event(&engine, None, SessionKind::Temporary).await;
    let temp_child = engine
        .create(session_spec(Some(temp_root), None, SessionKind::Project))
        .await
        .unwrap();
    let proj = engine.read_projection(temp_child).await.unwrap();
    assert_eq!(proj.session.project, None);
    assert_eq!(proj.session.kind, SessionKind::Temporary);
}

// ---------------------------------------------------------------------------
// Section 5: Workspace roots resolved at turn start (ADR-0024)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn project_session_roots_respect_order_nested_and_external_workdirs() {
    let dir = TestDir::new("roots-project-matrix");
    let (a, b) = (subdir(&dir, "a"), subdir(&dir, "b"));
    let (nested, outside) = (subdir(&dir, "b/src"), subdir(&dir, "elsewhere"));
    let h = probe_harness(3).await;
    let project_id = make_project(&h.engine, "demo", &[&a, &b]).await;

    for (label, workdir, expected) in [
        ("in-order", &a, vec![a.clone(), b.clone()]),
        ("nested-inside-root", &nested, vec![a.clone(), b.clone()]),
        (
            "outside-prepended",
            &outside,
            vec![outside.clone(), a.clone(), b.clone()],
        ),
    ] {
        let id = proj_sess(&h.engine, "build", workdir, project_id).await;
        assert_eq!(run_probe(&h, id, workdir).await.roots, expected, "{label}");
    }
}

#[tokio::test]
async fn session_roots_fallback_to_workdir_without_active_project() {
    let dir = TestDir::new("roots-fallback-matrix");
    let (a, b, w, scratch) = (
        subdir(&dir, "a"),
        subdir(&dir, "b"),
        subdir(&dir, "w"),
        subdir(&dir, "scratch"),
    );
    let h = probe_harness(3).await;
    let deleted_id = make_project(&h.engine, "deleted", &[&a, &b]).await;
    assert!(h.engine.store().delete_project(deleted_id).await.unwrap());

    for (label, workdir, project, kind) in [
        (
            "deleted-project",
            &a,
            Some(deleted_id),
            SessionKind::Project,
        ),
        ("no-project", &w, None, SessionKind::Project),
        ("temporary", &scratch, None, SessionKind::Temporary),
    ] {
        let id = create_sess(&h.engine, None, "build", workdir, project, kind).await;
        assert_eq!(
            run_probe(&h, id, workdir).await.roots,
            vec![workdir.clone()],
            "{label}"
        );
    }
}

#[tokio::test]
async fn session_roots_inherit_in_subagents_and_refresh_across_turns() {
    let dir = TestDir::new("roots-subagent-refresh");
    let (a, b, c) = (subdir(&dir, "a"), subdir(&dir, "b"), subdir(&dir, "c"));
    let h = probe_harness(3).await;
    let project_id = make_project(&h.engine, "demo", &[&a, &b]).await;
    let root = proj_sess(&h.engine, "build", &a, project_id).await;
    let child = child_sess(&h.engine, root, "build", &a).await;

    assert_eq!(
        run_probe(&h, root, &a).await.roots,
        vec![a.clone(), b.clone()]
    );
    assert_eq!(run_probe(&h, child, &a).await.roots, vec![a.clone(), b]);

    h.engine
        .store()
        .update_project(project_id, None, Some(&[path_text(&a), path_text(&c)]))
        .await
        .unwrap();
    assert_eq!(run_probe(&h, root, &a).await.roots, vec![a, c]);
}

// ---------------------------------------------------------------------------
// Section 6: Read-only project activity tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn project_activity_lists_project_peer_and_created_file_but_not_outsider_or_self() {
    let root = TestDir::new("project-activity");
    let outsider_root = TestDir::new("project-activity-outsider");
    let mut turns = tool_then_stop(
        "write",
        json!({"path": root.path().join("created.txt"), "content": "created"}),
        Some("write complete"),
    );
    turns.extend(tool_then_stop(
        "project_activity",
        json!({}),
        Some("activity complete"),
    ));
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted_turns(turns)),
        PermissionRules::new(vec![Rule::new(Action::Edit, "/**", Mode::Allow)]),
    )
    .await;
    let project_p = make_project(&engine, "P", &[root.path()]).await;
    let project_q = make_project(&engine, "Q", &[outsider_root.path()]).await;

    let a = proj_sess(&engine, "build", root.path(), project_p).await;
    let b = proj_sess(&engine, "build", root.path(), project_p).await;
    let child = create_sess(
        &engine,
        Some(a),
        "build",
        root.path(),
        Some(project_p),
        SessionKind::Project,
    )
    .await;
    let outsider = proj_sess(&engine, "build", outsider_root.path(), project_q).await;

    for id in [b, a] {
        run_prompt(&engine, id, "build", root.path(), "do the requested work").await;
    }

    let output = completed_tool_output(&engine, a, "project_activity").await;
    let sessions = output["sessions"].as_array().expect("sessions");
    let peer = sessions
        .iter()
        .find(|r| r["id"] == b.to_string())
        .expect("peer B");
    assert_eq!(peer["relation"], "unrelated");
    assert_eq!(peer["status"], "idle");
    assert_eq!(peer["lineage_root"], b.to_string());
    assert!(!sessions.iter().any(|r| r["id"] == a.to_string()));
    assert!(!sessions.iter().any(|r| r["id"] == outsider.to_string()));

    let sub = sessions
        .iter()
        .find(|r| r["id"] == child.to_string())
        .expect("subagent child of A");
    assert_eq!(sub["relation"], "child");
    assert_eq!(sub["parent"], a.to_string());
    assert_eq!(sub["lineage_root"], a.to_string());

    let files = output["files"].as_array().expect("files");
    let file = files
        .iter()
        .find(|r| r["session"] == b.to_string())
        .expect("created file");
    assert_eq!(file["change_kind"], "created");
    assert!(
        file["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("created.txt"))
    );
}
