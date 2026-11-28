//! `hya/network`: built-in ABI v2 native bundle providing `webfetch`,
//! `websearch`, per-bundle `config.yml` management (`NetworkService`), and
//! tool-visibility gating (`NetworkVisibilityHook`).
//!
//! Disabling `hya/network` in `config.yaml` (`bundles.builtin.disable: [hya/network]`)
//! or running with `--bare` cleanly removes all internet tools (air-gapped mode).

mod config;
mod service;
mod webfetch;
mod websearch;

use std::sync::{Arc, LazyLock};

use hya_runtime_abi::{NativePluginContext, NativeRegistrations};
use hya_tool::{Tool, ToolError};

pub use config::{
    BUNDLE_ID, NetworkConfig, NetworkPlane, WebFetchConfig, WebSearchConfig, WebSearchProvider,
};
pub use service::{NetworkService, NetworkVisibilityHook};
pub use webfetch::WebFetchTool;
pub use websearch::WebSearchTool;

fn http_client() -> Result<&'static reqwest::Client, ToolError> {
    static CLIENT: LazyLock<Result<reqwest::Client, reqwest::Error>> =
        LazyLock::new(|| reqwest::Client::builder().user_agent("hya").build());
    CLIENT
        .as_ref()
        .map_err(|error| ToolError::Other(error.to_string()))
}

/// Register `hya/network`'s tools, managed [`NetworkService`], and
/// [`NetworkVisibilityHook`] into `out`.
pub fn register(ctx: &NativePluginContext, out: &mut NativeRegistrations) {
    let runtime = Arc::clone(ctx.runtime());
    let plane = Arc::new(NetworkPlane::new(ctx.bundle_id(), NetworkConfig::default()));
    let tools: [Arc<dyn Tool>; 2] = [
        Arc::new(WebFetchTool::new(
            Arc::clone(&plane),
            Some(Arc::clone(&runtime)),
        )),
        Arc::new(WebSearchTool::new(
            Arc::clone(&plane),
            Some(Arc::clone(&runtime)),
        )),
    ];
    out.tools.extend(tools);
    out.services
        .push(Arc::new(NetworkService::new(Arc::clone(&plane))));
    out.hooks
        .push(Arc::new(NetworkVisibilityHook::new(plane, Some(runtime))));
}

hya_runtime_abi::export_native_bundle_v2!(|ctx, out| {
    register(ctx, out);
});

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use hya_bundle::{
        BuiltinBundleProfile, BuiltinBundleSelection, Bundle, BundleIdentity, PreparedAgent,
    };
    use hya_proto::{
        ActorClaim, AgentName, Envelope, Event, EventSeq, FinishReason, Message, ModelRef,
        Projection, SessionId,
    };
    use hya_runtime_abi::{
        BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
        BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind, HostBundleStorage,
        HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry, HostModelCaller,
        HostRuntime, HostSessionStore, HostTurnRunner, ModelCallRequest, ModelCallResponse,
        ModelStreamChunk, ModelWindowInfo, NativePluginContext, NativeRegistrations,
        SessionListFilter, SessionSummaryMeta, ToolVisibilityInput, ToolVisibilityOutcome,
        TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus,
    };
    use hya_tool::{
        Action, AgentDef, Mode, PermissionPlane, PermissionRules, Resource, Rule,
        SkillCatalogEntry, ToolCtx, ToolError,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::{broadcast, mpsc};
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct TestHostRuntime {
        config: Mutex<Value>,
        bus: broadcast::Sender<Envelope>,
    }

    impl TestHostRuntime {
        fn with_config(config: Value) -> Arc<Self> {
            let (bus, _) = broadcast::channel(8);
            Arc::new(Self {
                config: Mutex::new(config),
                bus,
            })
        }

        fn set_config(&self, config: Value) {
            *self.config.lock().unwrap() = config;
        }
    }

    #[async_trait]
    impl HostSessionStore for TestHostRuntime {
        async fn session_exists(&self, _session: SessionId) -> Result<bool, HostError> {
            Ok(false)
        }
        async fn read_events(
            &self,
            _session: SessionId,
            _after_seq: Option<EventSeq>,
            _limit: Option<usize>,
        ) -> Result<Vec<Envelope>, HostError> {
            Ok(Vec::new())
        }
        async fn append_events(
            &self,
            _session: SessionId,
            _claim: Option<ActorClaim>,
            _events: Vec<Event>,
        ) -> Result<Vec<Envelope>, HostError> {
            Ok(Vec::new())
        }
        async fn append_extension_event(
            &self,
            _session: SessionId,
            _bundle: &str,
            _kind: &str,
            _payload: Value,
            _claim: Option<ActorClaim>,
        ) -> Result<Envelope, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn read_projection(&self, _session: SessionId) -> Result<Arc<Projection>, HostError> {
            Ok(Arc::new(Projection::default()))
        }
        async fn read_extension_state(
            &self,
            _session: SessionId,
            _bundle: &str,
        ) -> Result<Option<Value>, HostError> {
            Ok(None)
        }
        async fn session_metadata(
            &self,
            _session: SessionId,
        ) -> Result<Option<SessionSummaryMeta>, HostError> {
            Ok(None)
        }
        async fn list_sessions(
            &self,
            _filter: SessionListFilter,
        ) -> Result<Vec<SessionSummaryMeta>, HostError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl HostBundleStorage for TestHostRuntime {
        async fn get_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
        ) -> Result<Option<BundleStateEntry>, HostError> {
            Ok(None)
        }
        async fn put_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
            _value: Value,
            _expected_version: Option<u64>,
        ) -> Result<BundleStateEntry, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn delete_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
        ) -> Result<bool, HostError> {
            Ok(false)
        }
        async fn list_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _prefix: Option<&str>,
        ) -> Result<Vec<BundleStateEntry>, HostError> {
            Ok(Vec::new())
        }
        async fn acquire_claim(
            &self,
            _request: BundleClaimRequest,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn renew_claim(
            &self,
            _lease: &BundleClaimLease,
            _ttl_millis: u64,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn release_claim(&self, _lease: &BundleClaimLease) -> Result<(), HostError> {
            Ok(())
        }
        async fn read_claim(
            &self,
            _bundle: &str,
            _resource_key: &str,
        ) -> Result<Option<BundleClaimLease>, HostError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl HostTurnRunner for TestHostRuntime {
        async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
            Ok(SessionId::new())
        }
        async fn run_turn(
            &self,
            request: TurnStartRequest,
            _cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            Ok(TurnOutcome {
                session: request.session,
                message_id: None,
                finish_reason: Some(FinishReason::Stop),
                output_text: String::new(),
                error: None,
            })
        }
        async fn spawn_session_and_run(
            &self,
            _request: TurnSpawnRequest,
            _cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
            Ok(())
        }
        async fn cancel_turn(
            &self,
            _session: SessionId,
            _reason: Option<String>,
        ) -> Result<bool, HostError> {
            Ok(false)
        }
        async fn turn_status(&self, _session: SessionId) -> Result<TurnStatus, HostError> {
            Ok(TurnStatus::Idle)
        }
    }

    #[async_trait]
    impl HostModelCaller for TestHostRuntime {
        async fn complete(
            &self,
            _request: ModelCallRequest,
            _cancel: CancellationToken,
        ) -> Result<ModelCallResponse, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn stream(
            &self,
            _request: ModelCallRequest,
            _cancel: CancellationToken,
        ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }
        async fn compact_window(
            &self,
            _model: &ModelRef,
            _messages: &[Message],
        ) -> Result<Option<String>, HostError> {
            Ok(None)
        }
        async fn model_window(
            &self,
            _model: &ModelRef,
        ) -> Result<Option<ModelWindowInfo>, HostError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl HostEventBus for TestHostRuntime {
        fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
            let _ = self.bus.send(envelope);
            Ok(())
        }
        fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
            Ok(self.bus.subscribe())
        }
        async fn publish_extension(
            &self,
            _session: SessionId,
            _bundle: &str,
            _kind: &str,
            _payload: Value,
        ) -> Result<(), HostError> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostCatalogReader for TestHostRuntime {
        async fn list_agents(
            &self,
            _session: Option<SessionId>,
        ) -> Result<Vec<AgentDef>, HostError> {
            Ok(Vec::new())
        }
        async fn get_agent(
            &self,
            _name: &AgentName,
            _session: Option<SessionId>,
        ) -> Result<Option<PreparedAgent>, HostError> {
            Ok(None)
        }
        async fn list_skills(
            &self,
            _session: Option<SessionId>,
        ) -> Result<Vec<SkillCatalogEntry>, HostError> {
            Ok(Vec::new())
        }
        async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
            Ok(vec![BundleCatalogSummary {
                identity: BundleIdentity {
                    id: BUNDLE_ID.to_string(),
                    version: "0.0.0".to_string(),
                    publisher: "hya".to_string(),
                },
                enabled: true,
                description: None,
            }])
        }
        async fn get_bundle(&self, _id: &str) -> Result<Option<Bundle>, HostError> {
            Ok(None)
        }
        async fn bundle_config(&self, _bundle_id: &str) -> Result<Value, HostError> {
            Ok(self.config.lock().unwrap().clone())
        }
        async fn global_config(&self) -> Result<Value, HostError> {
            Ok(json!({ "schema_version": 2 }))
        }
    }

    #[async_trait]
    impl HostExtensionRegistry for TestHostRuntime {
        async fn list_extensions(
            &self,
            _kind: Option<ExtensionKind>,
        ) -> Result<Vec<ExtensionEntry>, HostError> {
            Ok(Vec::new())
        }
        async fn has_bundle(&self, bundle_id: &str) -> Result<bool, HostError> {
            Ok(bundle_id == BUNDLE_ID)
        }
        async fn invoke_bundle_api(
            &self,
            _call: BundleApiInvocation,
        ) -> Result<BundleApiResult, HostError> {
            Ok(BundleApiResult {
                status: 200,
                body: Value::Null,
            })
        }
        async fn invoke_tool(
            &self,
            _session: Option<SessionId>,
            _tool_name: &str,
            input: Value,
            _cancel: CancellationToken,
        ) -> Result<Value, HostError> {
            Ok(input)
        }
    }

    impl HostRuntime for TestHostRuntime {
        fn session_store(&self) -> &dyn HostSessionStore {
            self
        }
        fn bundle_storage(&self) -> &dyn HostBundleStorage {
            self
        }
        fn turn_runner(&self) -> &dyn HostTurnRunner {
            self
        }
        fn model_caller(&self) -> &dyn HostModelCaller {
            self
        }
        fn event_bus(&self) -> &dyn HostEventBus {
            self
        }
        fn catalog_reader(&self) -> &dyn HostCatalogReader {
            self
        }
        fn extension_registry(&self) -> &dyn HostExtensionRegistry {
            self
        }
    }

    fn ctx_with(rules: Vec<Rule>, session: Option<SessionId>) -> ToolCtx {
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
        let mut ctx = ToolCtx::new(PathBuf::from("."), permission, CancellationToken::new());
        ctx.session = session;
        ctx
    }

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

    async fn serve_mcp_once(
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<(Value, String)>) {
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

    #[test]
    fn config_parsing_defaults_yaml_and_validation() {
        let defaults = NetworkConfig::default();
        assert!(defaults.webfetch.enabled);
        assert!(defaults.websearch.enabled);
        assert_eq!(defaults.websearch.provider, WebSearchProvider::Exa);
        assert!(defaults.websearch.endpoint.is_none());
        assert!(defaults.websearch.key.is_none());

        assert_eq!(NetworkConfig::from_json(&Value::Null).unwrap(), defaults);
        assert_eq!(NetworkConfig::from_json(&json!({})).unwrap(), defaults);

        let parsed = NetworkConfig::from_yaml(
            "webfetch:\n  enabled: false\nwebsearch:\n  provider: parallel\n  endpoint: https://search.example.test/mcp\n  key: secret-token\n  enabled: true\n",
        )
        .unwrap();
        assert!(!parsed.webfetch.enabled);
        assert!(parsed.websearch.enabled);
        assert_eq!(parsed.websearch.provider, WebSearchProvider::Parallel);
        assert_eq!(
            parsed.websearch.endpoint.as_deref(),
            Some("https://search.example.test/mcp")
        );
        assert_eq!(parsed.websearch.key.as_deref(), Some("secret-token"));

        assert!(NetworkConfig::from_json(&json!("not-an-object")).is_err());
        assert!(
            NetworkConfig::from_json(&json!({ "websearch": { "provider": "unknown" } })).is_err()
        );
    }

    #[tokio::test]
    async fn abi_v2_registration_service_lifecycle_and_visibility_hook() {
        let host = TestHostRuntime::with_config(json!({
            "webfetch": { "enabled": false },
            "websearch": { "provider": "parallel", "enabled": true }
        }));
        let runtime: Arc<dyn HostRuntime> = host.clone();
        let ctx = NativePluginContext::new(BUNDLE_ID, Arc::clone(&runtime));
        let mut regs = NativeRegistrations::new();
        register(&ctx, &mut regs);

        assert_eq!(regs.tools.len(), 2);
        assert_eq!(regs.services.len(), 1);
        assert_eq!(regs.hooks.len(), 1);
        assert_eq!(regs.services[0].id(), "network");
        assert!(!regs.services[0].health().await.healthy);

        regs.services[0].start(Arc::clone(&runtime)).await.unwrap();
        let health = regs.services[0].health().await;
        assert!(health.healthy);
        assert_eq!(health.status, "ready");
        assert_eq!(
            health.details.as_ref().unwrap()["webfetch"]["enabled"],
            false
        );
        assert_eq!(
            health.details.as_ref().unwrap()["websearch"]["provider"],
            "parallel"
        );

        let vis = regs.hooks[0]
            .tool_visibility(ToolVisibilityInput {
                session: SessionId::new(),
                agent: None,
                candidate_tools: vec!["webfetch".into(), "websearch".into()],
                interactive: true,
            })
            .await
            .unwrap();
        assert_eq!(
            vis,
            ToolVisibilityOutcome::Hide {
                tools: vec!["webfetch".to_string()]
            }
        );

        // Disabled webfetch rejects execution cleanly.
        let tool_ctx = ctx_with(vec![Rule::new(Action::WebFetch, "*", Mode::Allow)], None);
        let err = regs.tools[0]
            .execute(&tool_ctx, json!({ "url": "https://example.com" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("disabled"));

        // Reload with both tools enabled restores visibility.
        host.set_config(json!({
            "webfetch": { "enabled": true },
            "websearch": { "enabled": true }
        }));
        regs.services[0].start(runtime).await.unwrap();
        let vis2 = regs.hooks[0]
            .tool_visibility(ToolVisibilityInput {
                session: SessionId::new(),
                agent: None,
                candidate_tools: vec!["webfetch".into(), "websearch".into()],
                interactive: true,
            })
            .await
            .unwrap();
        assert_eq!(vis2, ToolVisibilityOutcome::Preserve);

        regs.services[0].stop().await.unwrap();
        assert!(!regs.services[0].health().await.healthy);
    }

    #[tokio::test]
    async fn webfetch_text_markdown_images_timeout_and_permissions() {
        let runtime: Arc<dyn HostRuntime> = TestHostRuntime::with_config(json!({}));
        let plane = NetworkPlane::configured(NetworkConfig::default());
        let tool = WebFetchTool::new(plane, Some(runtime));

        assert_eq!(
            tool.schema().input_schema["properties"]["timeout"]["type"],
            "number"
        );
        let ctx = ctx_with(
            vec![Rule::new(
                Action::WebFetch,
                "http://127.0.0.1:*",
                Mode::Allow,
            )],
            None,
        );

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
        assert!(
            mtxt.contains("# Title") && mtxt.contains("Hello world.") && !mtxt.contains("bad()")
        );

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

        let blocked_url = serve_bytes(b"blocked", "text/plain").await;
        let deny_ctx = ctx_with(vec![Rule::new(Action::WebFetch, "*", Mode::Deny)], None);
        assert!(
            tool.execute(&deny_ctx, json!({ "url": blocked_url, "format": "text" }))
                .await
                .is_err()
        );
        let rules = PermissionRules::new(vec![Rule::new(
            Action::WebFetch,
            "https://example.com/*",
            Mode::Allow,
        )]);
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
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
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
        let tool = WebFetchTool::new(NetworkPlane::configured(NetworkConfig::default()), None);
        let ctx = ctx_with(
            vec![Rule::new(
                Action::WebFetch,
                "http://127.0.0.1:*",
                Mode::Allow,
            )],
            None,
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
    async fn websearch_exa_parallel_host_config_and_permission() {
        // Exa with query-param API key & full params loaded from HostRuntime bundle_config.
        let (url, req_h) = serve_mcp_once(
            r#"{"result":{"content":[{"type":"text","text":"Result A\nResult B"}]}}"#,
        )
        .await;
        let host = TestHostRuntime::with_config(json!({
            "websearch": {
                "provider": "exa",
                "endpoint": url,
                "key": "secret",
                "enabled": true
            }
        }));
        let runtime: Arc<dyn HostRuntime> = host.clone();
        let ctx = NativePluginContext::new(BUNDLE_ID, Arc::clone(&runtime));
        let mut regs = NativeRegistrations::new();
        register(&ctx, &mut regs);
        regs.services[0].start(Arc::clone(&runtime)).await.unwrap();

        let tool = regs.tools.iter().find(|t| t.name() == "websearch").unwrap();
        let schema = tool.schema();
        assert!(
            schema.description.contains("The current year is")
                && schema.description.contains("MUST use this year")
        );

        let tool_ctx = ctx_with(
            vec![Rule::new(Action::WebSearch, "rust news 2026", Mode::Allow)],
            None,
        );
        let out = tool
            .execute(
                &tool_ctx,
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

        // Parallel with bearer token & session_id after live reload via NetworkService.
        let (url2, req_h2) =
            serve_mcp_once(r#"{"result":{"content":[{"type":"text","text":"P"}]}}"#).await;
        host.set_config(json!({
            "websearch": {
                "provider": "parallel",
                "endpoint": url2,
                "key": "secret",
                "enabled": true
            }
        }));
        regs.services[0].start(Arc::clone(&runtime)).await.unwrap();

        let session = SessionId::new();
        let tool_ctx2 = ctx_with(
            vec![Rule::new(Action::WebSearch, "rust", Mode::Allow)],
            Some(session),
        );
        tool.execute(&tool_ctx2, json!({ "query": "rust" }))
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
        let deny_ctx = ctx_with(vec![Rule::new(Action::WebSearch, "*", Mode::Deny)], None);
        assert!(matches!(
            tool.execute(&deny_ctx, json!({ "query": "blocked" })).await,
            Err(ToolError::Permission(_))
        ));
    }

    #[test]
    fn air_gapped_selection_disables_hya_network_cleanly() {
        let air_gapped = BuiltinBundleSelection {
            profile: BuiltinBundleProfile::Full,
            enable: Vec::new(),
            disable: vec![BUNDLE_ID.to_string()],
        };
        assert!(!air_gapped.is_enabled(BUNDLE_ID));
        assert!(!air_gapped.enabled_identities().contains(&BUNDLE_ID));
    }
}
