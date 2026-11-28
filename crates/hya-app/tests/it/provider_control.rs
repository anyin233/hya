//! Provider View backend and remote model cache integration tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use http_body_util::BodyExt;
use hya_engine::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::support::{self, EnvVarGuard, env_lock};

#[derive(Default)]
struct Seen {
    model_list_auth: Vec<Option<String>>,
    chat_bodies: Vec<Value>,
    chat_auth: Vec<Option<String>>,
}

#[derive(Clone, Default)]
struct Fake {
    seen: Arc<StdMutex<Seen>>,
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

async fn list_models(State(fake): State<Fake>, headers: HeaderMap) -> axum::response::Response {
    let auth = bearer(&headers);
    fake.seen.lock().unwrap().model_list_auth.push(auth.clone());
    if auth.as_deref() == Some("Bearer bad-key") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(json!({
        "data": [
            {"id": "alpha", "name": "Alpha", "context_length": 64000},
            {"id": "vendor/beta:free"}
        ]
    }))
    .into_response()
}

async fn chat(
    State(fake): State<Fake>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    {
        let mut seen = fake.seen.lock().unwrap();
        seen.chat_auth.push(bearer(&headers));
        seen.chat_bodies.push(body);
    }
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"h\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    ([(header::CONTENT_TYPE, "text/event-stream")], sse).into_response()
}

async fn fake_provider() -> (String, Fake) {
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}/v1"), fake)
}

struct Env {
    root: PathBuf,
    config: PathBuf,
    auth: PathBuf,
    cache_home: PathBuf,
    _guard: EnvVarGuard,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn env(label: &str, config_yaml: Option<&str>) -> Env {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "hya-provider-control-{label}-{}-{nanos}-{seq}",
        std::process::id()
    ));
    let config_home = root.join("config");
    let cache_home = root.join("cache");
    let home = root.join("home");
    std::fs::create_dir_all(config_home.join("hya")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let config = config_home.join("hya/config.yaml");
    if let Some(yaml) = config_yaml {
        std::fs::write(&config, yaml).unwrap();
    }
    let mut guard = EnvVarGuard::new();
    guard.set("XDG_CONFIG_HOME", &config_home);
    guard.set("XDG_CACHE_HOME", &cache_home);
    guard.set("HOME", &home);
    Env {
        auth: config_home.join("hya/auth"),
        root,
        config,
        cache_home,
        _guard: guard,
    }
}

fn full_kinds() -> Arc<hya_provider::ProviderKinds> {
    hya_app::provider_kinds::load(&hya_bundle::BuiltinBundleSelection::full()).unwrap()
}

async fn app(engine_router: hya_provider::ProviderRouter) -> (axum::Router, hya_server::AppState) {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(engine_router),
        support::test_runtime(Arc::new(hya_tool_loader::builtins()), &[]),
        permission,
        EventBus::default(),
    ));
    let state = hya_server::AppState::new(
        Arc::clone(&engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("hya/offline"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
    .with_provider_control(Arc::new(hya_app::ProviderManager::new(
        engine,
        full_kinds(),
    )));
    (hya_server::router(state.clone()), state)
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

fn model<'a>(provider: &'a Value, id: &str) -> Option<&'a Value> {
    provider["models"]
        .as_array()?
        .iter()
        .find(|model| model["modelId"] == id)
}

#[tokio::test]
async fn upsert_fetches_models_saves_keys_and_edits_models_live() {
    let _lock = env_lock().await;
    let env = env("upsert", None);
    let (base_url, fake) = fake_provider().await;
    let (new_url, new_endpoint) = fake_provider().await;
    let (offline, _) = hya_app::offline_router(None);
    let (app, state) = app(offline).await;
    let mut updates = state.subscribe_catalog_updates();

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/gw",
        json!({"kind": "openai", "baseUrl": base_url, "apiKey": "sk-first"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["discovery"]["ok"], true);
    assert_eq!(body["discovery"]["modelCount"], 2);
    let provider = &body["provider"];
    assert_eq!(provider["summary"]["kind"], "openai");
    assert_eq!(provider["summary"]["keySource"], "saved");
    assert_eq!(provider["summary"]["auth"], "AUTH_STATUS_CREDENTIALED");
    let alpha = model(provider, "alpha").expect("alpha fetched");
    assert_eq!(alpha["source"], "remote");
    assert_eq!(alpha["displayName"], "Alpha");
    assert_eq!(alpha["contextLimit"], "64000");
    assert!(model(provider, "vendor/beta:free").is_some());
    assert_eq!(updates.try_recv().unwrap()["type"], "catalog.updated");
    assert_eq!(
        fake.seen.lock().unwrap().model_list_auth,
        vec![Some("Bearer sk-first".to_string())]
    );

    let config = std::fs::read_to_string(&env.config).unwrap();
    assert!(
        config.contains("gw:") && !config.contains("sk-first"),
        "{config}"
    );
    let key_file = env.auth.join("gw.yaml");
    assert!(
        std::fs::read_to_string(&key_file)
            .unwrap()
            .contains("sk-first")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // Model probe runs through the live route with the saved key.
    let (status, probe) = send(
        &app,
        Method::POST,
        "/v2/providers/gw/test",
        json!({"modelId": "alpha"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{probe}");
    assert_eq!(probe["ok"], true);
    assert_eq!(probe["text"], "h");
    assert_eq!(probe["finishReason"], "length");
    {
        let seen = fake.seen.lock().unwrap();
        let req = seen.chat_bodies.last().unwrap();
        assert_eq!(req["model"], "alpha");
        assert_eq!(req["max_tokens"], 1);
        assert!(req.get("tools").is_none());
        assert_eq!(
            seen.chat_auth.last().unwrap().as_deref(),
            Some("Bearer sk-first")
        );
    }

    // Endpoint change without apiKey preserves the saved credential.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/gw",
        json!({"kind": "openai", "baseUrl": new_url}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["summary"]["keySource"], "saved");
    assert_eq!(
        new_endpoint.seen.lock().unwrap().model_list_auth,
        vec![Some("Bearer sk-first".to_string())]
    );

    // Replacing and removing keys applies live without restart.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/auth/gw",
        json!({"apiKey": "sk-second"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.get("discovery").is_none(),
        "cached rows skip discovery"
    );
    assert_eq!(
        send(
            &app,
            Method::POST,
            "/v2/providers/gw/test",
            json!({"modelId": "alpha"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        new_endpoint
            .seen
            .lock()
            .unwrap()
            .chat_auth
            .last()
            .unwrap()
            .as_deref(),
        Some("Bearer sk-second")
    );

    let (status, body) = send(&app, Method::DELETE, "/v2/auth/gw", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["summary"]["keySource"], "none");
    assert_eq!(
        body["provider"]["summary"]["auth"],
        "AUTH_STATUS_UNAUTHENTICATED"
    );
    assert!(!key_file.exists());
    assert_eq!(
        send(
            &app,
            Method::POST,
            "/v2/providers/gw/test",
            json!({"modelId": "alpha"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        new_endpoint.seen.lock().unwrap().chat_auth.last().unwrap(),
        &None
    );

    // Model override, patch, clear, bare entry, delete, and refresh.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/gw/models",
        json!({"modelId": "alpha", "displayName": "Alpha (mine)", "outputLimit": 1024}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alpha = model(&body["provider"], "alpha").unwrap();
    assert_eq!(alpha["source"], "override");
    assert_eq!(alpha["contextLimit"], "64000");
    assert_eq!(alpha["outputLimit"], "1024");

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/gw/models",
        json!({"modelId": "alpha", "contextLimit": 32000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alpha = model(&body["provider"], "alpha").unwrap();
    assert_eq!(alpha["displayName"], "Alpha (mine)");
    assert_eq!(alpha["contextLimit"], "32000");
    assert_eq!(alpha["outputLimit"], "1024");

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/gw/models",
        json!({"modelId": "alpha", "displayName": "", "outputLimit": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        model(&body["provider"], "alpha").unwrap()["displayName"],
        "Alpha"
    );

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/gw/models",
        json!({"modelId": "bare/m"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        model(&body["provider"], "bare/m").unwrap()["source"],
        "config"
    );

    let (status, body) = send(
        &app,
        Method::DELETE,
        "/v2/providers/gw/models?modelId=alpha",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        model(&body["provider"], "alpha").unwrap()["source"],
        "remote"
    );
    assert_eq!(
        send(
            &app,
            Method::DELETE,
            "/v2/providers/gw/models?modelId=alpha",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let (status, body) = send(&app, Method::POST, "/v2/providers/gw/refresh", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["discovery"]["modelCount"], 2);
    let (status, list) = send(&app, Method::GET, "/v2/providers", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !list["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "hya")
    );
    let (status, listed) = send(&app, Method::GET, "/v2/auth", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed
            .get("providerIds")
            .is_none_or(|ids| ids == &json!([]))
    );
}

#[tokio::test]
async fn failed_fetches_do_not_fail_upsert_and_bad_input_is_rejected() {
    let _lock = env_lock().await;
    let env = env(
        "failures",
        Some("schema_version: 2\ndefault_model: hya/offline\nproviders: {}\n"),
    );
    let (base_url, _fake) = fake_provider().await;
    let (offline, _) = hya_app::offline_router(None);
    let (app, _state) = app(offline).await;

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/gw",
        json!({"kind": "anthropic", "baseUrl": "http://127.0.0.1:9/v1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["discovery"].get("ok").is_none());
    assert!(
        body["discovery"]["errorMessage"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v2/providers/rejected",
        json!({"kind": "openai", "baseUrl": base_url, "apiKey": "bad-key"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["discovery"]["result"], "auth_rejected");
    assert_eq!(
        body["provider"]["summary"]["auth"],
        "AUTH_STATUS_AUTH_REJECTED"
    );

    for payload in [
        json!({"kind": "bogus", "baseUrl": "https://x.example"}),
        json!({"kind": "openai", "baseUrl": "ftp://x.example"}),
        json!({"kind": "openai", "baseUrl": "https://user:pw@x.example"}),
        json!({"kind": "openai", "baseUrl": "not a url"}),
    ] {
        let (status, body) = send(&app, Method::PUT, "/v2/providers/other", payload.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}: {body}");
    }
    assert!(
        !std::fs::read_to_string(&env.config)
            .unwrap()
            .contains("other")
    );
    assert_eq!(
        send(
            &app,
            Method::POST,
            "/v2/providers/nope/refresh",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            Method::PUT,
            "/v2/providers/nope/models",
            json!({"modelId": "m"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn deferred_discovery_publishes_one_global_catalog_notice() {
    let _lock = env_lock().await;
    let (base_url, endpoint) = fake_provider().await;
    let _env = env(
        "deferred-notice",
        Some(&format!(
            "schema_version: 2\nproviders:\n  gw:\n    kind: openai\n    base_url: {base_url}\n    models: [configured]\n"
        )),
    );
    let store = SessionStore::connect_memory().await.unwrap();
    let config = hya_app::resolve_runtime(None, hya_app::RuntimeFlags::default())
        .await
        .unwrap();
    let mut runtime = hya_app::ServerEngine::build(store, config)
        .await
        .unwrap()
        .assemble(hya_server::HostPolicy::loopback())
        .unwrap();
    let mut notices = runtime.app_state().subscribe_catalog_updates();
    let router = hya_server::router(runtime.app_state().clone());
    assert!(endpoint.seen.lock().unwrap().model_list_auth.is_empty());

    runtime.spawn_provider_catalog_refresh(|error| panic!("deferred discovery: {error:#}"));
    let notice = tokio::time::timeout(Duration::from_secs(10), notices.recv())
        .await
        .expect("catalog notice deadline")
        .unwrap();
    assert_eq!(notice["type"], "catalog.updated");
    assert_eq!(notice["properties"], json!({}));
    let (status, body) = send(&router, Method::GET, "/v2/providers/gw", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(model(&body, "alpha").is_some(), "{body}");
    assert!(matches!(
        notices.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    runtime.drain(hya_server::ShutdownReason::Stop, || {}).await;
    runtime.finish_shutdown().await.unwrap();
}

async fn seed_cache(provider: &str, rows: Vec<hya_app::model_cache::CachedModel>) {
    let cache = hya_app::model_cache::ModelCache::open_default()
        .await
        .expect("open cache");
    cache.replace_provider(provider, &rows).await.expect("seed");
    cache.close().await;
}

fn cached_row(id: &str) -> hya_app::model_cache::CachedModel {
    hya_app::model_cache::CachedModel {
        id: id.to_string(),
        tools: true,
        fetched_at_ms: 1,
        ..hya_app::model_cache::CachedModel::default()
    }
}

#[tokio::test]
async fn model_cache_restores_metadata_merges_pinned_and_queues_miss() {
    use hya_provider::ModelCatalogSource as S;

    let _lock = env_lock().await;

    // Warm cache hit restores rich metadata without waiting on unreachable discovery.
    let env_hit = env(
        "cache-rich",
        Some(
            "schema_version: 2\ndefault_model: gateway/rich-model\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: []\n",
        ),
    );
    seed_cache(
        "gateway",
        vec![
            hya_app::model_cache::CachedModel {
                display_name: Some("Rich Model".into()),
                context_limit: 128_000,
                output_limit: 16_384,
                reasoning_default: Some("medium".into()),
                reasoning_variants: vec![
                    "none".into(),
                    "low".into(),
                    "medium".into(),
                    "high".into(),
                ],
                ..cached_row("rich-model")
            },
            cached_row("other-model"),
        ],
    )
    .await;
    let started = Instant::now();
    let kinds = full_kinds();
    let loaded = hya_app::config::resolve(hya_app::config::parse().unwrap().unwrap(), &kinds)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    let rich = loaded
        .catalog
        .models()
        .iter()
        .find(|m| m.model_id == "rich-model")
        .unwrap();
    assert_eq!(rich.capabilities.max_context, 128_000);
    assert_eq!(rich.capabilities.max_output, 16_384);
    assert_eq!(rich.display_name.as_deref(), Some("Rich Model"));
    assert_eq!(rich.source, S::Discovered);
    assert_eq!(
        rich.reasoning_variants,
        vec!["none", "low", "medium", "high"]
    );
    assert_eq!(rich.reasoning_default, None);
    assert_eq!(loaded.pending_discovery.len(), 1);
    assert!(env_hit.cache_home.join("hya/model_cache.db").is_file());
    drop(env_hit);

    // Pinned provider merges cached rows with config entries (config fields win, no refresh).
    let _env_merge = env(
        "cache-merge",
        Some(
            "schema_version: 2\ndefault_model: gateway/shared\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models:\n      - id: shared\n        name: Shared (config)\n        limit:\n          output: 2048\n      - pinned\n",
        ),
    );
    seed_cache(
        "gateway",
        vec![
            hya_app::model_cache::CachedModel {
                display_name: Some("Shared (remote)".into()),
                context_limit: 64_000,
                output_limit: 8_192,
                ..cached_row("shared")
            },
            cached_row("remote"),
        ],
    )
    .await;
    let kinds = full_kinds();
    let loaded = hya_app::config::resolve(hya_app::config::parse().unwrap().unwrap(), &kinds)
        .await
        .unwrap();
    let find = |id: &str| {
        loaded
            .catalog
            .models()
            .iter()
            .find(|m| m.model_id == id)
            .unwrap()
            .clone()
    };
    let shared = find("shared");
    assert_eq!(shared.source, S::Overridden);
    assert_eq!(shared.display_name.as_deref(), Some("Shared (config)"));
    assert_eq!(shared.capabilities.max_context, 64_000);
    assert_eq!(shared.capabilities.max_output, 2_048);
    assert_eq!(find("remote").source, S::Discovered);
    assert_eq!(find("pinned").source, S::Configured);
    assert!(loaded.pending_discovery.is_empty());
    drop(_env_merge);

    // Pinned provider without cached rows starts immediately and queues discovery.
    let _env_miss = env(
        "cache-pinned-miss",
        Some(
            "schema_version: 2\ndefault_model: gateway/pinned\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: [pinned]\n",
        ),
    );
    let started = Instant::now();
    let kinds = full_kinds();
    let loaded = hya_app::config::resolve(hya_app::config::parse().unwrap().unwrap(), &kinds)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(loaded.pending_discovery.len(), 1);
    assert_eq!(loaded.catalog.default_model().as_str(), "gateway/pinned");
}
