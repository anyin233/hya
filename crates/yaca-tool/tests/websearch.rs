#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::path::PathBuf;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use yaca_proto::SessionId;
use yaca_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane, WebSearchProvider,
};

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn ctx_with(rules: Vec<Rule>, websearch: WebSearchPlane) -> ToolCtx {
    let session = SessionId::new();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch,
        lsp: LspPlane::default(),
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[test]
fn websearch_schema_includes_open_code_guidance() {
    // Given
    let tool = ToolRegistry::builtins().get("websearch").unwrap();

    // When
    let schema = tool.schema();
    let properties = &schema.input_schema["properties"];

    // Then
    assert!(schema.description.contains("The current year is"));
    assert!(schema.description.contains("MUST use this year"));
    assert_eq!(
        properties["livecrawl"]["description"],
        "Live crawl mode - 'fallback': use live crawling as backup if cached content unavailable, 'preferred': prioritize live crawling (default: 'fallback')"
    );
    assert_eq!(
        properties["type"]["description"],
        "Search type - 'auto': balanced search (default), 'fast': quick results, 'deep': comprehensive search"
    );
    assert_eq!(
        properties["contextMaxCharacters"]["description"],
        "Maximum characters for context string optimized for LLMs (default: 10000)"
    );
}

async fn serve_once(body: &'static str) -> (String, tokio::task::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
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
        payload
    });
    (format!("http://{addr}/mcp"), handle)
}

#[tokio::test]
async fn websearch_calls_mcp_provider_and_returns_open_code_shape() {
    // Given
    let (url, request) =
        serve_once(r#"{"result":{"content":[{"type":"text","text":"Result A\nResult B"}]}}"#).await;
    let tool = ToolRegistry::builtins().get("websearch").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::WebSearch, "rust news 2026")],
        WebSearchPlane::new(WebSearchProvider::Exa, url),
    );

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "query": "rust news 2026",
                "numResults": 3,
                "type": "fast",
                "livecrawl": "preferred",
                "contextMaxCharacters": 4096
            }),
        )
        .await
        .unwrap();
    let sent = request.await.unwrap();

    // Then
    assert_eq!(out["title"], "Exa Web Search: rust news 2026");
    assert_eq!(out["output"], "Result A\nResult B");
    assert_eq!(out["metadata"]["provider"], "exa");
    assert_eq!(sent["method"], "tools/call");
    assert_eq!(sent["params"]["name"], "web_search_exa");
    assert_eq!(sent["params"]["arguments"]["query"], "rust news 2026");
    assert_eq!(sent["params"]["arguments"]["numResults"], 3);
    assert_eq!(sent["params"]["arguments"]["type"], "fast");
    assert_eq!(sent["params"]["arguments"]["livecrawl"], "preferred");
    assert_eq!(sent["params"]["arguments"]["contextMaxCharacters"], 4096);
}

#[tokio::test]
async fn websearch_requires_permission_before_calling_provider() {
    // Given
    let (url, _request) =
        serve_once(r#"{"result":{"content":[{"type":"text","text":"should not call"}]}}"#).await;
    let tool = ToolRegistry::builtins().get("websearch").unwrap();
    let ctx = ctx_with(
        vec![deny(Action::WebSearch, "*")],
        WebSearchPlane::new(WebSearchProvider::Exa, url),
    );

    // When
    let result = tool
        .execute(&ctx, json!({ "query": "blocked search" }))
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
}
