//! Integration tests for `hya provider add|list|logout|remove` and `--refresh`
//! against a local fake model-list endpoint. Pure validation and formatting live
//! in `src/provider_cmd.rs` unit tests.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use serde_json::json;

use crate::common::{Scratch, TestResult, assert_success, combined_text};

struct FakeModels {
    base: String,
    fetches: Arc<AtomicUsize>,
    models: Arc<Mutex<Vec<&'static str>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for FakeModels {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl FakeModels {
    async fn start(key: &'static str) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let fetches = Arc::new(AtomicUsize::new(0));
        let models = Arc::new(Mutex::new(vec!["alpha", "beta"]));
        let (f, m) = (Arc::clone(&fetches), Arc::clone(&models));
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(move |h: HeaderMap| {
                let (f, m) = (Arc::clone(&f), Arc::clone(&m));
                async move {
                    f.fetch_add(1, Ordering::SeqCst);
                    let hdr = |k| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("");
                    if hdr("authorization") == format!("Bearer {key}") || hdr("x-api-key") == key {
                        let data: Vec<_> = m
                            .lock()
                            .map(|ids| {
                                ids.iter()
                                    .map(|id| json!({"id": id, "type": "model"}))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (
                            StatusCode::OK,
                            axum::Json(json!({"data": data, "has_more": false})),
                        )
                    } else {
                        (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(json!({"error": {"message": "bad key"}})),
                        )
                    }
                }
            }),
        );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            base,
            fetches,
            models,
            task,
        })
    }
}

fn provider(s: &Scratch, args: &[&str], input: &str) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = s
        .hya()
        .arg("provider")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("stdin")?
        .write_all(input.as_bytes())?;
    Ok(child.wait_with_output()?)
}

fn config(root: &Path) -> String {
    fs::read_to_string(root.join("config/hya/config.yaml")).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_add_reject_list_logout_and_remove_end_to_end() -> TestResult {
    let s = Scratch::new("hya-provider-e2e")?;
    let root = &s.0;
    let srv = FakeModels::start("sk-test").await?;

    let bad = provider(
        &s,
        &[
            "add",
            "--name",
            "bad",
            "--base-url",
            &srv.base,
            "--protocol",
            "openai",
            "--api-key",
            "sk-wrong",
        ],
        "",
    )?;
    assert!(!bad.status.success() && combined_text(&bad).contains("rejected"));
    assert!(!config(root).contains("bad:") && !root.join("config/hya/auth/bad.yaml").exists());

    let add = provider(
        &s,
        &["add"],
        &format!("{}\nopenai\nsk-test\nfake\n", srv.base),
    )?;
    assert_success("provider add", &add);
    let out = combined_text(&add);
    assert!(out.contains("alpha") && out.contains("Saved provider `fake`"));
    let yaml = config(root);
    let key_file = root.join("config/hya/auth/fake.yaml");
    assert!(
        yaml.contains("fake:")
            && yaml.contains("kind: openai")
            && !yaml.contains("sk-test")
            && fs::read_to_string(&key_file)?.contains("sk-test")
    );

    fs::write(
        root.join("config/hya/config.yaml"),
        yaml.replace(
            "    models: []",
            "    models:\n      - id: alpha\n        name: Alpha Custom\n      - gamma",
        ),
    )?;
    assert_success(
        "login orphan",
        &s.hya().args(["login", "orphan", "sk-orphan"]).output()?,
    );

    let listed = String::from_utf8(provider(&s, &["list"], "")?.stdout)?;
    for n in [
        "fake",
        "  openai  ",
        "saved key",
        "config override",
        "config only",
        "orphan",
    ] {
        assert!(listed.contains(n), "list lacks `{n}`:\n{listed}");
    }
    assert_eq!(
        String::from_utf8(s.hya().args(["providers", "list"]).output()?.stdout)?,
        listed
    );

    assert_success("logout", &provider(&s, &["logout", "fake"], "")?);
    assert!(
        !key_file.exists()
            && config(root).contains("fake:")
            && combined_text(&provider(&s, &["list"], "")?).contains("no key")
    );
    assert!(!provider(&s, &["remove", "fake"], "n\n")?.status.success());
    assert_success("remove", &provider(&s, &["remove", "fake"], "y\n")?);
    assert!(!config(root).contains("fake:"));
    let missing = provider(&s, &["remove", "fake", "--yes"], "")?;
    assert!(!missing.status.success() && combined_text(&missing).contains("not configured"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_updates_the_running_backend() -> TestResult {
    let s = Scratch::new("hya-provider-refresh")?;
    let srv = FakeModels::start("sk-test").await?;
    assert_success(
        "add",
        &provider(
            &s,
            &[
                "add",
                "--name",
                "fake",
                "--base-url",
                &srv.base,
                "--protocol",
                "openai",
                "--api-key",
                "sk-test",
            ],
            "",
        )?,
    );
    let db = s.0.join("s.db");
    let db_s = db.to_str().ok_or("db")?;
    let started = s
        .hya()
        .args(["serve", "start", "--json", "--db", db_s])
        .output()?;
    assert_success("serve start", &started);
    let url = serde_json::from_slice::<serde_json::Value>(&started.stdout)?["url"]
        .as_str()
        .ok_or("url")?
        .to_string();
    let http = reqwest::Client::new();
    let fetch = || http.get(format!("{url}/v2/models?providerId=fake")).send();

    let result = async {
        let before = fetch().await?.text().await?;
        assert!(
            before.contains("fake/beta") && !before.contains("fake/gamma"),
            "{before}"
        );
        if let Ok(mut m) = srv.models.lock() {
            *m = vec!["alpha", "beta", "gamma"];
        }
        let listed = s
            .hya()
            .args(["provider", "list", "--refresh", "--db", db_s])
            .output()?;
        assert!(
            listed.status.success()
                && combined_text(&listed).contains("fake/gamma")
                && srv.fetches.load(Ordering::SeqCst) == 3
        );
        assert!(fetch().await?.text().await?.contains("fake/gamma"));

        if let Ok(mut m) = srv.models.lock() {
            *m = vec!["alpha", "beta", "gamma", "delta"];
        }
        let models = s
            .hya()
            .args(["models", "fake", "--refresh", "--db", db_s])
            .output()?;
        assert!(
            models.status.success()
                && combined_text(&models).contains("delta")
                && srv.fetches.load(Ordering::SeqCst) == 4
        );
        assert!(fetch().await?.text().await?.contains("fake/delta"));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = s.hya().args(["serve", "stop", "--db", db_s]).output();
    result
}
