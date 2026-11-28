//! Integration tests running the `hya/loop:loop` driver through the app's
//! generic driver registry, and the actionable error when `hya/loop` is
//! disabled or running in `--bare` mode.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_app::{
    InvocationPolicy, agent_with_model, build_session_engine_pure, config::EngineSettings,
    open_store, prepare_driver,
};
use hya_bundle::{BuiltinBundleProfile, BuiltinBundleSelection};
use hya_engine::CreateSession;
use hya_proto::{FinishReason, SessionKind};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_runtime_abi::DriverRequest;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn router_with_turns(turns: Vec<Vec<FakeStep>>) -> ProviderRouter {
    ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(turns)))
}

#[tokio::test]
async fn hya_loop_native_driver_runs_verifier_planner_and_predicate_through_host_runtime() {
    let store = open_store("").await.unwrap();
    // Worker and judge share one router; calls interleave worker, verifier,
    // planner, worker, verifier.
    let router = router_with_turns(vec![
        vec![
            FakeStep::Text("attempt 1".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
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
            FakeStep::Text("attempt 2 verified".into()),
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
        router,
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (InvocationPolicy::default(), EngineSettings::default()),
    )
    .await
    .unwrap();

    assert!(
        built.driver("hya/loop:loop").is_some(),
        "hya/loop must register its driver in full mode"
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

    let request = |args| DriverRequest {
        session,
        agent: Some(agent.name.clone()),
        model: Some(agent.model.clone()),
        workdir: agent.workdir.clone(),
        args,
    };

    // 1. Multi-iteration verifier + planner loop completes with Achieved.
    let work = prepare_driver(
        &built,
        "hya/loop:loop",
        request(
            json!({ "target": "deliver feature", "budget": 5, "evaluator_model": "fake/judge" }),
        ),
        CancellationToken::new(),
    )
    .unwrap();
    let report = work.await.unwrap();
    assert_eq!(
        report.stdout,
        "loop outcome: Achieved { iterations: 2, reason: \"satisfied: score 95\" }\n"
    );
    assert_eq!(report.exit_code, 0);

    // 2. Deterministic `--until true` predicate stops on iteration 1 without calling evaluator.
    let pred_work = prepare_driver(
        &built,
        "hya/loop:loop",
        request(json!({ "target": "predicate check", "budget": 3, "until": "true" })),
        CancellationToken::new(),
    )
    .unwrap();
    let report = pred_work.await.unwrap();
    assert!(
        report
            .stdout
            .starts_with("loop outcome: Achieved { iterations: 1")
            && report.stdout.contains("until"),
        "{}",
        report.stdout
    );

    built.shutdown().await.unwrap();
}

#[tokio::test]
async fn loop_driver_is_absent_with_an_actionable_error_when_bare_or_disabled() {
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
            built.driver("hya/loop:loop").is_none(),
            "{label}: the loop driver must be absent when hya/loop is not enabled"
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

        let request = DriverRequest {
            session,
            agent: Some(agent.name.clone()),
            model: Some(agent.model.clone()),
            workdir: agent.workdir.clone(),
            args: json!({ "target": "do work" }),
        };
        let err = match prepare_driver(&built, "hya/loop:loop", request, CancellationToken::new()) {
            Ok(_) => panic!("{label}: prepare_driver must error without hya/loop"),
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
