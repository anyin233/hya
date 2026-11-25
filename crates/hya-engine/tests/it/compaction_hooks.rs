//! Integration tests for `hya-engine`: the P5 injection-point hooks wired into
//! the engine — `compaction.before`/`compaction.after` around the kernel's
//! context reduction (fail-open), and best-effort `session.start`/
//! `session.end`/`agent.spawn` notifications.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::support;
use support::resident::{ResidentTurn, run_residents};

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_engine::{
    AgentSpec, CompactionAfterInput, CompactionBeforeInput, CompactionConfig, CompactionDecision,
    CreateSession, EventBus, HookDispatcher, SessionEngine, SummarizeOptions, Summarizer,
};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use tokio_util::sync::CancellationToken;

/// Records hook invocations and answers `compaction.before` from a scripted
/// decision queue (defaulting to `Proceed`).
#[derive(Default)]
struct ScriptedHook {
    decisions: Mutex<Vec<CompactionDecision>>,
    calls: Mutex<Vec<String>>,
}

impl ScriptedHook {
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
impl HookDispatcher for ScriptedHook {
    fn dispatch_event(&self, _envelope: &hya_proto::Envelope) {}

    async fn command_execute_before(
        &self,
        input: hya_engine::CommandExecuteBeforeInput,
    ) -> hya_engine::CommandExecuteBeforeOutcome {
        hya_engine::CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(
        &self,
        input: hya_engine::TextCompleteInput,
    ) -> hya_engine::TextCompleteOutcome {
        hya_engine::TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(
        &self,
        input: hya_engine::MessageUserBeforeInput,
    ) -> hya_engine::MessageUserBeforeOutcome {
        hya_engine::MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(
        &self,
        input: hya_engine::ChatParamsInput,
    ) -> hya_engine::ChatParamsOutcome {
        hya_engine::ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(
        &self,
        input: hya_engine::ToolExecuteBeforeInput,
    ) -> hya_engine::ToolExecuteBeforeOutcome {
        hya_engine::ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(
        &self,
        input: hya_engine::ToolExecuteAfterInput,
    ) -> hya_engine::ToolExecuteAfterOutcome {
        hya_engine::ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }

    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        self.calls
            .lock()
            .unwrap()
            .push(format!("compaction.before:{:?}", input.trigger));
        let decision = self.decisions.lock().unwrap().pop();
        decision.unwrap_or(CompactionDecision::Proceed)
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

/// Context reduction never calls the summarizer; it only writes terminal
/// handoffs, which these tests do not reach.
struct NoSummarizer;

#[async_trait]
impl Summarizer for NoSummarizer {
    async fn summarize(
        &self,
        _messages: &[hya_proto::Message],
        _options: SummarizeOptions,
    ) -> Result<String, hya_engine::CoreError> {
        Err(hya_engine::CoreError::Invalid(
            "no summarizer in this test".to_string(),
        ))
    }
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hya-compaction-hooks-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn agent(dir: &Path) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir.to_path_buf(),
        reasoning: None,
    }
}

/// An engine whose second turn is over the threshold: the fake route
/// advertises a 40k window (threshold 32,000) and turn 1 answers with ~40k
/// tokens, which turn 2's kernel rewrite removes.
async fn compacting_engine(hooks: Arc<dyn HookDispatcher>) -> Arc<SessionEngine> {
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("detail across the earlier turn ".repeat(7_000)),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("second".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ])
    .with_max_context(40_000);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_hooks(hooks),
    )
}

/// Create a session and run turn 1 on `engine`; the next turn is over the
/// threshold.
async fn session_before_overflow(engine: &SessionEngine, dir: &Path) -> SessionId {
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "PROMPT_0".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent(dir), CancellationToken::new())
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "PROMPT_1".to_string())
        .await
        .unwrap();
    session
}

async fn context_compacted_count(engine: &SessionEngine, session: SessionId) -> usize {
    let envelopes = engine.replay(session).await.unwrap();
    envelopes
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
        .count()
}

/// `compaction.before` runs before the kernel's rewrite on an overflow and
/// `compaction.after` once it committed. There is no summarizer prompt to
/// replace, so a `Replace` reply changes nothing.
#[tokio::test]
async fn compaction_hooks_surround_the_kernel_rewrite() {
    let hooks = ScriptedHook::with_decisions(vec![CompactionDecision::Replace {
        instructions: "SUMMARIZE AS LIMERICKS".to_string(),
    }]);
    let engine = compacting_engine(hooks.clone()).await;
    let dir = tempdir();
    let session = session_before_overflow(&engine, &dir).await;
    engine
        .run_turn(session, &agent(&dir), CancellationToken::new())
        .await
        .unwrap();

    let calls: Vec<String> = hooks
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("compaction."))
        .collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0], "compaction.before:Overflow");
    assert!(calls[1].starts_with("compaction.after:"), "{calls:?}");
    assert_eq!(context_compacted_count(&engine, session).await, 1);
}

/// A `Skip` reply on an overflow-forced trigger is ignored: the rewrite
/// completes anyway, because honoring the skip would send the request out
/// over its window.
#[tokio::test]
async fn compaction_skip_on_overflow_is_ignored() {
    let hooks = ScriptedHook::with_decisions(vec![CompactionDecision::Skip {
        reason: "hook asks to skip".to_string(),
    }]);
    let engine = compacting_engine(hooks.clone()).await;
    let dir = tempdir();
    let session = session_before_overflow(&engine, &dir).await;
    engine
        .run_turn(session, &agent(&dir), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        context_compacted_count(&engine, session).await,
        1,
        "overflow-forced reduction must not be skippable by a hook"
    );
}

/// `session.start` fires when a session is created and `session.end` when it
/// is archived; both are best-effort and never fail the engine call.
#[tokio::test]
async fn session_lifecycle_hooks_fire_best_effort() {
    let hooks: Arc<ScriptedHook> = ScriptedHook::default().into();
    let summarizer = Arc::new(NoSummarizer);
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router,
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_compaction(summarizer, CompactionConfig::default())
        .with_hooks(hooks.clone()),
    );
    let dir = tempdir();
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    assert!(
        hooks.calls().contains(&format!("session.start:{session}")),
        "session.start must fire on create"
    );

    engine.archive_session(session).await.unwrap();
    assert!(
        hooks.calls().contains(&format!("session.end:{session}")),
        "session.end must fire when the session is archived"
    );
}

/// `agent.spawn` fires when a subagent is registered under its parent.
#[tokio::test]
async fn agent_spawn_hook_fires_on_member_registration() {
    let hooks: Arc<ScriptedHook> = ScriptedHook::default().into();
    let summarizer = Arc::new(NoSummarizer);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectiveFakeProvider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router,
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_compaction(summarizer, CompactionConfig::default())
        .with_hooks(hooks.clone()),
    );
    let dir = tempdir();
    let mut agent_spec = agent(&dir);
    agent_spec.name = AgentName::new("hya-task");
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: agent_spec.name.clone(),
            model: agent_spec.model.clone(),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let spec = ResidentTurn {
        agent: agent_spec.clone(),
        binding: engine.bind_runtime(&agent_spec.workdir).unwrap(),
        agents: Arc::from([]),
        resources: None,
        guidance: None,
        directive: "small task".to_string(),
        description: String::new(),

        sidecar_factory: None,
        tool_call: None,
    };
    run_residents(engine.clone(), lead, vec![spec], CancellationToken::new()).await;

    let spawns: Vec<String> = hooks
        .calls()
        .iter()
        .filter(|call| call.starts_with("agent.spawn:"))
        .cloned()
        .collect();
    assert_eq!(spawns.len(), 1, "exactly one member was registered");
    let child = engine.read_projection(lead).await.unwrap().session.members[0]
        .child
        .unwrap();
    assert_eq!(
        spawns[0],
        format!("agent.spawn:{lead}:{child}"),
        "the hook names the parent session and the child session"
    );
}

/// Text-only provider used by the member in the spawn test.
struct SelectiveFakeProvider;

#[async_trait]
impl hya_provider::Provider for SelectiveFakeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<hya_provider::Capabilities> {
        (model.as_str() == "fake").then_some(hya_provider::Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..hya_provider::Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: hya_provider::CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("MEMBERTEXT".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(futures::stream::iter(
            events
                .into_iter()
                .map(Ok::<Event, hya_provider::ProviderError>),
        )))
    }
}

#[tokio::test]
async fn compaction_commit_failure_is_atomic_and_fails_turn() {
    let hooks = ScriptedHook::with_decisions(Vec::new());
    let engine = compacting_engine(hooks.clone()).await;
    let dir = tempdir();
    let session = session_before_overflow(&engine, &dir).await;

    let mut tx = engine.store().begin_transaction().await.unwrap();
    sqlx::query("CREATE TRIGGER reject_compaction BEFORE INSERT ON event_log WHEN NEW.payload LIKE '%context_compacted%' BEGIN SELECT RAISE(ABORT, 'compaction commit rejected'); END")
        .execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let error = engine
        .run_turn(session, &agent(&dir), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("compaction commit rejected"),
        "{error}"
    );
    let events = engine.replay(session).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, Event::ContextCompacted { .. }))
    );
    assert!(!events.iter().any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains(hya_provider::COMPACT_CONTEXT_MARKER))));
}

/// `/summarize` commits its marker and `ContextCompacted` in one writer
/// transaction: SQLite's `data_version`, read on a separate connection,
/// advances once per writer transaction, however many rows it appends.
#[tokio::test]
async fn successful_compaction_is_one_transaction() {
    use sqlx::Connection;
    let dir = tempdir();
    let db = dir.join("events.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    let mut observer = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        store,
        Arc::new(
            ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![vec![
                FakeStep::Text("answer".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ]]))),
        ),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        perm,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "summarize this".to_string())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent(&dir), CancellationToken::new())
        .await
        .unwrap();
    let version = "PRAGMA data_version";
    let before: i64 = sqlx::query_scalar(version)
        .fetch_one(&mut observer)
        .await
        .unwrap();
    let marker = engine.summarize_session(session).await.unwrap();
    let after: i64 = sqlx::query_scalar(version)
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert_eq!(after - before, 1);
    assert!(
        engine.replay(session).await.unwrap().iter().any(
            |e| matches!(e.event, Event::ContextCompacted { message, .. } if message == marker)
        )
    );
}
