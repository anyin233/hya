//! Integration tests for `hya-core`: runtime catalog refresh.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hya_bundle::AgentRole;
use hya_core::{AgentSpec, CreateSession};
use hya_core::{
    CoreError, EventBus, EvidenceQuality, LoopConfig, LoopPlanner, LoopVerifier, PlannerOutput,
    RunOutcome, RuntimeCatalogRefresh, RuntimeRegistry, SessionEngine, VerifierVerdict, run_loop,
};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use tokio_util::sync::CancellationToken;

struct PublishingRefresh {
    replacement: Arc<hya_core::AgentCatalog>,
    calls: AtomicUsize,
}

#[async_trait]
impl RuntimeCatalogRefresh for PublishingRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        runtime.publish_catalog(Arc::clone(&self.replacement))?;
        Ok(true)
    }
}

struct LoopCatalogRefresh {
    first: Arc<hya_core::AgentCatalog>,
    second: Arc<hya_core::AgentCatalog>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl RuntimeCatalogRefresh for LoopCatalogRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let catalog = if call == 0 {
            Arc::clone(&self.first)
        } else {
            Arc::clone(&self.second)
        };
        runtime.publish_catalog(catalog)?;
        Ok(true)
    }
}

struct TwoIterationVerifier {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LoopVerifier for TwoIterationVerifier {
    async fn grade(&self, _target: &str, _transcript: &str) -> Result<VerifierVerdict, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let satisfied = call == 1;
        Ok(VerifierVerdict {
            score: if satisfied { 100 } else { 0 },
            satisfied,
            evidence_quality: if satisfied {
                EvidenceQuality::Verified
            } else {
                EvidenceQuality::ClaimOnly
            },
            critical_gaps: if satisfied {
                Vec::new()
            } else {
                vec!["continue".to_string()]
            },
            iteration_summary: format!("iteration {call}"),
            reason: String::new(),
        })
    }
}

struct OneDirectivePlanner;

#[async_trait]
impl LoopPlanner for OneDirectivePlanner {
    async fn plan_next(
        &self,
        _target: &str,
        _history: &[String],
        _last: &VerifierVerdict,
        _planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        Ok(PlannerOutput {
            directive: "continue the loop".to_string(),
            continuity_brief: String::new(),
            planner_notes: String::new(),
            strategy_change: false,
            change_note: String::new(),
        })
    }
}

#[tokio::test]
async fn root_binding_refreshes_before_capture_while_plain_binding_stays_pure() {
    let old_catalog = support::builtin_only_catalog();
    let replacement = support::test_catalog(&[("installed-agent", AgentRole::Main, &[])]);
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), old_catalog));
    let refresh = Arc::new(PublishingRefresh {
        replacement,
        calls: AtomicUsize::new(0),
    });
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("connect store"),
        Arc::new(ProviderRouter::new()),
        Arc::clone(&runtime),
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh.clone());
    let workdir = support::TestDir::new("root-catalog-refresh");

    let plain_before = engine.bind_runtime(workdir.path()).expect("plain binding");
    assert!(plain_before.resolve_agent("general").is_some());
    assert!(plain_before.resolve_agent("installed-agent").is_none());
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 0);

    let root = engine
        .bind_root_runtime(workdir.path())
        .await
        .expect("root binding");
    assert!(root.resolve_agent("installed-agent").is_some());
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);

    let plain_after = engine.bind_runtime(workdir.path()).expect("plain binding");
    assert!(plain_after.resolve_agent("installed-agent").is_some());
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn root_turn_refreshes_installed_catalog_before_agent_resolution() {
    let old_catalog = support::builtin_only_catalog();
    let replacement = support::test_catalog(&[("installed-agent", AgentRole::Main, &[])]);
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), old_catalog));
    let refresh = Arc::new(PublishingRefresh {
        replacement,
        calls: AtomicUsize::new(0),
    });
    let provider = FakeProvider::scripted(vec![FakeStep::Finish(FinishReason::Stop)]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("connect store"),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        runtime,
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh.clone());
    let workdir = support::TestDir::new("root-turn-catalog-refresh");
    let model = ModelRef::new("fake");
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("installed-agent"),
            model: model.clone(),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .expect("create root session");
    engine
        .admit_user_prompt(session, "use the installed agent".to_string())
        .await
        .expect("admit prompt");

    let finish = engine
        .run_turn(
            session,
            &AgentSpec {
                name: AgentName::new("general"),
                model,
                system_prompt: "base prompt".to_string(),
                workdir: workdir.path().to_path_buf(),
                reasoning: None,
            },
            CancellationToken::new(),
        )
        .await
        .expect("root turn");

    assert_eq!(finish, FinishReason::Stop);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn root_shell_refreshes_catalog_before_agent_resolution() {
    let old_catalog = support::builtin_only_catalog();
    let replacement = support::test_catalog(&[("installed-agent", AgentRole::Main, &[])]);
    let runtime = Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), old_catalog));
    let refresh = Arc::new(PublishingRefresh {
        replacement,
        calls: AtomicUsize::new(0),
    });
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("connect store"),
        Arc::new(ProviderRouter::new()),
        runtime,
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh.clone());
    let workdir = support::TestDir::new("root-shell-catalog-refresh");
    let model = ModelRef::new("fake");
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("installed-agent"),
            model: model.clone(),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .expect("create root session");

    let (_message, finish) = engine
        .run_shell(
            session,
            &AgentSpec {
                name: AgentName::new("general"),
                model,
                system_prompt: "base prompt".to_string(),
                workdir: workdir.path().to_path_buf(),
                reasoning: None,
            },
            "printf catalog-refresh-shell".to_string(),
            CancellationToken::new(),
        )
        .await
        .expect("root shell");

    assert_eq!(finish, FinishReason::Stop);
    assert_eq!(refresh.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn loop_children_reuse_one_root_catalog_binding() {
    let initial = support::builtin_only_catalog();
    let catalog_a = support::test_catalog(&[("loop-agent", AgentRole::Main, &[])]);
    let catalog_b = support::builtin_only_catalog();
    let calls = Arc::new(AtomicUsize::new(0));
    let refresh = Arc::new(LoopCatalogRefresh {
        first: catalog_a,
        second: catalog_b,
        calls: calls.clone(),
    });
    let provider = FakeProvider::scripted_turns(vec![
        vec![FakeStep::Finish(FinishReason::Stop)],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.expect("connect store"),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), initial)),
            permission,
            EventBus::default(),
        )
        .with_catalog_refresh(refresh),
    );
    let workdir = support::TestDir::new("loop-root-catalog-refresh");
    let agent = AgentSpec {
        name: AgentName::new("loop-agent"),
        model: ModelRef::new("fake"),
        system_prompt: "loop agent".to_string(),
        workdir: workdir.path().to_path_buf(),
        reasoning: None,
    };
    let lead_session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .expect("create loop root session");

    let outcome = run_loop(
        engine,
        lead_session,
        agent,
        "reach the target".to_string(),
        Arc::new(TwoIterationVerifier {
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(OneDirectivePlanner),
        LoopConfig {
            budget: 2,
            ..LoopConfig::default()
        },
        CancellationToken::new(),
    )
    .await
    .expect("loop should complete two iterations with one root binding");

    assert!(matches!(
        outcome,
        RunOutcome::Achieved { iterations: 2, .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
