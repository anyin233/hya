#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::path::PathBuf;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use yaca_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Resource, Rule,
    SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry, WebSearchPlane,
};

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
        permission,
        interaction,
        spawner,
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
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
    let tool = ToolRegistry::builtins().get("webfetch").unwrap();
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
async fn webfetch_converts_html_to_readable_text_for_markdown() {
    let url = serve_once(
        "<html><head><style>.x{}</style></head><body><h1>Title</h1><p>Hello <strong>world</strong>.</p><script>bad()</script></body></html>",
        "text/html; charset=utf-8",
    )
    .await;
    let tool = ToolRegistry::builtins().get("webfetch").unwrap();
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
    let tool = ToolRegistry::builtins().get("webfetch").unwrap();
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
    let tool = ToolRegistry::builtins().get("webfetch").unwrap();
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
