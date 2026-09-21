//! Integration tests for `hya-core`: goal loop.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hya_core::completion::PluginGoalEvaluator;
use hya_core::hooks::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    GoalEvaluateReply, HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome,
    TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_core::{
    AgentSpec, CoreError, CreateSession, EventBus, GoalEvaluator, ModelGoalEvaluator, RunOutcome,
    SafetyCaps, SessionEngine, Verdict, run_goal,
};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio_util::sync::CancellationToken;

struct ScriptedEvaluator {
    mets: Vec<bool>,
    idx: AtomicUsize,
}

#[async_trait]
impl GoalEvaluator for ScriptedEvaluator {
    async fn evaluate(&self, _condition: &str, _transcript: &str) -> Result<Verdict, CoreError> {
        let i = self.idx.fetch_add(1, Ordering::Relaxed);
        Ok(Verdict {
            met: self.mets.get(i).copied().unwrap_or(true),
            reason: format!("scripted {i}"),
        })
    }
}

async fn engine_with(provider: FakeProvider) -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

async fn new_session(engine: &SessionEngine) -> hya_proto::SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn goal_loops_until_met_then_stops() {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("working".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (engine, agent) = engine_with(provider).await;
    let session = new_session(&engine).await;
    let evaluator: Arc<dyn GoalEvaluator> = Arc::new(ScriptedEvaluator {
        mets: vec![false, false, false, true],
        idx: AtomicUsize::new(0),
    });

    let outcome = run_goal(
        engine.clone(),
        session,
        agent,
        "tests pass".to_string(),
        evaluator,
        SafetyCaps::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Achieved {
            iterations: 4,
            reason: "scripted 3".to_string(),
        }
    );
}

#[tokio::test]
async fn malformed_eval_counts_toward_cap() {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("not json".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (engine, agent) = engine_with(provider).await;
    let session = new_session(&engine).await;

    let evaluator: Arc<dyn GoalEvaluator> = Arc::new(ModelGoalEvaluator::new(
        Arc::new(
            ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![vec![
                FakeStep::Text("not json".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ]]))),
        ),
        ModelRef::new("fake"),
    ));

    let caps = SafetyCaps {
        max_iterations: 2,
        ..SafetyCaps::default()
    };
    let outcome = run_goal(
        engine.clone(),
        session,
        agent,
        "do the thing".to_string(),
        evaluator,
        caps,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Capped {
            iterations: 2,
            which: "max_iterations",
        }
    );
}

#[tokio::test]
async fn pre_cancelled_goal_returns_cancelled() {
    let provider = FakeProvider::scripted_turns(vec![vec![FakeStep::Finish(FinishReason::Stop)]]);
    let (engine, agent) = engine_with(provider).await;
    let session = new_session(&engine).await;
    let evaluator: Arc<dyn GoalEvaluator> = Arc::new(ScriptedEvaluator {
        mets: vec![true],
        idx: AtomicUsize::new(0),
    });
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = run_goal(
        engine.clone(),
        session,
        agent,
        "x".to_string(),
        evaluator,
        SafetyCaps::default(),
        cancel,
    )
    .await
    .unwrap();

    assert_eq!(outcome, RunOutcome::Cancelled);
}

/// Hook dispatcher that scripts `goal.evaluate` replies, standing in for a
/// plugin host so the adapter contract is exercised end to end through
/// `run_goal`.
struct ScriptedHookDispatcher {
    replies: Vec<GoalEvaluateReply>,
    idx: AtomicUsize,
}

#[async_trait]
impl HookDispatcher for ScriptedHookDispatcher {
    fn dispatch_event(&self, _envelope: &hya_proto::Envelope) {}

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

    async fn goal_evaluate(
        &self,
        _condition: &str,
        _transcript: &str,
    ) -> Result<GoalEvaluateReply, CoreError> {
        let i = self.idx.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .replies
            .get(i)
            .cloned()
            .unwrap_or(GoalEvaluateReply::Verdict {
                met: true,
                reason: "scripted default".to_string(),
            }))
    }
}

/// A scripted dispatcher evaluator reporting met=true stops the run after the
/// first iteration: the plugin verdict flows through `PluginGoalEvaluator`
/// into the same gate the built-in evaluator drives.
#[tokio::test]
async fn dispatcher_evaluator_met_stops_goal_early() {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("working".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (engine, agent) = engine_with(provider).await;
    let session = new_session(&engine).await;
    let dispatcher = Arc::new(ScriptedHookDispatcher {
        replies: vec![GoalEvaluateReply::Verdict {
            met: true,
            reason: "done by hook".to_string(),
        }],
        idx: AtomicUsize::new(0),
    });
    let evaluator: Arc<dyn GoalEvaluator> = Arc::new(PluginGoalEvaluator::new(dispatcher));

    let outcome = run_goal(
        engine.clone(),
        session,
        agent,
        "tests pass".to_string(),
        evaluator,
        SafetyCaps::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Achieved {
            iterations: 1,
            reason: "done by hook".to_string(),
        }
    );
}

/// A malformed dispatcher reply degrades to not-met and counts toward the
/// iteration cap — the adapter contract keeps bad verdicts from looping
/// forever or erroring the run.
#[tokio::test]
async fn dispatcher_evaluator_malformed_counts_toward_cap() {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("working".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (engine, agent) = engine_with(provider).await;
    let session = new_session(&engine).await;
    let dispatcher = Arc::new(ScriptedHookDispatcher {
        replies: vec![GoalEvaluateReply::Malformed, GoalEvaluateReply::Malformed],
        idx: AtomicUsize::new(0),
    });
    let evaluator: Arc<dyn GoalEvaluator> = Arc::new(PluginGoalEvaluator::new(dispatcher));

    let caps = SafetyCaps {
        max_iterations: 2,
        ..SafetyCaps::default()
    };
    let outcome = run_goal(
        engine.clone(),
        session,
        agent,
        "do the thing".to_string(),
        evaluator,
        caps,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        outcome,
        RunOutcome::Capped {
            iterations: 2,
            which: "max_iterations",
        }
    );
}
