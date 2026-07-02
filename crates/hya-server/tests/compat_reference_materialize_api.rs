#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

struct EnvGuard {
    key: &'static str,
    old: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let old = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("hya-compat-{label}-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn init_remote(root: &Path) -> PathBuf {
    let source = root.join("source");
    let remote = root.join("remotes").join("owner").join("repo.git");
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    git(&source, &["init"]);
    git(&source, &["config", "user.email", "test@example.com"]);
    git(&source, &["config", "user.name", "Test User"]);
    std::fs::write(source.join("README.md"), "hello\n").unwrap();
    git(&source, &["add", "README.md"]);
    git(&source, &["commit", "-m", "init"]);
    git(&source, &["branch", "-M", "main"]);
    git(&source, &["checkout", "-b", "feature"]);
    std::fs::write(source.join("README.md"), "feature\n").unwrap();
    git(&source, &["add", "README.md"]);
    git(&source, &["commit", "-m", "feature"]);
    git(&source, &["checkout", "main"]);
    let output = Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg(&source)
        .arg(&remote)
        .output()
        .unwrap();
    assert!(output.status.success());
    remote
}

async fn state(workdir: PathBuf) -> AppState {
    std::fs::create_dir_all(&workdir).unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir,
            reasoning: None,
        }),
    )
}

async fn request_json(app: axum::Router, method: Method, uri: &str, body: Value) -> Value {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = if body.is_null() {
        Body::empty()
    } else {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn wait_for_file(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("reference repository was not materialized");
}

async fn wait_for_content(path: &Path, expected: &str) {
    for _ in 0..100 {
        if std::fs::read_to_string(path).is_ok_and(|content| content == expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("reference repository content did not refresh");
}

#[tokio::test]
async fn compat_reference_materializes_git_repository_cache() {
    if !git_available() {
        return;
    }
    let root = tempdir("reference-materialize");
    let remote = init_remote(&root);
    let data_home = root.join("data");
    let _data_home = EnvGuard::set("XDG_DATA_HOME", &data_home);
    let _clone_base = EnvGuard::set(
        "COMPAT_REPO_CLONE_GITHUB_BASE_URL",
        format!(
            "file://{}",
            remote.parent().unwrap().parent().unwrap().display()
        ),
    );
    let app = router(state(root.join("work")).await);

    request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "localrepo": {
                    "repository": "owner/repo",
                    "branch": "main"
                }
            }
        }),
    )
    .await;
    let references = request_json(app.clone(), Method::GET, "/api/reference", Value::Null).await;
    let path = references["data"][0]["path"].as_str().unwrap();
    assert_eq!(
        path,
        data_home
            .join("compat/repos/github.com/owner/repo")
            .to_string_lossy()
    );

    let readme = PathBuf::from(path).join("README.md");
    wait_for_file(&readme).await;
    assert_eq!(std::fs::read_to_string(&readme).unwrap(), "hello\n");

    request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "localrepo": {
                    "repository": "owner/repo",
                    "branch": "feature"
                }
            }
        }),
    )
    .await;
    request_json(app, Method::GET, "/api/reference", Value::Null).await;
    wait_for_content(&readme, "feature\n").await;
    assert_eq!(
        std::fs::read_to_string(PathBuf::from(path).join("README.md")).unwrap(),
        "feature\n"
    );
}
