//! Hook dispatch, load-order chaining, notification fan-out, and fail-open /
//! fail-safe timeout contracts through `PluginHostAdapter`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_app::plugin_adapter::PluginHostAdapter;
use hya_engine::hooks::{
    AgentSpawnInput, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CompactionAfterInput, CompactionBeforeInput, CompactionDecision,
    CompactionTrigger, HookDispatcher, SessionLifecycleInput, TextCompleteInput,
    TextCompleteOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::PluginKindWire;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MessageId, ModelRef, PartId, SessionId, ToolCallId,
};
use serde_json::json;

use super::{host_info, py_spec, wire_request};

#[tokio::test]
async fn enrichment_and_notification_hooks_dispatch_through_host() {
    let log = std::env::temp_dir().join(format!(
        "hya_plugin_hooks_{}_{}.log",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let script = r#"
import json, os, sys
log = os.environ["HYA_HOOK_LOG"]
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    p = msg.get("params", {})
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "multi", "version": "0.1.0", "kind": "rust"},
            "hooks": [
                {"name": "tool.execute.before"},
                {"name": "command.execute.before"},
                {"name": "experimental.text.complete"},
                {"name": "chat.params"},
                {"name": "compaction.before"},
                {"name": "compaction.after"},
                {"name": "session.start"},
                {"name": "session.end"},
                {"name": "agent.spawn"},
                {"name": "event"},
            ],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/tool.execute.before":
        inp = p["input"]
        inp["mutated"] = True
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "input": inp}}), flush=True)
    elif method == "hook/command.execute.before":
        text = f'{p["text"]} [{p["command"]}:{p["arguments"]}]'
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "text": text}}), flush=True)
    elif method == "hook/experimental.text.complete":
        text = f'{p["text"]} [{p["message"]}:{p["part"]}]'
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "text": text}}), flush=True)
    elif method == "hook/chat.params":
        req = p["request"]
        if req["model"] == "fail/model":
            req["model"] = "evil/model"
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32000, "message": "poison"}}), flush=True)
        else:
            req["model"] = "routed/" + p["agent"] + "/" + p["root_session"]
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "request": req}}), flush=True)
    elif method == "hook/compaction.before":
        if p["messagesTokenEstimate"] == 999:
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32000, "message": "poison"}}), flush=True)
        else:
            assert p["trigger"] == "overflow", p
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "replace", "instructions": "SUMMARIZE AS LIMERICKS"}}), flush=True)
    else:
        if method == "hook/compaction.after":
            assert p["summaryTokens"] == 128, p
        elif method == "hook/agent.spawn":
            assert p["parent"] != p["child"], p
        with open(log, "a") as handle:
            handle.write((method or "?") + "\n")
        if "id" in msg:
            print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let mut spec = py_spec("multi", script);
    spec.env
        .insert("HYA_HOOK_LOG".into(), log.to_string_lossy().into_owned());
    let host = Arc::new(PluginHost::connect_all(vec![spec], host_info()).await);
    let adapter = PluginHostAdapter::new(host.clone());
    assert_eq!(host.len(), 1);

    let session = SessionId::new();
    let root = SessionId::new();
    match adapter
        .tool_execute_before(ToolExecuteBeforeInput {
            session,
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".into(),
            input: json!({"command": "ls"}),
        })
        .await
    {
        ToolExecuteBeforeOutcome::Continue { input } => {
            assert_eq!(input["mutated"], json!(true));
            assert_eq!(input["command"], json!("ls"));
        }
        ToolExecuteBeforeOutcome::Veto { reason } => panic!("unexpected veto: {reason}"),
    }

    let CommandExecuteBeforeOutcome::Continue { text } = adapter
        .command_execute_before(CommandExecuteBeforeInput {
            session,
            command: "review".into(),
            arguments: "commit".into(),
            text: "Review diff".into(),
        })
        .await;
    assert_eq!(text, "Review diff [review:commit]");

    let TextCompleteOutcome::Continue { text } = adapter
        .text_complete(TextCompleteInput {
            session,
            message: MessageId::new(),
            part: PartId::new(),
            text: "draft".into(),
        })
        .await;
    assert!(text.starts_with("draft ["));

    // chat.params lineage rewrite + JSON-RPC error fail-open.
    let ChatParamsOutcome::Continue { request } = adapter
        .chat_params(ChatParamsInput {
            session,
            root_session: root,
            agent: Some(AgentName::new("explore")),
            message: MessageId::new(),
            request: wire_request("fake/model"),
        })
        .await;
    assert_eq!(request.model.as_str(), format!("routed/explore/{root}"));

    let ChatParamsOutcome::Continue { request } = adapter
        .chat_params(ChatParamsInput {
            session,
            root_session: root,
            agent: None,
            message: MessageId::new(),
            request: wire_request("fail/model"),
        })
        .await;
    assert_eq!(request.model.as_str(), "fail/model");

    // compaction.before replace + JSON-RPC error fail-open to Proceed.
    assert_eq!(
        adapter
            .compaction_before(CompactionBeforeInput {
                session,
                trigger: CompactionTrigger::Overflow,
                messages_token_estimate: 4_200,
            })
            .await,
        CompactionDecision::Replace {
            instructions: "SUMMARIZE AS LIMERICKS".into(),
        }
    );
    assert_eq!(
        adapter
            .compaction_before(CompactionBeforeInput {
                session,
                trigger: CompactionTrigger::Overflow,
                messages_token_estimate: 999,
            })
            .await,
        CompactionDecision::Proceed
    );

    // Notification hooks + event fan-out.
    adapter
        .session_start(SessionLifecycleInput { session })
        .await;
    adapter.session_end(SessionLifecycleInput { session }).await;
    adapter
        .agent_spawn(AgentSpawnInput {
            parent: session,
            child: SessionId::new(),
        })
        .await;
    adapter
        .compaction_after(CompactionAfterInput {
            session,
            summary_tokens: 128,
        })
        .await;
    adapter.dispatch_event(&Envelope {
        seq: EventSeq(1),
        ts_millis: 0,
        event: Event::SessionTitled {
            session,
            title: "hello".into(),
        },
    });

    let expected = [
        "hook/session.start",
        "hook/session.end",
        "hook/agent.spawn",
        "hook/compaction.after",
        "event",
    ];
    let mut delivered = BTreeSet::new();
    for _ in 0..200 {
        if let Ok(body) = std::fs::read_to_string(&log) {
            for line in body.lines() {
                delivered.insert(line.to_string());
            }
        }
        if expected.iter().all(|m| delivered.contains(*m)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = std::fs::remove_file(&log);
    for method in expected {
        assert!(delivered.contains(method), "{method} must reach the plugin");
    }
}

#[tokio::test]
async fn hook_chains_fold_in_load_order_across_plugins() {
    let script = r#"
import json, sys, time
tag = sys.argv[1]
delay = float(sys.argv[2])
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        if delay > 0:
            time.sleep(delay)
        result = {
            "protocol_version": 1,
            "plugin": {"id": tag, "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "tool.execute.before"}, {"name": "compaction.before"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/tool.execute.before":
        inp = msg["params"]["input"]
        inp.setdefault("tags", []).append(tag)
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "input": inp}}), flush=True)
    elif method == "hook/compaction.before":
        res = {"outcome": "proceed"} if tag == "first" else {"outcome": "skip", "reason": "not worth folding yet"}
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let make = |tag: &str, delay: &str| PluginSpec {
        id: tag.into(),
        kind: PluginKindWire::Rust,
        command: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            tag.into(),
            delay.into(),
        ],
        timeout_ms: Some(4000),
        env: BTreeMap::new(),
        plugin_dir: None,
        launch: Default::default(),
    };
    let host = Arc::new(
        PluginHost::connect_all(
            vec![make("first", "0.08"), make("second", "0.0")],
            host_info(),
        )
        .await,
    );
    let adapter = PluginHostAdapter::new(host.clone());
    assert_eq!(host.len(), 2);
    assert_eq!(
        host.contributions()
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );

    match adapter
        .tool_execute_before(ToolExecuteBeforeInput {
            session: SessionId::new(),
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".into(),
            input: json!({}),
        })
        .await
    {
        ToolExecuteBeforeOutcome::Continue { input } => {
            assert_eq!(input["tags"], json!(["first", "second"]));
        }
        ToolExecuteBeforeOutcome::Veto { reason } => panic!("unexpected veto: {reason}"),
    }

    assert_eq!(
        adapter
            .compaction_before(CompactionBeforeInput {
                session: SessionId::new(),
                trigger: CompactionTrigger::Proactive,
                messages_token_estimate: 10,
            })
            .await,
        CompactionDecision::Skip {
            reason: "not worth folding yet".into(),
        }
    );
}

#[tokio::test]
async fn timeouts_fail_safe_for_guards_and_open_for_enrichment() {
    let script = |id: &str| {
        format!(
            r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        result = {{
            "protocol_version": 1,
            "plugin": {{"id": "{id}", "version": "0.1.0", "kind": "rust"}},
            "hooks": [{{"name": "tool.execute.before"}}, {{"name": "chat.params"}}],
            "tools": [],
        }}
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": result}}), flush=True)
    elif method in ("hook/tool.execute.before", "hook/chat.params"):
        # Hold without answering so the host's deadline fires cleanly, then exit on EOF.
        sys.stdin.read()
        break
    elif "id" in msg:
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": {{}}}}), flush=True)
"#
        )
    };
    let guard_script = script("stall-guard");
    let enrich_script = script("stall-enrich");
    let mut guard_spec = py_spec("stall-guard", &guard_script);
    guard_spec.timeout_ms = Some(300);
    let mut enrich_spec = py_spec("stall-enrich", &enrich_script);
    enrich_spec.timeout_ms = Some(300);

    let (guard_host, enrich_host) = tokio::join!(
        PluginHost::connect_all(vec![guard_spec], host_info()),
        PluginHost::connect_all(vec![enrich_spec], host_info()),
    );
    assert!(
        !guard_host.prepared_plugins().is_empty() && !enrich_host.prepared_plugins().is_empty(),
        "stall plugins must complete initialize before testing hook timeouts"
    );
    let guard_adapter = PluginHostAdapter::new(Arc::new(guard_host));
    let enrich_adapter = PluginHostAdapter::new(Arc::new(enrich_host));

    let mut original = wire_request("test-model");
    original.temperature = Some(0.5);
    let (guard_out, enrich_out) = tokio::join!(
        guard_adapter.tool_execute_before(ToolExecuteBeforeInput {
            session: SessionId::new(),
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".into(),
            input: json!({"command": "rm -rf /"}),
        }),
        enrich_adapter.chat_params(ChatParamsInput {
            session: SessionId::new(),
            root_session: SessionId::new(),
            agent: None,
            message: MessageId::new(),
            request: original,
        }),
    );

    match guard_out {
        ToolExecuteBeforeOutcome::Veto { reason } => {
            assert!(
                reason.contains("guard failed safe"),
                "expected safe-guard veto reason, got: {reason}"
            );
        }
        ToolExecuteBeforeOutcome::Continue { .. } => {
            panic!("safe guard timeout must veto, not continue");
        }
    }
    let ChatParamsOutcome::Continue { request } = enrich_out;
    assert_eq!(request.temperature, Some(0.5));
    assert_eq!(request.model, ModelRef::new("test-model"));
}
