//! P18 — deterministic custom slash commands and dynamic resource process coverage.
//!
//! These scenarios deliberately drive the real backend process.  The catalog,
//! Compat routes, native route, Skill plane, plugin host, and MCP manager all
//! remain production code; fixtures only provide deterministic stdio/model
//! observations.
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
const USE_SKILL_COMMAND: &str = "Call builtin skill with name=\"user-playbook\", then return SKILL_BODY_USER_PLAYBOOK and the nonce $ARGUMENTS.";
const USE_PLUGIN_COMMAND: &str =
    "Call plugin Tool toolbox__remember with value=$ARGUMENTS, then return the plugin result.";
const USE_MCP_COMMAND: &str =
    "Call mcp__echo__ping with msg=$ARGUMENTS, then return echo:$ARGUMENTS.";

/// The required project plugin manifest.  The relative command is intentional:
/// a project plugin starts with its Project root (the temporary project) as
/// its cwd.
const PLUGIN_MANIFEST: &str = r#"id = "toolbox"
kind = "rust"
command = ["python3", ".hya/plugins/toolbox/plugin.py"]
timeout_ms = 1000
"#;

/// Deterministic plugin protocol fixture for the `remember` Tool.
///
/// In addition to the normal response, values used by negative cases exercise
/// malformed input, process death, and a call log used to prove replay does not
/// execute a Tool again.
const PLUGIN_SCRIPT: &str = r#"import json
import os
import sys


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
            "tools": [{
                "name": "remember",
                "description": "Remember a fact",
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"]
                }
            }]
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
        if value == "MALFORMED_FRAME":
            print("not-json", flush=True)
            continue
        if not isinstance(value, str):
            response(message["id"], {
                "ok": False,
                "output": {"error": {"type": "invalid_params", "message": "value must be string"}}
            })
            continue
        response(message["id"], {
            "ok": True,
            "output": {
                "tool": "remember",
                "value": value,
                "session": params.get("session"),
                "plugin": "toolbox"
            },
            "time_ms": 2
        })
    elif "id" in message:
        response(message["id"], {})
"#;

/// A changed declaration used to prove that an old PluginHost fails closed on
/// respawn instead of silently accepting declaration drift.
const PLUGIN_SCRIPT_DRIFT: &str = r#"import json
import sys


def response(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        response(message["id"], {
            "protocol_version": 1,
            "plugin": {"id": "toolbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Changed declaration",
                "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
            }]
        })
    elif message.get("method") == "tool/call":
        params = message.get("params", {})
        response(message["id"], {"ok": True, "output": {"tool": "remember", "value": (params.get("input") or {}).get("value")}})
    elif "id" in message:
        response(message["id"], {})
"#;

/// Plugin command used after a backend restart.  Its changed description is a
/// direct schema oracle for the restart boundary.
const PLUGIN_SCRIPT_V2: &str = r#"import json
import sys


def response(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        response(message["id"], {
            "protocol_version": 1,
            "plugin": {"id": "toolbox", "version": "0.2.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Remember v2",
                "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
            }]
        })
    elif message.get("method") == "tool/call":
        params = message.get("params", {})
        response(message["id"], {"ok": True, "output": {"tool": "remember", "value": (params.get("input") or {}).get("value"), "plugin": "v2"}})
    elif "id" in message:
        response(message["id"], {})
"#;

/// Second plugin declaration for duplicate-name rejection.
const PLUGIN_MANIFEST_SECOND: &str = r#"id = "otherbox"
kind = "rust"
command = ["python3", ".hya/plugins/otherbox/plugin.py"]
timeout_ms = 1000
"#;

const PLUGIN_SCRIPT_SECOND: &str = r#"import json
import sys


def response(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        response(message["id"], {
            "protocol_version": 1,
            "plugin": {"id": "otherbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Second remember",
                "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
            }]
        })
    elif message.get("method") == "tool/call":
        response(message["id"], {"ok": True, "output": {"tool": "remember", "value": "second"}})
    elif "id" in message:
        response(message["id"], {})
"#;

const PLUGIN_SCRIPT_READ: &str = r#"import json
import sys


def response(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "initialize":
        response(message["id"], {
            "protocol_version": 1,
            "plugin": {"id": "toolbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "read",
                "description": "Plugin read collision",
                "inputSchema": {"type": "object", "properties": {"value": {"type": "string"}}}
            }]
        })
    elif "id" in message:
        response(message["id"], {})
"#;

/// MCP protocol fixture with deterministic success, error, timeout, malformed,
/// oversized-frame, and process-death modes selected by the `msg` argument.
const MCP_SCENARIO_SCRIPT: &str = r#"import json
import os
import sys


def response(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        response(request["id"], {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "echo", "version": "0.0.1"}
        })
    elif method == "tools/list":
        response(request["id"], {
            "tools": [{
                "name": "ping",
                "description": "Ping echo",
                "inputSchema": {"type": "object", "properties": {"msg": {"type": "string"}}}
            }]
        })
    elif method == "tools/call":
        arguments = (request.get("params") or {}).get("arguments") or {}
        msg = arguments.get("msg", "pong")
        if msg == "TIMEOUT":
            continue
        if msg == "FRAME":
            print("{", flush=True)
            continue
        if msg == "OVERSIZED":
            print("x" * (1024 * 1024 + 1), flush=True)
            continue
        if msg == "DEATH":
            os._exit(23)
        if msg == "ERROR":
            response(request["id"], {
                "content": [{"type": "text", "text": "MCP_ERROR_MARKER"}],
                "isError": True
            })
            continue
        if msg == "MALFORMED":
            response(request["id"], {"unexpected": True})
            continue
        response(request["id"], {
            "content": [{"type": "text", "text": "echo:" + str(msg)}],
            "isError": False
        })
    else:
        # Answer everything else (e.g. `resources/list` on connect) at once;
        # a silent server makes each connect wait out the MCP call timeout.
        response(request["id"], {})
"#;

/// MCP fixture that exposes two tool names whose server prefixes can be crafted
/// into the same model-facing namespace.
const MCP_COLLISION_SCRIPT: &str = r#"import json
import sys


tool_name = sys.argv[1] if len(sys.argv) > 1 else "echo"


def response(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        response(request["id"], {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "collision", "version": "0.0.1"}})
    elif method == "tools/list":
        response(request["id"], {"tools": [
            {"name": tool_name, "description": "collision tool", "inputSchema": {"type": "object"}}
        ]})
    elif method == "tools/call":
        response(request["id"], {"content": [{"type": "text", "text": "collision-result"}], "isError": False})
    else:
        response(request["id"], {})
"#;
/// Encode a Skill fixture with the parser's exact frontmatter/body contract.
fn skill_markdown(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n{body}")
}

/// Write a Skill beneath one of the process fixture's discovery roots.
fn write_skill(root: &Path, relative: &str, name: &str, description: &str, body: &str) {
    let directory = root.join(relative);
    std::fs::create_dir_all(&directory).expect("skill directory");
    std::fs::write(
        directory.join("SKILL.md"),
        skill_markdown(name, description, body),
    )
    .expect("skill file");
}

/// Write a command Markdown fixture, creating nested roots as needed.
fn write_command(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("command directory");
    }
    std::fs::write(path, body).expect("command file");
}

/// Return the isolated HOME used by `BackendProcess`.
fn private_home(env: &E2eEnv) -> PathBuf {
    env.backend
        .xdg_config_home
        .parent()
        .expect("backend root")
        .join("home")
}

/// Return the array payload for either bare Compat or `{data: [...]}` routes.
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

/// Fetch the full command catalog for the temporary project.
async fn command_catalog(env: &E2eEnv) -> Value {
    env.get_json(&format!(
        "/v1/commands?directory={}",
        env.backend.workdir_str()
    ))
    .await
    .expect("command catalog")
}

/// Fetch the full Skill catalog for the temporary project.
async fn skill_catalog(env: &E2eEnv) -> Value {
    env.get_json(&format!(
        "/v1/skills?directory={}",
        env.backend.workdir_str()
    ))
    .await
    .expect("skill catalog")
}

/// Find one catalog entry by name, failing with the complete catalog on error.
fn catalog_entry<'a>(catalog: &'a Value, name: &str) -> &'a Value {
    array_data(catalog)
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("missing catalog entry {name}: {catalog}"))
}

/// Assert that catalog names are unique and return them in wire order.
fn unique_names(catalog: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let names = array_data(catalog)
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .expect("catalog name")
                .to_string()
        })
        .collect::<Vec<_>>();
    for name in &names {
        assert!(
            seen.insert(name),
            "duplicate catalog name {name}: {catalog}"
        );
    }
    names
}

/// Extract model Tool names from either OpenAI function-schema representation.
fn tool_names(request: &Value) -> Vec<String> {
    request
        .get("tools")
        .and_then(Value::as_array)
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

/// Send one raw JSON request and preserve both status and body for negative
/// route assertions.
async fn request_json(
    env: &E2eEnv,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = env
        .http
        .request(method, format!("{}{path}", env.backend.url));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("HTTP request");
    let status = response.status();
    let text = response.text().await.expect("HTTP body");
    let value = serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text));
    (status, value)
}

/// POST a JSON route and require a successful JSON response.
async fn post_ok(env: &E2eEnv, path: &str, body: Value) -> Value {
    let (status, value) = request_json(env, Method::POST, path, Some(body)).await;
    assert!(
        status.is_success(),
        "POST {path} returned {status}: {value}; {}",
        env.diagnostics()
    );
    value
}

/// Build the shared command request shape, omitting `text` unless explicit text
/// bypass is under test.
fn command_request(command: &str, arguments: &str, text: Option<&str>) -> Value {
    let mut inner = json!({"command": command, "arguments": arguments});
    if let Some(text) = text {
        inner["text"] = json!(text);
    }
    json!({ "command": inner })
}

/// POST one v1 command turn and return the persisted user text wrapped in
/// the legacy response shape so `response_text` keeps working.
async fn command_turn(env: &E2eEnv, session: impl std::fmt::Display, body: Value) -> Value {
    let (status, value) = request_json(
        env,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        Some(body),
    )
    .await;
    assert!(
        status.is_success(),
        "POST v1 turn returned {status}: {value}; {}",
        env.diagnostics()
    );
    if let Ok(session_id) = format!("{session}").parse::<hya_proto::SessionId>() {
        env.wait_session_idle(&session_id, TIMEOUT)
            .await
            .expect("v1 command turn completion");
    }
    let messages = env
        .get_json(&format!("/v1/sessions/{session}/messages"))
        .await
        .expect("v1 transcript");
    let text = messages
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("ROLE_USER"))
        })
        .and_then(|message| {
            message
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| parts.first())
        })
        .and_then(|part| {
            part.get("text")
                .and_then(|text| text.get("text"))
                .or_else(|| part.get("text"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_string();
    json!({ "parts": [{ "text": text }] })
}

/// Read the first user text from a legacy or V2 command response.
fn response_text(response: &Value) -> &str {
    response
        .pointer("/parts/0/text")
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .pointer("/data/parts/0/text")
                .and_then(Value::as_str)
        })
        .expect("command response user text")
}

/// Assert a command event carries the exact command/arguments and a message id.
fn assert_command_event(events: &[Envelope], command: &str, arguments: &str) {
    assert!(
        events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::CommandExecuted {
                    command: event_command,
                    arguments: event_arguments,
                    ..
                } if event_command == command && event_arguments == arguments
            )
        }),
        "missing correlated CommandExecuted for {command} {arguments:?}: {events:?}"
    );
}

/// Count terminal Tool events for one canonical Tool name.
fn terminal_tool_count(events: &[Envelope], name: &str) -> usize {
    events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolResult { .. } | Event::ToolError { .. }
            )
        })
        .filter(|envelope| {
            events.iter().any(|candidate| {
                matches!(
                    (&candidate.event, &envelope.event),
                    (
                        Event::ToolCallRequested { call: requested, name: requested_name, .. },
                        Event::ToolResult { call: result_call, .. }
                    ) if requested == result_call && requested_name.as_str() == name
                ) || matches!(
                    (&candidate.event, &envelope.event),
                    (
                        Event::ToolCallRequested { call: requested, name: requested_name, .. },
                        Event::ToolError { call: error_call, .. }
                    ) if requested == error_call && requested_name.as_str() == name
                )
            })
        })
        .count()
}

/// Return the one ToolError for a scripted Tool call.
fn find_tool_error<'a>(events: &'a [Envelope], name: &str) -> &'a Event {
    events
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::ToolError { .. } => {
                let call = match &envelope.event {
                    Event::ToolError { call, .. } => call,
                    _ => unreachable!(),
                };
                let requested = events.iter().find(|candidate| {
                    matches!(
                        &candidate.event,
                        Event::ToolCallRequested {
                            call: requested_call,
                            name: requested_name,
                            ..
                        } if requested_call == call && requested_name.as_str() == name
                    )
                });
                requested.map(|_| &envelope.event)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing ToolError for {name}: {events:?}"))
}

/// Assert that a Tool call has one request and one terminal result.
fn assert_one_tool_terminal(events: &[Envelope], name: &str) {
    let requests = events
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolCallRequested { name: event_name, .. } if event_name.as_str() == name
            )
        })
        .count();
    assert_eq!(
        requests, 1,
        "expected one {name} ToolCallRequested: {events:?}"
    );
    assert_eq!(
        terminal_tool_count(events, name),
        1,
        "expected one terminal {name} Tool event: {events:?}"
    );
}

/// Assert the projected tool card has one completed/error state and carries the
/// supplied marker.  This is the same payload consumed by the TUI card.
fn assert_context_tool_marker(context: &Value, marker: &str, state: &str) {
    let text = context.to_string();
    assert!(text.contains(marker), "context lacks {marker}: {context}");
    assert!(
        text.contains(state),
        "context lacks tool state {state}: {context}"
    );
}

/// Build an environment with the required plugin fixture.
fn plugin_builder(scripts: Vec<hya_e2e::ScriptStep>) -> E2eEnvBuilder {
    E2eEnvBuilder::new()
        .project_file(
            ".hya/plugins/toolbox/plugin.toml",
            PLUGIN_MANIFEST.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/plugins/toolbox/plugin.py",
            PLUGIN_SCRIPT.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/commands/use-plugin.md",
            format!("---\ndescription: use plugin\n---\n{USE_PLUGIN_COMMAND}\n").into_bytes(),
        )
        .scripts(scripts)
}

/// Build an environment with the MCP fixture and the custom command that asks
/// the model to invoke it.  `with_mcp_echo` sets `HYA_DEFER_SIDEPLANES=0` in
/// `BackendProcess` before the first schema request.
fn mcp_builder(scripts: Vec<hya_e2e::ScriptStep>) -> E2eEnvBuilder {
    E2eEnvBuilder::new()
        .with_mcp_echo()
        .project_file(
            ".hya/commands/use-mcp.md",
            format!("---\ndescription: use MCP\n---\n{USE_MCP_COMMAND}\n").into_bytes(),
        )
        .scripts(scripts)
}

/// Build the custom MCP behavior while retaining the existing P06 fixture seam.
fn mcp_scenario_builder(scripts: Vec<hya_e2e::ScriptStep>) -> E2eEnvBuilder {
    mcp_builder(scripts).project_file(
        "fixtures/mcp_echo.py",
        MCP_SCENARIO_SCRIPT.as_bytes().to_vec(),
    )
}

#[tokio::test]
async fn custom_slash_catalog_and_routes_expand_all_supported_sources() {
    let env = E2eEnvBuilder::new()
        .skill_file(SKILL_PATH, skill_markdown("user-playbook", "User playbook", SKILL_BODY))
        .project_file(
            ".hya/command/markdown-root.md",
            b"---\ndescription: markdown root\nagent: reviewer\nmodel: fake/reviewer\nsubtask: true\n---\nMARKDOWN_ROOT $1 $ARGUMENTS\n"
                .to_vec(),
        )
        .project_file(
            ".hya/commands/help.md",
            b"---\ndescription: later project help\n---\nLATER_HELP $ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/user-playbook.md",
            b"---\ndescription: command beats Skill\n---\nCOMMAND_WINS $ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/nested/inspect.md",
            b"NESTED_INSPECT $ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/quotes.md",
            b"---\ndescription: quote handling\n---\nQUOTES=$ARGUMENTS|$1|$2\n".to_vec(),
        )
        .project_file(
            ".hya/commands/unclosed.md",
            b"---\ndescription: unclosed quote\n---\nUNCLOSED=$1|$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/empty.md",
            b"EMPTY=$1|$2|$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/positions.md",
            b"POSITION=$1|$10|$2|$11|$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".hya/commands/multiline.md",
            b"MULTILINE-BEGIN\n$ARGUMENTS\nMULTILINE-END\n".to_vec(),
        )
        .project_file(
            ".hya/commands/plain-fence.md",
            b"---\nthis is not closed frontmatter\n".to_vec(),
        )
        .project_file(
            ".hya/commands/bad.md",
            b"---\ndescription: [broken\n---\nOMITTED\n".to_vec(),
        )
        .project_file(
            ".hya/commands/ignored.txt",
            b"not a Markdown command".to_vec(),
        )
        .scripts((0..32).map(|n| text_step(format!("ROUTE_{n}"))).collect())
        .build()
        .await
        .expect("e2e env");

    // Legacy external command roots are intentionally unsupported. The process
    // HOME is private, so this assertion cannot accidentally inspect a user's
    // real command files.
    write_command(
        &private_home(&env),
        ".config/opencode/command/global.md",
        "GLOBAL_IGNORED",
    );
    write_command(
        &private_home(&env),
        ".config/opencode/commands/global-plural.md",
        "GLOBAL_PLURAL_IGNORED",
    );
    write_command(
        &env.project_path("."),
        ".opencode/commands/legacy.md",
        "LEGACY_IGNORED",
    );
    std::fs::write(env.project_path("opencode.json"), "{}").expect("legacy config");

    let catalog = command_catalog(&env).await;
    let names = unique_names(&catalog);
    for name in [
        "markdown-root",
        "nested/inspect",
        "quotes",
        "unclosed",
        "empty",
        "positions",
        "multiline",
        "plain-fence",
        "user-playbook",
    ] {
        assert!(
            names.iter().any(|candidate| candidate == name),
            "missing {name}: {catalog}"
        );
    }
    for ignored in ["bad", "ignored.txt", "global", "global-plural", "legacy"] {
        assert!(
            !names.iter().any(|candidate| candidate == ignored),
            "ignored {ignored} leaked: {catalog}"
        );
    }

    let markdown_root = catalog_entry(&catalog, "markdown-root");
    assert_eq!(markdown_root["source"], "command");
    assert_eq!(markdown_root["template"], "MARKDOWN_ROOT $1 $ARGUMENTS");
    assert_eq!(markdown_root["hints"], json!(["$1", "$ARGUMENTS"]));
    assert_eq!(markdown_root["agent"], "reviewer");
    assert_eq!(markdown_root["model"], "fake/reviewer");
    assert_eq!(markdown_root["subtask"], true);
    assert_eq!(
        catalog_entry(&catalog, "nested/inspect")["template"],
        "NESTED_INSPECT $ARGUMENTS"
    );
    assert_eq!(
        catalog_entry(&catalog, "help")["description"],
        "later project help"
    );
    assert_eq!(
        catalog_entry(&catalog, "help")["template"],
        "LATER_HELP $ARGUMENTS"
    );
    // A command overrides a skill of the same name.
    assert_eq!(
        catalog_entry(&catalog, "user-playbook")["template"],
        "COMMAND_WINS $ARGUMENTS"
    );
    assert_eq!(
        catalog_entry(&catalog, "user-playbook")["source"],
        "command"
    );
    assert_eq!(
        catalog_entry(&catalog, "plain-fence")["template"],
        "---\nthis is not closed frontmatter"
    );
    assert_eq!(
        catalog_entry(&catalog, "quotes")["hints"],
        json!(["$1", "$2", "$ARGUMENTS"])
    );
    assert_eq!(
        catalog_entry(&catalog, "positions")["hints"],
        json!(["$1", "$10", "$11", "$2", "$ARGUMENTS"])
    );

    // Legacy and V2 command routes expand server-side and persist the
    // correlated CommandExecuted event.  The native route intentionally keeps
    // the literal slash because it has no catalog expansion seam.
    let legacy_session = env.create_session().await.expect("legacy session");
    let legacy = command_turn(
        &env,
        legacy_session,
        command_request("markdown-root", "alpha beta", None),
    )
    .await;
    assert_eq!(response_text(&legacy), "MARKDOWN_ROOT alpha alpha beta");
    assert_command_event(
        &env.events(legacy_session, None)
            .await
            .expect("legacy events"),
        "markdown-root",
        "alpha beta",
    );

    let v2_session = env.compat_create_session().await.expect("v2 session");
    let v2 = command_turn(
        &env,
        v2_session,
        command_request("nested/inspect", "inspect-target", None),
    )
    .await;
    assert_eq!(response_text(&v2), "NESTED_INSPECT inspect-target");
    assert_command_event(
        &env.events(v2_session, None).await.expect("v2 events"),
        "nested/inspect",
        "inspect-target",
    );

    // v1 unifies the surfaces: every command turn expands through the
    // same catalog seam (the historical native-literal behavior is gone).

    for (command, arguments, expected) in [
        (
            "quotes",
            "\"hello world\" tail",
            "QUOTES=\"hello world\" tail|hello world|tail",
        ),
        (
            "unclosed",
            "\"open value",
            "UNCLOSED=open value|\"open value",
        ),
        ("empty", "", "EMPTY=||"),
        (
            "positions",
            "a b c d e f g h i j literal-$1",
            "POSITION=a|j|b|literal-$1|a b c d e f g h i j literal-$1",
        ),
        (
            "multiline",
            "first line\nsecond line",
            "MULTILINE-BEGIN\nfirst line\nsecond line\nMULTILINE-END",
        ),
    ] {
        let session = env.create_session().await.expect("expansion session");
        let response = command_turn(&env, session, command_request(command, arguments, None)).await;
        assert_eq!(response_text(&response), expected, "command={command}");
    }

    // Explicit text bypasses expansion.
    let explicit_session = env.create_session().await.expect("explicit session");
    let explicit = command_turn(
        &env,
        explicit_session,
        command_request("positions", "one two", Some("EXPLICIT_TEXT")),
    )
    .await;
    assert_eq!(response_text(&explicit), "EXPLICIT_TEXT");
}

#[tokio::test]
async fn skill_backed_slash_expands_without_skill_tool_call() {
    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("strict")
        .skill_file(
            SKILL_PATH,
            skill_markdown("user-playbook", "User playbook", SKILL_BODY),
        )
        .scripts(vec![
            text_step("DIRECT_SKILL_DONE"),
            text_step("SKILLS_PICKER_DONE"),
            text_step("STALE_SKILL_DONE"),
            text_step("NEW_SKILL_LITERAL_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    // Populate every supported root, including the private HOME roots.  Each
    // root has one unique name; all roots also carry a duplicate to prove the
    // exact first-name-wins order.
    let home = private_home(&env);
    let roots = [
        env.backend.project.join(".hya/skills"),
        home.join(".config/hya/skills"),
        home.join(".claude/skills"),
        env.backend.project.join(".agents/skills"),
        home.join(".codex/skills"),
        home.join(".agents/skills"),
    ];
    for (index, root) in roots.iter().enumerate() {
        write_skill(
            root,
            &format!("root-only-{index}"),
            &format!("root-only-{index}"),
            "root coverage",
            &format!("ROOT_{index}"),
        );
        if index > 0 {
            write_skill(
                root,
                "duplicate-playbook",
                "duplicate-playbook",
                "duplicate",
                &format!("DUPLICATE_{index}"),
            );
        }
    }
    write_skill(
        &roots[0],
        "duplicate-playbook",
        "duplicate-playbook",
        "duplicate",
        "DUPLICATE_0",
    );
    write_skill(&roots[2], "invalid", "", "", "invalid frontmatter");
    std::fs::write(
        roots[2].join("invalid/SKILL.md"),
        "---\nname: invalid\nmissing-description: true\n---\ninvalid\n",
    )
    .expect("invalid skill");

    let skills = skill_catalog(&env).await;
    let skill_names = unique_names(&skills);
    for index in 0..roots.len() {
        assert!(
            skill_names
                .iter()
                .any(|name| name == &format!("root-only-{index}"))
        );
    }
    assert!(
        !skill_names.iter().any(|name| name == "invalid"),
        "invalid Skill frontmatter must be omitted: {skills}"
    );
    let duplicate = catalog_entry(&skills, "duplicate-playbook");
    assert_eq!(duplicate["content"], "DUPLICATE_0");
    for builtin in ["agent-bundle-authoring", "secure-self-update"] {
        let entry = catalog_entry(&skills, builtin);
        assert_eq!(entry["location"], "<built-in>");
    }

    // The direct Skill-backed command is expanded before admission.  Strict
    // permissions would reject Action::Skill, but no Skill Tool call or
    // permission request is involved in this path.
    let direct_session = env.create_session().await.expect("direct skill session");
    let direct = command_turn(
        &env,
        direct_session,
        command_request("user-playbook", "DIRECT_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&direct),
        "SKILL_BODY_USER_PLAYBOOK DIRECT_NONCE\n"
    );
    let direct_events = env
        .events(direct_session, None)
        .await
        .expect("direct events");
    assert!(
        !direct_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolCallRequested { name, .. } if name.as_str() == "skill"
            )
        }),
        "direct Skill command must not call builtin skill: {direct_events:?}"
    );
    assert!(
        env.list_permissions()
            .await
            .expect("permission list")
            .as_array()
            .is_some_and(|permissions| permissions.is_empty()),
        "Action::Skill deny must not block direct expansion"
    );

    // `/skills` selection uses the same command transport, while the catalog
    // remains the selection oracle.
    let picker_skill = catalog_entry(&skills, "user-playbook");
    assert_eq!(picker_skill["content"], SKILL_BODY);
    let picker_session = env.create_session().await.expect("skills picker session");
    let picker = command_turn(
        &env,
        picker_session,
        command_request("user-playbook", "PICKER_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&picker),
        "SKILL_BODY_USER_PLAYBOOK PICKER_NONCE\n"
    );

    // A removed Skill leaves a stale TUI name on command transport, but the
    // backend catalog correctly falls back to literal slash text.
    std::fs::remove_file(env.project_path(SKILL_PATH)).expect("remove Skill");
    let stale_session = env.create_session().await.expect("stale Skill session");
    let stale = command_turn(
        &env,
        stale_session,
        command_request("user-playbook", "STALE_NONCE", None),
    )
    .await;
    assert_eq!(response_text(&stale), "/user-playbook STALE_NONCE");
    assert!(
        !command_catalog(&env)
            .await
            .to_string()
            .contains("user-playbook")
    );

    // A new Skill is not present in the bootstrap snapshot captured above.  A
    // typed command using that stale snapshot is therefore admitted literally;
    // a TUI restart is the documented refresh boundary for slash names.
    let bootstrap_catalog = command_catalog(&env).await;
    write_skill(
        &env.backend.project,
        ".hya/skills/new-after-bootstrap",
        "new-after-bootstrap",
        "new Skill",
        "NEW_SKILL_BODY",
    );
    assert!(
        !array_data(&bootstrap_catalog)
            .iter()
            .any(|entry| entry["name"] == "new-after-bootstrap")
    );
    let new_session = env.create_session().await.expect("new Skill session");
    let new_command = command_turn(
        &env,
        new_session,
        command_request("new-after-bootstrap", "ARG", None),
    )
    .await;
    let new_context = env
        .session_context(&new_session)
        .await
        .expect("new Skill context");
    assert!(
        new_command
            .pointer("/parts/0/text")
            .and_then(Value::as_str)
            .is_some(),
        "v1 command turn response: {new_command}"
    );
    // v1 unifies on the expansion seam: the freshly written skill is
    // expanded at command time even though the bootstrap catalog snapshot
    // (asserted above) stays stale until the next bootstrap.
    assert!(new_context.to_string().contains("NEW_SKILL_BODY"));
}

#[tokio::test]
async fn custom_command_invokes_builtin_skill_tool() {
    let env = E2eEnvBuilder::new()
        .skill_file(
            SKILL_PATH,
            skill_markdown("user-playbook", "User playbook", SKILL_BODY),
        )
        .project_file(
            ".hya/commands/use-skill.md",
            format!("---\ndescription: use builtin Skill\n---\n{USE_SKILL_COMMAND}\n").into_bytes(),
        )
        .scripts(vec![
            tool_step("skill", json!({"name": "user-playbook"})),
            text_step("SKILL_TOOL_FINAL"),
            tool_step("skill", json!({"name": "does-not-exist"})),
            text_step("UNKNOWN_SKILL_RECOVERED"),
            tool_step("skill", json!({})),
            text_step("MISSING_NAME_RECOVERED"),
            tool_step("skill", json!({"name": "user-playbook"})),
            text_step("UNAVAILABLE_SKILL_RECOVERED"),
            tool_step("skill", json!({"name": "user-playbook"})),
            text_step("VALID_AFTER_ERRORS"),
            text_step("STALE_COMMAND_LITERAL"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let success = command_turn(
        &env,
        session,
        command_request("use-skill", "SKILL_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&success),
        "Call builtin skill with name=\"user-playbook\", then return SKILL_BODY_USER_PLAYBOOK and the nonce SKILL_NONCE."
    );
    let events = env.events(session, None).await.expect("success events");
    assert_one_tool_terminal(&events, "skill");
    assert!(events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output.to_string().contains("SKILL_BODY_USER_PLAYBOOK")
        )
    }));
    assert!(
        fake_requests_from(&env.fake_requests().expect("requests"), 1)
            .contains("SKILL_BODY_USER_PLAYBOOK")
    );

    // A separate non-yolo script rejects Action::Skill, then explicitly allows
    // the next valid call in the same Session.
    let denied_env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("strict")
        .skill_file(
            SKILL_PATH,
            skill_markdown("user-playbook", "User playbook", SKILL_BODY),
        )
        .project_file(
            ".hya/commands/use-skill.md",
            format!("---\ndescription: use builtin Skill\n---\n{USE_SKILL_COMMAND}\n").into_bytes(),
        )
        .scripts(vec![
            tool_step("skill", json!({"name": "user-playbook"})),
            text_step("SKILL_DENIED_RECOVERED"),
            tool_step("skill", json!({"name": "user-playbook"})),
            text_step("SKILL_ALLOWED_AFTER_DENIAL"),
        ])
        .build()
        .await
        .expect("denied Skill env");
    let denied_session = denied_env
        .create_session()
        .await
        .expect("denied Skill session");
    denied_env
        .prompt_with_permission_reply(denied_session, "/use-skill DENIED", "reject", TIMEOUT)
        .await
        .expect("denied Skill turn");
    let denied_events = denied_env
        .events(denied_session, None)
        .await
        .expect("denied Skill events");
    assert!(
        denied_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolError { value: Some(value), .. } if value["error"]["type"] == "permission"
            )
        }),
        "expected structured Skill permission error: {denied_events:#?}"
    );
    denied_env
        .prompt_with_permission_reply(denied_session, "/use-skill ALLOWED", "once", TIMEOUT)
        .await
        .expect("allowed Skill recovery");
    let denied_recovery = denied_env
        .events(denied_session, None)
        .await
        .expect("Skill recovery events");
    assert!(denied_recovery.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output.to_string().contains("SKILL_BODY_USER_PLAYBOOK")
        )
    }));

    let unknown = command_turn(
        &env,
        session,
        command_request("use-skill", "UNKNOWN_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&unknown),
        "Call builtin skill with name=\"user-playbook\", then return SKILL_BODY_USER_PLAYBOOK and the nonce UNKNOWN_NONCE."
    );
    let unknown_events = env.events(session, None).await.expect("unknown events");
    let unknown_error = find_tool_error(&unknown_events, "skill");
    assert!(format!("{unknown_error:?}").contains("value"));
    assert!(
        unknown_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolError { value: Some(value), .. } if value["error"]["type"] == "unknown"
            )
        }),
        "unknown Skill must be structured: {unknown_events:?}"
    );

    let missing = command_turn(
        &env,
        session,
        command_request("use-skill", "MISSING_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&missing),
        "Call builtin skill with name=\"user-playbook\", then return SKILL_BODY_USER_PLAYBOOK and the nonce MISSING_NONCE."
    );
    let missing_events = env.events(session, None).await.expect("missing events");
    assert!(
        missing_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolError { value: Some(value), .. } if value["error"]["type"] == "input"
            )
        }),
        "missing Skill name must be input error: {missing_events:?}"
    );

    // Removing the Skill makes a previously valid Tool name unavailable.  The
    // command transport itself remains usable and the same Session recovers.
    std::fs::remove_file(env.project_path(SKILL_PATH)).expect("remove Skill");
    let unavailable = command_turn(
        &env,
        session,
        command_request("use-skill", "UNAVAILABLE_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&unavailable),
        "Call builtin skill with name=\"user-playbook\", then return SKILL_BODY_USER_PLAYBOOK and the nonce UNAVAILABLE_NONCE."
    );
    let unavailable_events = env.events(session, None).await.expect("unavailable events");
    assert!(
        unavailable_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolError { message_text, .. } if message_text.contains("skill")
            )
        }),
        "missing Skill resource must produce a structured unavailable error"
    );

    // Restore the resource and prove a later valid command succeeds in the same
    // Session after all three independent negative scripts.
    std::fs::write(
        env.project_path(SKILL_PATH),
        skill_markdown("user-playbook", "User playbook", SKILL_BODY),
    )
    .expect("restore Skill");
    let recovered = command_turn(
        &env,
        session,
        command_request("use-skill", "RECOVERED_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&recovered),
        "Call builtin skill with name=\"user-playbook\", then return SKILL_BODY_USER_PLAYBOOK and the nonce RECOVERED_NONCE."
    );
    let recovered_events = env.events(session, None).await.expect("recovered events");
    assert!(
        recovered_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolResult { output, .. } if output.to_string().contains("SKILL_BODY_USER_PLAYBOOK")
            )
        }),
        "valid Skill call must recover the same Session"
    );

    // A stale command name is not a catalog error.  Removing the command file
    // causes command transport to store literal slash text.
    std::fs::remove_file(env.project_path(".hya/commands/use-skill.md")).expect("remove command");
    let stale = command_turn(
        &env,
        session,
        command_request("use-skill", "STALE_COMMAND", None),
    )
    .await;
    assert_eq!(response_text(&stale), "/use-skill STALE_COMMAND");
    env.wait_session_idle(&session, TIMEOUT)
        .await
        .expect("session idle");
}

#[tokio::test]
async fn custom_command_invokes_plugin_tool() {
    let env = plugin_builder(vec![
        tool_step("toolbox__remember", json!({"value": "PLUGIN_NONCE"})),
        text_step("PLUGIN_FINAL"),
        tool_step("toolbox__remember", json!({"value": 42})),
        text_step("MALFORMED_INPUT_RECOVERED"),
        tool_step("toolbox__remember", json!({"value": "KILL"})),
        text_step("PLUGIN_DEATH_RECOVERED"),
        tool_step("toolbox__remember", json!({"value": "RESPAWN"})),
        text_step("PLUGIN_RESPAWNED"),
        tool_step("toolbox__remember", json!({"value": "KILL"})),
        text_step("PLUGIN_DRIFT_KILL_RECOVERED"),
        tool_step("toolbox__remember", json!({"value": "DRIFT"})),
        text_step("PLUGIN_DRIFT_ERROR"),
        text_step("SESSION_AFTER_DRIFT"),
        text_step("PLUGIN_RESPAWNED_AFTER_EDIT"),
    ])
    .yolo(true)
    .build()
    .await
    .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let success = command_turn(
        &env,
        session,
        command_request("use-plugin", "PLUGIN_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&success),
        "Call plugin Tool toolbox__remember with value=PLUGIN_NONCE, then return the plugin result."
    );
    let success_events = env.events(session, None).await.expect("plugin events");
    assert_one_tool_terminal(&success_events, "toolbox__remember");
    let plugin_output = success_events
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } if output["plugin"] == "toolbox" => Some(output),
            _ => None,
        });
    assert_eq!(
        plugin_output.expect("plugin output")["value"],
        "PLUGIN_NONCE"
    );
    let requests = env.fake_requests().expect("plugin requests");
    assert!(
        tool_names(&requests[0])
            .iter()
            .any(|name| name == "toolbox__remember")
    );
    assert!(fake_requests_from(&requests, 1).contains("PLUGIN_NONCE"));
    let context = env.session_context(&session).await.expect("plugin context");
    assert_context_tool_marker(&context, "PLUGIN_NONCE", "TOOL_EXECUTION_STATE_OK");
    // Action::Write rejection is independent from plugin Tool authorization.
    // Use a separate non-yolo process so the first scripted Tool is guaranteed
    // to traverse the permission plane and the denied file remains absent.
    let write_env = plugin_builder(vec![
        tool_step(
            "write",
            json!({"path": "denied.txt", "content": "must-not-write"}),
        ),
        text_step("WRITE_REJECTED"),
    ])
    .yolo(false)
    .permission_model("default")
    .build()
    .await
    .expect("write denial env");
    let write_session = write_env
        .create_session()
        .await
        .expect("write denial session");
    write_env
        .prompt_with_permission_reply(write_session, "ask model to write", "reject", TIMEOUT)
        .await
        .expect("write denial turn");
    assert!(
        !write_env.project_path("denied.txt").exists(),
        "Action::Write rejection leaked bytes"
    );
    let write_events = write_env
        .events(write_session, None)
        .await
        .expect("write denial events");
    assert!(write_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolError { value: Some(value), .. } if value["error"]["type"] == "permission"
        )
    }));

    let malformed = command_turn(
        &env,
        session,
        command_request("use-plugin", "MALFORMED_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&malformed),
        "Call plugin Tool toolbox__remember with value=MALFORMED_NONCE, then return the plugin result."
    );
    let malformed_events = env
        .events(session, None)
        .await
        .expect("malformed plugin events");
    assert!(malformed_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolError { message_text, value: Some(value), .. }
                if message_text.contains("invalid_params") && value["error"]["type"] == "unknown"
        )
    }));

    let killed = command_turn(&env, session, command_request("use-plugin", "KILL", None)).await;
    assert_eq!(
        response_text(&killed),
        "Call plugin Tool toolbox__remember with value=KILL, then return the plugin result."
    );
    let after_kill = command_turn(
        &env,
        session,
        command_request("use-plugin", "RESPAWN", None),
    )
    .await;
    assert_eq!(
        response_text(&after_kill),
        "Call plugin Tool toolbox__remember with value=RESPAWN, then return the plugin result."
    );
    let respawn_events = env.events(session, None).await.expect("respawn events");
    assert!(
        respawn_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolResult { output, .. } if output["value"] == "RESPAWN"
            )
        }),
        "same declaration must lazily respawn: {respawn_events:?}"
    );

    // Once the child dies, a changed initialize declaration fails closed on the
    // next lazy respawn.  No partial result is published.
    std::fs::write(
        env.project_path(".hya/plugins/toolbox/plugin.py"),
        PLUGIN_SCRIPT_DRIFT,
    )
    .expect("drift plugin script");
    let drift_kill = command_turn(&env, session, command_request("use-plugin", "KILL", None)).await;
    assert_eq!(
        response_text(&drift_kill),
        "Call plugin Tool toolbox__remember with value=KILL, then return the plugin result."
    );
    let drift_error =
        command_turn(&env, session, command_request("use-plugin", "DRIFT", None)).await;
    assert_eq!(
        response_text(&drift_error),
        "Call plugin Tool toolbox__remember with value=DRIFT, then return the plugin result."
    );
    let drift_events = env.events(session, None).await.expect("drift events");
    assert!(
        drift_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolError { message_text, .. } if message_text.contains("declaration")
            )
        }),
        "declaration drift must fail closed: {drift_events:?}"
    );
    let after_drift = env
        .prompt(session, "ordinary prompt after plugin drift")
        .await;
    assert!(
        after_drift.is_ok(),
        "Session must remain usable after drift"
    );

    // The session's workdir is the fixture directory, so the session belongs
    // to a Project rooted there and the plugin runs as that Project's plugin
    // (spawned in the root, so the relative command resolves).  Editing
    // plugin.toml respawns the Project's plugins at its next bind: the next
    // turn of the same running backend already sees the new declaration.
    std::fs::write(
        env.project_path(".hya/plugins/toolbox/plugin-v2.py"),
        PLUGIN_SCRIPT_V2,
    )
    .expect("write v2 plugin script");
    std::fs::write(
        env.project_path(".hya/plugins/toolbox/plugin.toml"),
        "id = \"toolbox\"\nkind = \"rust\"\ncommand = [\"python3\", \".hya/plugins/toolbox/plugin-v2.py\"]\ntimeout_ms = 1000\n",
    )
    .expect("edit plugin manifest");
    let before_edit = env.fake_requests().expect("requests before edit").len();
    env.prompt(session, "inspect respawned plugin")
        .await
        .expect("prompt after manifest edit");
    let respawned = env.fake_requests().expect("requests after edit");
    let schema = respawned[before_edit].to_string();
    assert!(
        schema.contains("Remember v2"),
        "manifest edit visible at the next bind: {schema}"
    );
    assert!(
        !schema.contains("Remember a fact"),
        "the old declaration is gone after the respawn: {schema}"
    );
}

#[tokio::test]
async fn custom_command_invokes_mcp_tool() {
    // Success and one permission decision use the P06 fixture path.  The
    // builder sets HYA_DEFER_SIDEPLANES=0 before the first model schema request.
    let env = mcp_builder(vec![
        tool_step("mcp__echo__ping", json!({"msg": "MCP_NONCE"})),
        text_step("MCP_FINAL"),
    ])
    .build()
    .await
    .expect("mcp env");
    env.wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("MCP connected");
    let session = env.create_session().await.expect("mcp session");
    let command = command_turn(&env, session, command_request("use-mcp", "MCP_NONCE", None)).await;
    assert_eq!(
        response_text(&command),
        "Call mcp__echo__ping with msg=MCP_NONCE, then return echo:MCP_NONCE."
    );
    let events = env.events(session, None).await.expect("MCP events");
    assert_one_tool_terminal(&events, "mcp__echo__ping");
    assert!(events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output.to_string().contains("echo:MCP_NONCE")
        )
    }));
    assert!(
        fake_requests_from(&env.fake_requests().expect("MCP requests"), 1)
            .contains("echo:MCP_NONCE")
    );
    let context = env.session_context(&session).await.expect("MCP context");
    assert_context_tool_marker(&context, "echo:MCP_NONCE", "TOOL_EXECUTION_STATE_OK");

    // A separate non-yolo process proves that the MCP permission is asked once
    // and that the explicit allow is consumed before the terminal Tool event.
    let permission_env = mcp_builder(vec![
        tool_step("mcp__echo__ping", json!({"msg": "MCP_PERMISSION"})),
        text_step("MCP_PERMISSION_FINAL"),
    ])
    .yolo(false)
    .permission_model("default")
    .build()
    .await
    .expect("MCP permission env");
    permission_env
        .wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("MCP permission startup");
    let permission_session = permission_env
        .create_session()
        .await
        .expect("MCP permission session");
    permission_env
        .prompt_with_permission_reply(
            permission_session,
            "/use-mcp MCP_PERMISSION",
            "once",
            TIMEOUT,
        )
        .await
        .expect("MCP permission once");
    let permission_events = permission_env
        .events(permission_session, None)
        .await
        .expect("MCP permission events");
    assert_one_tool_terminal(&permission_events, "mcp__echo__ping");
    assert_eq!(
        array_data(
            &permission_env
                .list_permissions()
                .await
                .expect("permission list")
        )
        .len(),
        0
    );

    // Disconnected and unknown servers are separate observable control errors.
    let disconnected_env = mcp_scenario_builder(vec![
        tool_step("mcp__echo__ping", json!({"msg": "DISCONNECTED"})),
        text_step("DISCONNECTED_RECOVERED"),
        tool_step("mcp__echo__ping", json!({"msg": "RECONNECTED"})),
        text_step("RECONNECTED_FINAL"),
    ])
    .build()
    .await
    .expect("disconnected MCP env");
    disconnected_env
        .wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("disconnected MCP startup");
    post_ok(&disconnected_env, "/v1/mcp/echo/disconnect", Value::Null).await;
    let status = disconnected_env
        .get_json("/v1/mcp")
        .await
        .expect("disabled status");
    assert_eq!(
        status["servers"][0]["state"],
        "MCP_SERVER_STATE_DISCONNECTED"
    );
    let disconnected_session = disconnected_env
        .create_session()
        .await
        .expect("disconnected session");
    disconnected_env
        .prompt(disconnected_session, "call while disconnected")
        .await
        .expect("disconnected turn");
    let disconnected_events = disconnected_env
        .events(disconnected_session, None)
        .await
        .expect("disconnected events");
    assert!(disconnected_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolError { message_text, .. } if message_text.contains("unknown tool")
        )
    }));
    assert_eq!(
        disconnected_env
            .get_json("/v1/mcp")
            .await
            .expect("no auto respawn")["servers"][0]["state"],
        "MCP_SERVER_STATE_DISCONNECTED"
    );
    let (missing_status, missing_body) = request_json(
        &disconnected_env,
        Method::POST,
        "/v1/mcp/unknown/connect",
        None,
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert!(missing_body.to_string().contains("unknown mcp server"));
    post_ok(&disconnected_env, "/v1/mcp/echo/connect", Value::Null).await;
    disconnected_env
        .wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("MCP reconnect");
    disconnected_env
        .prompt(disconnected_session, "call after explicit reconnect")
        .await
        .expect("reconnected turn");
    let reconnect_events = disconnected_env
        .events(disconnected_session, None)
        .await
        .expect("reconnect events");
    assert!(reconnect_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output.to_string().contains("echo:RECONNECTED")
        )
    }));

    // `isError` is a structured MCP result error, not a transport failure.
    let error_env = mcp_scenario_builder(vec![
        tool_step("mcp__echo__ping", json!({"msg": "ERROR"})),
        text_step("MCP_ERROR_RECOVERED"),
    ])
    .build()
    .await
    .expect("MCP isError env");
    let error_session = error_env.create_session().await.expect("isError session");
    error_env
        .prompt(error_session, "MCP error")
        .await
        .expect("isError turn");
    let error_events = error_env
        .events(error_session, None)
        .await
        .expect("isError events");
    assert!(error_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolError { message_text, value: Some(value), .. }
                if message_text.contains("MCP_ERROR_MARKER") && value["error"]["type"] == "unknown"
        )
    }));

    // Malformed result and malformed frame each get their own process because
    // the MCP reader closes its pending map on the first framing error.
    for (marker, expected) in [
        ("MALFORMED", "content"),
        ("FRAME", "json"),
        ("OVERSIZED", "1048576"),
    ] {
        let case_env = mcp_scenario_builder(vec![
            tool_step("mcp__echo__ping", json!({"msg": marker})),
            text_step("MCP_CASE_RECOVERED"),
        ])
        .build()
        .await
        .expect("MCP malformed env");
        let case_session = case_env
            .create_session()
            .await
            .expect("MCP malformed session");
        case_env
            .prompt(case_session, format!("MCP {marker}"))
            .await
            .expect("MCP malformed turn");
        let case_events = case_env
            .events(case_session, None)
            .await
            .expect("MCP malformed events");
        assert!(case_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolError { message_text, .. } if message_text.to_lowercase().contains(expected)
            )
        }), "{marker} must preserve structured error: {case_events:?}");
    }

    // A short dynamic server timeout keeps the timeout case bounded while still
    // using the same project-local MCP fixture.
    let timeout_env = mcp_scenario_builder(vec![
        tool_step("mcp__fast__ping", json!({"msg": "TIMEOUT"})),
        text_step("MCP_TIMEOUT_RECOVERED"),
    ])
    .build()
    .await
    .expect("MCP timeout env");
    let (add_status, add_body) = request_json(
        &timeout_env,
        Method::POST,
        "/v1/mcp",
        Some(json!({
            "name": "fast",
            "command": {"command": "python3", "args": ["fixtures/mcp_echo.py"]}
        })),
    )
    .await;
    assert_eq!(
        add_status,
        StatusCode::OK,
        "dynamic timeout MCP add: {add_body}"
    );
    let timeout_session = timeout_env.create_session().await.expect("timeout session");
    timeout_env
        .prompt(timeout_session, "MCP timeout")
        .await
        .expect("timeout turn");
    let timeout_events = timeout_env
        .events(timeout_session, None)
        .await
        .expect("timeout events");
    assert!(timeout_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolError { message_text, .. } if message_text.contains("timed out")
        )
    }));

    // Post-publication process death is closed and not auto-respawned.  Only an
    // explicit disconnect/connect publishes a fresh MCP generation.
    let death_env = mcp_scenario_builder(vec![
        tool_step("mcp__echo__ping", json!({"msg": "DEATH"})),
        text_step("MCP_DEATH_RECOVERED"),
        tool_step("mcp__echo__ping", json!({"msg": "DEATH_AGAIN"})),
        text_step("MCP_CLOSED_AGAIN"),
        tool_step("mcp__echo__ping", json!({"msg": "AFTER_RECONNECT"})),
        text_step("MCP_RECONNECTED_FINAL"),
    ])
    .build()
    .await
    .expect("MCP death env");
    let death_session = death_env.create_session().await.expect("death session");
    death_env
        .prompt(death_session, "MCP death")
        .await
        .expect("death turn");
    death_env
        .prompt(death_session, "MCP second closed call")
        .await
        .expect("closed turn");
    let death_events = death_env
        .events(death_session, None)
        .await
        .expect("death events");
    let closed_error_count = death_events
        .iter()
        .filter(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
        .count();
    assert_eq!(
        closed_error_count, 2,
        "each call through the dead transport must fail without auto-respawn: {death_events:?}"
    );
    assert!(
        !death_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolResult { output, .. } if output.to_string().contains("DEATH_AGAIN")
            )
        }),
        "the second call must not reach a respawned server: {death_events:?}"
    );
    let old_generation = death_events
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        });
    post_ok(&death_env, "/v1/mcp/echo/disconnect", Value::Null).await;
    post_ok(&death_env, "/v1/mcp/echo/connect", Value::Null).await;
    death_env
        .wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("death explicit reconnect");
    death_env
        .prompt(death_session, "MCP after reconnect")
        .await
        .expect("reconnect root turn");
    let after_reconnect_events = death_env
        .events(death_session, None)
        .await
        .expect("after reconnect events");
    let generations = after_reconnect_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        })
        .collect::<Vec<_>>();
    if let Some(old_generation) = old_generation {
        assert!(
            generations
                .iter()
                .any(|generation| *generation != old_generation)
        );
        assert_eq!(
            generations[0], old_generation,
            "old TurnBinding was rewritten"
        );
    }
    assert!(after_reconnect_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output.to_string().contains("echo:AFTER_RECONNECT")
        )
    }));
}

#[tokio::test]
async fn resource_name_conflicts_fail_closed() {
    // Two plugin declarations export the same Tool.  Runtime publication is
    // rejected as a generation, while the builtin registry remains complete.
    let duplicate = E2eEnvBuilder::new()
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
        .scripts(vec![text_step("DUPLICATE_PLUGIN_REJECTED")])
        .build()
        .await
        .expect("duplicate plugin env");
    let duplicate_session = duplicate.create_session().await.expect("duplicate session");
    duplicate
        .prompt(duplicate_session, "inspect duplicate plugins")
        .await
        .expect("duplicate prompt");
    // With host-composed qualified names, two plugins may declare the same
    // local tool name: they coexist as `toolbox__remember` / `otherbox__remember`.
    let duplicate_names = tool_names(&duplicate.fake_requests().expect("duplicate requests")[0]);
    assert!(
        duplicate_names
            .iter()
            .any(|name| name == "toolbox__remember")
    );
    assert!(
        duplicate_names
            .iter()
            .any(|name| name == "otherbox__remember")
    );
    assert!(
        !duplicate_names.iter().any(|name| name == "remember"),
        "bare contributed names must never reach the model"
    );

    // A plugin-versus-builtin collision rejects only the candidate plugin
    // generation; the builtin `read` remains exactly once.
    let builtin_collision = E2eEnvBuilder::new()
        .project_file(
            ".hya/plugins/toolbox/plugin.toml",
            PLUGIN_MANIFEST.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/plugins/toolbox/plugin.py",
            PLUGIN_SCRIPT_READ.as_bytes().to_vec(),
        )
        .scripts(vec![text_step("BUILTIN_COLLISION_REJECTED")])
        .build()
        .await
        .expect("builtin collision env");
    let builtin_session = builtin_collision
        .create_session()
        .await
        .expect("builtin collision session");
    builtin_collision
        .prompt(builtin_session, "inspect builtin collision")
        .await
        .expect("builtin collision prompt");
    let builtin_names = tool_names(
        &builtin_collision
            .fake_requests()
            .expect("builtin collision requests")[0],
    );
    assert_eq!(
        builtin_names.iter().filter(|name| *name == "read").count(),
        1,
        "the protected built-in `read` stays bare and unique"
    );
    assert!(
        builtin_names.iter().any(|name| *name == "toolbox__read"),
        "a plugin tool named `read` is qualified, not merged with the built-in"
    );

    // Ambiguous MCP compositions fail closed at the door: a server key or
    // tool name containing `__` can no longer publish (the old
    // `mcp__a__b__c` collision from (a__b,c) vs (a,b__c) is unconstructable).
    let mcp = mcp_builder(vec![
        text_step("MCP_NAMESPACE_FIRST"),
        text_step("MCP_NAMESPACE_SECOND"),
    ])
    .project_file(
        "fixtures/mcp_echo.py",
        MCP_COLLISION_SCRIPT.as_bytes().to_vec(),
    )
    .build()
    .await
    .expect("MCP collision env");
    for (name, tool) in [("a__b", "c"), ("a", "b__c")] {
        let (status, body) = request_json(
            &mcp,
            Method::POST,
            "/v1/mcp",
            Some(json!({
                "name": name,
                "command": {"command": "python3", "args": ["fixtures/mcp_echo.py", tool]}
            })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "ambiguous MCP add {name}/{tool} must fail closed: {body}"
        );
        assert!(
            body.to_string().contains("must be"),
            "rejection must name the composition rule: {body}"
        );
    }
    let mcp_session = mcp.create_session().await.expect("MCP collision session");
    mcp.prompt(mcp_session, "inspect MCP namespace collision")
        .await
        .expect("MCP collision prompt");
    let mcp_names = tool_names(&mcp.fake_requests().expect("MCP collision requests")[0]);
    assert_eq!(
        mcp_names
            .iter()
            .filter(|name| *name == "mcp__a__b__c")
            .count(),
        0,
        "no ambiguous name may reach the model: {mcp_names:?}"
    );

    // Command/Skill collisions are metadata precedence, not runtime resource
    // conflicts.
    let command_skill = E2eEnvBuilder::new()
        .skill_file(
            SKILL_PATH,
            skill_markdown("same-name", "Skill", "SKILL_SHOULD_LOSE"),
        )
        .project_file(
            ".hya/commands/same-name.md",
            b"---\ndescription: command wins\n---\nCOMMAND_WINS\n".to_vec(),
        )
        .scripts(vec![text_step("COMMAND_SKILL_COLLISION")])
        .build()
        .await
        .expect("command Skill collision env");
    let collision_catalog = command_catalog(&command_skill).await;
    let same = catalog_entry(&collision_catalog, "same-name");
    assert_eq!(same["source"], "command");
    assert_eq!(same["template"], "COMMAND_WINS");
}

#[tokio::test]
async fn dynamic_resource_snapshots_and_reload() {
    let env = mcp_scenario_builder(vec![
        text_step("SKILL_OLD_TURN"),
        text_step("SKILL_EDITED_TURN"),
        tool_step("mcp__echo__ping", json!({"msg": "MCP_RELOAD"})),
        text_step("MCP_RELOAD_FINAL"),
    ])
    .skill_file(
        SKILL_PATH,
        skill_markdown(
            "user-playbook",
            "User playbook",
            "SKILL_OLD_BODY $ARGUMENTS\n",
        ),
    )
    .project_file(
        ".hya/commands/new-known.md",
        b"---\ndescription: known\n---\nKNOWN_OLD $ARGUMENTS\n".to_vec(),
    )
    .build()
    .await
    .expect("dynamic env");

    let first = env.create_session().await.expect("dynamic session");
    let first_command =
        command_turn(&env, first, command_request("user-playbook", "OLD", None)).await;
    assert_eq!(response_text(&first_command), "SKILL_OLD_BODY OLD\n");
    let before_events = env
        .events(first, None)
        .await
        .expect("before dynamic events");
    let old_generation = before_events
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        })
        .expect("old generation");

    // Skill content is a dynamic source: the next root Turn sees the edit, but
    // the old event and admitted message remain byte-for-byte unchanged.
    std::fs::write(
        env.project_path(SKILL_PATH),
        skill_markdown(
            "user-playbook",
            "User playbook",
            "SKILL_EDITED_BODY $ARGUMENTS\n",
        ),
    )
    .expect("edit Skill");
    let second = command_turn(&env, first, command_request("user-playbook", "NEW", None)).await;
    assert_eq!(response_text(&second), "SKILL_EDITED_BODY NEW\n");
    let after_skill_events = env.events(first, None).await.expect("after Skill events");
    assert!(
        after_skill_events
            .iter()
            .any(|envelope| { matches!(&envelope.event, Event::MessageStarted { .. }) })
    );
    assert!(
        after_skill_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::TurnBindingRecorded { generation, .. } => Some(*generation),
                _ => None,
            })
            .any(|generation| generation != old_generation),
        "edited Skill must publish a new generation"
    );
    let first_context = env.session_context(&first).await.expect("Skill context");
    assert!(first_context.to_string().contains("SKILL_OLD_BODY OLD"));
    assert!(first_context.to_string().contains("SKILL_EDITED_BODY NEW"));

    // MCP disconnect/connect is the explicit dynamic publication boundary.
    post_ok(&env, "/v1/mcp/echo/disconnect", Value::Null).await;
    post_ok(&env, "/v1/mcp/echo/connect", Value::Null).await;
    env.wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("MCP refreshed");
    command_turn(&env, first, command_request("use-mcp", "MCP_RELOAD", None)).await;
    let reload_events = env.events(first, None).await.expect("MCP reload events");
    let reload_generations = reload_events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(*generation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        reload_generations
            .iter()
            .any(|generation| *generation != old_generation)
    );
    assert_eq!(
        reload_generations[0], old_generation,
        "old TurnBinding changed"
    );
    assert!(reload_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output.to_string().contains("echo:MCP_RELOAD")
        )
    }));

    // Two fresh processes with different plugin commands each publish their
    // own declaration (in-process hot respawn on a manifest edit is covered by
    // `custom_command_invokes_plugin_tool`).
    let plugin_old = plugin_builder(vec![text_step("PLUGIN_OLD")])
        .build()
        .await
        .expect("plugin old env");
    let old_session = plugin_old
        .create_session()
        .await
        .expect("plugin old session");
    plugin_old
        .prompt(old_session, "plugin old")
        .await
        .expect("plugin old prompt");
    assert!(
        plugin_old.fake_requests().expect("plugin old requests")[0]
            .to_string()
            .contains("Remember a fact")
    );
    let plugin_new = E2eEnvBuilder::new()
        .project_file(
            ".hya/plugins/toolbox/plugin.toml",
            b"id = \"toolbox\"\nkind = \"rust\"\ncommand = [\"python3\", \".hya/plugins/toolbox/plugin-v2.py\"]\n".to_vec(),
        )
        .project_file(
            ".hya/plugins/toolbox/plugin-v2.py",
            PLUGIN_SCRIPT_V2.as_bytes().to_vec(),
        )
        .scripts(vec![text_step("PLUGIN_NEW")])
        .build()
        .await
        .expect("plugin new env");
    let new_session = plugin_new
        .create_session()
        .await
        .expect("plugin new session");
    plugin_new
        .prompt(new_session, "plugin new")
        .await
        .expect("plugin new prompt");
    assert!(
        plugin_new.fake_requests().expect("plugin new requests")[0]
            .to_string()
            .contains("Remember v2")
    );

    // Existing bootstrap metadata remains unchanged after adding command/Skill
    // files.  This is the explicit sync.data.command cache contract.
    let bootstrap = command_catalog(&env).await;
    write_command(
        &env.backend.project,
        ".hya/commands/added-after-bootstrap.md",
        "ADDED_AFTER_BOOTSTRAP",
    );
    write_skill(
        &env.backend.project,
        ".hya/skills/added-after-bootstrap",
        "added-after-bootstrap",
        "new",
        "ADDED_SKILL_AFTER_BOOTSTRAP",
    );
    assert!(
        !array_data(&bootstrap)
            .iter()
            .any(|entry| entry["name"] == "added-after-bootstrap")
    );
    // A current backend catalog sees them; a TUI that has not restarted keeps
    // the bootstrap names, so this distinction is directly observable.
    let refreshed = command_catalog(&env).await;
    assert!(
        array_data(&refreshed)
            .iter()
            .any(|entry| entry["name"] == "added-after-bootstrap")
    );
}

#[tokio::test]
async fn structured_custom_tool_errors_replay_and_session_recovers() {
    let env = plugin_builder(vec![
        tool_step("toolbox__remember", json!({"value": "ERR_ONCE"})),
        text_step("AFTER_PLUGIN_ERROR"),
        tool_step("toolbox__remember", json!({"value": "VALID_AFTER_ERROR"})),
        text_step("VALID_PLUGIN_FINAL"),
    ])
    .build()
    .await
    .expect("structured error env");
    let session = env
        .create_session()
        .await
        .expect("structured error session");

    command_turn(
        &env,
        session,
        command_request("use-plugin", "ERR_ONCE", None),
    )
    .await;
    env.wait_session_idle(&session, TIMEOUT)
        .await
        .expect("idle after custom error");
    let canonical = env.events(session, None).await.expect("canonical replay");
    assert_one_tool_terminal(&canonical, "toolbox__remember");
    let error = canonical
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::ToolError {
                value: Some(value),
                message_text,
                ..
            } => Some((value.clone(), message_text.clone())),
            _ => None,
        })
        .expect("structured custom ToolError");
    assert_eq!(error.0["error"]["type"], "unknown");
    assert!(
        error.0["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert!(
        error.0["error"]["message"]
            .as_str()
            .expect("error message")
            .chars()
            .count()
            <= 2048
    );

    // Canonical replay and the projected transcript retain the typed tool
    // error code/message, not only a flattened human string.
    let envelopes = env
        .events(session, None)
        .await
        .expect("API canonical replay");
    let replay = serde_json::to_value(&envelopes).unwrap_or_default();
    assert!(replay.to_string().contains("\"type\":\"unknown\""));
    assert!(replay.to_string().contains("ERR_ONCE"));
    let context = env
        .session_context(&session)
        .await
        .expect("TUI context replay");
    assert!(context.to_string().contains("TOOL_EXECUTION_STATE_ERROR"));
    assert!(context.to_string().contains("\"errorCode\":\"unknown\""));
    assert!(context.to_string().contains("ERR_ONCE"));
    assert!(
        context.to_string().len() <= 64 * 1024,
        "TUI error presentation is unbounded"
    );

    // Reading replay must not execute the plugin again.  The call log and
    // FakeLlm request count are independent execution oracles.
    let calls_path = env.project_path(".hya/plugin-calls.log");
    let call_count_before = std::fs::read_to_string(&calls_path)
        .expect("plugin call log")
        .lines()
        .count();
    let fake_count_before = env.fake_requests().expect("fake count").len();
    let _ = env
        .events(session, None)
        .await
        .expect("second canonical replay");
    let _ = env
        .session_context(&session)
        .await
        .expect("second context replay");
    let call_count_after = std::fs::read_to_string(&calls_path)
        .expect("plugin call log after replay")
        .lines()
        .count();
    assert_eq!(
        call_count_after, call_count_before,
        "replay executed custom Tool"
    );
    assert_eq!(
        env.fake_requests().expect("fake count after replay").len(),
        fake_count_before
    );

    // A later valid custom slash command succeeds in the same Session.
    command_turn(
        &env,
        session,
        command_request("use-plugin", "VALID_AFTER_ERROR", None),
    )
    .await;
    let recovered = env
        .events(session, None)
        .await
        .expect("recovered custom events");
    assert!(recovered.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::ToolResult { output, .. } if output["value"] == "VALID_AFTER_ERROR"
        )
    }));
    env.wait_session_idle(&session, TIMEOUT)
        .await
        .expect("idle after recovery");
}
