//! Provider View backend end to end: the real `ProviderManager` behind the
//! v1 routes, a fake OpenAI-compatible provider over HTTP, and isolated
//! config/auth/cache directories. Covers provider upsert with model fetch,
//! live key save/remove (no restart), refresh, config model overrides, the
//! one-token model test, and the credential file format.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::{Mutex, MutexGuard};
use tower::ServiceExt;

async fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: env-sensitive tests in this binary hold `env_lock`.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// What the fake provider saw.
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
    _guards: Vec<EnvGuard>,
}

fn env(label: &str, config_yaml: Option<&str>) -> Env {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let root = std::env::temp_dir().join(format!(
        "hya-provider-control-{label}-{}-{nanos}",
        std::process::id()
    ));
    let config_home = root.join("config");
    std::fs::create_dir_all(config_home.join("hya")).unwrap();
    let config = config_home.join("hya/config.yaml");
    if let Some(yaml) = config_yaml {
        std::fs::write(&config, yaml).unwrap();
    }
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let guards = vec![
        EnvGuard::set("XDG_CONFIG_HOME", &config_home),
        EnvGuard::set("XDG_CACHE_HOME", &root.join("cache")),
        EnvGuard::set("HOME", &home),
    ];
    Env {
        auth: config_home.join("hya/auth"),
        root,
        config,
        _guards: guards,
    }
}

async fn app(engine_router: hya_provider::ProviderRouter) -> (axum::Router, hya_server::AppState) {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(engine_router),
        support::test_runtime(Arc::new(ToolRegistry::builtins()), &[]),
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
    .with_provider_control(Arc::new(hya_app::ProviderManager::new(engine)));
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
async fn upsert_fetches_models_saves_the_key_and_applies_live_then_keys_and_models_edit_live() {
    let _lock = env_lock().await;
    let env = env("upsert", None);
    let (base_url, fake) = fake_provider().await;
    let (offline, _) = hya_app::offline_router(None);
    let (app, state) = app(offline).await;
    let mut updates = state.subscribe_catalog_updates();

    // Add a provider: config written, key saved, remote list fetched + cached.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/gw",
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
    assert!(config.contains("gw:"), "{config}");
    assert!(config.contains("kind: openai"), "{config}");
    assert!(
        !config.contains("sk-first"),
        "keys never land in config.yaml"
    );
    let key_file = env.auth.join("gw.yaml");
    let key = std::fs::read_to_string(&key_file).unwrap();
    assert!(key.contains("type: api"), "{key}");
    assert!(key.contains("sk-first"), "{key}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // The new route is live: the model test goes through it with the key.
    let (status, probe) = send(
        &app,
        Method::POST,
        "/v1/providers/gw/test",
        json!({"modelId": "alpha"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{probe}");
    assert_eq!(probe["ok"], true, "{probe}");
    assert_eq!(probe["text"], "h");
    assert_eq!(probe["finishReason"], "length");
    {
        let seen = fake.seen.lock().unwrap();
        let request = seen.chat_bodies.last().unwrap();
        assert_eq!(request["model"], "alpha");
        assert_eq!(request["max_tokens"], 1, "{request}");
        assert!(request.get("tools").is_none(), "{request}");
        assert_eq!(request["messages"].as_array().unwrap().len(), 1);
        assert_eq!(request["messages"][0]["role"], "user");
        assert_eq!(
            seen.chat_auth.last().unwrap().as_deref(),
            Some("Bearer sk-first")
        );
    }

    // Replacing the key applies live (no restart) and keeps cached models.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/auth/gw",
        json!({"apiKey": "sk-second"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.get("discovery").is_none(),
        "cached rows: no fetch on key save"
    );
    let (status, _) = send(
        &app,
        Method::POST,
        "/v1/providers/gw/test",
        json!({"modelId": "alpha"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fake.seen
            .lock()
            .unwrap()
            .chat_auth
            .last()
            .unwrap()
            .as_deref(),
        Some("Bearer sk-second")
    );

    // Removing the key applies live too.
    let (status, body) = send(&app, Method::DELETE, "/v1/auth/gw", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"]["summary"]["keySource"], "none");
    assert_eq!(
        body["provider"]["summary"]["auth"],
        "AUTH_STATUS_UNAUTHENTICATED"
    );
    assert!(!key_file.exists());
    let (status, _) = send(
        &app,
        Method::POST,
        "/v1/providers/gw/test",
        json!({"modelId": "alpha"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fake.seen.lock().unwrap().chat_auth.last().unwrap(), &None);

    // Override a remote model's metadata and add a config-only model.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/gw/models",
        json!({"modelId": "alpha", "displayName": "Alpha (mine)", "outputLimit": 1024}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alpha = model(&body["provider"], "alpha").unwrap();
    assert_eq!(alpha["source"], "override");
    assert_eq!(alpha["displayName"], "Alpha (mine)");
    assert_eq!(
        alpha["contextLimit"], "64000",
        "unset config field keeps the remote value"
    );
    assert_eq!(alpha["outputLimit"], "1024");
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/gw/models",
        json!({"modelId": "local/only", "contextLimit": 8000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        model(&body["provider"], "local/only").unwrap()["source"],
        "config"
    );
    let config = std::fs::read_to_string(&env.config).unwrap();
    assert!(config.contains("name: Alpha (mine)"), "{config}");
    assert!(config.contains("local/only"), "{config}");

    // Editing one field patches the entry: the name and output limit stay.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/gw/models",
        json!({"modelId": "alpha", "contextLimit": 32000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alpha = model(&body["provider"], "alpha").unwrap();
    assert_eq!(alpha["displayName"], "Alpha (mine)");
    assert_eq!(alpha["contextLimit"], "32000");
    assert_eq!(alpha["outputLimit"], "1024");
    // An empty name clears the override back to the remote name; 0 clears
    // a limit.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/gw/models",
        json!({"modelId": "alpha", "displayName": "", "outputLimit": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alpha = model(&body["provider"], "alpha").unwrap();
    assert_eq!(alpha["displayName"], "Alpha");
    assert_eq!(alpha["contextLimit"], "32000");
    let config = std::fs::read_to_string(&env.config).unwrap();
    assert!(!config.contains("Alpha (mine)"), "{config}");
    assert!(!config.contains("output: 1024"), "{config}");
    // A request that sets nothing adds the model as a bare entry.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/gw/models",
        json!({"modelId": "bare/m"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        model(&body["provider"], "bare/m").unwrap()["source"],
        "config"
    );
    let config = std::fs::read_to_string(&env.config).unwrap();
    assert!(config.contains("- bare/m\n"), "{config}");

    // Removing the override keeps the remote model (from the cache).
    let (status, body) = send(
        &app,
        Method::DELETE,
        "/v1/providers/gw/models?modelId=alpha",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alpha = model(&body["provider"], "alpha").unwrap();
    assert_eq!(alpha["source"], "remote");
    assert_eq!(alpha["displayName"], "Alpha");
    let (status, body) = send(
        &app,
        Method::DELETE,
        "/v1/providers/gw/models?modelId=alpha",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Refresh re-fetches; the provider list shows kind/base URL/key source.
    let (status, body) = send(&app, Method::POST, "/v1/providers/gw/refresh", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["discovery"]["modelCount"], 2);
    let (status, list) = send(&app, Method::GET, "/v1/providers", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let row = list["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "gw")
        .unwrap();
    assert_eq!(row["baseUrl"], base_url.as_str());
    assert_eq!(row["modelCount"], 4);
    assert!(
        !list["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == "hya"),
        "the offline provider leaves once a live provider exists"
    );

    let (status, listed) = send(&app, Method::GET, "/v1/auth", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed
            .get("providerIds")
            .is_none_or(|ids| ids == &json!([]))
    );

    let _ = std::fs::remove_dir_all(&env.root);
}

#[tokio::test]
async fn failed_fetches_do_not_fail_the_upsert_and_bad_input_is_rejected() {
    let _lock = env_lock().await;
    let env = env(
        "failures",
        Some("default_model: hya/offline\nproviders: {}\n"),
    );
    let (base_url, _fake) = fake_provider().await;
    let (offline, _) = hya_app::offline_router(None);
    let (app, _state) = app(offline).await;

    // A rejected key: the upsert succeeds, the discovery outcome says why.
    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/gw",
        json!({"kind": "anthropic", "baseUrl": "http://127.0.0.1:9/v1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["discovery"].get("ok").is_none(), "{body}");
    assert!(
        body["discovery"]["errorMessage"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "{body}"
    );
    assert_eq!(body["provider"]["summary"]["kind"], "anthropic");

    let (status, body) = send(
        &app,
        Method::PUT,
        "/v1/providers/rejected",
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
        let (status, body) = send(&app, Method::PUT, "/v1/providers/other", payload.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}: {body}");
    }
    let config = std::fs::read_to_string(&env.config).unwrap();
    assert!(!config.contains("other"), "{config}");

    let (status, _) = send(
        &app,
        Method::POST,
        "/v1/providers/nope/refresh",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &app,
        Method::PUT,
        "/v1/providers/nope/models",
        json!({"modelId": "m"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&env.root);
}
