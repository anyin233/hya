//! T2.15 — model catalog discovery, caching, anonymous rediscovery, credentialed
//! 403 rejection, mixed-provider consistency across CLI / `/v2/models` /
//! `/v2/providers` / `/v2/bootstrap`, and `hya/offline` fallback.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::get;
use axum::{Json, Router};
use hya_e2e::{default_backend_bin, parse_listen_url, unique_temp_dir};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command as TokioCommand};

#[derive(Clone)]
struct CatalogState {
    requests: Arc<AtomicUsize>,
    saw_auth: Arc<AtomicBool>,
    reject_gateway: Arc<AtomicBool>,
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
    if uri.path().starts_with("/gateway/") && state.reject_gateway.load(Ordering::SeqCst) {
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

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

fn write_yaml(root: &Path, yaml: &str) -> PathBuf {
    let dir = root.join("config/hya");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

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
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let url = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some(url) = parse_listen_url(&line) {
                return url.to_string();
            }
        }
        panic!("backend exited before readiness")
    })
    .await
    .expect("backend readiness");
    RunningBackend { child, url }
}

async fn get_json(base: &str, path: &str) -> (StatusCode, Value) {
    let resp = reqwest::get(format!("{base}{path}")).await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

fn model_ids(body: &Value) -> BTreeSet<String> {
    body["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect()
}

fn provider_row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter()
        .find(|p| p["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("missing provider {id}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_catalog_discovery_mixed_providers_and_offline_fallback() {
    let state = CatalogState {
        requests: Arc::new(AtomicUsize::new(0)),
        saw_auth: Arc::new(AtomicBool::new(false)),
        reject_gateway: Arc::new(AtomicBool::new(false)),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/gateway/v1/models", get(list_models))
        .route("/valid/v1/models", get(list_models))
        .route("/failed/v1/models", get(list_models))
        .route("/rejected/v1/models", get(list_models))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let binary = default_backend_bin();

    // 1. Fresh/cached pinned list, anonymous rediscovery, and 401 -> `hya/offline` + exec explanation.
    let root1 = TempDir(unique_temp_dir("hya-model-catalog-1").unwrap());
    std::fs::create_dir_all(root1.0.join("home/.config/opencode")).unwrap();
    let foreign = root1.0.join("home/.config/opencode/opencode.json");
    std::fs::write(&foreign, b"foreign catalog sentinel").unwrap();
    let gw_url = format!("http://{address}/gateway/v1");

    let cfg1 = write_yaml(
        &root1.0,
        &format!(
            "schema_version: 2\ndefault_model: gateway/discovered\nproviders:\n  gateway:\n    kind: openai\n    base_url: {gw_url}\n    models:\n      - explicit\n      - ' explicit '\n      - ''\n"
        ),
    );
    let pinned_bytes = std::fs::read(&cfg1).unwrap();
    let explicit = run_models(&binary, &root1.0);
    assert!(explicit.status.success());
    assert_eq!(
        String::from_utf8_lossy(&explicit.stdout).trim(),
        "gateway/discovered\ngateway/explicit"
    );
    assert_eq!(state.requests.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(&cfg1).unwrap(), pinned_bytes);
    // Cached rows exist: pinned provider does not re-fetch.
    assert!(run_models(&binary, &root1.0).status.success());
    assert_eq!(state.requests.load(Ordering::SeqCst), 1);

    // Empty `models: []` rediscovers anonymously each run without mutating config.
    write_yaml(
        &root1.0,
        &format!(
            "schema_version: 2\ndefault_model: gateway/discovered\nproviders:\n  gateway:\n    kind: openai\n    base_url: {gw_url}\n    models: []\n"
        ),
    );
    let empty_bytes = std::fs::read(&cfg1).unwrap();
    for expected in 2..=3 {
        let out = run_models(&binary, &root1.0);
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "gateway/discovered"
        );
        assert_eq!(state.requests.load(Ordering::SeqCst), expected);
        assert_eq!(std::fs::read(&cfg1).unwrap(), empty_bytes);
    }
    assert!(!state.saw_auth.load(Ordering::SeqCst));

    // 401 -> `hya/offline`, and `exec` explains configuration without touching config/foreign file.
    state.reject_gateway.store(true, Ordering::SeqCst);
    let offline = run_models(&binary, &root1.0);
    assert!(offline.status.success());
    assert_eq!(
        String::from_utf8_lossy(&offline.stdout).trim(),
        "hya/offline"
    );
    let exec = run_exec(&binary, &root1.0, "offline process proof");
    assert!(
        exec.status.success(),
        "{}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let exec_out = String::from_utf8_lossy(&exec.stdout);
    assert!(
        exec_out.contains("offline process proof")
            && exec_out
                .contains("No live provider is available. Configure a provider to continue.")
    );
    assert_eq!(std::fs::read(&cfg1).unwrap(), empty_bytes);
    assert_eq!(
        std::fs::read(&foreign).unwrap(),
        b"foreign catalog sentinel"
    );

    // 2. Mixed valid + failed (500) + credentialed-forbidden (403) across CLI and all v1 surfaces.
    let root2 = TempDir(unique_temp_dir("hya-model-catalog-2").unwrap());
    let cfg2 = write_yaml(
        &root2.0,
        &format!(
            "schema_version: 2\ndefault_model: valid/discovered\nproviders:\n  valid:\n    kind: openai\n    base_url: http://{address}/valid/v1\n    models: []\n  failed:\n    kind: openai\n    base_url: http://{address}/failed/v1\n    models: []\n  rejected:\n    kind: openai\n    base_url: http://{address}/rejected/v1\n    api_key: credential-token\n    models: []\n"
        ),
    );
    let cfg2_bytes = std::fs::read(&cfg2).unwrap();
    let cli = run_models(&binary, &root2.0);
    assert!(cli.status.success());
    assert_eq!(
        String::from_utf8_lossy(&cli.stdout).trim(),
        "valid/discovered"
    );

    let mut backend = start_backend(&binary, &root2.0).await;
    let expected = BTreeSet::from(["valid/discovered".to_string()]);
    let (s, api_models) = get_json(&backend.url, "/v2/models").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(model_ids(&api_models), expected);

    let (s, api_providers) = get_json(&backend.url, "/v2/providers").await;
    assert_eq!(s, StatusCode::OK);
    let rows = api_providers["providers"].as_array().unwrap();
    assert_eq!(provider_row(rows, "valid")["result"], "models");
    assert_eq!(provider_row(rows, "failed")["result"], "unavailable");
    let rejected = provider_row(rows, "rejected");
    assert_eq!(rejected["auth"], "AUTH_STATUS_AUTH_REJECTED");
    assert_eq!(rejected["result"], "unavailable");
    assert!(state.saw_auth.load(Ordering::SeqCst));

    let (s, detail) = get_json(&backend.url, "/v2/providers/valid").await;
    assert_eq!(s, StatusCode::OK);
    let detail_ids: BTreeSet<String> = detail["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(detail_ids, expected);

    let (s, bootstrap) = get_json(&backend.url, "/v2/bootstrap").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(model_ids(&bootstrap), expected);
    assert_eq!(std::fs::read(&cfg2).unwrap(), cfg2_bytes);

    let _ = backend.child.kill().await;
    let _ = backend.child.wait().await;
    server.abort();
}
