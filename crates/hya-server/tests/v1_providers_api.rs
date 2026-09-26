//! Provider View routes over a fake `ProviderControl`: provider rows with
//! kind/base URL/key source, provider upsert, refresh, config model
//! overrides (model ids with `/`), key set/remove through the control, the
//! one-shot model test, and HTTP/gRPC parity for the new rpcs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{
    Capabilities, FakeProvider, FakeStep, ModelCatalogSource, ProviderAuthState,
    ProviderCatalogResult, ProviderCatalogSnapshot, ProviderCatalogSource, ProviderCatalogState,
    ProviderKind, ProviderModel, ProviderRouter,
};
use hya_server::{
    AppState, PROVIDER_NOT_FOUND, ProviderChange, ProviderControl, ProviderControlError,
    ProviderControlFuture, ProviderDiscoveryReport, ProviderKeySource, ProviderModelOverride,
    ProviderSettings, ProviderUpsert, V1Grpc, router,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Debug, Clone, PartialEq)]
enum Call {
    SetKey(String, String),
    RemoveKey(String),
    Upsert(ProviderUpsert),
    Refresh(String),
    SetModel(String, String, ProviderModelOverride),
    RemoveModel(String, String),
}

#[derive(Default)]
struct FakeControl {
    calls: Mutex<Vec<Call>>,
}

fn discovered(count: usize) -> ProviderChange {
    ProviderChange {
        configured: true,
        discovery: Some(ProviderDiscoveryReport {
            ok: true,
            result: "models".to_string(),
            error_message: None,
            model_count: count,
        }),
    }
}

impl FakeControl {
    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl ProviderControl for FakeControl {
    fn available(&self) -> bool {
        true
    }

    fn list_saved_keys(&self) -> ProviderControlFuture<'_, Vec<String>> {
        Box::pin(async { Ok(vec!["acme".to_string()]) })
    }

    fn list_settings(&self) -> ProviderControlFuture<'_, Vec<ProviderSettings>> {
        Box::pin(async {
            Ok(vec![
                ProviderSettings {
                    id: "acme".to_string(),
                    kind: "openai".to_string(),
                    base_url: "https://acme.example/v1".to_string(),
                    key_source: ProviderKeySource::Saved,
                },
                ProviderSettings {
                    id: "empty".to_string(),
                    kind: "anthropic".to_string(),
                    base_url: "https://empty.example/v1".to_string(),
                    key_source: ProviderKeySource::None,
                },
            ])
        })
    }

    fn set_key(
        &self,
        provider_id: String,
        key: String,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(Call::SetKey(provider_id, key));
        Box::pin(async { Ok(discovered(2)) })
    }

    fn remove_key(&self, provider_id: String) -> ProviderControlFuture<'_, (bool, ProviderChange)> {
        self.record(Call::RemoveKey(provider_id));
        Box::pin(async {
            Ok((
                true,
                ProviderChange {
                    configured: true,
                    discovery: None,
                },
            ))
        })
    }

    fn upsert_provider(
        &self,
        request: ProviderUpsert,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(Call::Upsert(request));
        Box::pin(async {
            Ok(ProviderChange {
                configured: true,
                discovery: Some(ProviderDiscoveryReport {
                    ok: false,
                    result: "unavailable".to_string(),
                    error_message: Some("provider catalog transport failed".to_string()),
                    model_count: 0,
                }),
            })
        })
    }

    fn refresh_provider(&self, provider_id: String) -> ProviderControlFuture<'_, ProviderChange> {
        let missing = provider_id == "missing";
        self.record(Call::Refresh(provider_id));
        Box::pin(async move {
            if missing {
                Err(ProviderControlError::new(
                    PROVIDER_NOT_FOUND,
                    "provider not configured: missing",
                ))
            } else {
                Ok(discovered(2))
            }
        })
    }

    fn set_model(
        &self,
        provider_id: String,
        model_id: String,
        metadata: ProviderModelOverride,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(Call::SetModel(provider_id, model_id, metadata));
        Box::pin(async {
            Ok(ProviderChange {
                configured: true,
                discovery: None,
            })
        })
    }

    fn remove_model(
        &self,
        provider_id: String,
        model_id: String,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(Call::RemoveModel(provider_id, model_id));
        Box::pin(async {
            Ok(ProviderChange {
                configured: true,
                discovery: None,
            })
        })
    }
}

fn row(model: &str, source: ModelCatalogSource, display: Option<&str>) -> ProviderModel {
    ProviderModel {
        provider_id: "acme".to_string(),
        model_id: model.to_string(),
        capabilities: Capabilities {
            max_context: 64_000,
            max_output: 4_096,
            ..Capabilities::default()
        },
        reasoning_variants: vec!["low".to_string()],
        reasoning_default: None,
        display_name: display.map(str::to_string),
        source,
    }
}

async fn app_state(control: Option<Arc<FakeControl>>) -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("h".to_string()),
        FakeStep::Finish(FinishReason::Length),
    ]]);
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ));
    engine.publish_provider_catalog(
        engine.provider_router(),
        Arc::new(ProviderCatalogSnapshot::build(
            vec![
                row(
                    "vendor/m-1",
                    ModelCatalogSource::Overridden,
                    Some("Vendor M1"),
                ),
                row("local", ModelCatalogSource::Configured, None),
                row("remote-only", ModelCatalogSource::Discovered, None),
            ],
            vec![
                ProviderCatalogState {
                    provider_id: "acme".to_string(),
                    kind: ProviderKind::OpenAiCompatible,
                    source: ProviderCatalogSource::Discovered,
                    auth: ProviderAuthState::Unauthenticated,
                    result: ProviderCatalogResult::Models,
                },
                ProviderCatalogState {
                    provider_id: "empty".to_string(),
                    kind: ProviderKind::Anthropic,
                    source: ProviderCatalogSource::None,
                    auth: ProviderAuthState::Unauthenticated,
                    result: ProviderCatalogResult::Unavailable,
                },
            ],
            None,
        )),
    );
    let state = AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("acme/local"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    );
    match control {
        Some(control) => state.with_provider_control(control),
        None => state,
    }
}

async fn send(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

#[tokio::test]
async fn provider_rows_show_kind_base_url_key_source_and_model_sources() {
    let app = router(app_state(Some(Arc::new(FakeControl::default()))).await);
    let (status, body) = send(&app, Method::GET, "/v1/providers", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["providers"].as_array().unwrap();
    let acme = rows.iter().find(|row| row["id"] == "acme").unwrap();
    assert_eq!(acme["kind"], "openai");
    assert_eq!(acme["baseUrl"], "https://acme.example/v1");
    assert_eq!(acme["keySource"], "saved");
    assert_eq!(acme["auth"], "AUTH_STATUS_CREDENTIALED");
    assert_eq!(acme["modelCount"], 3);
    let empty = rows.iter().find(|row| row["id"] == "empty").unwrap();
    assert_eq!(empty["keySource"], "none");
    assert_eq!(empty["auth"], "AUTH_STATUS_UNAUTHENTICATED");

    let (status, detail) = send(&app, Method::GET, "/v1/providers/acme", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["summary"]["auth"], "AUTH_STATUS_CREDENTIALED");
    let models = detail["models"].as_array().unwrap();
    let vendor = models
        .iter()
        .find(|model| model["modelId"] == "vendor/m-1")
        .unwrap();
    assert_eq!(vendor["source"], "override");
    assert_eq!(vendor["displayName"], "Vendor M1");
    assert_eq!(vendor["contextLimit"], "64000");
    assert!(models.iter().any(|model| model["source"] == "config"));
    assert!(models.iter().any(|model| model["source"] == "remote"));

    // A configured provider with zero models is still a provider.
    let (status, empty) = send(&app, Method::GET, "/v1/providers/empty", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["summary"]["kind"], "anthropic");
    assert!(
        empty
            .get("models")
            .is_none_or(|models| models == &json!([]))
    );

    let (status, missing) = send(&app, Method::GET, "/v1/providers/nope", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
}

#[tokio::test]
async fn provider_mutations_forward_to_the_control_and_emit_catalog_updated() {
    let control = Arc::new(FakeControl::default());
    let state = app_state(Some(Arc::clone(&control))).await;
    let mut updates = state.subscribe_catalog_updates();
    let app = router(state);

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/acme",
        json!({"kind": " openai ", "baseUrl": "https://acme.example/v1", "apiKey": " sk-1 "}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["summary"]["id"], "acme");
    // protojson omits default values: `ok: false` is an absent field.
    assert!(body["discovery"].get("ok").is_none());
    assert_eq!(body["discovery"]["result"], "unavailable");
    assert_eq!(
        body["discovery"]["errorMessage"],
        "provider catalog transport failed"
    );
    assert_eq!(
        updates.try_recv().unwrap()["type"],
        json!("catalog.updated")
    );

    let (status, body) = send(
        &app,
        Method::POST,
        "/v1/providers/acme/refresh",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["discovery"]["modelCount"], 2);
    assert_eq!(body["provider"]["models"].as_array().unwrap().len(), 3);

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/acme/models",
        json!({"modelId": "vendor/m-1", "displayName": "M1", "contextLimit": 1000, "outputLimit": 100, "reasoning": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("discovery").is_none());
    // Patch semantics: an empty name and zero limits reach the control as
    // explicit clears; absent fields stay `None` (keep).
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/acme/models",
        json!({"modelId": "vendor/m-1", "displayName": " ", "outputLimit": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = send(
        &app,
        Method::DELETE,
        "/v1/providers/acme/models?modelId=vendor%2Fm-1%3Afree",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/auth/acme",
        json!({"apiKey": "sk-2"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "AUTH_STATUS_CREDENTIALED");
    assert_eq!(body["provider"]["summary"]["keySource"], "saved");
    assert_eq!(body["discovery"]["modelCount"], 2);

    let (status, body) = send(&app, Method::DELETE, "/v1/auth/acme", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["summary"]["id"], "acme");

    let (status, listed) = send(&app, Method::GET, "/v1/auth", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["providerIds"], json!(["acme"]));

    assert_eq!(
        control.calls(),
        vec![
            Call::Upsert(ProviderUpsert {
                id: "acme".to_string(),
                kind: "openai".to_string(),
                base_url: "https://acme.example/v1".to_string(),
                api_key: Some("sk-1".to_string()),
            }),
            Call::Refresh("acme".to_string()),
            Call::SetModel(
                "acme".to_string(),
                "vendor/m-1".to_string(),
                ProviderModelOverride {
                    display_name: Some("M1".to_string()),
                    context_limit: Some(1000),
                    output_limit: Some(100),
                    reasoning: Some(false),
                },
            ),
            Call::SetModel(
                "acme".to_string(),
                "vendor/m-1".to_string(),
                ProviderModelOverride {
                    display_name: Some(String::new()),
                    context_limit: None,
                    output_limit: Some(0),
                    reasoning: None,
                },
            ),
            Call::RemoveModel("acme".to_string(), "vendor/m-1:free".to_string()),
            Call::SetKey("acme".to_string(), "sk-2".to_string()),
            Call::RemoveKey("acme".to_string()),
        ]
    );
}

#[tokio::test]
async fn provider_routes_reject_bad_input_and_map_control_errors() {
    let control = Arc::new(FakeControl::default());
    let app = router(app_state(Some(Arc::clone(&control))).await);
    for (method, uri, body) in [
        (
            Method::PUT,
            "/v1/providers/bad.id",
            json!({"kind": "openai", "baseUrl": "https://x"}),
        ),
        (
            Method::PUT,
            "/v1/providers/hya",
            json!({"kind": "openai", "baseUrl": "https://x"}),
        ),
        (
            Method::PUT,
            "/v1/providers/acme/models",
            json!({"modelId": "  "}),
        ),
        (
            Method::PUT,
            "/v1/providers/acme/models",
            json!({"modelId": "m", "contextLimit": 10, "outputLimit": 20}),
        ),
        (Method::PUT, "/v1/auth/bad..id", json!({"apiKey": "x"})),
        (Method::PUT, "/v1/auth/acme", json!({"apiKey": "  "})),
    ] {
        let (status, body) = send(&app, method.clone(), uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {body}");
        assert_eq!(body["error"]["code"], "invalid_argument");
    }
    assert!(control.calls().is_empty(), "{:?}", control.calls());

    let (status, body) = send(
        &app,
        Method::POST,
        "/v1/providers/missing/refresh",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "not_found");

    // Without an installed control, key and provider writes are unavailable.
    let bare = router(app_state(None).await);
    for (method, uri, body) in [
        (Method::PUT, "/v1/auth/acme", json!({"apiKey": "k"})),
        (Method::DELETE, "/v1/auth/acme", Value::Null),
        (Method::GET, "/v1/auth", Value::Null),
        (
            Method::PUT,
            "/v1/providers/acme",
            json!({"kind": "openai", "baseUrl": "https://x"}),
        ),
    ] {
        let (status, body) = send(&bare, method.clone(), uri, body).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {uri}: {body}"
        );
    }
    // Reads still work from the live catalog alone.
    let (status, body) = send(&bare, Method::GET, "/v1/providers/acme", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["summary"]["kind"], "openai");
    assert_eq!(body["summary"]["keySource"], "none");
}

#[tokio::test]
async fn model_test_probe_reports_a_length_finish_as_ok_and_unknown_models_as_not_found() {
    let app = router(app_state(Some(Arc::new(FakeControl::default()))).await);
    let (status, body) = send(
        &app,
        Method::POST,
        "/v1/providers/acme/test",
        json!({"modelId": "local"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["text"], "h");
    assert_eq!(body["finishReason"], "length");
    assert!(body.get("latencyMs").is_none_or(Value::is_number));

    let (status, body) = send(
        &app,
        Method::POST,
        "/v1/providers/acme/test",
        json!({"modelId": "absent"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn provider_rpcs_match_between_http_and_grpc() {
    use tonic::transport::Server;
    let state = app_state(Some(Arc::new(FakeControl::default()))).await;
    let app = router(state.clone());
    let grpc = V1Grpc::new(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = Server::builder()
        .add_service(pb::catalog_server::CatalogServer::new(grpc.clone()))
        .add_service(pb::auth_server::AuthServer::new(grpc))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut catalog = pb::catalog_client::CatalogClient::new(channel.clone());
    let mut auth = pb::auth_client::AuthClient::new(channel);

    let grpc_provider = serde_json::to_value(
        catalog
            .get_provider(tonic::Request::new(pb::GetProviderRequest {
                provider_id: "acme".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (_, http_provider) = send(&app, Method::GET, "/v1/providers/acme", Value::Null).await;
    assert_eq!(grpc_provider, http_provider);

    let grpc_upsert = serde_json::to_value(
        catalog
            .upsert_provider(tonic::Request::new(pb::UpsertProviderRequest {
                provider_id: "acme".into(),
                kind: "openai".into(),
                base_url: "https://acme.example/v1".into(),
                api_key: Some("sk".into()),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (_, http_upsert) = send(
        &app,
        Method::PUT,
        "/v1/providers/acme",
        json!({"kind": "openai", "baseUrl": "https://acme.example/v1", "apiKey": "sk"}),
    )
    .await;
    assert_eq!(grpc_upsert, http_upsert);

    let grpc_refresh = serde_json::to_value(
        catalog
            .refresh_provider(tonic::Request::new(pb::RefreshProviderRequest {
                provider_id: "acme".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (_, http_refresh) = send(
        &app,
        Method::POST,
        "/v1/providers/acme/refresh",
        Value::Null,
    )
    .await;
    assert_eq!(grpc_refresh, http_refresh);

    let grpc_set = serde_json::to_value(
        catalog
            .set_provider_model(tonic::Request::new(pb::SetProviderModelRequest {
                provider_id: "acme".into(),
                model_id: "vendor/m-1".into(),
                display_name: Some("M1".into()),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (_, http_set) = send(
        &app,
        Method::PUT,
        "/v1/providers/acme/models",
        json!({"modelId": "vendor/m-1", "displayName": "M1"}),
    )
    .await;
    assert_eq!(grpc_set, http_set);

    let grpc_remove = serde_json::to_value(
        catalog
            .remove_provider_model(tonic::Request::new(pb::RemoveProviderModelRequest {
                provider_id: "acme".into(),
                model_id: "vendor/m-1".into(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner(),
    )
    .unwrap();
    let (_, http_remove) = send(
        &app,
        Method::DELETE,
        "/v1/providers/acme/models?modelId=vendor%2Fm-1",
        Value::Null,
    )
    .await;
    assert_eq!(grpc_remove, http_remove);

    let grpc_missing = catalog
        .refresh_provider(tonic::Request::new(pb::RefreshProviderRequest {
            provider_id: "missing".into(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(grpc_missing.code(), tonic::Code::NotFound);

    let grpc_test = catalog
        .test_provider_model(tonic::Request::new(pb::TestProviderModelRequest {
            provider_id: "acme".into(),
            model_id: "absent".into(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(grpc_test.code(), tonic::Code::NotFound);

    let grpc_key = serde_json::to_value(
        auth.set_provider_auth(tonic::Request::new(pb::SetProviderAuthRequest {
            provider_id: "acme".into(),
            secret: Some(pb::set_provider_auth_request::Secret::ApiKey("sk".into())),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner(),
    )
    .unwrap();
    let (_, http_key) = send(&app, Method::PUT, "/v1/auth/acme", json!({"apiKey": "sk"})).await;
    assert_eq!(grpc_key, http_key);
}
