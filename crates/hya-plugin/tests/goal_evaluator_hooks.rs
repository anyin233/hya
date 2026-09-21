//! Integration tests for `hya-plugin`: the revived goal/loop dispatch arms
//! (`goal.evaluate`, `loop.verifier`, `loop.planner`) on the wire and through
//! the `PluginHost` dispatcher, plus the `PluginGoalEvaluator` adapter
//! contract over a real host (dev_plan 6.2/6.3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_core::completion::PluginGoalEvaluator;
use hya_core::hooks::{GoalEvaluateReply, HookDispatcher};
use hya_core::{CoreError, GoalEvaluator, Verdict};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HookName, HookPosture, HostInfo, PluginKindWire};

fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: "0.0.0".to_string(),
    }
}

fn spec(id: &str, script: &str) -> PluginSpec {
    PluginSpec {
        id: id.to_string(),
        kind: PluginKindWire::Rust,
        command: vec!["python3".to_string(), "-c".to_string(), script.to_string()],
        timeout_ms: Some(4000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
        plugin_dir: None,
    }
}

/// The three revived evaluator hook names use their documented wire spellings
/// and default to the Open posture: evaluators are not guards.
#[test]
fn evaluator_hook_names_round_trip_and_default_open() {
    for (name, wire) in [
        (HookName::GoalEvaluate, "goal.evaluate"),
        (HookName::LoopVerifier, "loop.verifier"),
        (HookName::LoopPlanner, "loop.planner"),
    ] {
        assert_eq!(
            serde_json::to_string(&name).unwrap(),
            format!("\"{wire}\""),
            "{wire} must serialize to its documented spelling"
        );
        assert_eq!(HookName::from_wire(wire), Some(name));
        assert_eq!(name.method(), format!("hook/{wire}"));
        assert_eq!(
            name.default_posture(),
            HookPosture::Open,
            "{wire} is an evaluator, not a guard, and must default Open"
        );
    }
}

/// dev_plan 6.2: a plugin registering `goal.evaluate` receives the call and a
/// well-formed verdict propagates to the engine-facing reply; the capability
/// probe flips for registered hosts.
#[tokio::test]
async fn goal_evaluate_reaches_registered_plugin_and_propagates_verdict() {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "judge", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "goal.evaluate"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/goal.evaluate":
        assert msg["params"]["condition"] == "tests pass", msg["params"]
        assert "worker output" in msg["params"]["transcript"], msg["params"]
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "met": True, "reason": "all green"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("judge", script)], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");
    assert!(
        HookDispatcher::has_goal_evaluate(&host),
        "a host with a registered goal.evaluate provider must probe true"
    );

    let reply = host
        .goal_evaluate("tests pass", "[User] worker output")
        .await;
    match reply {
        Ok(GoalEvaluateReply::Verdict { met, reason }) => {
            assert!(met, "the plugin said met=true");
            assert_eq!(reason, "all green");
        }
        other => panic!("expected a verdict reply, got {other:?}"),
    }
}

/// No registration: the probe stays false and the dispatch reports the hook as
/// unregistered rather than inventing a verdict.
#[tokio::test]
async fn goal_evaluate_without_registration_errors_and_probe_is_false() {
    let host = PluginHost::connect_all(Vec::new(), host_info()).await;
    assert!(!HookDispatcher::has_goal_evaluate(&host));
    let reply = host.goal_evaluate("condition", "transcript").await;
    match reply {
        Err(CoreError::Invalid(message)) => {
            assert!(
                message.contains("not registered"),
                "expected not-registered, got: {message}"
            );
        }
        other => panic!("expected not-registered error, got {other:?}"),
    }
}

/// A registered provider that replies with a non-verdict object is reported as
/// [`GoalEvaluateReply::Malformed`] — decisive, so the driver counts it as
/// not-met against the iteration cap instead of shopping for another opinion.
#[tokio::test]
async fn malformed_verdict_reply_is_reported_as_malformed() {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "sloppy", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "goal.evaluate"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/goal.evaluate":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                          "result": {"met": "yes", "verdict": True}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("sloppy", script)], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let reply = host.goal_evaluate("condition", "transcript").await;
    match reply {
        Ok(GoalEvaluateReply::Malformed) => {}
        other => panic!("expected Malformed, got {other:?}"),
    }
}

/// Evaluators fail open: a JSON-RPC error from the first provider in load
/// order falls through to the next, whose verdict wins.
#[tokio::test]
async fn goal_evaluate_hook_error_fails_open_to_next_provider() {
    let poison = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "poison", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "goal.evaluate"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/goal.evaluate":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                          "error": {"code": -32000, "message": "poisoned"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let healthy = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "healthy", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "goal.evaluate"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/goal.evaluate":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "met": False, "reason": "not yet"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(
        vec![spec("poison", poison), spec("healthy", healthy)],
        host_info(),
    )
    .await;
    assert_eq!(host.len(), 2, "both fixtures must connect");

    let reply = host.goal_evaluate("condition", "transcript").await;
    match reply {
        Ok(GoalEvaluateReply::Verdict { met: false, reason }) => {
            assert_eq!(reason, "not yet");
        }
        other => panic!("expected the healthy later provider's not-met verdict, got {other:?}"),
    }
}

/// Adapter contract over the real host: a well-formed verdict passes through,
/// and a hook error degrades to not-met (never a hard `Err`) so a broken
/// evaluator only burns iterations.
#[tokio::test]
async fn plugin_goal_evaluator_maps_verdicts_and_failures_to_not_met() {
    let poison = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "poison-adapter", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "goal.evaluate"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/goal.evaluate":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                          "error": {"code": -32000, "message": "poisoned"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host: Arc<PluginHost> =
        Arc::new(PluginHost::connect_all(vec![spec("poison-adapter", poison)], host_info()).await);
    assert_eq!(host.len(), 1, "fixture must connect");

    let dispatcher: Arc<dyn HookDispatcher> = host.clone();
    let evaluator = PluginGoalEvaluator::new(dispatcher);
    let verdict = evaluator.evaluate("condition", "transcript").await;
    match verdict {
        Ok(Verdict { met, reason }) => {
            assert!(!met, "a hook error must degrade to not-met");
            assert_eq!(reason, "goal.evaluate hook error");
        }
        Err(error) => panic!("hook error must not abort the run: {error:?}"),
    }
}

/// dev_plan 6.2: `loop.verifier` and `loop.planner` arms reach registered
/// plugins and their structured replies round-trip into the native verdict and
/// planner output types.
#[tokio::test]
async fn loop_verifier_and_planner_reach_registered_plugins() {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "loop-brain", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "loop.verifier"}, {"name": "loop.planner"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/loop.verifier":
        assert msg["params"]["target"] == "green build", msg["params"]
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "score": 80, "satisfied": False, "evidence_quality": "supported",
            "critical_gaps": ["tests still red"], "iteration_summary": "edited main.rs",
            "reason": "close"}}), flush=True)
    elif msg.get("method") == "hook/loop.planner":
        assert msg["params"]["last"]["score"] == 80, msg["params"]
        assert msg["params"]["last"]["evidence_quality"] == "supported", msg["params"]
        assert msg["params"]["planner_notes"] == "focus on unit tests", msg["params"]
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "directive": "fix the failing unit test",
            "continuity_brief": "main.rs was edited",
            "planner_notes": "focus on unit tests",
            "strategy_change": False, "change_note": ""}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("loop-brain", script)], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let verdict = host
        .loop_verify("green build", "[User] work")
        .await
        .unwrap();
    assert_eq!(verdict.score, 80);
    assert!(!verdict.satisfied);
    assert_eq!(verdict.critical_gaps, ["tests still red"]);
    assert_eq!(verdict.iteration_summary, "edited main.rs");
    assert_eq!(verdict.reason, "close");

    let output = host
        .loop_plan(
            "green build",
            &["first attempt".to_string()],
            &verdict,
            "focus on unit tests",
        )
        .await
        .unwrap();
    assert_eq!(output.directive, "fix the failing unit test");
    assert_eq!(output.continuity_brief, "main.rs was edited");
    assert_eq!(output.planner_notes, "focus on unit tests");
    assert!(!output.strategy_change);
}
