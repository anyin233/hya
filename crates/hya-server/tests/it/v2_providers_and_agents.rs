//! Providers & auth control routes, model catalog metadata (limits & reasoning
//! variants), `AgentModels` preferences/config/scope, reasoning-effort wire
//! reporting, per-message attribution, team-member turn/mutation guards, and
//! mail steering authorization over HTTP & gRPC.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use futures::FutureExt as _;
use hya_api::v2 as pb;
use hya_engine::{
    AgentModelConfiguration, AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine,
    TurnBinding,
};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId, SubagentMode,
    TokenUsage,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, ModelCatalogSource,
    Provider, ProviderAuthState, ProviderCatalogResult, ProviderCatalogSnapshot,
    ProviderCatalogSource, ProviderCatalogState, ProviderError, ProviderKind, ProviderModel,
    ProviderRouter, ReasoningEffort,
};
use hya_server::{
    AGENT_MODEL_CONFIGURED, AGENT_MODEL_CONTROL_FAILURE, AGENT_MODEL_UNAVAILABLE,
    AGENT_MODEL_UNKNOWN_AGENT, AgentModelControl, AgentModelControlError, AgentModelControlFuture,
    AgentModelEffective, AgentModelIdentity, AgentModelSource, AgentModelState, AppState,
    PROVIDER_NOT_FOUND, ProviderChange, ProviderControl, ProviderControlError,
    ProviderControlFuture, ProviderDiscoveryReport, ProviderKeySource, ProviderModelOverride,
    ProviderSettings, ProviderUpsert, V2Grpc, router,
};
use hya_store::{OwnerRunId, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules};
use serde_json::{Value, json};

use crate::harness::{
    assert_err, collect_sse, create_session, create_session_id, default_caps, engine_on, get_json,
    grpc_channel, grpc_code, grpc_json, id_str, ok_del, ok_get, ok_patch, ok_post, ok_put, patch,
    post, put, run_prompt, send, state_of, state_with_provider,
};
use crate::support;

// ---- Providers & Models API ----

#[derive(Debug, Clone, PartialEq)]
enum ProviderCall {
    SetKey(String, String),
    RemoveKey(String),
    Upsert(ProviderUpsert),
    Refresh(String),
    SetModel(String, String, ProviderModelOverride),
    RemoveModel(String, String),
}

#[derive(Default)]
struct FakeProviderControl(Mutex<Vec<ProviderCall>>);

fn discovered(count: usize) -> ProviderChange {
    ProviderChange {
        configured: true,
        discovery: Some(ProviderDiscoveryReport {
            ok: true,
            result: "models".into(),
            error_message: None,
            model_count: count,
        }),
    }
}

impl FakeProviderControl {
    fn record(&self, call: ProviderCall) {
        self.0.lock().unwrap().push(call);
    }
    fn calls(&self) -> Vec<ProviderCall> {
        self.0.lock().unwrap().clone()
    }
}

impl ProviderControl for FakeProviderControl {
    fn available(&self) -> bool {
        true
    }
    fn list_saved_keys(&self) -> ProviderControlFuture<'_, Vec<String>> {
        Box::pin(async { Ok(vec!["acme".into()]) })
    }
    fn list_settings(&self) -> ProviderControlFuture<'_, Vec<ProviderSettings>> {
        Box::pin(async {
            Ok(vec![
                ProviderSettings {
                    id: "acme".into(),
                    kind: "openai".into(),
                    base_url: "https://acme.example/v2".into(),
                    key_source: ProviderKeySource::Saved,
                },
                ProviderSettings {
                    id: "empty".into(),
                    kind: "anthropic".into(),
                    base_url: "https://empty.example/v2".into(),
                    key_source: ProviderKeySource::None,
                },
            ])
        })
    }
    fn set_key(&self, id: String, key: String) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(ProviderCall::SetKey(id, key));
        Box::pin(async { Ok(discovered(2)) })
    }
    fn remove_key(&self, id: String) -> ProviderControlFuture<'_, (bool, ProviderChange)> {
        self.record(ProviderCall::RemoveKey(id));
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
    fn upsert_provider(&self, req: ProviderUpsert) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(ProviderCall::Upsert(req));
        Box::pin(async {
            Ok(ProviderChange {
                configured: true,
                discovery: Some(ProviderDiscoveryReport {
                    ok: false,
                    result: "unavailable".into(),
                    error_message: Some("transport failed".into()),
                    model_count: 0,
                }),
            })
        })
    }
    fn refresh_provider(&self, id: String) -> ProviderControlFuture<'_, ProviderChange> {
        let missing = id == "missing";
        self.record(ProviderCall::Refresh(id));
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
        id: String,
        model: String,
        meta: ProviderModelOverride,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(ProviderCall::SetModel(id, model, meta));
        Box::pin(async {
            Ok(ProviderChange {
                configured: true,
                discovery: None,
            })
        })
    }
    fn remove_model(&self, id: String, model: String) -> ProviderControlFuture<'_, ProviderChange> {
        self.record(ProviderCall::RemoveModel(id, model));
        Box::pin(async {
            Ok(ProviderChange {
                configured: true,
                discovery: None,
            })
        })
    }
}

fn catalog_row(
    model: &str,
    source: ModelCatalogSource,
    display: Option<&str>,
    context: u32,
    output: u32,
) -> ProviderModel {
    ProviderModel {
        provider_id: "acme".into(),
        model_id: model.into(),
        capabilities: Capabilities {
            max_context: context,
            max_output: output,
            ..Capabilities::default()
        },
        reasoning_variants: vec!["low".into()],
        reasoning_default: Some(ReasoningEffort::Low),
        reasoning: Some(true),
        display_name: display.map(str::to_string),
        source,
    }
}

async fn provider_state(control: Option<Arc<FakeProviderControl>>) -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("h".into()),
        FakeStep::Finish(FinishReason::Length),
    ]]);
    let (engine, _) = engine_on(
        SessionStore::connect_memory().await.unwrap(),
        provider,
        PermissionRules::default(),
    );
    engine.publish_provider_catalog(
        engine.provider_router(),
        Arc::new(ProviderCatalogSnapshot::build(
            vec![
                catalog_row(
                    "vendor/m-1",
                    ModelCatalogSource::Overridden,
                    Some("Vendor M1"),
                    64_000,
                    4_096,
                ),
                catalog_row(
                    "local",
                    ModelCatalogSource::Configured,
                    None,
                    400_000,
                    128_000,
                ),
                catalog_row(
                    "remote-only",
                    ModelCatalogSource::Discovered,
                    None,
                    32_000,
                    0,
                ),
            ],
            vec![
                ProviderCatalogState {
                    provider_id: "acme".into(),
                    kind: ProviderKind::OpenAiCompatible,
                    source: ProviderCatalogSource::Discovered,
                    auth: ProviderAuthState::Unauthenticated,
                    result: ProviderCatalogResult::Models,
                },
                ProviderCatalogState {
                    provider_id: "empty".into(),
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
            system_prompt: "x".into(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    );
    match control {
        Some(c) => state.with_provider_control(c),
        None => state,
    }
}

#[tokio::test]
async fn providers_models_mutations_validation_and_test_probe() {
    let control = Arc::new(FakeProviderControl::default());
    let state = provider_state(Some(Arc::clone(&control))).await;
    let mut updates = state.subscribe_catalog_updates();
    let app = router(state.clone());

    // Provider list/detail + model limits & reasoning variants.
    let list = ok_get(&app, "/v2/providers").await;
    let acme = list["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "acme")
        .unwrap();
    assert_eq!(
        (
            &acme["kind"],
            &acme["keySource"],
            &acme["auth"],
            &acme["modelCount"]
        ),
        (
            &json!("openai"),
            &json!("saved"),
            &json!("AUTH_STATUS_CREDENTIALED"),
            &json!(3)
        )
    );
    let detail = ok_get(&app, "/v2/providers/acme").await;
    let vendor = detail["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["modelId"] == "vendor/m-1")
        .unwrap();
    assert_eq!(
        (
            &vendor["source"],
            &vendor["displayName"],
            &vendor["contextLimit"]
        ),
        (&json!("override"), &json!("Vendor M1"), &json!("64000"))
    );
    let _ = ok_get(&app, "/v2/providers/empty").await;
    assert_eq!(
        get_json(&app, "/v2/providers/nope").await.0,
        StatusCode::NOT_FOUND
    );

    let rows = ok_get(&app, "/v2/models").await["models"]
        .as_array()
        .cloned()
        .unwrap();
    let m_vendor = rows.iter().find(|m| m["modelId"] == "vendor/m-1").unwrap();
    assert_eq!(
        (
            &m_vendor["reasoningVariants"],
            &m_vendor["reasoningDefault"],
            &m_vendor["reasoning"]
        ),
        (&json!(["low"]), &json!("low"), &json!(true))
    );
    let m_big = rows.iter().find(|m| m["id"] == "acme/local").unwrap();
    assert_eq!(
        (&m_big["contextLimit"], &m_big["outputLimit"]),
        (&json!("400000"), &json!("128000"))
    );
    assert!(
        rows.iter()
            .find(|m| m["id"] == "acme/remote-only")
            .unwrap()
            .get("outputLimit")
            .is_none()
    );

    // Mutations forward to ProviderControl & emit catalog.updated.
    assert_eq!(
        ok_put(
            &app,
            "/v2/providers/acme",
            json!({"kind": " openai ", "baseUrl": "https://acme.example/v2", "apiKey": " sk-1 "})
        )
        .await["discovery"]["result"],
        "unavailable"
    );
    assert_eq!(
        updates.try_recv().unwrap()["type"],
        json!("catalog.updated")
    );
    assert_eq!(
        ok_post(&app, "/v2/providers/acme/refresh", Value::Null).await["discovery"]["modelCount"],
        2
    );
    let _ = ok_put(&app, "/v2/providers/acme/models", json!({"modelId": "vendor/m-1", "displayName": "M1", "contextLimit": 1000, "outputLimit": 100, "reasoning": false})).await;
    let _ = ok_put(
        &app,
        "/v2/providers/acme/models",
        json!({"modelId": "vendor/m-1", "displayName": " ", "outputLimit": 0}),
    )
    .await;
    let _ = ok_del(
        &app,
        "/v2/providers/acme/models?modelId=vendor%2Fm-1%3Afree",
    )
    .await;
    assert_eq!(
        ok_put(&app, "/v2/auth/acme", json!({"apiKey": "sk-2"})).await["status"],
        "AUTH_STATUS_CREDENTIALED"
    );
    let _ = ok_del(&app, "/v2/auth/acme").await;
    assert_eq!(
        ok_get(&app, "/v2/auth").await["providerIds"],
        json!(["acme"])
    );
    assert_eq!(control.calls().len(), 7);
    assert_eq!(
        control.calls()[4],
        ProviderCall::RemoveModel("acme".into(), "vendor/m-1:free".into())
    );

    // Model test probe: length finish is ok, unknown model is 404.
    let probe = ok_post(&app, "/v2/providers/acme/test", json!({"modelId": "local"})).await;
    assert_eq!(
        (&probe["ok"], &probe["text"], &probe["finishReason"]),
        (&json!(true), &json!("h"), &json!("length"))
    );
    assert_eq!(
        post(
            &app,
            "/v2/providers/acme/test",
            json!({"modelId": "absent"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // Invalid inputs -> 400 invalid_argument; missing provider -> 404; bare server -> 503.
    let before = control.calls().len();
    for (method, uri, body) in [
        (
            Method::PUT,
            "/v2/providers/bad.id",
            json!({"kind": "openai", "baseUrl": "https://x"}),
        ),
        (
            Method::PUT,
            "/v2/providers/hya",
            json!({"kind": "openai", "baseUrl": "https://x"}),
        ),
        (
            Method::PUT,
            "/v2/providers/acme/models",
            json!({"modelId": "  "}),
        ),
        (
            Method::PUT,
            "/v2/providers/acme/models",
            json!({"modelId": "m", "contextLimit": 10, "outputLimit": 20}),
        ),
        (
            Method::PUT,
            "/v2/providers/acme/models",
            json!({"modelId": "m", "contextLimit": -1}),
        ),
        (
            Method::PUT,
            "/v2/providers/acme/models",
            json!({"modelId": "m", "outputLimit": 5_000_000_000_u64}),
        ),
        (
            Method::PUT,
            "/v2/providers/acme/models",
            json!({"modelId": "m", "contextLimit": "lots"}),
        ),
        (Method::POST, "/v2/sessions", json!({"title": 7})),
        (Method::PUT, "/v2/auth/bad..id", json!({"apiKey": "x"})),
        (Method::PUT, "/v2/auth/acme", json!({"apiKey": "  "})),
    ] {
        assert_err(
            send(&app, method, uri, body).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }
    assert_eq!(control.calls().len(), before);
    assert_err(
        post(&app, "/v2/providers/missing/refresh", Value::Null).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    let bare = router(provider_state(None).await);
    for (method, uri, body) in [
        (Method::PUT, "/v2/auth/acme", json!({"apiKey": "k"})),
        (Method::DELETE, "/v2/auth/acme", Value::Null),
        (Method::GET, "/v2/auth", Value::Null),
        (
            Method::PUT,
            "/v2/providers/acme",
            json!({"kind": "openai", "baseUrl": "https://x"}),
        ),
    ] {
        assert_err(
            send(&bare, method, uri, body).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        );
    }
    assert_eq!(
        ok_get(&bare, "/v2/providers/acme").await["summary"]["keySource"],
        "none"
    );
}

#[tokio::test]
async fn provider_rpcs_match_between_http_and_grpc() {
    let state = provider_state(Some(Arc::new(FakeProviderControl::default()))).await;
    let app = router(state.clone());
    let ch = grpc_channel(state).await;
    let mut catalog = pb::catalog_client::CatalogClient::new(ch.clone());
    let mut auth = pb::auth_client::AuthClient::new(ch);

    assert_eq!(
        grpc_json(
            catalog
                .get_provider(pb::GetProviderRequest {
                    provider_id: "acme".into(),
                    ..Default::default()
                })
                .await
        ),
        ok_get(&app, "/v2/providers/acme").await
    );
    assert_eq!(
        grpc_json(
            catalog
                .upsert_provider(pb::UpsertProviderRequest {
                    provider_id: "acme".into(),
                    kind: "openai".into(),
                    base_url: "https://acme.example/v2".into(),
                    api_key: Some("sk".into()),
                    ..Default::default()
                })
                .await
        ),
        ok_put(
            &app,
            "/v2/providers/acme",
            json!({"kind": "openai", "baseUrl": "https://acme.example/v2", "apiKey": "sk"})
        )
        .await
    );
    assert_eq!(
        grpc_json(
            catalog
                .refresh_provider(pb::RefreshProviderRequest {
                    provider_id: "acme".into(),
                    ..Default::default()
                })
                .await
        ),
        ok_post(&app, "/v2/providers/acme/refresh", Value::Null).await
    );
    assert_eq!(
        grpc_json(
            catalog
                .set_provider_model(pb::SetProviderModelRequest {
                    provider_id: "acme".into(),
                    model_id: "vendor/m-1".into(),
                    display_name: Some("M1".into()),
                    ..Default::default()
                })
                .await
        ),
        ok_put(
            &app,
            "/v2/providers/acme/models",
            json!({"modelId": "vendor/m-1", "displayName": "M1"})
        )
        .await
    );
    assert_eq!(
        grpc_json(
            catalog
                .remove_provider_model(pb::RemoveProviderModelRequest {
                    provider_id: "acme".into(),
                    model_id: "vendor/m-1".into(),
                    ..Default::default()
                })
                .await
        ),
        ok_del(&app, "/v2/providers/acme/models?modelId=vendor%2Fm-1").await
    );
    assert_eq!(
        grpc_json(
            auth.set_provider_auth(pb::SetProviderAuthRequest {
                provider_id: "acme".into(),
                secret: Some(pb::set_provider_auth_request::Secret::ApiKey("sk".into())),
                ..Default::default()
            })
            .await
        ),
        ok_put(&app, "/v2/auth/acme", json!({"apiKey": "sk"})).await
    );
    assert_eq!(
        grpc_code(
            catalog
                .refresh_provider(pb::RefreshProviderRequest {
                    provider_id: "missing".into(),
                    ..Default::default()
                })
                .await
        ),
        tonic::Code::NotFound
    );
    assert_eq!(
        grpc_code(
            catalog
                .test_provider_model(pb::TestProviderModelRequest {
                    provider_id: "acme".into(),
                    model_id: "absent".into(),
                    ..Default::default()
                })
                .await
        ),
        tonic::Code::NotFound
    );
}

// ---- AgentModels API ----

type SavedConfigs = Arc<Mutex<Vec<(String, Option<AgentModelIdentity>)>>>;

#[derive(Clone, Default)]
struct FakeAgentModelControl(SavedConfigs);

fn agent_model_row(preference: Option<AgentModelIdentity>) -> AgentModelState {
    let available = preference.is_some();
    let effective = preference
        .clone()
        .unwrap_or_else(|| AgentModelIdentity::new("hya", "offline"));
    AgentModelState {
        agent_id: "general".into(),
        description: Some("General agent".into()),
        mode: "subagent".into(),
        hidden: false,
        configured: false,
        settable: true,
        preference,
        preference_available: available,
        effective: AgentModelEffective {
            model: effective,
            source: if available {
                AgentModelSource::Remembered
            } else {
                AgentModelSource::Default
            },
        },
        configuration: None,
        configuration_path: None,
        session_override: None,
    }
}

impl AgentModelControl for FakeAgentModelControl {
    fn available(&self) -> bool {
        true
    }
    fn configuration_available(&self) -> bool {
        false
    }
    fn list(
        &self,
        _: TurnBinding,
        _: ModelRef,
    ) -> AgentModelControlFuture<'_, Vec<AgentModelState>> {
        async move {
            Ok(vec![agent_model_row(Some(AgentModelIdentity::new(
                "hya", "offline",
            )))])
        }
        .boxed()
    }
    fn set(
        &self,
        _: TurnBinding,
        agent_id: String,
        pref: Option<AgentModelIdentity>,
        _: ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        async move {
            match agent_id.as_str() {
                "missing" => Err(AgentModelControlError::new(
                    AGENT_MODEL_UNKNOWN_AGENT,
                    "unknown",
                )),
                "configured" => Err(AgentModelControlError::new(
                    AGENT_MODEL_CONFIGURED,
                    "configured",
                )),
                "store-failure" => Err(AgentModelControlError::new(
                    AGENT_MODEL_CONTROL_FAILURE,
                    "failed",
                )),
                _ if pref.as_ref().is_some_and(|m| m.provider_id == "missing") => Err(
                    AgentModelControlError::new(AGENT_MODEL_UNAVAILABLE, "unavailable"),
                ),
                _ => Ok(agent_model_row(pref)),
            }
        }
        .boxed()
    }
    fn save_configuration(
        &self,
        _: TurnBinding,
        agent_id: String,
        model: Option<AgentModelIdentity>,
        _: ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        async move {
            self.0.lock().unwrap().push((agent_id, model.clone()));
            let mut st = agent_model_row(model);
            st.configuration_path = Some("/tmp/config.yaml".into());
            st.configuration = st.preference.clone();
            Ok(st)
        }
        .boxed()
    }
    fn list_model_effort_preferences(
        &self,
    ) -> AgentModelControlFuture<'_, Vec<hya_store::ModelEffortPreference>> {
        async move { Err(AgentModelControlError::unavailable()) }.boxed()
    }
    fn set_model_effort_preference(
        &self,
        _: String,
        _: String,
        _: String,
    ) -> AgentModelControlFuture<'_, ()> {
        async move { Err(AgentModelControlError::unavailable()) }.boxed()
    }
}

#[tokio::test]
async fn agent_models_crud_config_body_scope_and_error_codes() {
    let fake = FakeAgentModelControl::default();
    let saved = Arc::clone(&fake.0);
    let app = router(
        state_with_provider(FakeProvider::scripted(vec![]))
            .await
            .with_agent_model_control(Arc::new(fake)),
    );

    let list = ok_get(&app, "/v2/agent-models").await;
    assert_eq!(
        (&list["agents"][0]["agentId"], &list["agents"][0]["source"]),
        (&json!("general"), &json!("AGENT_MODEL_SOURCE_REMEMBERED"))
    );
    assert_eq!(
        ok_put(
            &app,
            "/v2/agent-models/general",
            json!({"preference": {"providerId": "fake", "modelId": "model"}})
        )
        .await["preference"]["modelId"],
        json!("model")
    );
    let cleared = ok_put(
        &app,
        "/v2/agent-models/general",
        json!({"preference": null}),
    )
    .await;
    assert_eq!(cleared["source"], json!("AGENT_MODEL_SOURCE_DEFAULT"));
    assert!(cleared.get("preference").is_none());

    let cfg = ok_put(
        &app,
        "/v2/agent-models/general/configuration",
        json!({"model": {"providerId": "fake", "modelId": "saved"}}),
    )
    .await;
    assert_eq!(
        (&cfg["configurationPath"], &cfg["configuration"]["modelId"]),
        (&json!("/tmp/config.yaml"), &json!("saved"))
    );
    assert!(
        ok_put(&app, "/v2/agent-models/general/configuration", json!({}))
            .await
            .get("configuration")
            .is_none()
    );
    assert_eq!(
        saved.lock().unwrap().as_slice(),
        &[
            (
                "general".into(),
                Some(AgentModelIdentity::new("fake", "saved"))
            ),
            ("general".into(), None)
        ]
    );

    // Scope read from PUT body, not query string.
    let pref = json!({"providerId": "fake", "modelId": "model"});
    assert_err(
        put(
            &app,
            "/v2/agent-models/general",
            json!({"directory": "relative/dir", "preference": pref}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    assert_err(
        put(
            &app,
            "/v2/agent-models/general",
            json!({"session": "not-a-session", "preference": pref}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    let _ = ok_put(
        &app,
        "/v2/agent-models/general?directory=relative",
        json!({"preference": pref}),
    )
    .await;

    let dummy = json!({"preference": {"providerId": "hya", "modelId": "offline"}});
    for (path, body, status, code) in [
        (
            "/v2/agent-models/missing",
            dummy.clone(),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            "/v2/agent-models/configured",
            dummy.clone(),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            "/v2/agent-models/store-failure",
            dummy,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
        (
            "/v2/agent-models/general",
            json!({"preference": {"providerId": "missing", "modelId": "model"}}),
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/v2/agent-models/general",
            json!({"preference": {"providerId": "", "modelId": ""}}),
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        ),
    ] {
        assert_err(put(&app, path, body).await, status, code);
    }
    let bare = router(state_with_provider(FakeProvider::scripted(vec![])).await);
    assert_err(
        get_json(&bare, "/v2/agent-models").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
    );
}

// ---- Reasoning Effort Wire Contract ----

struct EffortProvider(Arc<Mutex<Vec<CompletionRequest>>>);

#[async_trait]
impl Provider for EffortProvider {
    fn id(&self) -> &str {
        "effort"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            ..Capabilities::default()
        })
    }
    fn reasoning_default(&self, model: &ModelRef) -> Option<ReasoningEffort> {
        let base = model
            .as_str()
            .split_once('#')
            .map_or(model.as_str(), |(b, _)| b);
        match base.strip_prefix("effort/").unwrap_or(base) {
            "third" => Some(ReasoningEffort::Low),
            _ => None,
        }
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.0.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

#[tokio::test]
async fn reasoning_effort_variants_and_session_info_effort_sources() {
    let dir = support::tempdir("effort").canonicalize().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new().with(Arc::new(EffortProvider(Arc::clone(&requests))))),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_global_reasoning(Some(ReasoningEffort::High)),
    );
    let app = router(state_of(Arc::clone(&engine), dir.clone()));
    let session = id_str(
        &ok_post(
            &app,
            "/v2/sessions",
            json!({"agent": "build", "model": "effort/third", "workdir": dir.to_string_lossy()}),
        )
        .await["session"]["id"],
    );
    let uri = format!("/v2/sessions/{session}");

    // Model default -> preference -> `#none` suffix -> invalid `#bogus`.
    run_prompt(&app, &session, "go").await;
    let info = ok_get(&app, &uri).await;
    assert_eq!(
        (&info["effectiveEffort"], &info["effortSource"]),
        (&json!("low"), &json!("EFFORT_SOURCE_MODEL_DEFAULT"))
    );

    store
        .upsert_model_effort_preference(owner, "effort", "other", "medium", 1)
        .await
        .unwrap();
    let _ = ok_patch(&app, &uri, json!({"model": "effort/other"})).await;
    run_prompt(&app, &session, "go").await;
    let info = ok_get(&app, &uri).await;
    assert_eq!(
        (&info["effectiveEffort"], &info["effortSource"]),
        (&json!("medium"), &json!("EFFORT_SOURCE_PREFERENCE"))
    );

    let _ = ok_patch(&app, &uri, json!({"model": "effort/other#none"})).await;
    run_prompt(&app, &session, "go").await;
    let info = ok_get(&app, &uri).await;
    assert_eq!(
        (&info["effectiveEffort"], &info["effortSource"]),
        (&json!("none"), &json!("EFFORT_SOURCE_SUFFIX"))
    );

    let _ = ok_patch(&app, &uri, json!({"model": "effort/fake#bogus"})).await;
    run_prompt(&app, &session, "go").await;
    assert_eq!(requests.lock().unwrap().last().unwrap().reasoning, None);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Message Attribution ----

struct EchoAttributionProvider;
#[async_trait]
impl Provider for EchoAttributionProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(default_caps())
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let part = PartId::new();
        Ok(Box::pin(futures::stream::iter(
            [
                Event::TextStart {
                    session,
                    message,
                    part,
                },
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: format!("answered by {}", req.model),
                },
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: FinishReason::Stop,
                    tokens: Some(TokenUsage {
                        input: 3,
                        output: 2,
                        ..TokenUsage::default()
                    }),
                    cause: None,
                },
            ]
            .into_iter()
            .map(Ok),
        )))
    }
}

#[tokio::test]
async fn message_attribution_reports_per_turn_agent_model_times_and_explicit_override() {
    let runtime = support::test_runtime(Arc::new(hya_tool_loader::builtins()));
    runtime.publish_agent_model_configuration(AgentModelConfiguration {
        bundles: [(
            "hya/server-tests-build".into(),
            [("build".into(), ModelRef::new("fake/pinned"))].into(),
        )]
        .into(),
        ..AgentModelConfiguration::default()
    });
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(EchoAttributionProvider))),
        runtime,
        perm,
        EventBus::default(),
    ));
    let app = router(AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake/alpha"),
            system_prompt: "x".into(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    ));

    let session = id_str(
        &ok_post(
            &app,
            "/v2/sessions",
            json!({"agent": "build", "workdir": std::env::temp_dir().to_string_lossy()}),
        )
        .await["session"]["id"],
    );
    run_prompt(&app, &session, "first").await;

    let _ = ok_patch(
        &app,
        &format!("/v2/sessions/{session}"),
        json!({"model": "fake/beta", "agent": "plan"}),
    )
    .await;
    let collector = collect_sse(
        &app,
        &format!("/v2/sessions/{session}/events/stream"),
        |f| f["event"]["messageStarted"]["role"] == json!("ROLE_ASSISTANT"),
    )
    .await;
    run_prompt(&app, &session, "second").await;

    let listed = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await;
    let assistants: Vec<&Value> = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == json!("ROLE_ASSISTANT"))
        .collect();
    assert_eq!(
        assistants
            .iter()
            .map(|m| (&m["agent"], &m["model"]))
            .collect::<Vec<_>>(),
        vec![
            (&json!("build"), &json!("fake/pinned")),
            (&json!("plan"), &json!("fake/beta"))
        ]
    );
    let millis = |ts: &Value| {
        let p: pbjson_types::Timestamp = serde_json::from_value(ts.clone()).unwrap();
        p.seconds * 1000 + i64::from(p.nanos) / 1_000_000
    };
    assert!(
        millis(&assistants[0]["timeCreated"]) > 0
            && millis(&assistants[0]["timeUpdated"]) <= millis(&assistants[1]["timeCreated"])
    );

    let frames = collector.await.unwrap();
    let started = frames
        .iter()
        .find_map(|f| {
            (f["event"]["messageStarted"]["role"] == json!("ROLE_ASSISTANT"))
                .then_some(&f["event"]["messageStarted"])
        })
        .unwrap();
    assert_eq!(
        (&started["message"], &started["agent"], &started["model"]),
        (&assistants[1]["id"], &json!("plan"), &json!("fake/beta"))
    );
}

// ---- Member Turn Guards & Mail Authorization ----

struct WaitingMemberProvider(tokio::sync::Notify);
#[async_trait]
impl Provider for WaitingMemberProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(default_caps())
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        _: SessionId,
        _: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.0.notify_one();
        Ok(Box::pin(futures::stream::pending()))
    }
}

async fn member_fixture() -> (AppState, Arc<ResidentSupervisor>, SessionId, SessionId) {
    let provider = Arc::new(WaitingMemberProvider(tokio::sync::Notify::new()));
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(provider.clone())),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        permission,
        EventBus::default(),
    ));
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    });
    let spec = || CreateSession {
        parent: None,
        agent: agent.name.clone(),
        model: agent.model.clone(),
        workdir: agent.workdir.to_string_lossy().into_owned(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    };
    let root = engine.create(spec()).await.unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            ..spec()
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("build").unwrap();
    supervisor
        .ensure_main(
            root,
            (*agent).clone(),
            (binding, Arc::from([]), resources),
            None,
            None,
        )
        .await
        .unwrap();
    supervisor
        .register_existing_resident(
            root,
            child,
            "worker".into(),
            (*agent).clone(),
            Some("work".into()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), provider.0.notified())
        .await
        .unwrap();
    assert!(engine.cancel_user_turn(child));
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.turn_active(child) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    (AppState::new(engine, agent), supervisor, root, child)
}

#[tokio::test]
async fn member_turns_mutations_and_mail_steering_refused_over_http_and_grpc() {
    use pb::turn_server::Turn as _;

    let (state, supervisor, root, child) = member_fixture().await;
    let app = router(state.clone());
    let grpc = V2Grpc::new(state.clone());
    let turn_bodies = [
        json!({"prompt": {"text": "no"}}),
        json!({"command": {"command": "custom", "text": "no", "model": "other"}}),
        json!({"command": {"command": "workflow", "arguments": "invalid-operation", "model": "other"}}),
        json!({"shell": {"command": "echo no"}}),
    ];
    for archived in [false, true] {
        if archived {
            supervisor.end_member(root, "main/worker").await.unwrap();
        }
        let before = state.engine.replay(child).await.unwrap().len();
        for body in &turn_bodies {
            let (s, resp) = post(&app, &format!("/v2/sessions/{child}/turns"), body.clone()).await;
            assert_eq!(
                (s, &resp["error"]["code"]),
                (StatusCode::BAD_REQUEST, &json!("invalid_argument"))
            );
            assert!(
                resp["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("steer this subagent with mail")
            );
            let mut req: pb::CreateTurnRequest = serde_json::from_value(body.clone()).unwrap();
            req.session = child.to_string();
            let err = grpc
                .create_turn(tonic::Request::new(req))
                .await
                .unwrap_err();
            assert!(
                err.code() == tonic::Code::InvalidArgument
                    && err.message().contains("steer this subagent with mail")
            );
        }
        for p in [
            json!({"model": "other", "title": "no"}),
            json!({"agent": "other", "title": "no"}),
        ] {
            let (s, resp) = patch(&app, &format!("/v2/sessions/{child}"), p).await;
            assert_eq!(
                (s, &resp["error"]["code"]),
                (StatusCode::BAD_REQUEST, &json!("invalid_argument"))
            );
            assert!(
                resp["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("steer this subagent with mail")
            );
        }
        for undo in [false, true] {
            assert_err(
                post(
                    &app,
                    &format!("/v2/sessions/{child}/revert"),
                    json!({"undo": undo}),
                )
                .await,
                StatusCode::BAD_REQUEST,
                "invalid_argument",
            );
        }
        assert_eq!(state.engine.replay(child).await.unwrap().len(), before);
    }

    // Steer mail authorization rejections map to 403 / PermissionDenied on HTTP & gRPC.
    let mail_state = state_with_provider(FakeProvider::scripted(vec![])).await;
    let mail_app = router(mail_state.clone());
    let root2: SessionId = create_session(&mail_app, &std::env::temp_dir())
        .await
        .parse()
        .unwrap();
    let child2: SessionId = create_session_id(
        &mail_app,
        json!({"parent": root2.to_string(), "workdir": std::env::temp_dir().to_string_lossy()}),
    )
    .await
    .parse()
    .unwrap();
    for event in [
        hya_tool::extension::ExtensionEvent::AgentRegistered {
            session: root2,
            agent_session: root2,
            handle: "main".into(),
            parent: None,
            agent_type: AgentName::new("build"),
            mode: SubagentMode::Transient,
        }
        .into_event(),
        hya_tool::extension::ExtensionEvent::AgentRegistered {
            session: root2,
            agent_session: child2,
            handle: "worker-1".into(),
            parent: Some("main".into()),
            agent_type: AgentName::new("build"),
            mode: SubagentMode::Resident,
        }
        .into_event(),
        hya_tool::extension::ExtensionEvent::ChannelCreated {
            session: root2,
            channel: "DM-12345678".into(),
            kind: hya_proto::ChannelKind::Dm,
            unit: None,
            members: vec!["main".into(), "main/worker-1".into()],
        }
        .into_event(),
    ] {
        mail_state
            .engine
            .store()
            .append_event(root2, &event)
            .await
            .unwrap();
    }
    let before = mail_state.engine.replay(root2).await.unwrap().len();
    let (s, body) = post(
        &mail_app,
        &format!("/v2/sessions/{child2}/bundles/hya%2Fmulti-agent/steer"),
        json!({"text": "continue"}),
    )
    .await;
    assert_eq!(
        (s, &body["error"]["code"]),
        (StatusCode::FORBIDDEN, &json!("permission_denied"))
    );
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("main -> DM-12345678"), "unexpected: {msg}");
    let reply = pb::bundle_api_server::BundleApi::invoke_session_bundle_api(
        &V2Grpc::new(mail_state.clone()),
        tonic::Request::new(pb::InvokeSessionBundleApiRequest {
            session: child2.to_string(),
            bundle: "hya/multi-agent".into(),
            method: "POST".into(),
            path: "/steer".into(),
            query: Default::default(),
            body: Some(serde_json::from_value(json!({"text": "continue"})).unwrap()),
        }),
    )
    .await
    .unwrap()
    .into_inner();
    let grpc_body = serde_json::to_value(reply.body.unwrap()).unwrap();
    assert_eq!((reply.status, grpc_body), (403, body));
    assert_eq!(mail_state.engine.replay(root2).await.unwrap().len(), before);
}

#[tokio::test]
async fn member_fork_copies_snapshot_without_mutating_source() {
    let (state, supervisor, root, child) = member_fixture().await;
    let app = router(state.clone());
    for archived in [false, true] {
        if archived {
            supervisor.end_member(root, "main/worker").await.unwrap();
        }
        let before = state.engine.replay(child).await.unwrap().len();
        let fork: SessionId = ok_post(&app, &format!("/v2/sessions/{child}/fork"), json!({})).await
            ["session"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            state
                .engine
                .read_projection(fork)
                .await
                .unwrap()
                .session
                .parent
                .is_none()
        );
        state
            .engine
            .admit_user_prompt(fork, "independent root".into())
            .await
            .unwrap();
        assert_eq!(state.engine.replay(child).await.unwrap().len(), before);
    }
}
