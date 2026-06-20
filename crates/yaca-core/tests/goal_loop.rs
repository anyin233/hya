#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use yaca_core::{
    AgentSpec, CoreError, CreateSession, EventBus, GoalEvaluator, ModelGoalEvaluator, RunOutcome,
    SafetyCaps, SessionEngine, Verdict, run_goal,
};
use yaca_proto::{AgentName, FinishReason, ModelRef};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

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
        tools,
        perm,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
    };
    (engine, agent)
}

async fn new_session(engine: &SessionEngine) -> yaca_proto::SessionId {
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
