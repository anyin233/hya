//! Integration tests for `hya-core`: the P5 injection-point hooks wired into
//! the engine — `compaction.before`/`compaction.after` around the reduction
//! ladder (fail-open), and best-effort `session.start`/`session.end`/
//! `agent.spawn` notifications.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_core::{
    AgentSpec, CompactionAfterInput, CompactionBeforeInput, CompactionConfig, CompactionDecision,
    CreateSession, EventBus, HookDispatcher, MemberSpec, SessionEngine, SummarizeOptions,
    Summarizer, run_team,
};
use hya_proto::{AgentName, Event, FinishReason, MemberId, MessageId, ModelRef, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
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
        input: hya_core::CommandExecuteBeforeInput,
    ) -> hya_core::CommandExecuteBeforeOutcome {
        hya_core::CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(
        &self,
        input: hya_core::TextCompleteInput,
    ) -> hya_core::TextCompleteOutcome {
        hya_core::TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(
        &self,
        input: hya_core::MessageUserBeforeInput,
    ) -> hya_core::MessageUserBeforeOutcome {
        hya_core::MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: hya_core::ChatParamsInput) -> hya_core::ChatParamsOutcome {
        hya_core::ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(
        &self,
        input: hya_core::ToolExecuteBeforeInput,
    ) -> hya_core::ToolExecuteBeforeOutcome {
        hya_core::ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(
        &self,
        input: hya_core::ToolExecuteAfterInput,
    ) -> hya_core::ToolExecuteAfterOutcome {
        hya_core::ToolExecuteAfterOutcome::Continue {
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

    async fn session_start(&self, input: hya_core::SessionLifecycleInput) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("session.start:{}", input.session));
    }

    async fn session_end(&self, input: hya_core::SessionLifecycleInput) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("session.end:{}", input.session));
    }

    async fn agent_spawn(&self, input: hya_core::AgentSpawnInput) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("agent.spawn:{}:{}", input.parent, input.child));
    }
}

/// Summarizer that records the system prompt it was handed, so a `Replace`
/// decision's instructions are observable.
struct RecordingSummarizer {
    seen_system: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl Summarizer for RecordingSummarizer {
    async fn summarize(
        &self,
        _messages: &[hya_proto::Message],
        options: SummarizeOptions,
    ) -> Result<String, hya_core::CoreError> {
        self.seen_system
            .lock()
            .unwrap()
            .push(options.system.clone());
        Ok("HOOKED SUMMARY".to_string())
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

/// An engine that compacts on the first turn: the fake route advertises a
/// 200k window, so `context_fraction: 0.001` clamps to the 1000-token floor
/// and three ~750-token prompts overflow it.
async fn compacting_engine(
    summarizer: Arc<dyn Summarizer>,
    hooks: Arc<dyn HookDispatcher>,
) -> Arc<SessionEngine> {
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("first".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("second".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(Arc::new(ToolRegistry::builtins())),
            perm,
            EventBus::default(),
        )
        .with_compaction(
            summarizer,
            CompactionConfig {
                token_threshold: 1_000_000,
                keep_recent: 1,
                context_fraction: 0.001,
                ..CompactionConfig::default()
            },
        )
        .with_hooks(hooks),
    )
}

async fn context_compacted_count(engine: &SessionEngine, session: SessionId) -> usize {
    let envelopes = engine.replay(session).await.unwrap();
    envelopes
        .iter()
        .filter(|e| matches!(e.event, Event::ContextCompacted { .. }))
        .count()
}

/// A `Replace` decision must steer the summarizer: the returned instructions
/// become the summarizer prompt, and the compaction itself still completes.
#[tokio::test]
async fn compaction_replace_instructions_reach_the_summarizer() {
    let seen_system = Arc::new(Mutex::new(Vec::new()));
    let summarizer = Arc::new(RecordingSummarizer {
        seen_system: seen_system.clone(),
    });
    let hooks = ScriptedHook::with_decisions(vec![CompactionDecision::Replace {
        instructions: "SUMMARIZE AS LIMERICKS".to_string(),
    }]);
    let engine = compacting_engine(summarizer, hooks.clone()).await;
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
    for i in 0..3 {
        engine
            .admit_user_prompt(session, format!("PROMPT_{i} {}", "p".repeat(3000)))
            .await
            .unwrap();
    }
    let spec = agent(&dir);
    engine
        .run_turn(session, &spec, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        hooks
            .calls()
            .iter()
            .any(|call| call.starts_with("compaction.before:Overflow")),
        "the engine must consult compaction.before on an overflow-forced trigger"
    );
    let seen = seen_system.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![Some("SUMMARIZE AS LIMERICKS".to_string())],
        "the Replace instructions must replace the summarizer prompt"
    );
    assert_eq!(
        context_compacted_count(&engine, session).await,
        1,
        "compaction must still complete under Replace"
    );
    assert!(
        hooks
            .calls()
            .iter()
            .any(|call| call.starts_with("compaction.after:")),
        "compaction.after must be notified after the fold commits"
    );
}

/// A `Skip` decision on an overflow-forced trigger is ignored: built-in
/// compaction completes anyway, because honoring the skip would send the
/// request out over its window.
#[tokio::test]
async fn compaction_skip_on_overflow_falls_back_to_builtin() {
    let seen_system = Arc::new(Mutex::new(Vec::new()));
    let summarizer = Arc::new(RecordingSummarizer {
        seen_system: seen_system.clone(),
    });
    let hooks = ScriptedHook::with_decisions(vec![CompactionDecision::Skip {
        reason: "hook asks to skip".to_string(),
    }]);
    let engine = compacting_engine(summarizer, hooks.clone()).await;
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
    for i in 0..3 {
        engine
            .admit_user_prompt(session, format!("PROMPT_{i} {}", "p".repeat(3000)))
            .await
            .unwrap();
    }
    let spec = agent(&dir);
    engine
        .run_turn(session, &spec, CancellationToken::new())
        .await
        .unwrap();

    let seen = seen_system.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "the built-in summarizer must still run exactly once"
    );
    assert_ne!(
        seen[0].as_deref(),
        Some("hook asks to skip"),
        "no Replace was requested, so the summarizer keeps the built-in prompt"
    );
    assert_eq!(
        context_compacted_count(&engine, session).await,
        1,
        "overflow-forced compaction must not be skippable by a hook"
    );
}

/// `session.start` fires when a session is created and `session.end` when it
/// is archived; both are best-effort and never fail the engine call.
#[tokio::test]
async fn session_lifecycle_hooks_fire_best_effort() {
    let hooks: Arc<ScriptedHook> = ScriptedHook::default().into();
    let summarizer = Arc::new(RecordingSummarizer {
        seen_system: Arc::new(Mutex::new(Vec::new())),
    });
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
            support::test_runtime(Arc::new(ToolRegistry::builtins())),
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
    let summarizer = Arc::new(RecordingSummarizer {
        seen_system: Arc::new(Mutex::new(Vec::new())),
    });
    let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectiveFakeProvider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router,
            support::test_runtime(Arc::new(ToolRegistry::builtins())),
            perm,
            EventBus::default(),
        )
        .with_compaction(summarizer, CompactionConfig::default())
        .with_hooks(hooks.clone()),
    );
    let dir = tempdir();
    let agent_spec = agent(&dir);
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
    let spec = MemberSpec {
        id: MemberId::new(),
        agent: agent_spec.clone(),
        binding: engine.bind_runtime(&agent_spec.workdir).unwrap(),
        agents: Arc::from([]),
        resources: None,
        guidance: None,
        directive: "small task".to_string(),
        description: String::new(),
        session: None,
        sidecar_factory: None,
        tool_call: None,
    };
    run_team(engine.clone(), lead, vec![spec], CancellationToken::new()).await;

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
            parallel_tool_calls: true,
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
