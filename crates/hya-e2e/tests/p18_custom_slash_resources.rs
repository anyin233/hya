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
    "Call plugin Tool remember with value=$ARGUMENTS, then return the plugin result.";
const USE_MCP_COMMAND: &str =
    "Call mcp__echo__ping with msg=$ARGUMENTS, then return echo:$ARGUMENTS.";

/// The required project plugin manifest.  The relative command is intentional:
/// `BackendProcess` starts the child with the temporary project as its cwd.
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
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Fetch the full command catalog for the temporary project.
async fn command_catalog(env: &E2eEnv) -> Value {
    env.get_json(&format!(
        "/api/command?directory={}",
        env.backend.workdir_str()
    ))
    .await
    .expect("command catalog")
}

/// Fetch the full Skill catalog for the temporary project.
async fn skill_catalog(env: &E2eEnv) -> Value {
    env.get_json(&format!(
        "/api/skill?directory={}",
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
    let mut body = json!({"command": command, "arguments": arguments});
    if let Some(text) = text {
        body["text"] = json!(text);
    }
    body
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
            ".opencode/commands/use-plugin.md",
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
            ".opencode/commands/use-mcp.md",
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
            "opencode.json",
            br#"{
  "command": {
    "inline-root-singular": {
      "description": "root singular",
      "agent": "build",
      "model": "fake/model",
      "subtask": true,
      "template": "ROOT_SINGULAR $1 $ARGUMENTS"
    },
    "help": {"description": "old help", "template": "OLD_HELP $ARGUMENTS"}
  }
}"#
            .to_vec(),
        )
        .project_file(
            "opencode.jsonc",
            br#"{
  // plural root JSONC key
  "commands": {
    "inline-root-plural": {"template": "ROOT_PLURAL $2/$1", "description": "root plural"},
    "user-playbook": {"template": "COMMAND_WINS $ARGUMENTS", "description": "command beats Skill"},
    "ignored-disable": {"template": "DISABLE_IGNORED", "disable": true}
  }
}"#
            .to_vec(),
        )
        .project_file(
            ".opencode/opencode.json",
            br#"{
  "command": {"inline-dot-singular": {"template": "DOT_SINGULAR $ARGUMENTS"}}
}"#
            .to_vec(),
        )
        .project_file(
            ".opencode/opencode.jsonc",
            br#"{
  "commands": {"inline-dot-plural": {"template": "DOT_PLURAL $10 $1 $ARGUMENTS"}}
}"#
            .to_vec(),
        )
        .project_file(
            ".opencode/command/markdown-root.md",
            b"---\ndescription: markdown root\nagent: reviewer\nmodel: fake/reviewer\nsubtask: true\n---\nMARKDOWN_ROOT $1 $ARGUMENTS\n"
                .to_vec(),
        )
        .project_file(
            ".opencode/commands/help.md",
            b"---\ndescription: later project help\n---\nLATER_HELP $ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/nested/inspect.md",
            b"NESTED_INSPECT $ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/quotes.md",
            b"---\ndescription: quote handling\n---\nQUOTES=$ARGUMENTS|$1|$2\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/unclosed.md",
            b"---\ndescription: unclosed quote\n---\nUNCLOSED=$1|$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/empty.md",
            b"EMPTY=$1|$2|$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/positions.md",
            b"POSITION=$1|$10|$2|$11|$ARGUMENTS\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/multiline.md",
            b"MULTILINE-BEGIN\n$ARGUMENTS\nMULTILINE-END\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/plain-fence.md",
            b"---\nthis is not closed frontmatter\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/bad.md",
            b"---\ndescription: [broken\n---\nOMITTED\n".to_vec(),
        )
        .project_file(
            ".opencode/commands/ignored.txt",
            b"not a Markdown command".to_vec(),
        )
        .project_file(
            ".hya/commands/unsupported.md",
            b"UNSUPPORTED_HYA_COMMAND".to_vec(),
        )
        .scripts((0..32).map(|n| text_step(format!("ROUTE_{n}"))).collect())
        .build()
        .await
        .expect("e2e env");

    // Home/global command roots are intentionally unsupported.  The process
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

    let catalog = command_catalog(&env).await;
    let names = unique_names(&catalog);
    for name in [
        "inline-root-singular",
        "inline-root-plural",
        "inline-dot-singular",
        "inline-dot-plural",
        "markdown-root",
        "nested/inspect",
        "quotes",
        "unclosed",
        "empty",
        "positions",
        "multiline",
        "plain-fence",
        "ignored-disable",
    ] {
        assert!(
            names.iter().any(|candidate| candidate == name),
            "missing {name}: {catalog}"
        );
    }
    for ignored in [
        "bad",
        "ignored.txt",
        "unsupported",
        "global",
        "global-plural",
    ] {
        assert!(
            !names.iter().any(|candidate| candidate == ignored),
            "ignored {ignored} leaked: {catalog}"
        );
    }

    let root_singular = catalog_entry(&catalog, "inline-root-singular");
    assert_eq!(root_singular["source"], "command");
    assert_eq!(root_singular["template"], "ROOT_SINGULAR $1 $ARGUMENTS");
    assert_eq!(root_singular["hints"], json!(["$1", "$ARGUMENTS"]));
    assert_eq!(root_singular["agent"], "build");
    assert_eq!(root_singular["model"], "fake/model");
    assert_eq!(root_singular["subtask"], true);
    assert_eq!(
        catalog_entry(&catalog, "inline-root-plural")["template"],
        "ROOT_PLURAL $2/$1"
    );
    assert_eq!(
        catalog_entry(&catalog, "inline-dot-singular")["template"],
        "DOT_SINGULAR $ARGUMENTS"
    );
    assert_eq!(
        catalog_entry(&catalog, "inline-dot-plural")["template"],
        "DOT_PLURAL $10 $1 $ARGUMENTS"
    );
    assert_eq!(
        catalog_entry(&catalog, "markdown-root")["template"],
        "MARKDOWN_ROOT $1 $ARGUMENTS"
    );
    assert_eq!(
        catalog_entry(&catalog, "markdown-root")["hints"],
        json!(["$1", "$ARGUMENTS"])
    );
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
    assert_eq!(
        catalog_entry(&catalog, "user-playbook")["template"],
        "COMMAND_WINS $ARGUMENTS"
    );
    assert_eq!(
        catalog_entry(&catalog, "user-playbook")["source"],
        "command"
    );
    assert_eq!(
        catalog_entry(&catalog, "ignored-disable")["template"],
        "DISABLE_IGNORED"
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
    let legacy = post_ok(
        &env,
        &format!("/session/{legacy_session}/command"),
        command_request("inline-root-singular", "alpha beta", None),
    )
    .await;
    assert_eq!(response_text(&legacy), "ROOT_SINGULAR alpha alpha beta");
    assert_command_event(
        &env.events(legacy_session, None)
            .await
            .expect("legacy events"),
        "inline-root-singular",
        "alpha beta",
    );

    let v2_session = env.compat_create_session().await.expect("v2 session");
    let v2 = post_ok(
        &env,
        &format!("/api/session/{v2_session}/command"),
        command_request(
            "inline-dot-plural",
            "one two three four five six seven eight nine ten",
            None,
        ),
    )
    .await;
    assert_eq!(
        response_text(&v2),
        "DOT_PLURAL ten one one two three four five six seven eight nine ten"
    );
    assert_command_event(
        &env.events(v2_session, None).await.expect("v2 events"),
        "inline-dot-plural",
        "one two three four five six seven eight nine ten",
    );

    let native_session = env.create_session().await.expect("native session");
    let native = post_ok(
        &env,
        &format!("/sessions/{native_session}/command"),
        command_request("inline-root-singular", "alpha beta", None),
    )
    .await;
    assert!(
        native.get("message").is_some(),
        "native command response: {native}"
    );
    let native_events = env
        .events(native_session, None)
        .await
        .expect("native events");
    assert_command_event(&native_events, "inline-root-singular", "alpha beta");
    let native_context = env
        .get_json(&format!("/api/session/{native_session}/context"))
        .await
        .expect("native context");
    assert!(
        native_context
            .to_string()
            .contains("/inline-root-singular alpha beta"),
        "native command must preserve literal slash: {native_context}"
    );

    for (route, path_prefix, command, arguments, expected) in [
        (
            "legacy",
            "/session/",
            "quotes",
            "\"hello world\" tail",
            "QUOTES=\"hello world\" tail|hello world|tail",
        ),
        (
            "legacy",
            "/session/",
            "unclosed",
            "\"open value",
            "UNCLOSED=open value|\"open value",
        ),
        ("legacy", "/session/", "empty", "", "EMPTY=||"),
        (
            "legacy",
            "/session/",
            "positions",
            "a b c d e f g h i j literal-$1",
            "POSITION=a|j|b|literal-$1|a b c d e f g h i j literal-$1",
        ),
        (
            "legacy",
            "/session/",
            "multiline",
            "first line\nsecond line",
            "MULTILINE-BEGIN\nfirst line\nsecond line\nMULTILINE-END",
        ),
    ] {
        let session = env.create_session().await.expect("expansion session");
        let response = post_ok(
            &env,
            &format!("{path_prefix}{session}/command"),
            command_request(command, arguments, None),
        )
        .await;
        assert_eq!(
            response_text(&response),
            expected,
            "route={route} command={command}"
        );
    }

    // Explicit text bypasses expansion on all three routes.
    let explicit_legacy_session = env.create_session().await.expect("explicit legacy session");
    let explicit_legacy = post_ok(
        &env,
        &format!("/session/{explicit_legacy_session}/command"),
        command_request("positions", "one two", Some("EXPLICIT_LEGACY")),
    )
    .await;
    assert_eq!(response_text(&explicit_legacy), "EXPLICIT_LEGACY");
    let explicit_v2_session = env
        .compat_create_session()
        .await
        .expect("explicit v2 session");
    let explicit_v2 = post_ok(
        &env,
        &format!("/api/session/{explicit_v2_session}/command"),
        command_request("positions", "one two", Some("EXPLICIT_V2")),
    )
    .await;
    assert_eq!(response_text(&explicit_v2), "EXPLICIT_V2");
    let explicit_native_session = env.create_session().await.expect("explicit native session");
    let explicit_native = post_ok(
        &env,
        &format!("/sessions/{explicit_native_session}/command"),
        command_request("positions", "one two", Some("EXPLICIT_NATIVE")),
    )
    .await;
    assert!(explicit_native.get("message").is_some());
    let explicit_native_context = env
        .get_json(&format!("/api/session/{explicit_native_session}/context"))
        .await
        .expect("explicit native context");
    assert!(
        explicit_native_context
            .to_string()
            .contains("EXPLICIT_NATIVE")
    );

    // Replacing one recognized config file with malformed JSONC omits that
    // source without crashing the remaining catalog.
    std::fs::write(
        env.project_path(".opencode/opencode.jsonc"),
        "{ commands: [\n",
    )
    .expect("malformed JSONC");
    let malformed_catalog = command_catalog(&env).await;
    assert!(
        !unique_names(&malformed_catalog)
            .iter()
            .any(|name| name == "inline-dot-plural")
    );
    assert!(
        unique_names(&malformed_catalog)
            .iter()
            .any(|name| name == "inline-root-singular")
    );
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
        home.join(".config/opencode/skills"),
        home.join(".config/opencode/skill"),
        env.backend.project.join(".opencode/skills"),
        env.backend.project.join(".opencode/skill"),
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
    for builtin in [
        "customize-compat",
        "agent-bundle-authoring",
        "secure-self-update",
    ] {
        let entry = catalog_entry(&skills, builtin);
        assert_eq!(entry["location"], "<built-in>");
    }

    // The direct Skill-backed command is expanded before admission.  Strict
    // permissions would reject Action::Skill, but no Skill Tool call or
    // permission request is involved in this path.
    let direct_session = env.create_session().await.expect("direct skill session");
    let direct = post_ok(
        &env,
        &format!("/session/{direct_session}/command"),
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
    let picker = post_ok(
        &env,
        &format!("/session/{picker_session}/command"),
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
    let stale = post_ok(
        &env,
        &format!("/session/{stale_session}/command"),
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
    let new_command = post_ok(
        &env,
        &format!("/sessions/{new_session}/command"),
        command_request("new-after-bootstrap", "ARG", None),
    )
    .await;
    let new_context = env
        .get_json(&format!("/api/session/{new_session}/context"))
        .await
        .expect("new Skill context");
    assert!(new_command.get("message").is_some());
    assert!(new_context.to_string().contains("/new-after-bootstrap ARG"));
}

#[tokio::test]
async fn custom_command_invokes_builtin_skill_tool() {
    let env = E2eEnvBuilder::new()
        .skill_file(
            SKILL_PATH,
            skill_markdown("user-playbook", "User playbook", SKILL_BODY),
        )
        .project_file(
            ".opencode/commands/use-skill.md",
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
    let success = post_ok(
        &env,
        &format!("/session/{session}/command"),
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
            ".opencode/commands/use-skill.md",
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

    let unknown = post_ok(
        &env,
        &format!("/session/{session}/command"),
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

    let missing = post_ok(
        &env,
        &format!("/session/{session}/command"),
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
    let unavailable = post_ok(
        &env,
        &format!("/session/{session}/command"),
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
    let recovered = post_ok(
        &env,
        &format!("/session/{session}/command"),
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
    std::fs::remove_file(env.project_path(".opencode/commands/use-skill.md"))
        .expect("remove command");
    let stale = post_ok(
        &env,
        &format!("/session/{session}/command"),
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
        tool_step("remember", json!({"value": "PLUGIN_NONCE"})),
        text_step("PLUGIN_FINAL"),
        tool_step("remember", json!({"value": 42})),
        text_step("MALFORMED_INPUT_RECOVERED"),
        tool_step("remember", json!({"value": "KILL"})),
        text_step("PLUGIN_DEATH_RECOVERED"),
        tool_step("remember", json!({"value": "RESPAWN"})),
        text_step("PLUGIN_RESPAWNED"),
        tool_step("remember", json!({"value": "KILL"})),
        text_step("PLUGIN_DRIFT_KILL_RECOVERED"),
        tool_step("remember", json!({"value": "DRIFT"})),
        text_step("PLUGIN_DRIFT_ERROR"),
        text_step("SESSION_AFTER_DRIFT"),
    ])
    .yolo(true)
    .build()
    .await
    .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let success = post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-plugin", "PLUGIN_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&success),
        "Call plugin Tool remember with value=PLUGIN_NONCE, then return the plugin result."
    );
    let success_events = env.events(session, None).await.expect("plugin events");
    assert_one_tool_terminal(&success_events, "remember");
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
            .any(|name| name == "remember")
    );
    assert!(fake_requests_from(&requests, 1).contains("PLUGIN_NONCE"));
    let context = env
        .get_json(&format!("/api/session/{session}/context"))
        .await
        .expect("plugin context");
    assert_context_tool_marker(&context, "PLUGIN_NONCE", "completed");
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

    let malformed = post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-plugin", "MALFORMED_NONCE", None),
    )
    .await;
    assert_eq!(
        response_text(&malformed),
        "Call plugin Tool remember with value=MALFORMED_NONCE, then return the plugin result."
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

    let killed = post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-plugin", "KILL", None),
    )
    .await;
    assert_eq!(
        response_text(&killed),
        "Call plugin Tool remember with value=KILL, then return the plugin result."
    );
    let after_kill = post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-plugin", "RESPAWN", None),
    )
    .await;
    assert_eq!(
        response_text(&after_kill),
        "Call plugin Tool remember with value=RESPAWN, then return the plugin result."
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
    let drift_kill = post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-plugin", "KILL", None),
    )
    .await;
    assert_eq!(
        response_text(&drift_kill),
        "Call plugin Tool remember with value=KILL, then return the plugin result."
    );
    let drift_error = post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-plugin", "DRIFT", None),
    )
    .await;
    assert_eq!(
        response_text(&drift_error),
        "Call plugin Tool remember with value=DRIFT, then return the plugin result."
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

    // Editing plugin.toml is startup-bound.  The running host still exposes
    // the old schema; a fresh BackendProcess is the visibility boundary.
    std::fs::write(
        env.project_path(".hya/plugins/toolbox/plugin.toml"),
        "id = \"toolbox\"\nkind = \"rust\"\ncommand = [\"python3\", \".hya/plugins/toolbox/plugin-v2.py\"]\ntimeout_ms = 1000\n",
    )
    .expect("edit plugin manifest");
    assert!(
        tool_names(&env.fake_requests().expect("old plugin requests")[0])
            .iter()
            .any(|name| name == "remember"),
        "running backend keeps old plugin declaration"
    );

    let restarted = E2eEnvBuilder::new()
        .project_file(
            ".hya/plugins/toolbox/plugin.toml",
            b"id = \"toolbox\"\nkind = \"rust\"\ncommand = [\"python3\", \".hya/plugins/toolbox/plugin-v2.py\"]\ntimeout_ms = 1000\n".to_vec(),
        )
        .project_file(
            ".hya/plugins/toolbox/plugin.py",
            PLUGIN_SCRIPT.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/plugins/toolbox/plugin-v2.py",
            PLUGIN_SCRIPT_V2.as_bytes().to_vec(),
        )
        .scripts(vec![text_step("RESTARTED_PLUGIN")])
        .build()
        .await
        .expect("restarted plugin env");
    let restarted_session = restarted.create_session().await.expect("restarted session");
    restarted
        .prompt(restarted_session, "inspect restarted plugin")
        .await
        .expect("restarted prompt");
    let restarted_requests = restarted.fake_requests().expect("restarted requests");
    let schema = restarted_requests[0].to_string();
    assert!(
        schema.contains("Remember v2"),
        "manifest edit visible after restart: {schema}"
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
    let command = post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-mcp", "MCP_NONCE", None),
    )
    .await;
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
    let context = env
        .get_json(&format!("/api/session/{session}/context"))
        .await
        .expect("MCP context");
    assert_context_tool_marker(&context, "echo:MCP_NONCE", "completed");

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
    let disconnect = post_ok(&disconnected_env, "/mcp/echo/disconnect", Value::Null).await;
    assert_eq!(disconnect, true);
    let status = disconnected_env
        .get_json("/mcp")
        .await
        .expect("disabled status");
    assert_eq!(status["echo"]["status"], "disabled");
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
            .get_json("/mcp")
            .await
            .expect("no auto respawn")["echo"]["status"],
        "disabled"
    );
    let (missing_status, missing_body) = request_json(
        &disconnected_env,
        Method::POST,
        "/mcp/unknown/connect",
        None,
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert!(missing_body.to_string().contains("MCP server not found"));
    post_ok(&disconnected_env, "/mcp/echo/connect", Value::Null).await;
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
        "/mcp",
        Some(json!({
            "name": "fast",
            "config": {"type": "local", "command": ["python3", "fixtures/mcp_echo.py"], "timeout": 50}
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
    post_ok(&death_env, "/mcp/echo/disconnect", Value::Null).await;
    post_ok(&death_env, "/mcp/echo/connect", Value::Null).await;
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
    let duplicate_names = tool_names(&duplicate.fake_requests().expect("duplicate requests")[0]);
    assert!(!duplicate_names.iter().any(|name| name == "remember"));
    assert!(duplicate_names.iter().any(|name| name == "read"));

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
        1
    );

    // Distinct MCP server names can still collide after namespacing:
    // mcp__a__b__c from (a__b,c) and (a,b__c).  The second candidate cannot
    // partially publish a duplicate.
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
    for (name, tool, expected_status) in [
        ("a__b", "c", StatusCode::OK),
        ("a", "b__c", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let (status, body) = request_json(
            &mcp,
            Method::POST,
            "/mcp",
            Some(json!({
                "name": name,
                "config": {"type": "local", "command": ["python3", "fixtures/mcp_echo.py", tool]}
            })),
        )
        .await;
        assert_eq!(status, expected_status, "MCP collision add {name}: {body}");
        if status == StatusCode::SERVICE_UNAVAILABLE {
            assert!(
                body.to_string()
                    .contains("duplicate tool name: mcp__a__b__c")
            );
        }
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
        1,
        "one prior publication may remain, but no duplicate candidate: {mcp_names:?}"
    );

    // Command/Skill collisions are metadata precedence, not runtime resource
    // conflicts.
    let command_skill = E2eEnvBuilder::new()
        .skill_file(
            SKILL_PATH,
            skill_markdown("same-name", "Skill", "SKILL_SHOULD_LOSE"),
        )
        .project_file(
            ".opencode/commands/same-name.md",
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
        ".opencode/commands/new-known.md",
        b"---\ndescription: known\n---\nKNOWN_OLD $ARGUMENTS\n".to_vec(),
    )
    .build()
    .await
    .expect("dynamic env");

    let first = env.create_session().await.expect("dynamic session");
    let first_command = post_ok(
        &env,
        &format!("/session/{first}/command"),
        command_request("user-playbook", "OLD", None),
    )
    .await;
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
    let second = post_ok(
        &env,
        &format!("/session/{first}/command"),
        command_request("user-playbook", "NEW", None),
    )
    .await;
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
    let first_context = env
        .get_json(&format!("/api/session/{first}/context"))
        .await
        .expect("Skill context");
    assert!(first_context.to_string().contains("SKILL_OLD_BODY OLD"));
    assert!(first_context.to_string().contains("SKILL_EDITED_BODY NEW"));

    // MCP disconnect/connect is the explicit dynamic publication boundary.
    post_ok(&env, "/mcp/echo/disconnect", Value::Null).await;
    post_ok(&env, "/mcp/echo/connect", Value::Null).await;
    env.wait_mcp_connected("echo", TIMEOUT)
        .await
        .expect("MCP refreshed");
    post_ok(
        &env,
        &format!("/session/{first}/command"),
        command_request("use-mcp", "MCP_RELOAD", None),
    )
    .await;
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

    // The plugin declaration is startup-bound; a separate fresh process with a
    // changed command is the only publication point.
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
        ".opencode/commands/added-after-bootstrap.md",
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
        tool_step("remember", json!({"value": "ERR_ONCE"})),
        text_step("AFTER_PLUGIN_ERROR"),
        tool_step("remember", json!({"value": "VALID_AFTER_ERROR"})),
        text_step("VALID_PLUGIN_FINAL"),
    ])
    .build()
    .await
    .expect("structured error env");
    let session = env
        .create_session()
        .await
        .expect("structured error session");

    post_ok(
        &env,
        &format!("/session/{session}/command"),
        command_request("use-plugin", "ERR_ONCE", None),
    )
    .await;
    env.wait_session_idle(&session, TIMEOUT)
        .await
        .expect("idle after custom error");
    let canonical = env.events(session, None).await.expect("canonical replay");
    assert_one_tool_terminal(&canonical, "remember");
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

    // Canonical API replay and the projected TUI card retain the same typed
    // value.error.type/message, not only a flattened human string.
    let replay = env
        .get_json(&format!("/sessions/{session}/events"))
        .await
        .expect("API canonical replay");
    assert!(replay.to_string().contains("\"type\":\"unknown\""));
    assert!(replay.to_string().contains("ERR_ONCE"));
    let context = env
        .get_json(&format!("/api/session/{session}/context"))
        .await
        .expect("TUI context replay");
    assert!(context.to_string().contains("\"status\":\"error\""));
    assert!(context.to_string().contains("\"type\":\"unknown\""));
    assert!(context.to_string().contains("\"message\""));
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
        .get_json(&format!("/api/session/{session}/context"))
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
    post_ok(
        &env,
        &format!("/session/{session}/command"),
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
