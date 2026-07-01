#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-server-skill-metadata-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir,
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn find_named<'a>(items: &'a Value, name: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("missing {name}: {items}"))
}

static ENV_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        while ENV_LOCK
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Acquire,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_err()
        {
            std::thread::yield_now();
        }
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
        }
        ENV_LOCK.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn write_skill(root: &std::path::Path, rel: &str, name: &str, description: &str, body: &str) {
    let dir = root.join(rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

#[tokio::test]
async fn opencode_skill_and_command_routes_keep_project_hya_before_home_duplicate() {
    let workdir = tempdir();
    let home = tempdir();
    let _home = HomeGuard::set(&home);
    write_skill(
        &workdir,
        ".hya/skills/project-home-dupe",
        "project-home-dupe",
        "Project duplicate",
        "Project duplicate body\n",
    );
    write_skill(
        &home,
        ".config/hya/skills/project-home-dupe",
        "project-home-dupe",
        "Home duplicate",
        "Home duplicate body\n",
    );
    let app = router(state(workdir.clone()).await);

    let (skill_status, skills) = get_json(
        app.clone(),
        &format!("/skill?directory={}", workdir.display()),
    )
    .await;
    let (command_status, commands) =
        get_json(app, &format!("/command?directory={}", workdir.display())).await;

    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "project-home-dupe");
    assert_eq!(skill["description"], "Project duplicate");
    assert_eq!(skill["content"], "Project duplicate body\n");
    assert!(
        !skills
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["content"] == "Home duplicate body\n")
    );
    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "project-home-dupe");
    assert_eq!(command["source"], "skill");
    assert_eq!(command["template"], "Project duplicate body\n");
}

#[tokio::test]
async fn opencode_skill_and_command_routes_include_builtin_customize_skill() {
    // Given: a server with no workspace skills on disk.
    let app = router(state(tempdir()).await);

    // When: the OpenCode skill and command metadata routes are listed.
    let (skill_status, skills) = get_json(app.clone(), "/skill").await;
    let (command_status, commands) = get_json(app, "/command").await;

    // Then: OpenCode's built-in customize-opencode skill is present in both surfaces.
    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "customize-opencode");
    assert_eq!(skill["location"], "<built-in>");
    assert!(
        skill["description"]
            .as_str()
            .unwrap()
            .starts_with("Use ONLY")
    );
    assert!(
        skill["content"]
            .as_str()
            .unwrap()
            .contains("# Customizing opencode")
    );

    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "customize-opencode");
    assert_eq!(command["source"], "skill");
    assert!(
        command["template"]
            .as_str()
            .unwrap()
            .contains("opencode.json")
    );
}

#[tokio::test]
async fn opencode_skill_and_command_routes_discover_opencode_project_skills() {
    // Given: a workspace with an OpenCode project skill on disk.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/skills/release")).unwrap();
    std::fs::write(
        workdir.join(".opencode/skills/release/SKILL.md"),
        "---\nname: release\ndescription: Prepare a release\n---\nCheck version, changelog, and tag.\n",
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: metadata is requested for that workspace.
    let uri = format!("/skill?directory={}", workdir.display());
    let (skill_status, skills) = get_json(app.clone(), &uri).await;
    let uri = format!("/command?directory={}", workdir.display());
    let (command_status, commands) = get_json(app, &uri).await;

    // Then: the OpenCode project skill is available as both a skill and command.
    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "release");
    assert_eq!(skill["description"], "Prepare a release");
    assert_eq!(skill["content"], "Check version, changelog, and tag.\n");

    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "release");
    assert_eq!(command["source"], "skill");
    assert_eq!(command["template"], "Check version, changelog, and tag.\n");
}

#[tokio::test]
async fn opencode_skill_command_does_not_override_existing_command_name() {
    // Given: a workspace skill collides with the built-in help command.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/skills/help")).unwrap();
    std::fs::write(
        workdir.join(".opencode/skills/help/SKILL.md"),
        "---\nname: help\ndescription: Workspace help skill\n---\nDisk help skill body.\n",
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: skill and command metadata are requested for that workspace.
    let uri = format!("/skill?directory={}", workdir.display());
    let (skill_status, skills) = get_json(app.clone(), &uri).await;
    let uri = format!("/command?directory={}", workdir.display());
    let (command_status, commands) = get_json(app, &uri).await;

    // Then: the disk skill is listed without overriding the built-in help command.
    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "help");
    assert_eq!(skill["description"], "Workspace help skill");
    assert_eq!(skill["content"], "Disk help skill body.\n");

    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "help");
    assert_eq!(command["source"], "command");
    assert_eq!(command["template"], "/help");
}
