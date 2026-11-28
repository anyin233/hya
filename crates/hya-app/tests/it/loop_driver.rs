//! Integration tests verifying `hya/loop` ABI v2 `LoopDriver` loading, execution
//! through `HostRuntime::loop_driver()`, and actionable errors when `hya/loop`
//! is disabled or running in `--bare` mode.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use hya_app::{
    HeadlessEvaluatorRoute, InvocationPolicy, LoopPredicateMode, LoopPredicateSpec, LoopRunOptions,
    agent_with_model, build_session_engine_pure, config::EngineSettings, open_store, prepare_loop,
};
use hya_bundle::{BuiltinBundleProfile, BuiltinBundleSelection};
use hya_engine::CreateSession;
use hya_proto::{FinishReason, SessionKind};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_runtime_abi::DriverRunOutcome;
use tokio_util::sync::CancellationToken;

fn router_with_turns(turns: Vec<Vec<FakeStep>>) -> ProviderRouter {
    ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(turns)))
}

#[tokio::test]
async fn hya_loop_native_driver_runs_verifier_planner_and_predicate_through_host_runtime() {
    let store = open_store("").await.unwrap();
    let worker_router = router_with_turns(vec![
        vec![
            FakeStep::Text("attempt 1".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("attempt 2 verified".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let evaluator_router = router_with_turns(vec![
        vec![
            FakeStep::Text(
                r#"{"score": 40, "satisfied": false, "evidence_quality": "claim_only", "critical_gaps": ["missing check"], "iteration_summary": "first try", "reason": "needs verification"}"#.into(),
            ),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text(
                r#"{"directive": "run check and report evidence", "continuity_brief": "keep changes small", "planner_notes": "verify step", "strategy_change": false}"#.into(),
            ),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text(
                r#"{"score": 95, "satisfied": true, "evidence_quality": "verified", "critical_gaps": [], "iteration_summary": "verified", "reason": "all criteria met"}"#.into(),
            ),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);

    let mut agent = agent_with_model("fake/worker", None);
    agent.workdir = std::env::temp_dir();
    let mut built = build_session_engine_pure(
        store,
        worker_router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (InvocationPolicy::default(), EngineSettings::default()),
    )
    .await
    .unwrap();

    let driver = built
        .loop_driver()
        .expect("hya/loop LoopDriver must be registered in full mode");
    assert_eq!(driver.id(), "loop");

    let session = built
        .engine()
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap();

    // 1. Multi-iteration verifier + planner loop completes with Achieved.
    let work = prepare_loop(
        built.host_runtime(),
        session,
        agent.clone(),
        HeadlessEvaluatorRoute {
            router: &evaluator_router,
            model_override: Some("fake/judge"),
            default_model: "fake/worker",
        },
        "deliver feature".into(),
        |_| {
            Ok(LoopRunOptions {
                budget: 5,
                ..LoopRunOptions::default()
            })
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let outcome = work.await.unwrap();
    assert_eq!(
        outcome,
        DriverRunOutcome::Achieved {
            iterations: 2,
            reason: "satisfied: score 95".into(),
        }
    );

    // 2. Deterministic `--until true` predicate stops on iteration 1 without calling evaluator.
    let pred_work = prepare_loop(
        built.host_runtime(),
        session,
        agent,
        HeadlessEvaluatorRoute {
            router: &evaluator_router,
            model_override: Some("fake/judge"),
            default_model: "fake/worker",
        },
        "predicate check".into(),
        |workdir| {
            Ok(LoopRunOptions {
                budget: 3,
                predicate: Some(LoopPredicateSpec {
                    command: "true".into(),
                    mode: LoopPredicateMode::Until,
                    workdir: PathBuf::from(workdir),
                }),
                ..LoopRunOptions::default()
            })
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    match pred_work.await.unwrap() {
        DriverRunOutcome::Achieved { iterations, reason } => {
            assert_eq!(iterations, 1);
            assert!(reason.contains("until"), "{reason}");
        }
        other => panic!("expected achieved predicate outcome, got {other:?}"),
    }

    built.shutdown().await.unwrap();
}

#[tokio::test]
async fn prepare_loop_returns_actionable_error_when_bare_or_hya_loop_disabled() {
    for (label, selection) in [
        ("bare", BuiltinBundleSelection::bare()),
        (
            "disabled",
            BuiltinBundleSelection {
                profile: BuiltinBundleProfile::Full,
                enable: Vec::new(),
                disable: vec!["hya/loop".to_string()],
            },
        ),
    ] {
        let store = open_store("").await.unwrap();
        let router = router_with_turns(vec![vec![
            FakeStep::Text("ok".into()),
            FakeStep::Finish(FinishReason::Stop),
        ]]);
        let mut settings = EngineSettings::default();
        settings.set_builtin_bundles(selection);
        let mut agent = agent_with_model("fake/worker", None);
        agent.workdir = std::env::temp_dir();
        let mut built = build_session_engine_pure(
            store,
            router.clone(),
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (InvocationPolicy::default(), settings),
        )
        .await
        .unwrap();

        assert!(
            built.loop_driver().is_none(),
            "{label}: loop_driver must be None when hya/loop is not enabled"
        );
        let session = built
            .engine()
            .create(CreateSession {
                parent: None,
                agent: agent.name.clone(),
                model: agent.model.clone(),
                workdir: agent.workdir.to_string_lossy().into_owned(),
                project: None,
                kind: SessionKind::Project,
            })
            .await
            .unwrap();

        let err = match prepare_loop(
            built.host_runtime(),
            session,
            agent,
            HeadlessEvaluatorRoute {
                router: &router,
                model_override: None,
                default_model: "fake/worker",
            },
            "do work".into(),
            |_| Ok(LoopRunOptions::default()),
            CancellationToken::new(),
        )
        .await
        {
            Ok(_) => panic!("{label}: prepare_loop must error without hya/loop"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("hya/loop") && msg.contains("--bare"),
            "{label}: error must be actionable: {msg}"
        );
        built.shutdown().await.unwrap();
    }
}
