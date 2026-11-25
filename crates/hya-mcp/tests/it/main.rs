//! Real MCP servers over HTTP transports (Streamable HTTP + classic HTTP+SSE)
//! and stdio EOF handling.
//!
//! Tests boot stdlib-only Python fixture servers on ephemeral loopback ports
//! and drive them through the same `prepare` path used for stdio servers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
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

/// A loopback port with no listener, for the connection-refused cases.
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

/// Start a fixture on a port the fixture itself binds (`0`) and reports on its
/// first stdout line, so parallel tests never race for a port.
fn spawn_fixture(script: &str, args: &[&str]) -> (u16, FixtureServer) {
    let mut child = Command::new("python3")
        .arg(fixture_path(script))
        .args(args)
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fixture server");
    let stdout = child.stdout.take().expect("fixture stdout");
    let server = FixtureServer(child);
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("read fixture port");
    let port = line.trim().parse().expect("fixture prints its port");
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
        nested: hya_tool::NestedToolPlane::disconnected(),
        permission,
        interaction,
        spawner,
        workflows: hya_tool::WorkflowPlane::disconnected(),
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
        session: None,
        parent_session: None,
        skills: hya_tool::SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: std::env::temp_dir(),
        roots: vec![std::env::temp_dir()],
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

async fn call_tool_result(
    server: &hya_mcp::PreparedMcpServer,
    tool_suffix: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value, hya_tool::ToolError> {
    let tool = server
        .tools()
        .into_iter()
        .find(|tool| tool.name().ends_with(tool_suffix))
        .unwrap_or_else(|| panic!("tool {tool_suffix} not registered"));
    tool.execute(&ctx_allowing_mcp(), input).await
}

async fn call_tool(
    server: &hya_mcp::PreparedMcpServer,
    tool_suffix: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    call_tool_result(server, tool_suffix, input)
        .await
        .expect("tool call succeeds")
}

#[tokio::test]
async fn streamable_http_json_sse_errors_sessionless_404_and_timeout() {
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
    assert_eq!(
        call_tool(&prepared, "__ping", json!({ "msg": "hya" })).await["output"],
        "pong:hya"
    );
    // stream_add replies with text/event-stream instead of JSON: demux SSE frame.
    assert_eq!(
        call_tool(&prepared, "__stream_add", json!({ "a": 2, "b": 3 })).await["output"],
        "stream_sum:5"
    );

    let err = call_tool_result(&prepared, "__fail_tool", json!({}))
        .await
        .expect_err("isError result is a tool error");
    assert!(err.to_string().contains("deliberate tool failure"));

    let err = call_tool_result(&prepared, "__rpc_error", json!({}))
        .await
        .expect_err("json-rpc error is a tool error");
    assert!(err.to_string().contains("deliberate failure"));

    // Raw client skipping initialize has no Mcp-Session-Id -> server returns 404.
    let raw = hya_mcp::McpClient::connect_streamable_http(format!("http://127.0.0.1:{port}/mcp"))
        .expect("client builds");
    let err = raw
        .call("tools/list", json!({}), Duration::from_secs(5))
        .await
        .expect_err("session-less call must fail");
    assert!(err.to_string().contains("404"), "got: {err}");

    // Slow tool exceeding configured timeout returns a typed timeout error.
    let short_timeout = McpServerConfig {
        timeout_ms: Some(80),
        ..url_config(port)
    };
    let timed = hya_mcp::prepare("http".into(), short_timeout)
        .await
        .expect("connects");
    let err = call_tool_result(&timed, "__slow", json!({ "seconds": 5 }))
        .await
        .expect_err("5s sleep inside an 80ms budget times out");
    assert!(err.to_string().contains("timed out"), "got: {err}");
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
async fn streamable_http_surfaces_http_500_and_malformed_json() {
    for (flag, expected_substrings) in [
        ("--fail5xx", &["500", "deliberate"][..]),
        ("--malformed", &["json"][..]),
    ] {
        let (port, _server) = spawn_fixture(STREAMABLE_FIXTURE, &[flag]);
        let err = match hya_mcp::prepare("http".into(), url_config(port)).await {
            Err(error) => error,
            Ok(_) => panic!("{flag} must fail prepare"),
        };
        let msg = err.to_string();
        for needle in expected_substrings {
            assert!(
                msg.contains(needle),
                "{flag}: expected `{needle}` in `{msg}`"
            );
        }
    }
}

#[tokio::test]
async fn classic_http_sse_connects_calls_tool_and_surfaces_error_paths() {
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
    assert_eq!(
        call_tool(&prepared, "__ping", json!({ "msg": "sse" })).await["output"],
        "pong:sse"
    );

    let err = call_tool_result(&prepared, "__fail_tool", json!({}))
        .await
        .expect_err("isError becomes a tool error");
    assert!(err.to_string().contains("deliberate tool failure"));

    let err = call_tool_result(&prepared, "__rpc_error", json!({}))
        .await
        .expect_err("json-rpc error becomes a tool error");
    assert!(err.to_string().contains("deliberate failure"));
}

#[tokio::test]
async fn http_transports_connection_refused_fail_with_io_error() {
    let port = free_port();
    let streamable_err = match hya_mcp::prepare("http".into(), url_config(port)).await {
        Err(error) => error,
        Ok(_) => panic!("no listener on port, prepare must fail"),
    };
    assert!(
        streamable_err.to_string().contains("io"),
        "got: {streamable_err}"
    );

    let sse_err = match hya_mcp::McpClient::connect_classic_sse(&format!(
        "http://127.0.0.1:{port}/sse"
    ))
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("no listener on port, connect must fail"),
    };
    assert!(sse_err.to_string().contains("io"), "got: {sse_err}");
}

#[tokio::test]
async fn stdio_server_exit_mid_call_surfaces_closed() {
    use hya_mcp::McpClient;
    use tokio::process::Command as AsyncCommand;

    // The child exits without answering: the reader sees EOF while a request
    // is pending, so the call resolves as Closed rather than hanging.
    let mut child = AsyncCommand::new("python3")
        .arg("-c")
        .arg("import sys; sys.stdin.readline(); sys.exit(0)")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn exiting child");
    let stdout = child.stdout.take().expect("stdout");
    let stdin = child.stdin.take().expect("stdin");
    let client = McpClient::new(stdout, stdin);
    let error = client
        .call("tools/list", json!({}), Duration::from_secs(5))
        .await
        .expect_err("child exits without answering");
    assert!(error.to_string().contains("closed"), "got: {error}");
    let _ = child.kill().await;
    let _ = child.wait().await;
}
