//! Evaluator (`goal.evaluate`, `loop.verifier`, `loop.planner`,
//! `loop.should_stop`), `model.fallback`, and `permission.approve` hooks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use hya_app::plugin_adapter::PluginHostAdapter;
use hya_engine::completion::PluginGoalEvaluator;
use hya_engine::hooks::{
    GoalEvaluateReply, HookDispatcher, ModelFailureClass, ModelFallbackInput, ModelFallbackOutcome,
    PermissionApproveInput,
};
use hya_engine::{CoreError, GoalEvaluator, Verdict};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_proto::{AgentName, MessageId, ModelRef, SessionId};
use hya_tool::{Action, Decision, Resource};

use super::{host_info, py_spec};

#[tokio::test]
async fn evaluator_hooks_dispatch_and_fail_open() {
    // Empty host: unregistered probes and defaults.
    let empty = PluginHostAdapter::new(Arc::new(
        PluginHost::connect_all(Vec::new(), host_info()).await,
    ));
    assert!(!HookDispatcher::has_goal_evaluate(&empty));
    assert!(matches!(
        empty.goal_evaluate("condition", "transcript").await,
        Err(CoreError::Invalid(ref m)) if m.contains("not registered")
    ));
    assert_eq!(empty.loop_should_stop("target", "transcript").await, None);

    // Chain: poison plugin fails open to healthy plugin, plus malformed replies and
    // PluginGoalEvaluator error degradation.
    let poison = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    m = msg.get("method")
    if m == "initialize":
        res = {"protocol_version": 1, "plugin": {"id": "poison", "version": "0.1.0", "kind": "rust"},
               "hooks": [{"name": "goal.evaluate"}, {"name": "loop.should_stop"}], "tools": []}
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}), flush=True)
    elif m == "hook/goal.evaluate":
        if msg["params"]["condition"] == "malformed":
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"met": "yes", "verdict": True}}), flush=True)
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32000, "message": "poisoned"}}), flush=True)
    elif m == "hook/loop.should_stop":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"stop": "yes", "verdict": True}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let healthy = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    m = msg.get("method")
    if m == "initialize":
        res = {"protocol_version": 1, "plugin": {"id": "healthy", "version": "0.1.0", "kind": "rust"},
               "hooks": [{"name": "goal.evaluate"}, {"name": "loop.verifier"},
                         {"name": "loop.planner"}, {"name": "loop.should_stop"}], "tools": []}
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}), flush=True)
    elif m == "hook/goal.evaluate":
        cond = msg["params"]["condition"]
        if cond == "tests pass":
            assert "worker output" in msg["params"]["transcript"]
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"met": True, "reason": "all green"}}), flush=True)
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"met": False, "reason": "not yet"}}), flush=True)
    elif m == "hook/loop.verifier":
        assert msg["params"]["target"] == "green build"
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "score": 80, "satisfied": False, "evidence_quality": "supported",
            "critical_gaps": ["tests still red"], "iteration_summary": "edited main.rs",
            "reason": "close"}}), flush=True)
    elif m == "hook/loop.planner":
        assert msg["params"]["last"]["score"] == 80
        assert msg["params"]["planner_notes"] == "focus on unit tests"
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "directive": "fix the failing unit test", "continuity_brief": "main.rs was edited",
            "planner_notes": "focus on unit tests", "strategy_change": False, "change_note": ""}}), flush=True)
    elif m == "hook/loop.should_stop":
        if msg["params"]["target"] == "green tests":
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"stop": True, "reason": "worker reports done"}}), flush=True)
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"stop": False}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = Arc::new(
        PluginHost::connect_all(
            vec![py_spec("poison", poison), py_spec("healthy", healthy)],
            host_info(),
        )
        .await,
    );
    let adapter = PluginHostAdapter::new(host.clone());
    assert_eq!(host.len(), 2);
    assert!(HookDispatcher::has_goal_evaluate(&adapter));

    // Poison errors -> falls through to healthy's verdict.
    assert!(matches!(
        adapter.goal_evaluate("tests pass", "[User] worker output").await,
        Ok(GoalEvaluateReply::Verdict { met: true, ref reason }) if reason == "all green"
    ));
    // Malformed verdict from first provider is decisive.
    assert!(matches!(
        adapter.goal_evaluate("malformed", "transcript").await,
        Ok(GoalEvaluateReply::Malformed)
    ));

    // loop.verifier + loop.planner round-trip.
    let verdict = adapter
        .loop_verify("green build", "[User] work")
        .await
        .unwrap();
    assert_eq!(verdict.score, 80);
    assert!(!verdict.satisfied);
    assert_eq!(verdict.critical_gaps, ["tests still red"]);
    let plan = adapter
        .loop_plan(
            "green build",
            &["first attempt".into()],
            &verdict,
            "focus on unit tests",
        )
        .await
        .unwrap();
    assert_eq!(plan.directive, "fix the failing unit test");
    assert!(!plan.strategy_change);

    // loop.should_stop: malformed first plugin fails open to second plugin (stop=true and stop=false).
    assert_eq!(
        adapter
            .loop_should_stop("green tests", "[User] worker output")
            .await
            .as_deref(),
        Some("worker reports done")
    );
    assert_eq!(
        adapter.loop_should_stop("keep-going", "transcript").await,
        None
    );

    // Standalone poisoned PluginGoalEvaluator degrades to not-met instead of Err.
    let only_poison =
        Arc::new(PluginHost::connect_all(vec![py_spec("poison", poison)], host_info()).await);
    let evaluator = PluginGoalEvaluator::new(Arc::new(PluginHostAdapter::new(only_poison)));
    assert_eq!(
        evaluator.evaluate("condition", "transcript").await.unwrap(),
        Verdict {
            met: false,
            reason: "goal.evaluate hook error".into(),
        }
    );
}

#[tokio::test]
async fn model_fallback_dispatches_and_first_retry_wins() {
    let make = |id: &str, reply: Option<&str>| -> PluginSpec {
        let answer = match reply {
            Some(expr) => format!(
                "print(json.dumps({{'jsonrpc':'2.0','id':msg['id'],'result':{expr}}}), flush=True)"
            ),
            None => "print(json.dumps({'jsonrpc':'2.0','id':msg['id'],'error':{'code':-32000,'message':'boom'}}), flush=True)".into(),
        };
        let script = format!(
            r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        res = {{"protocol_version":1,"plugin":{{"id":"{id}","version":"0.1.0","kind":"rust"}},
               "hooks":[{{"name":"model.fallback"}}],"tools":[]}}
        print(json.dumps({{"jsonrpc":"2.0","id":msg["id"],"result":res}}), flush=True)
    elif msg.get("method") == "hook/model.fallback":
        p = msg["params"]
        {answer}
    elif "id" in msg:
        print(json.dumps({{"jsonrpc":"2.0","id":msg["id"],"result":{{}}}}), flush=True)
"#
        );
        py_spec(id, &script)
    };
    let input = |attempt: u32| ModelFallbackInput {
        session: SessionId::new(),
        root_session: SessionId::new(),
        agent: Some(AgentName::new("explore")),
        message: MessageId::new(),
        model: ModelRef::new("anthropic/claude-opus-5-5"),
        error_class: ModelFailureClass::Retryable,
        error_message: "http status 529: overloaded".into(),
        attempt,
        tried: vec![
            ModelRef::new("anthropic/claude-opus-5-5"),
            ModelRef::new("anthropic/claude-sonnet-5"),
        ],
    };

    // Chain: give_up, error, malformed, empty-model all pass on; first valid retry wins.
    let first_expr = "{'outcome':'give_up'} if p['attempt'] == 9 else \
        {'outcome':'retry','model':'next/' + p['agent'] + '/' + p['error']['class'] \
         + '/' + str(p['attempt']) + '/' + str(len(p['tried'])) + '/' + p['model'] \
         + '/' + str(p['root_session'] != p['session'])}";
    let second_expr = "{'outcome':'give_up'} if p['attempt'] == 9 else {'outcome':'retry','model':'second/model'}";
    let host = Arc::new(
        PluginHost::connect_all(
            vec![
                make("gives-up", Some("{'outcome':'give_up'}")),
                make("errors", None),
                make("malformed", Some("{'outcome':'maybe'}")),
                make("empty", Some("{'outcome':'retry','model':' '}")),
                make("first", Some(first_expr)),
                make("second", Some(second_expr)),
            ],
            host_info(),
        )
        .await,
    );
    let adapter = PluginHostAdapter::new(host.clone());
    assert_eq!(host.len(), 6);

    assert_eq!(
        adapter.model_fallback(input(2)).await,
        ModelFallbackOutcome::Retry {
            model: ModelRef::new("next/explore/retryable/2/2/anthropic/claude-opus-5-5/True"),
        }
    );
    assert_eq!(
        adapter.model_fallback(input(9)).await,
        ModelFallbackOutcome::GiveUp
    );
}

#[tokio::test]
async fn permission_approve_dispatches_and_falls_through() {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        res = {"protocol_version":1,"plugin":{"id":"approver","version":"0.1.0","kind":"rust"},
               "hooks":[{"name":"permission.approve"}],"tools":[]}
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":res}), flush=True)
    elif msg.get("method") == "hook/permission.approve":
        p = msg["params"]
        mode = p["mode"]
        if mode == "careful" and p["agent"] == "build" and p["action"] == "bash" and p["resource"]["value"] == "ls" and p["root_session"] != p["session"]:
            out = {"result": {"outcome": "allow_once"}}
        elif mode == "careful":
            out = {"result": {"outcome": "reject", "feedback": "mode " + mode + " says no"}}
        elif mode == "defer":
            out = {"result": {"outcome": "defer"}}
        elif mode == "error":
            out = {"error": {"code": -32000, "message": "boom"}}
        else:
            out = {"result": {"outcome": "maybe"}}
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],**out}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":{}}), flush=True)
"#;
    let host =
        Arc::new(PluginHost::connect_all(vec![py_spec("approver", script)], host_info()).await);
    let adapter = PluginHostAdapter::new(host);
    let req = |mode: &str, action: Action, resource: Resource| PermissionApproveInput {
        session: SessionId::new(),
        root_session: SessionId::new(),
        agent: Some(AgentName::new("build")),
        mode: mode.into(),
        action,
        resource,
    };

    assert_eq!(
        adapter
            .permission_approve(req("careful", Action::Bash, Resource::Command("ls".into())))
            .await,
        Some(Decision::AllowOnce)
    );
    assert_eq!(
        HookDispatcher::permission_approve(
            &adapter,
            req("careful", Action::Edit, Resource::Path("x".into()))
        )
        .await,
        Some(Decision::Reject {
            feedback: Some("mode careful says no".into()),
        })
    );
    for mode in ["defer", "error", "malformed"] {
        assert_eq!(
            adapter
                .permission_approve(req(mode, Action::Bash, Resource::Command("ls".into())))
                .await,
            None,
            "{mode} must fall through to user prompt"
        );
    }
}
