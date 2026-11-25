//! Integration tests for `hya-tool`: webfetch.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Resource, Rule,
    SkillPlane, SpawnerPlane, ToolCtx, WebSearchPlane, handle::ArtifactPlane,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn ctx_with(rules: Vec<Rule>) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: PathBuf::from("."),
        roots: vec![PathBuf::from(".")],
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

async fn serve_once(body: &'static str, content_type: &'static str) -> String {
    serve_bytes(body.as_bytes(), content_type).await
}

async fn serve_bytes(body: &'static [u8], content_type: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
    });
    format!("http://{addr}/doc")
}

#[tokio::test]
async fn webfetch_fetches_text_from_http_url() {
    let url = serve_once("plain body", "text/plain").await;
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    let ctx = ctx_with(vec![allow(Action::WebFetch, "http://127.0.0.1:*")]);

    let out = tool
        .execute(&ctx, json!({ "url": url, "format": "text" }))
        .await
        .unwrap();

    assert_eq!(out["output"], "plain body");
    assert_eq!(out["status"], 200);
    assert_eq!(out["content_type"], "text/plain");
}

#[tokio::test]
async fn webfetch_rejects_an_oversized_chunked_body_before_eof() {
    const CHUNK_BYTES: usize = 64 * 1024;
    const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        let mut chunk = format!("{CHUNK_BYTES:x}\r\n").into_bytes();
        chunk.extend(std::iter::repeat_n(b'x', CHUNK_BYTES));
        chunk.extend_from_slice(b"\r\n");
        for _ in 0..MAX_RESPONSE_BYTES / CHUNK_BYTES {
            socket.write_all(&chunk).await.unwrap();
        }
        socket.write_all(b"1\r\nx\r\n").await.unwrap();
        // Keep the response open to distinguish rejecting the extra byte from waiting for EOF.
        let _ = socket.read(&mut request).await;
    });
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    let ctx = ctx_with(vec![allow(Action::WebFetch, "http://127.0.0.1:*")]);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tool.execute(&ctx, json!({"url": format!("http://{addr}/large")})),
    )
    .await;
    server.abort();
    let error = result
        .expect("webfetch must reject the oversized body before EOF")
        .unwrap_err();
    assert_eq!(error.to_string(), "response too large (exceeds 5MB limit)");
}

#[tokio::test]
async fn webfetch_accepts_fractional_timeout_seconds() {
    let url = serve_once("fractional timeout", "text/plain").await;
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    let schema = tool.schema().input_schema;
    assert_eq!(schema["properties"]["timeout"]["type"], "number");
    let ctx = ctx_with(vec![allow(Action::WebFetch, "http://127.0.0.1:*")]);

    let out = tool
        .execute(
            &ctx,
            json!({ "url": url, "format": "text", "timeout": 0.5 }),
        )
        .await
        .unwrap();

    assert_eq!(out["output"], "fractional timeout");
}

#[tokio::test]
async fn webfetch_converts_html_to_readable_text_for_markdown() {
    let url = serve_once(
        "<html><head><style>.x{}</style></head><body><h1>Title</h1><p>Hello <strong>world</strong>.</p><script>bad()</script></body></html>",
        "text/html; charset=utf-8",
    )
    .await;
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    let ctx = ctx_with(vec![allow(Action::WebFetch, "http://127.0.0.1:*")]);

    let out = tool
        .execute(&ctx, json!({ "url": url, "format": "markdown" }))
        .await
        .unwrap();
    let output = out["output"].as_str().unwrap();

    assert!(output.contains("# Title"));
    assert!(output.contains("Hello world."));
    assert!(!output.contains("bad()"));
}

#[tokio::test]
async fn webfetch_returns_open_code_attachment_for_images() {
    let url = serve_bytes(
        &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
        "image/png",
    )
    .await;
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    let ctx = ctx_with(vec![allow(Action::WebFetch, "http://127.0.0.1:*")]);

    let out = tool
        .execute(&ctx, json!({ "url": url, "format": "markdown" }))
        .await
        .unwrap();

    assert_eq!(out["output"], "Image fetched successfully");
    assert_eq!(out["metadata"], json!({}));
    assert_eq!(out["attachments"][0]["type"], "file");
    assert_eq!(out["attachments"][0]["mime"], "image/png");
    assert_eq!(
        out["attachments"][0]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
}

#[tokio::test]
async fn webfetch_requires_webfetch_permission() {
    let url = serve_once("blocked", "text/plain").await;
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    let ctx = ctx_with(vec![deny(Action::WebFetch, "*")]);

    let err = tool
        .execute(&ctx, json!({ "url": url, "format": "text" }))
        .await;

    assert!(err.is_err());
}

#[test]
fn url_resource_patterns_match_permission_rules() {
    let rules = PermissionRules::new(vec![allow(Action::WebFetch, "https://example.com/*")]);

    assert_eq!(
        rules.evaluate(
            Action::WebFetch,
            &Resource::Url("https://example.com/docs".into())
        ),
        Mode::Allow
    );
}
