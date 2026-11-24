//! The skill catalog over v1: `ListSkills` reports the skill files it
//! skipped or warns about (`diagnostics`) and `disable-model-invocation`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

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
use serde_json::Value;
use support::AgentFixture;
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
    let _ = std::fs::remove_dir_all(&dir);
}
