//! Network tools: `webfetch` and `websearch` contracts, auth headers, limits, and permissions.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::Duration;

use hya_proto::SessionId;
use hya_tool::{
    Action, Mode, PermissionRules, Resource, ToolError, WebSearchConfig, WebSearchPlane,
    WebSearchProvider,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::common::{allow, ctx_with, deny};

async fn serve_bytes(body: &'static [u8], content_type: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await;
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(body).await;
    });
    format!("http://{addr}/doc")
}

async fn serve_mcp_once(body: &'static str) -> (String, tokio::task::JoinHandle<(Value, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let json_start = request.find("\r\n\r\n").unwrap() + 4;
        let payload: Value = serde_json::from_str(&request[json_start..]).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        (payload, request)
    });
    (format!("http://{addr}/mcp"), handle)
}

#[tokio::test]
async fn webfetch_text_markdown_images_timeout_and_permissions() {
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    assert_eq!(
        tool.schema().input_schema["properties"]["timeout"]["type"],
        "number"
    );
    let ctx = ctx_with(
        vec![allow(Action::WebFetch, "http://127.0.0.1:*")],
        PathBuf::from("."),
    );

    // Plain text + fractional timeout.
    let url = serve_bytes(b"plain body", "text/plain").await;
    let out = tool
        .execute(
            &ctx,
            json!({ "url": url, "format": "text", "timeout": 0.5 }),
        )
        .await
        .unwrap();
    assert_eq!(out["output"], "plain body");
    assert_eq!(out["status"], 200);
    assert_eq!(out["content_type"], "text/plain");

    // HTML -> markdown strips script/style.
    let html_url = serve_bytes(
        b"<html><head><style>.x{}</style></head><body><h1>Title</h1><p>Hello <strong>world</strong>.</p><script>bad()</script></body></html>",
        "text/html; charset=utf-8",
    )
    .await;
    let md = tool
        .execute(&ctx, json!({ "url": html_url, "format": "markdown" }))
        .await
        .unwrap();
    let mtxt = md["output"].as_str().unwrap();
    assert!(mtxt.contains("# Title") && mtxt.contains("Hello world.") && !mtxt.contains("bad()"));

    // Image attachment.
    let img_url = serve_bytes(
        &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
        "image/png",
    )
    .await;
    let img = tool
        .execute(&ctx, json!({ "url": img_url, "format": "markdown" }))
        .await
        .unwrap();
    assert_eq!(img["output"], "Image fetched successfully");
    assert_eq!(img["attachments"][0]["mime"], "image/png");
    assert_eq!(
        img["attachments"][0]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );

    // Permission gate & URL glob matching.
    let blocked_url = serve_bytes(b"blocked", "text/plain").await;
    let deny_ctx = ctx_with(vec![deny(Action::WebFetch, "*")], PathBuf::from("."));
    assert!(
        tool.execute(&deny_ctx, json!({ "url": blocked_url, "format": "text" }))
            .await
            .is_err()
    );
    let rules = PermissionRules::new(vec![allow(Action::WebFetch, "https://example.com/*")]);
    assert_eq!(
        rules.evaluate(
            Action::WebFetch,
            &Resource::Url("https://example.com/docs".into())
        ),
        Mode::Allow
    );
}

#[tokio::test]
async fn webfetch_rejects_oversized_chunked_body_before_eof() {
    const CHUNK_BYTES: usize = 64 * 1024;
    const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut req = [0; 1024];
        let _ = socket.read(&mut req).await;
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await;
        let mut chunk = format!("{CHUNK_BYTES:x}\r\n").into_bytes();
        chunk.extend(std::iter::repeat_n(b'x', CHUNK_BYTES));
        chunk.extend_from_slice(b"\r\n");
        for _ in 0..MAX_RESPONSE_BYTES / CHUNK_BYTES {
            if socket.write_all(&chunk).await.is_err() {
                return;
            }
        }
        let _ = socket.write_all(b"1\r\nx\r\n").await;
        let _ = socket.read(&mut req).await;
    });
    let tool = hya_tool_loader::builtins().get("webfetch").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::WebFetch, "http://127.0.0.1:*")],
        PathBuf::from("."),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tool.execute(&ctx, json!({"url": format!("http://{addr}/large")})),
    )
    .await;
    server.abort();
    assert_eq!(
        result
            .expect("rejected before EOF")
            .unwrap_err()
            .to_string(),
        "response too large (exceeds 5MB limit)"
    );
}

#[tokio::test]
async fn websearch_schema_defaults_exa_parallel_auth_and_permission() {
    let tool = hya_tool_loader::builtins().get("websearch").unwrap();
    let schema = tool.schema();
    assert!(
        schema.description.contains("The current year is")
            && schema.description.contains("MUST use this year")
    );
    assert!(
        schema.input_schema["properties"]["livecrawl"]["description"]
            .as_str()
            .unwrap()
            .contains("fallback")
    );

    let defaults = WebSearchConfig::default();
    assert_eq!(defaults.provider, WebSearchProvider::Exa);
    assert!(defaults.endpoint.is_none() && defaults.key.is_none() && defaults.enabled);

    // Exa with query-param API key & full params.
    let (url, req_h) =
        serve_mcp_once(r#"{"result":{"content":[{"type":"text","text":"Result A\nResult B"}]}}"#)
            .await;
    let mut ctx = ctx_with(
        vec![allow(Action::WebSearch, "rust news 2026")],
        PathBuf::from("."),
    );
    ctx.websearch = WebSearchPlane::configured(WebSearchConfig {
        provider: WebSearchProvider::Exa,
        endpoint: Some(url),
        key: Some("secret".into()),
        enabled: true,
    });
    let out = tool
        .execute(
            &ctx,
            json!({
                "query": "rust news 2026", "numResults": 3, "type": "fast",
                "livecrawl": "preferred", "contextMaxCharacters": 4096
            }),
        )
        .await
        .unwrap();
    let (sent, raw) = req_h.await.unwrap();
    assert_eq!(out["title"], "Exa Web Search: rust news 2026");
    assert_eq!(out["output"], "Result A\nResult B");
    assert_eq!(out["metadata"]["provider"], "exa");
    assert_eq!(sent["params"]["name"], "web_search_exa");
    assert_eq!(sent["params"]["arguments"]["numResults"], 3);
    assert!(raw.starts_with("POST /mcp?exaApiKey=secret HTTP/1.1\r\n"));

    // Parallel with bearer token & session_id.
    let (url2, req_h2) =
        serve_mcp_once(r#"{"result":{"content":[{"type":"text","text":"P"}]}}"#).await;
    let session = SessionId::new();
    let mut ctx2 = ctx_with(vec![allow(Action::WebSearch, "rust")], PathBuf::from("."));
    ctx2.session = Some(session);
    ctx2.websearch = WebSearchPlane::configured(WebSearchConfig {
        provider: WebSearchProvider::Parallel,
        endpoint: Some(url2),
        key: Some("secret".into()),
        enabled: true,
    });
    tool.execute(&ctx2, json!({ "query": "rust" }))
        .await
        .unwrap();
    let (sent2, raw2) = req_h2.await.unwrap();
    assert_eq!(sent2["params"]["name"], "web_search");
    assert_eq!(
        sent2["params"]["arguments"]["session_id"],
        session.to_string()
    );
    assert!(raw2.contains("\r\nauthorization: Bearer secret\r\n"));

    // Permission gate blocks before calling provider.
    let deny_ctx = ctx_with(vec![deny(Action::WebSearch, "*")], PathBuf::from("."));
    assert!(matches!(
        tool.execute(&deny_ctx, json!({ "query": "blocked" })).await,
        Err(ToolError::Permission(_))
    ));
}
