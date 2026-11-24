//! The skill catalog over v1: `ListSkills` reports the skill files it
//! skipped or warns about (`diagnostics`) and `disable-model-invocation`;
//! with `session` it lists what that session's agent can load, bundle
//! skills included, and `ListCommands` with `session` lists their
//! `skill:<name>` rows; a `skill:<name>` command turn expands a bundle
//! skill without a reference directory (it has none on disk).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::sync::Arc;

use crate::support::AgentFixture;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_engine::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

struct TextProvider;

#[async_trait]
impl Provider for TextProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "done".to_string(),
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
                tokens: None,
                cause: None,
            },
        ];
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

async fn app() -> axum::Router {
    let mut agents = support::test_agents();
    agents.push(AgentFixture::main("helper").bundle_skill("helper-guide"));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(TextProvider))),
        support::runtime_with_catalog(Arc::new(ToolRegistry::builtins()), &agents),
        perm,
        EventBus::default(),
    ));
    router(AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    ))
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
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
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn encode(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn named<'a>(rows: &'a Value, field: &str, name: &str) -> Option<&'a Value> {
    rows[field]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == name))
}

fn write(dir: &std::path::Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test]
async fn skill_listing_reports_diagnostics_and_manual_only_skills() {
    let app = app().await;
    let dir = support::tempdir("skill-diagnostics");
    write(
        &dir,
        ".hya/skills/manual/SKILL.md",
        "---\nname: manual\ndescription: by hand\ndisable-model-invocation: true\n---\nbody\n",
    );
    write(
        &dir,
        ".hya/skills/broken/SKILL.md",
        "---\nname: broken\n---\nno description\n",
    );
    write(&dir, ".hya/commands/skill:taken.md", "reserved\n");
    let (status, skills) = call(
        &app,
        Method::GET,
        &format!("/v1/skills?directory={}", encode(&dir)),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{skills}");
    assert_eq!(
        named(&skills, "skills", "manual").unwrap()["disableModelInvocation"],
        true
    );
    let diagnostics = skills["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|row| row["kind"] == "missing_description"
                && row["path"]
                    .as_str()
                    .unwrap()
                    .ends_with(".hya/skills/broken/SKILL.md")),
        "{skills}"
    );
    let (_, commands) = call(
        &app,
        Method::GET,
        &format!("/v1/commands?directory={}", encode(&dir)),
        Value::Null,
    )
    .await;
    assert_eq!(
        named(&commands, "commands", "skill:manual").unwrap()["scope"],
        "project"
    );
    assert!(
        commands["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["kind"] == "reserved"),
        "{commands}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_session_lists_and_runs_its_agents_bundle_skills() {
    let app = app().await;
    let dir = support::tempdir("skill-session");
    write(
        &dir,
        ".hya/skills/local/SKILL.md",
        "---\nname: local\ndescription: a project skill\n---\nlocal body\n",
    );
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/sessions",
        json!({ "agent": "helper", "model": "fake", "workdir": dir.to_string_lossy() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let session = body["session"]["id"].as_str().unwrap().to_owned();

    // The directory view has no bundle skill; the session's view has its agent's.
    let (_, by_directory) = call(
        &app,
        Method::GET,
        &format!("/v1/skills?directory={}", encode(&dir)),
        Value::Null,
    )
    .await;
    assert!(named(&by_directory, "skills", "helper-guide").is_none());
    assert!(named(&by_directory, "skills", "local").is_some());
    let (status, by_session) = call(
        &app,
        Method::GET,
        &format!("/v1/skills?session={session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_session}");
    let guide = named(&by_session, "skills", "helper-guide").expect("bundle skill");
    assert_eq!(guide["source"], "bundle");
    assert_eq!(guide["bundleId"], "hya/server-tests-helper");
    assert!(
        named(&by_session, "skills", "local").is_some(),
        "{by_session}"
    );

    let (_, commands) = call(
        &app,
        Method::GET,
        &format!("/v1/commands?session={session}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        named(&commands, "commands", "skill:helper-guide").unwrap()["scope"],
        "bundle"
    );

    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({ "command": { "command": "skill:helper-guide", "arguments": "go" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, messages) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    let text = messages["messages"][0]["parts"][0]["text"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(
        text,
        "<skill name=\"helper-guide\" location=\"bundle:hya/server-tests-helper/resources/skills/helper-guide.md\">\nhelper-guide body\n</skill>\n\ngo",
        "{messages}"
    );

    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/skills?session={}", SessionId::new()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}
