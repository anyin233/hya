//! T1.10, T1.16–T1.19, T1.21–T1.23 — MCP stdio tool lifecycle & namespace rules,
//! custom slash catalog/expansion, skill-backed slash & live reload, builtin skill
//! tool recovery, and project plugin tools (qualification, structured errors, replay
//! immutability, crash respawn, declaration drift fail-closed, hot manifest reload).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_e2e::{E2eEnv, E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use hya_proto::{Envelope, Event};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(20);
const SKILL_PATH: &str = ".hya/skills/user-playbook/SKILL.md";
const SKILL_BODY: &str = "SKILL_BODY_USER_PLAYBOOK $ARGUMENTS\n";

const PLUGIN_MANIFEST: &str = r#"id = "toolbox"
kind = "rust"
command = ["python3", ".hya/plugins/toolbox/plugin.py"]
timeout_ms = 1000
"#;

const PLUGIN_MANIFEST_SECOND: &str = r#"id = "otherbox"
kind = "rust"
command = ["python3", ".hya/plugins/otherbox/plugin.py"]
timeout_ms = 1000
"#;

const PLUGIN_SCRIPT: &str = r#"import json, os, sys

def response(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        response(message["id"], {
            "protocol_version": 1,
            "plugin": {"id": "toolbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [
                {
                    "name": "remember",
                    "description": "Remember a fact",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"value": {"type": "string"}},
                        "required": ["value"]
                    }
                },
                {
                    "name": "read",
                    "description": "Plugin read qualified",
                    "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
                }
            ]
        })
    elif method == "tool/call":
        params = message.get("params", {})
        incoming = params.get("input") or {}
        value = incoming.get("value")
        with open(".hya/plugin-calls.log", "a", encoding="utf-8") as calls:
            calls.write(json.dumps(incoming, sort_keys=True) + "\n")
        if value == "KILL":
            os._exit(17)
        if value == "ERR_ONCE":
            response(message["id"], {
                "ok": False,
                "output": {"error": {"type": "fixture_error", "message": "PLUGIN_ERROR_MARKER"}}
            })
            continue
        response(message["id"], {
            "ok": True,
            "output": {"tool": "remember", "value": value, "plugin": "toolbox"},
            "time_ms": 2
        })
    elif "id" in message:
        response(message["id"], {})
"#;

const PLUGIN_SCRIPT_DRIFT: &str = r#"import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": {
            "protocol_version": 1,
            "plugin": {"id": "toolbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Changed declaration",
                "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
            }]
        }}), flush=True)
    elif "id" in message:
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": {}}), flush=True)
"#;

const PLUGIN_SCRIPT_V2: &str = r#"import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": {
            "protocol_version": 1,
            "plugin": {"id": "toolbox", "version": "0.2.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Remember v2",
                "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
            }]
        }}), flush=True)
    elif "id" in message:
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": {}}), flush=True)
"#;

const PLUGIN_SCRIPT_SECOND: &str = r#"import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": {
            "protocol_version": 1,
            "plugin": {"id": "otherbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Second remember",
                "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
            }]
        }}), flush=True)
    elif "id" in message:
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": {}}), flush=True)
"#;

fn skill_markdown(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}")
}

fn write_skill(root: &Path, relative: &str, name: &str, description: &str, body: &str) {
    let dir = root.join(relative);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        skill_markdown(name, description, body),
    )
    .unwrap();
}

fn write_command(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn private_home(env: &E2eEnv) -> PathBuf {
    env.backend
        .xdg_config_home
        .parent()
        .expect("backend root")
        .join("home")
}

fn array_data(value: &Value) -> &[Value] {
    value
        .get("data")
        .or_else(|| value.get("commands"))
        .or_else(|| value.get("skills"))
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn catalog_entry<'a>(catalog: &'a Value, name: &str) -> &'a Value {
    array_data(catalog)
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("missing catalog entry {name}: {catalog}"))
}

fn unique_names(catalog: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let names = array_data(catalog)
        .iter()
        .map(|entry| entry["name"].as_str().expect("catalog name").to_string())
        .collect::<Vec<_>>();
    for name in &names {
        assert!(
            seen.insert(name),
            "duplicate catalog name {name}: {catalog}"
        );
    }
    names
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(Value::as_str)
                .or_else(|| tool.get("name").and_then(Value::as_str))
                .map(str::to_string)
        })
        .collect()
}

async fn request_json(
    env: &E2eEnv,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = env
        .http
        .request(method, format!("{}{path}", env.backend.url));
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.expect("HTTP request");
    let status = resp.status();
    let text = resp.text().await.expect("HTTP body");
    let value = serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text));
    (status, value)
}

fn command_request(command: &str, arguments: &str, text: Option<&str>) -> Value {
    let mut inner = json!({"command": command, "arguments": arguments});
    if let Some(text) = text {
        inner["text"] = json!(text);
    }
    json!({ "command": inner })
}

async fn command_turn(env: &E2eEnv, session: hya_proto::SessionId, body: Value) -> String {
    let (status, value) = request_json(
        env,
        Method::POST,
        &format!("/v2/sessions/{session}/turns"),
        Some(body),
    )
    .await;
    assert!(
        status.is_success(),
        "POST v1 turn returned {status}: {value}; {}",
        env.diagnostics()
    );
    env.wait_session_idle(&session, TIMEOUT)
        .await
        .expect("v1 command turn completion");
    let messages = env
        .get_json(&format!("/v2/sessions/{session}/messages"))
        .await
        .expect("v1 transcript");
    messages["messages"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .rev()
                .find(|m| m["role"].as_str() == Some("ROLE_USER"))
        })
        .and_then(|m| m["parts"].as_array().and_then(|p| p.first()))
        .and_then(|part| {
            part.get("text")
                .and_then(|t| t.get("text"))
                .or_else(|| part.get("text"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_string()
}

fn assert_command_event(events: &[Envelope], command: &str, arguments: &str) {
    assert!(
        events.iter().any(|e| matches!(
            &e.event,
            Event::CommandExecuted { command: c, arguments: a, .. }
                if c == command && a == arguments
        )),
        "missing CommandExecuted for {command} {arguments:?}"
    );
}

fn assert_one_tool_terminal(events: &[Envelope], name: &str) {
    let reqs = events
        .iter()
        .filter(
            |e| matches!(&e.event, Event::ToolCallRequested { name: n, .. } if n.as_str() == name),
        )
        .count();
    let terms = events
        .iter()
        .filter(|e| matches!(&e.event, Event::ToolResult { .. } | Event::ToolError { .. }))
        .filter(|e| {
            let term_call = match &e.event {
                Event::ToolResult { call, .. } | Event::ToolError { call, .. } => call,
                _ => return false,
            };
            events.iter().any(|c| {
                matches!(&c.event, Event::ToolCallRequested { call, name: n, .. } if call == term_call && n.as_str() == name)
            })
        })
        .count();
    assert_eq!(reqs, 1, "expected 1 request for {name}");
    assert_eq!(terms, 1, "expected 1 terminal event for {name}");
}

/// T1.10, T1.16, T1.17, T1.18, T1.21 (MCP), T1.22 (Skill + MCP reload).
#[tokio::test]
async fn mcp_and_custom_slash_skill_expansion_and_reload() {
    let env = E2eEnvBuilder::new()
        .with_mcp_echo()
        .skill_file(
            SKILL_PATH,
            skill_markdown("user-playbook", "User playbook", SKILL_BODY),
        )
        .project_file(
            ".hya/command/markdown-root.md",
            b"---\ndescription: markdown root\nagent: reviewer\nmodel: fake/reviewer\nsubtask: true\n---\nMARKDOWN_ROOT $1 $ARGUMENTS\n"
                .to_vec(),
        )
        .project_file(
            ".hya/commands/nested/inspect.md",
            b"NESTED_INSPECT $ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/quotes.md",
            b"QUOTES=$ARGUMENTS|$1|$2\n".to_vec(),
        )
        .project_file(
            ".hya/commands/positions.md",
            b"POSITION=$1|$10|$2|$11|$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/user-playbook.md",
            b"---\ndescription: command beats Skill\n---\nCOMMAND_WINS $ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/use-skill.md",
            b"---\ndescription: use skill\n---\nCall builtin skill user-playbook $ARGUMENTS\n"
                .to_vec(),
        )
        .project_file(
            ".hya/commands/use-mcp.md",
            b"---\ndescription: use mcp\n---\nCall mcp__echo__ping msg=$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/bad.md",
            b"---\ndescription: [broken\n---\nOMITTED\n".to_vec(),
        )
        .build()
        .await
        .expect("e2e env");

    // Legacy roots are ignored; project skill wins over HOME skill on duplicate names.
    write_command(
        &private_home(&env),
        ".config/opencode/command/global.md",
        "GLOBAL_IGNORED",
    );
    write_command(
        &env.backend.project,
        ".opencode/commands/legacy.md",
        "LEGACY_IGNORED",
    );
    write_skill(
        &private_home(&env).join(".config/hya/skills"),
        "duplicate-playbook",
        "duplicate-playbook",
        "home dup",
        "DUPLICATE_HOME",
    );
    write_skill(
        &env.backend.project.join(".hya/skills"),
        "duplicate-playbook",
        "duplicate-playbook",
        "project dup",
        "DUPLICATE_PROJECT",
    );

    // 1. T1.10: MCP echo connects, lists tools, and returns `echo:hya-e2e-mcp` to follow-up.
    let status = env
        .wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("mcp connected");
    let echo = status["servers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"] == "echo"))
        .expect("echo row");
    assert_eq!(echo["state"], "MCP_SERVER_STATE_CONNECTED");
    assert_eq!(echo["tools"], json!(["mcp__echo__ping", "mcp__echo__slow"]));

    let mcp_session = env.create_session().await.unwrap();
    env.fake
        .push_scripts([
            tool_step("mcp__echo__ping", json!({ "msg": "hya-e2e-mcp" })),
            text_step("MCP_DONE"),
        ])
        .unwrap();
    env.prompt(mcp_session, "call mcp echo ping")
        .await
        .expect("mcp prompt");
    let reqs = env.fake.requests().unwrap();
    assert!(
        fake_requests_from(&reqs, 1).contains("echo:hya-e2e-mcp"),
        "follow-up must include MCP echo result"
    );

    // 2. T1.21 (MCP): ambiguous `__` server/tool names fail closed at the door.
    let (bad_status, bad_body) = request_json(
        &env,
        Method::POST,
        "/v2/mcp",
        Some(json!({
            "name": "a__b",
            "command": {"command": "python3", "args": ["fixtures/mcp_echo.py"]}
        })),
    )
    .await;
    assert_eq!(bad_status, StatusCode::INTERNAL_SERVER_ERROR, "{bad_body}");
    assert!(bad_body.to_string().contains("must be"), "{bad_body}");

    // 3. T1.16: command & skill catalog discovery, precedence, expansion, explicit text bypass.
    let catalog = env
        .get_json(&format!(
            "/v2/commands?directory={}",
            env.backend.workdir_str()
        ))
        .await
        .unwrap();
    let names = unique_names(&catalog);
    for expected in [
        "markdown-root",
        "nested/inspect",
        "user-playbook",
        "skill:user-playbook",
    ] {
        assert!(names.iter().any(|n| n == expected), "missing {expected}");
    }
    for ignored in ["bad", "global", "legacy"] {
        assert!(!names.iter().any(|n| n == ignored), "leaked {ignored}");
    }
    let root_entry = catalog_entry(&catalog, "markdown-root");
    assert_eq!(root_entry["source"], "command");
    assert_eq!(root_entry["agent"], "reviewer");
    assert_eq!(root_entry["model"], "fake/reviewer");
    assert_eq!(root_entry["subtask"], true);
    assert_eq!(
        catalog_entry(&catalog, "user-playbook")["source"],
        "command"
    );
    assert_eq!(
        catalog_entry(&catalog, "skill:user-playbook")["source"],
        "skill"
    );

    let skills = env
        .get_json(&format!(
            "/v2/skills?directory={}",
            env.backend.workdir_str()
        ))
        .await
        .unwrap();
    assert_eq!(
        catalog_entry(&skills, "duplicate-playbook")["content"],
        "DUPLICATE_PROJECT"
    );

    let cmd_session = env.create_session().await.unwrap();
    env.fake
        .push_scripts([
            text_step("R0"),
            text_step("R1"),
            text_step("R2"),
            text_step("R3"),
            text_step("R4"),
        ])
        .unwrap();
    let r0 = command_turn(
        &env,
        cmd_session,
        command_request("markdown-root", "alpha beta", None),
    )
    .await;
    assert_eq!(r0, "MARKDOWN_ROOT alpha alpha beta");
    assert_command_event(
        &env.events(cmd_session, None).await.unwrap(),
        "markdown-root",
        "alpha beta",
    );
    assert_eq!(
        command_turn(
            &env,
            cmd_session,
            command_request("nested/inspect", "target", None)
        )
        .await,
        "NESTED_INSPECT target"
    );
    assert_eq!(
        command_turn(
            &env,
            cmd_session,
            command_request("quotes", "\"hello world\" tail", None)
        )
        .await,
        "QUOTES=\"hello world\" tail|hello world|tail"
    );
    assert_eq!(
        command_turn(
            &env,
            cmd_session,
            command_request("positions", "a b c d e f g h i j literal-$1", None)
        )
        .await,
        "POSITION=a|j|b|literal-$1|a b c d e f g h i j literal-$1"
    );
    assert_eq!(
        command_turn(
            &env,
            cmd_session,
            command_request("positions", "one two", Some("EXPLICIT_TEXT"))
        )
        .await,
        "EXPLICIT_TEXT"
    );

    // 4. T1.17 + T1.22: direct `/skill:user-playbook` expands without calling builtin `skill`,
    // refreshes on edit while prior transcript stays immutable, and falls back to literal when removed.
    let dyn_session = env.create_session().await.unwrap();
    env.fake
        .push_scripts([text_step("S_OLD"), text_step("S_NEW"), text_step("S_STALE")])
        .unwrap();
    let s_old = command_turn(
        &env,
        dyn_session,
        command_request("skill:user-playbook", "OLD", None),
    )
    .await;
    assert!(
        s_old.starts_with("<skill name=\"user-playbook\"")
            && s_old.ends_with("SKILL_BODY_USER_PLAYBOOK $ARGUMENTS\n</skill>\n\nOLD"),
        "{s_old}"
    );
    let old_events = env.events(dyn_session, None).await.unwrap();
    assert!(!old_events.iter().any(|e| matches!(
        &e.event,
        Event::ToolCallRequested { name, .. } if name.as_str() == "skill"
    )));
    let old_gen = old_events
        .iter()
        .find_map(|e| match &e.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        })
        .expect("old generation");

    std::fs::write(
        env.project_path(SKILL_PATH),
        skill_markdown("user-playbook", "User playbook", "SKILL_EDITED_BODY\n"),
    )
    .unwrap();
    let s_new = command_turn(
        &env,
        dyn_session,
        command_request("skill:user-playbook", "NEW", None),
    )
    .await;
    assert!(
        s_new.ends_with("SKILL_EDITED_BODY\n</skill>\n\nNEW"),
        "{s_new}"
    );
    let after_edit_ctx = env.session_context(&dyn_session).await.unwrap().to_string();
    assert!(
        after_edit_ctx.contains("SKILL_BODY_USER_PLAYBOOK")
            && after_edit_ctx.contains("SKILL_EDITED_BODY"),
        "both old immutable turn and new edited turn must be present"
    );
    let new_gens: Vec<_> = env
        .events(dyn_session, None)
        .await
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        })
        .collect();
    assert_eq!(new_gens[0], old_gen, "old TurnBinding must stay immutable");
    assert!(
        new_gens.iter().any(|g| *g != old_gen),
        "edit must publish new generation"
    );

    std::fs::remove_file(env.project_path(SKILL_PATH)).unwrap();
    let s_stale = command_turn(
        &env,
        dyn_session,
        command_request("skill:user-playbook", "STALE", None),
    )
    .await;
    assert_eq!(s_stale, "/skill:user-playbook STALE");

    // 5. T1.22 (MCP disconnect/reconnect boundary).
    env.post_json("/v2/mcp/echo/disconnect", &Value::Null)
        .await
        .unwrap();
    let disc = env.get_json("/v2/mcp").await.unwrap();
    let disc_echo = disc["servers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"] == "echo"))
        .unwrap_or_else(|| panic!("echo row missing: {disc}"));
    assert_eq!(disc_echo["state"], "MCP_SERVER_STATE_DISCONNECTED");
    assert!(
        disc_echo
            .get("tools")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    );
    env.post_json("/v2/mcp/echo/connect", &Value::Null)
        .await
        .unwrap();
    env.wait_mcp_connected("echo", TIMEOUT).await.unwrap();
    env.fake
        .push_scripts([
            tool_step("mcp__echo__ping", json!({"msg": "MCP_RELOAD"})),
            text_step("MCP_RELOAD_DONE"),
        ])
        .unwrap();
    command_turn(
        &env,
        dyn_session,
        command_request("use-mcp", "MCP_RELOAD", None),
    )
    .await;
    assert!(
        env.events(dyn_session, None)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                &e.event,
                Event::ToolResult { output, .. } if output.to_string().contains("echo:MCP_RELOAD")
            ))
    );

    // 6. T1.18: custom command invokes builtin `skill` tool; unknown skill emits structured
    // ToolError and same session recovers on next valid call.
    std::fs::write(
        env.project_path(SKILL_PATH),
        skill_markdown("user-playbook", "User playbook", SKILL_BODY),
    )
    .unwrap();
    let skill_session = env.create_session().await.unwrap();
    let before_skill = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([
            tool_step("skill", json!({"name": "does-not-exist"})),
            text_step("UNKNOWN_RECOVERED"),
            tool_step("skill", json!({"name": "user-playbook"})),
            text_step("SKILL_RECOVERED"),
        ])
        .unwrap();
    command_turn(
        &env,
        skill_session,
        command_request("use-skill", "ERR_NONCE", None),
    )
    .await;
    command_turn(
        &env,
        skill_session,
        command_request("use-skill", "OK_NONCE", None),
    )
    .await;
    let skill_events = env.events(skill_session, None).await.unwrap();
    assert!(skill_events.iter().any(|e| matches!(
        &e.event,
        Event::ToolError { value: Some(v), .. } if v["error"]["type"] == "unknown"
    )));
    assert!(skill_events.iter().any(|e| matches!(
        &e.event,
        Event::ToolResult { output, .. } if output.to_string().contains("SKILL_BODY_USER_PLAYBOOK")
    )));
    assert!(
        fake_requests_from(&env.fake.requests().unwrap(), before_skill + 1)
            .contains("SKILL_BODY_USER_PLAYBOOK")
    );
}

/// T1.19, T1.21 (plugin qualification & builtin protection), T1.22 (plugin hot reload),
/// T1.23 (structured custom Tool errors, replay immutability, same-session recovery).
#[tokio::test]
async fn plugin_tool_conflicts_errors_respawn_and_hot_reload() {
    let env = E2eEnvBuilder::new()
        .project_file(
            ".hya/plugins/toolbox/plugin.toml",
            PLUGIN_MANIFEST.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/plugins/toolbox/plugin.py",
            PLUGIN_SCRIPT.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/plugins/otherbox/plugin.toml",
            PLUGIN_MANIFEST_SECOND.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/plugins/otherbox/plugin.py",
            PLUGIN_SCRIPT_SECOND.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/commands/use-plugin.md",
            b"---\ndescription: use plugin\n---\nCall toolbox__remember value=$ARGUMENTS\n"
                .to_vec(),
        )
        .build()
        .await
        .expect("plugin env");

    // 1. T1.19 + T1.21: plugin tools are qualified (`toolbox__remember`, `otherbox__remember`,
    // `toolbox__read`), built-in `read` stays bare & unique, and `/use-plugin` runs the tool.
    let session = env.create_session().await.unwrap();
    env.fake
        .push_scripts([
            tool_step("toolbox__remember", json!({"value": "PLUGIN_NONCE"})),
            text_step("PLUGIN_FINAL"),
        ])
        .unwrap();
    let expanded = command_turn(
        &env,
        session,
        command_request("use-plugin", "PLUGIN_NONCE", None),
    )
    .await;
    assert_eq!(expanded, "Call toolbox__remember value=PLUGIN_NONCE");

    let reqs = env.fake.requests().unwrap();
    let offered = tool_names(&reqs[0]);
    assert!(offered.iter().any(|n| n == "toolbox__remember"));
    assert!(offered.iter().any(|n| n == "otherbox__remember"));
    assert!(offered.iter().any(|n| n == "toolbox__read"));
    assert!(!offered.iter().any(|n| n == "remember"));
    assert_eq!(offered.iter().filter(|n| *n == "read").count(), 1);
    assert!(fake_requests_from(&reqs, 1).contains("PLUGIN_NONCE"));

    let events = env.events(session, None).await.unwrap();
    assert_one_tool_terminal(&events, "toolbox__remember");
    let ctx = env.session_context(&session).await.unwrap().to_string();
    assert!(ctx.contains("PLUGIN_NONCE") && ctx.contains("TOOL_EXECUTION_STATE_OK"));

    // 2. T1.23: structured ToolError, replay does NOT re-execute the plugin, same session recovers.
    let err_session = env.create_session().await.unwrap();
    env.fake
        .push_scripts([
            tool_step("toolbox__remember", json!({"value": "ERR_ONCE"})),
            text_step("AFTER_ERR"),
            tool_step("toolbox__remember", json!({"value": "VALID_AFTER_ERROR"})),
            text_step("RECOVERED_FINAL"),
        ])
        .unwrap();
    command_turn(
        &env,
        err_session,
        command_request("use-plugin", "ERR_ONCE", None),
    )
    .await;
    let err_events = env.events(err_session, None).await.unwrap();
    assert_one_tool_terminal(&err_events, "toolbox__remember");
    assert!(err_events.iter().any(|e| matches!(
        &e.event,
        Event::ToolError { value: Some(v), .. } if v["error"]["type"] == "unknown"
    )));
    let err_ctx = env.session_context(&err_session).await.unwrap().to_string();
    assert!(
        err_ctx.contains("TOOL_EXECUTION_STATE_ERROR")
            && err_ctx.contains("\"errorCode\":\"unknown\"")
    );

    let calls_path = env.project_path(".hya/plugin-calls.log");
    let calls_before = std::fs::read_to_string(&calls_path)
        .unwrap()
        .lines()
        .count();
    let fake_before = env.fake.requests().unwrap().len();
    let _ = env.events(err_session, None).await.unwrap();
    let _ = env.session_context(&err_session).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        calls_before,
        "replay must never re-execute plugin tool"
    );
    assert_eq!(env.fake.requests().unwrap().len(), fake_before);

    command_turn(
        &env,
        err_session,
        command_request("use-plugin", "VALID_AFTER_ERROR", None),
    )
    .await;
    assert!(
        env.events(err_session, None)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                &e.event,
                Event::ToolResult { output, .. } if output["value"] == "VALID_AFTER_ERROR"
            ))
    );

    // 3. T1.19 + T1.22: plugin crash → lazy respawn → declaration drift fails closed → hot manifest reload.
    env.fake
        .push_scripts([
            tool_step("toolbox__remember", json!({"value": "KILL"})),
            text_step("KILL_RECOVERED"),
            tool_step("toolbox__remember", json!({"value": "RESPAWN"})),
            text_step("RESPAWN_OK"),
            tool_step("toolbox__remember", json!({"value": "KILL"})),
            text_step("DRIFT_KILL_OK"),
            tool_step("toolbox__remember", json!({"value": "DRIFT"})),
            text_step("DRIFT_ERROR_OK"),
            text_step("AFTER_DRIFT_OK"),
            text_step("HOT_RELOAD_OK"),
        ])
        .unwrap();
    command_turn(&env, session, command_request("use-plugin", "KILL", None)).await;
    command_turn(
        &env,
        session,
        command_request("use-plugin", "RESPAWN", None),
    )
    .await;
    assert!(
        env.events(session, None)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                &e.event,
                Event::ToolResult { output, .. } if output["value"] == "RESPAWN"
            ))
    );

    std::fs::write(
        env.project_path(".hya/plugins/toolbox/plugin.py"),
        PLUGIN_SCRIPT_DRIFT,
    )
    .unwrap();
    command_turn(&env, session, command_request("use-plugin", "KILL", None)).await;
    command_turn(&env, session, command_request("use-plugin", "DRIFT", None)).await;
    assert!(
        env.events(session, None)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                &e.event,
                Event::ToolError { message_text, .. } if message_text.contains("declaration")
            ))
    );
    env.prompt(session, "ordinary prompt after drift")
        .await
        .expect("session usable after drift");

    std::fs::write(
        env.project_path(".hya/plugins/toolbox/plugin-v2.py"),
        PLUGIN_SCRIPT_V2,
    )
    .unwrap();
    std::fs::write(
        env.project_path(".hya/plugins/toolbox/plugin.toml"),
        "id = \"toolbox\"\nkind = \"rust\"\ncommand = [\"python3\", \".hya/plugins/toolbox/plugin-v2.py\"]\ntimeout_ms = 1000\n",
    )
    .unwrap();
    let before_edit = env.fake.requests().unwrap().len();
    env.prompt(session, "inspect respawned plugin")
        .await
        .expect("prompt after manifest edit");
    let schema = env.fake.requests().unwrap()[before_edit].to_string();
    assert!(
        schema.contains("Remember v2") && !schema.contains("Remember a fact"),
        "manifest edit must hot-respawn v2: {schema}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_stdio_permissions_and_session_mcp_lifecycle() {
    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .code_mode("with_tools")
        .scripts(vec![
            tool_step("mcp__peer__alpha", json!({})),
            text_step("ALLOWED"),
            tool_step("mcp__peer__beta", json!({})),
            text_step("DENIED"),
            tool_step("mcp__peer__alpha", json!({})),
            text_step("PARALLEL"),
            text_step("RESUMED"),
            tool_step("mcp__peer__alpha", json!({})),
            text_step("SURVIVED"),
            hya_e2e::http_error_step(401),
        ])
        .build()
        .await
        .unwrap();
    let output = tokio::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/acp_editor.py"
        ))
        .arg(&env.backend.binary)
        .arg(&env.backend.db)
        .current_dir(&env.backend.project)
        .env("XDG_CONFIG_HOME", &env.backend.xdg_config_home)
        .env("XDG_DATA_HOME", &env.backend.xdg_data_home)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = env.fake.requests().unwrap();
    for (index, tool, foreign) in [(0, "alpha", "beta"), (2, "beta", "alpha")] {
        let tools = requests[index]["tools"].to_string();
        assert!(
            tools.contains(&format!("mcp__peer__{tool}"))
                && !tools.contains(&format!("mcp__peer__{foreign}")),
            "{tools}"
        );
    }
    assert!(
        requests[0]["messages"]
            .to_string()
            .contains("allow [fixture](file:///fixture) after-link")
    );
    let sessions = env.get_json("/v2/sessions").await.unwrap();
    let rows = sessions["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "failed discovery must not leave a session");
    let first = rows
        .iter()
        .find(|row| row["workdir"] == env.backend.workdir_str())
        .unwrap();
    let second = rows
        .iter()
        .find(|row| row["workdir"] == env.project_path("second").display().to_string())
        .unwrap();
    assert!(first["projectId"].is_string());
    assert_eq!(first["projectId"], second["projectId"]);
    let global = env.get_json("/v2/mcp").await.unwrap();
    assert!(!global.to_string().contains("mcp__peer__"));
}
