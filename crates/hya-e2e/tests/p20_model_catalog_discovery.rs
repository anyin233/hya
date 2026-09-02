//! Process-level model catalog discovery and offline fallback contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::get;
use axum::{Json, Router};
use hya_e2e::default_backend_bin;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command as TokioCommand};

#[derive(Clone)]
struct CatalogState {
    requests: Arc<AtomicUsize>,
    saw_auth: Arc<AtomicBool>,
    reject: Arc<AtomicBool>,
}

async fn list_models(
    State(state): State<CatalogState>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Json<Value>, StatusCode> {
    state.requests.fetch_add(1, Ordering::SeqCst);
    if headers.contains_key("authorization") {
        state.saw_auth.store(true, Ordering::SeqCst);
    }
    if uri.path().starts_with("/rejected/") {
        return Err(StatusCode::FORBIDDEN);
    }
    if uri.path().starts_with("/failed/") {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    if state.reject.load(Ordering::SeqCst) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(json!({
        "object": "list",
        "data": [
            { "id": " discovered " },
            { "id": "discovered" },
            { "id": "" }
        ]
    })))
}

fn temp_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "hya-model-catalog-e2e-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Write one Hya config fixture and return its path.
fn write_yaml(root: &Path, yaml: &str) -> PathBuf {
    let config_dir = root.join("config/hya");
    std::fs::create_dir_all(&config_dir).unwrap();
    let path = config_dir.join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// Write the single-provider fixture used by the fresh-start contract.
fn write_config(root: &Path, base_url: &str, models: &str) -> PathBuf {
    write_yaml(
        root,
        &format!(
            "default_model: gateway/discovered\nproviders:\n  gateway:\n    kind: openai\n    base_url: {base_url}\n    models:{models}\n"
        ),
    )
}

/// Run the model-list command with isolated Hya roots.
fn run_models(binary: &Path, root: &Path) -> std::process::Output {
    Command::new(binary)
        .arg("models")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env_remove("HYA_MODEL")
        .output()
        .unwrap()
}

/// Run one headless prompt with isolated Hya roots.
fn run_exec(binary: &Path, root: &Path, prompt: &str) -> std::process::Output {
    Command::new(binary)
        .args(["--yolo", "--db"])
        .arg(root.join("exec.db"))
        .args(["exec", prompt])
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env_remove("HYA_MODEL")
        .output()
        .unwrap()
}

struct RunningBackend {
    child: Child,
    url: String,
}

impl Drop for RunningBackend {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl RunningBackend {
    /// Stop the backend and wait for its process to be reaped.
    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

/// Start a real backend process against the fixture's Hya config.
async fn start_backend(binary: &Path, root: &Path) -> RunningBackend {
    let mut child = TokioCommand::new(binary)
        .args(["--yolo", "--db"])
        .arg(root.join("server.db"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env_remove("HYA_MODEL")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let url = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some(url) = line.strip_prefix("hya server listening on ") {
                return url.to_string();
            }
        }
        panic!("backend exited before readiness")
    })
    .await
    .expect("backend readiness timeout");
    RunningBackend { child, url }
}

/// Fetch one JSON API payload from the running backend.
async fn get_json(base_url: &str, path: &str) -> (StatusCode, Value) {
    let response = reqwest::get(format!("{base_url}{path}")).await.unwrap();
    let status = response.status();
    let body = response.json().await.unwrap();
    (status, body)
}

/// Convert `/api/model` rows to canonical provider/model references.
fn model_ids(body: &Value) -> BTreeSet<String> {
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| {
            Some(format!(
                "{}/{}",
                model["providerID"].as_str()?,
                model["id"].as_str()?
            ))
        })
        .collect()
}

/// Convert provider DTO rows to canonical provider/model references.
fn provider_model_ids(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .flat_map(|provider| {
            let provider_id = provider["id"].as_str().unwrap_or_default();
            provider["models"]
                .as_object()
                .into_iter()
                .flat_map(move |models| {
                    models
                        .keys()
                        .map(move |model| format!("{provider_id}/{model}"))
                })
        })
        .collect()
}

/// Return one provider DTO by its stable id.
fn provider_row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|provider| provider["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("provider row missing: {id}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_catalog_is_fresh_ephemeral_anonymous_and_offline_on_auth_required() {
    let state = CatalogState {
        requests: Arc::new(AtomicUsize::new(0)),
        saw_auth: Arc::new(AtomicBool::new(false)),
        reject: Arc::new(AtomicBool::new(false)),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/models", get(list_models))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let root = temp_root();
    std::fs::create_dir_all(root.join("home/.config/opencode")).unwrap();
    let foreign = root.join("home/.config/opencode/opencode.json");
    std::fs::write(&foreign, b"foreign catalog sentinel").unwrap();
    let base_url = format!("http://{address}/v1");
    let binary = default_backend_bin();

    let config = write_config(
        &root,
        &base_url,
        "\n      - explicit\n      - ' explicit '\n      - ''",
    );
    let explicit_bytes = std::fs::read(&config).unwrap();
    let explicit = run_models(&binary, &root);
    assert!(
        explicit.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&explicit.stdout).trim(),
        "gateway/explicit"
    );
    assert_eq!(state.requests.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&config).unwrap(), explicit_bytes);

    write_config(&root, &base_url, " []");
    let discovery_bytes = std::fs::read(&config).unwrap();
    for expected_requests in 1..=2 {
        let discovered = run_models(&binary, &root);
        assert!(
            discovered.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&discovered.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&discovered.stdout).trim(),
            "gateway/discovered"
        );
        assert_eq!(state.requests.load(Ordering::SeqCst), expected_requests);
        assert_eq!(std::fs::read(&config).unwrap(), discovery_bytes);
    }
    assert!(!state.saw_auth.load(Ordering::SeqCst));

    state.reject.store(true, Ordering::SeqCst);
    let offline = run_models(&binary, &root);
    assert!(
        offline.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&offline.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&offline.stdout).trim(),
        "hya/offline"
    );
    assert_eq!(state.requests.load(Ordering::SeqCst), 3);
    assert_eq!(std::fs::read(&config).unwrap(), discovery_bytes);
    assert_eq!(
        std::fs::read(&foreign).unwrap(),
        b"foreign catalog sentinel"
    );

    server.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentialed_forbidden_catalog_is_rejected_and_offline_exec_explains_configuration() {
    let state = CatalogState {
        requests: Arc::new(AtomicUsize::new(0)),
        saw_auth: Arc::new(AtomicBool::new(false)),
        reject: Arc::new(AtomicBool::new(false)),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/rejected/v1/models", get(list_models))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let root = temp_root();
    let config = write_yaml(
        &root,
        &format!(
            "default_model: rejected/guessed\nproviders:\n  rejected:\n    kind: openai\n    base_url: http://{address}/rejected/v1\n    api_key: credential-token\n    models: []\n"
        ),
    );
    let config_bytes = std::fs::read(&config).unwrap();
    let binary = default_backend_bin();
    let mut backend = start_backend(&binary, &root).await;
    let (status, providers) = get_json(&backend.url, "/api/provider").await;
    assert_eq!(status, StatusCode::OK);
    let provider_rows = providers["data"].as_array().unwrap();
    let rejected = provider_row(provider_rows, "rejected");
    assert_eq!(rejected["auth"], "auth_rejected");
    assert_eq!(rejected["result"], "unavailable");
    assert!(rejected["models"].as_object().unwrap().is_empty());
    assert!(state.saw_auth.load(Ordering::SeqCst));

    let (status, models) = get_json(&backend.url, "/api/model").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        model_ids(&models),
        BTreeSet::from(["hya/offline".to_string()])
    );
    let exec = run_exec(&binary, &root, "offline process proof");
    assert!(
        exec.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let output = String::from_utf8_lossy(&exec.stdout);
    assert!(output.contains("offline process proof"));
    assert!(output.contains("No live provider is available. Configure a provider to continue."));
    assert_eq!(std::fs::read(&config).unwrap(), config_bytes);

    backend.stop().await;
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_provider_failure_keeps_valid_rows_equal_across_catalog_surfaces() {
    let state = CatalogState {
        requests: Arc::new(AtomicUsize::new(0)),
        saw_auth: Arc::new(AtomicBool::new(false)),
        reject: Arc::new(AtomicBool::new(false)),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/valid/v1/models", get(list_models))
        .route("/failed/v1/models", get(list_models))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let root = temp_root();
    let config = write_yaml(
        &root,
        &format!(
            "default_model: valid/discovered\nproviders:\n  valid:\n    kind: openai\n    base_url: http://{address}/valid/v1\n    models: []\n  failed:\n    kind: openai\n    base_url: http://{address}/failed/v1\n    models: []\n"
        ),
    );
    let config_bytes = std::fs::read(&config).unwrap();
    let binary = default_backend_bin();
    let cli = run_models(&binary, &root);
    assert!(
        cli.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&cli.stdout).trim(),
        "valid/discovered"
    );

    let mut backend = start_backend(&binary, &root).await;
    let (status, api_models) = get_json(&backend.url, "/api/model").await;
    assert_eq!(status, StatusCode::OK);
    let expected = BTreeSet::from(["valid/discovered".to_string()]);
    assert_eq!(model_ids(&api_models), expected);

    let (status, api_providers) = get_json(&backend.url, "/api/provider").await;
    assert_eq!(status, StatusCode::OK);
    let api_rows = api_providers["data"].as_array().unwrap();
    assert_eq!(provider_model_ids(api_rows), expected);
    let (status, provider_detail) = get_json(&backend.url, "/api/provider/valid").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        provider_model_ids(std::slice::from_ref(&provider_detail["data"])),
        expected
    );
    assert_eq!(provider_row(api_rows, "valid")["result"], "models");
    assert_eq!(provider_row(api_rows, "failed")["result"], "unavailable");
    assert!(
        provider_row(api_rows, "failed")["models"]
            .as_object()
            .unwrap()
            .is_empty()
    );

    let (status, legacy_providers) = get_json(&backend.url, "/provider").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        provider_model_ids(legacy_providers["all"].as_array().unwrap()),
        expected
    );
    let (status, config_providers) = get_json(&backend.url, "/config/providers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        provider_model_ids(config_providers["providers"].as_array().unwrap()),
        expected
    );

    let (status, bootstrap) = get_json(&backend.url, "/tui/bootstrap").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        provider_model_ids(bootstrap["providers"]["providers"].as_array().unwrap()),
        expected
    );
    assert_eq!(
        provider_model_ids(bootstrap["provider_list"]["all"].as_array().unwrap()),
        expected
    );
    assert_eq!(std::fs::read(&config).unwrap(), config_bytes);

    backend.stop().await;
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}
