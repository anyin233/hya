//! Real MCP servers over HTTP transports (Streamable HTTP + classic HTTP+SSE).
//!
//! Each test boots a stdlib-only Python fixture server on an ephemeral loopback
//! port and drives it through the same `prepare` path used for stdio servers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use hya_mcp::McpServerConfig;
use serde_json::json;

const STREAMABLE_FIXTURE: &str = "tests/fixtures/mcp_streamable_http.py";
const SSE_FIXTURE: &str = "tests/fixtures/mcp_http_sse.py";

/// Owns the fixture child process; kill on drop so failed asserts clean up.
struct FixtureServer(Child);

impl Drop for FixtureServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name)
}

/// Start a fixture server and wait until its TCP port accepts connections.
fn spawn_fixture(script: &str, args: &[&str]) -> (u16, FixtureServer) {
    let port = free_port();
    let port_string = port.to_string();
    let child = Command::new("python3")
        .arg(fixture_path(script))
        .args(args)
        .arg(&port_string)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fixture server");
    let server = FixtureServer(child);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("fixture server did not start on port {port}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (port, server)
}

fn url_config(port: u16) -> McpServerConfig {
    McpServerConfig {
        url: Some(format!("http://127.0.0.1:{port}/mcp")),
        timeout_ms: Some(5000),
        ..McpServerConfig::default()
    }
}

fn ctx_allowing_mcp() -> hya_tool::ToolCtx {
    let (permission, _rx) =
        hya_tool::PermissionPlane::new(hya_tool::PermissionRules::new(vec![hya_tool::Rule::new(
            hya_tool::Action::Mcp,
            "*",
            hya_tool::Mode::Allow,
        )]));
    let (interaction, _irx) = hya_tool::InteractionPlane::new();
    let (spawner, _srx) = hya_tool::SpawnerPlane::new();
    hya_tool::ToolCtx {
        permission,
        interaction,
        spawner,
        workflows: hya_tool::WorkflowPlane::disconnected(),
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        todo: hya_tool::TodoPlane::default(),
        skills: hya_tool::SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: std::env::temp_dir(),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

async fn call_tool(
    server: &hya_mcp::PreparedMcpServer,
    tool_suffix: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    let tool = server
        .tools()
        .into_iter()
        .find(|tool| tool.name().ends_with(tool_suffix))
        .unwrap_or_else(|| panic!("tool {tool_suffix} not registered"));
    tool.execute(&ctx_allowing_mcp(), input)
        .await
        .expect("tool call succeeds")
}

#[tokio::test]
async fn streamable_http_connects_and_calls_tool_over_json_response() {
    let (port, _server) = spawn_fixture(STREAMABLE_FIXTURE, &[]);
    let prepared = hya_mcp::prepare("http".into(), url_config(port))
        .await
        .expect("streamable http server connects");

    assert!(
        prepared
            .tools()
            .iter()
            .any(|tool| tool.name() == "mcp__http__ping"),
        "ping tool registered"
    );
    let out = call_tool(&prepared, "__ping", json!({ "msg": "hya" })).await;
    assert_eq!(out["output"], "pong:hya");
}

#[tokio::test]
async fn streamable_http_parses_sse_streamed_tool_response() {
    let (port, _server) = spawn_fixture(STREAMABLE_FIXTURE, &[]);
    let prepared = hya_mcp::prepare("http".into(), url_config(port))
        .await
        .expect("streamable http server connects");

    // stream_add replies with text/event-stream instead of JSON: the client
    // must demux the SSE frame and find the JSON-RPC result inside.
    let out = call_tool(&prepared, "__stream_add", json!({ "a": 2, "b": 3 })).await;
    assert_eq!(out["output"], "stream_sum:5");
}

#[tokio::test]
async fn streamable_http_surfaces_tool_iserror_and_rpc_error() {
    let (port, _server) = spawn_fixture(STREAMABLE_FIXTURE, &[]);
    let prepared = hya_mcp::prepare("http".into(), url_config(port))
        .await
        .expect("streamable http server connects");

    let ctx = ctx_allowing_mcp();
    let fail = prepared
        .tools()
        .into_iter()
        .find(|tool| tool.name().ends_with("__fail_tool"))
        .expect("fail_tool registered");
    let err = fail
        .execute(&ctx, json!({}))
        .await
        .expect_err("isError result is a tool error");
    assert!(err.to_string().contains("deliberate tool failure"));

    let rpc = prepared
        .tools()
        .into_iter()
        .find(|tool| tool.name().ends_with("__rpc_error"))
        .expect("rpc_error registered");
    let err = rpc
        .execute(&ctx, json!({}))
        .await
        .expect_err("json-rpc error is a tool error");
    assert!(err.to_string().contains("deliberate failure"));
}

#[tokio::test]
async fn streamable_http_stateless_server_connects_without_sessions() {
    let (port, _server) = spawn_fixture(STREAMABLE_FIXTURE, &["--stateless"]);
    let prepared = hya_mcp::prepare("http".into(), url_config(port))
        .await
        .expect("stateless server connects");

    let out = call_tool(&prepared, "__add", json!({ "a": 20, "b": 22 })).await;
    assert_eq!(out["output"], "sum:42");
}

#[tokio::test]
async fn classic_http_sse_connects_and_calls_tool() {
    let (port, _server) = spawn_fixture(SSE_FIXTURE, &[]);
    let config = McpServerConfig {
        url: Some(format!("http://127.0.0.1:{port}/sse")),
        transport: Some("sse".into()),
        timeout_ms: Some(5000),
        ..McpServerConfig::default()
    };
    let prepared = hya_mcp::prepare("sse".into(), config)
        .await
        .expect("classic http+sse server connects");

    assert!(
        prepared
            .tools()
            .iter()
            .any(|tool| tool.name() == "mcp__sse__ping"),
        "ping tool registered"
    );
    let out = call_tool(&prepared, "__ping", json!({ "msg": "sse" })).await;
    assert_eq!(out["output"], "pong:sse");
}
