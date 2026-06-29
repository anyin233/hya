#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use serde_json::{Value, json};
use tower::ServiceExt;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-command-metadata-test-{nanos}-{serial}-{}",
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

fn find_command<'a>(commands: &'a Value, name: &str) -> &'a Value {
    commands
        .as_array()
        .unwrap()
        .iter()
        .find(|command| command["name"] == name)
        .unwrap_or_else(|| panic!("missing command {name}: {commands}"))
}

#[tokio::test]
async fn opencode_command_route_includes_native_init_and_review_commands() {
    // Given: a server exposing the OpenCode-compatible instance routes.
    let app = router(state(tempdir()).await);

    // When: the OpenCode /command route is listed.
    let (status, commands) = get_json(app, "/command").await;

    // Then: OpenCode's native init and review commands are advertised.
    assert_eq!(status, StatusCode::OK);

    let init = find_command(&commands, "init");
    assert_eq!(init["description"], "guided AGENTS.md setup");
    assert_eq!(init["source"], "command");
    assert_eq!(init["hints"], json!(["$ARGUMENTS"]));
    assert!(init["template"].as_str().unwrap().contains("AGENTS.md"));

    let review = find_command(&commands, "review");
    assert_eq!(
        review["description"],
        "review changes [commit|branch|pr], defaults to uncommitted"
    );
    assert_eq!(review["source"], "command");
    assert_eq!(review["subtask"], true);
    assert_eq!(review["hints"], json!(["$ARGUMENTS"]));
    assert!(
        review["template"]
            .as_str()
            .unwrap()
            .contains("You are a code reviewer.")
    );
}

#[tokio::test]
async fn opencode_command_route_exposes_workspace_skills_as_commands() {
    // Given: a workspace with a hya skill discovered from disk.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".hya/skills/deploy")).unwrap();
    std::fs::write(
        workdir.join(".hya/skills/deploy/SKILL.md"),
        "---\nname: deploy\ndescription: Deploy the current project\n---\nRun the deployment checklist.\n",
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: the OpenCode /command route is listed for that workspace.
    let (status, commands) =
        get_json(app, &format!("/command?directory={}", workdir.display())).await;

    // Then: the skill is also available as a command prompt.
    assert_eq!(status, StatusCode::OK);
    let deploy = find_command(&commands, "deploy");
    assert_eq!(deploy["description"], "Deploy the current project");
    assert_eq!(deploy["source"], "skill");
    assert_eq!(deploy["hints"], json!([]));
    assert_eq!(deploy["template"], "Run the deployment checklist.\n");
}

#[tokio::test]
async fn opencode_command_route_discovers_project_markdown_commands() {
    // Given: a workspace with OpenCode command markdown files on disk.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/commands/nested")).unwrap();
    std::fs::write(
        workdir.join(".opencode/commands/review.md"),
        "---\ndescription: File review\nagent: reviewer\nmodel: anthropic/claude\nsubtask: true\n---\nReview $1 and $2 with $ARGUMENTS\n",
    )
    .unwrap();
    std::fs::write(
        workdir.join(".opencode/commands/nested/docs.md"),
        "Write docs\n",
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: the OpenCode /command route is listed for that workspace.
    let (status, commands) =
        get_json(app, &format!("/command?directory={}", workdir.display())).await;

    // Then: file commands are exposed with OpenCode-compatible names and metadata.
    assert_eq!(status, StatusCode::OK);
    let review = find_command(&commands, "review");
    assert_eq!(review["description"], "File review");
    assert_eq!(review["source"], "command");
    assert_eq!(review["agent"], "reviewer");
    assert_eq!(review["model"], "anthropic/claude");
    assert_eq!(review["subtask"], true);
    assert_eq!(review["hints"], json!(["$1", "$2", "$ARGUMENTS"]));
    assert_eq!(review["template"], "Review $1 and $2 with $ARGUMENTS");

    let docs = find_command(&commands, "nested/docs");
    assert_eq!(docs["source"], "command");
    assert!(docs.get("description").is_none());
    assert_eq!(docs["template"], "Write docs");
}

#[tokio::test]
async fn opencode_command_route_discovers_inline_config_commands() {
    // Given: a workspace with inline OpenCode command config files.
    let workdir = tempdir();
    std::fs::write(
        workdir.join("opencode.json"),
        r#"{
  "command": {
    "deploy": {
      "description": "Deploy from config",
      "agent": "build",
      "model": "openai/gpt-5",
      "subtask": true,
      "template": "Deploy $ARGUMENTS to $1"
    }
  }
}"#,
    )
    .unwrap();
    std::fs::create_dir_all(workdir.join(".opencode")).unwrap();
    std::fs::write(
        workdir.join(".opencode/opencode.jsonc"),
        r#"{
  // Newer OpenCode config also accepts "commands".
  "commands": {
    "review": {
      "description": "Review from config",
      "template": "Review $2 and $1",
    },
  },
}"#,
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: the OpenCode /command route is listed for that workspace.
    let (status, commands) =
        get_json(app, &format!("/command?directory={}", workdir.display())).await;

    // Then: inline config commands are exposed and can override built-ins.
    assert_eq!(status, StatusCode::OK);
    let deploy = find_command(&commands, "deploy");
    assert_eq!(deploy["description"], "Deploy from config");
    assert_eq!(deploy["source"], "command");
    assert_eq!(deploy["agent"], "build");
    assert_eq!(deploy["model"], "openai/gpt-5");
    assert_eq!(deploy["subtask"], true);
    assert_eq!(deploy["hints"], json!(["$1", "$ARGUMENTS"]));
    assert_eq!(deploy["template"], "Deploy $ARGUMENTS to $1");

    let review = find_command(&commands, "review");
    assert_eq!(review["description"], "Review from config");
    assert_eq!(review["hints"], json!(["$1", "$2"]));
    assert_eq!(review["template"], "Review $2 and $1");
}
