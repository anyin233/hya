#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures::stream;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, Role};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

use support::{AgentFixture, runtime_with_catalog, test_runtime};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Distinct Bundle prompt body for end-to-end effective-prompt parity.
const BUNDLE_PROMPT_MARKER: &str = "EXACT_BUNDLE_PROMPT_HYA_MAIN_REF_GUIDANCE";
/// Base AgentSpec prompt that must not survive a non-empty Bundle prompt.
const BASE_PROMPT_MARKER: &str = "BASE_PROMPT_SHOULD_NOT_APPEAR_IN_PROVIDER";
/// Legacy disk agent body that must not reach the provider.
const LEGACY_PROMPT_MARKER: &str = "LEGACY_DISK_AGENT_PROMPT_MARKER";
/// Workdir AGENTS.md body marker (Consult24: project guidance, once).
const AGENTS_MARKER: &str = "WORKDIR_AGENTS_MD_MARKER_EXACT_ONCE";
/// Configured reference description marker.
const REFERENCE_DESC_MARKER: &str = "CONFIGURED_REFERENCE_DESC_MARKER";
/// AGENTS.md content rewritten mid-turn (must not appear after capture).
const AGENTS_MARKER_V2: &str = "WORKDIR_AGENTS_MD_MARKER_MUTATED_V2";

struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(stream::iter([Ok(Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
        })])))
    }
}

fn workdir() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-compat-reference-guidance-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn child_dir(root: &str, name: &str) -> String {
    let dir = PathBuf::from(root).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn sibling_dir(root: &str, name: &str) -> PathBuf {
    let dir = PathBuf::from(root).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

async fn state(workdir: &str, requests: Arc<Mutex<Vec<CompletionRequest>>>) -> AppState {
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
    std::fs::create_dir_all(format!("{workdir}/hidden")).unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider { requests })));
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = test_runtime(tools);
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, runtime, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "base system".to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn state_with_runtime(
    workdir: &str,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    runtime: Arc<hya_core::RuntimeRegistry>,
    agent_name: &str,
    system_prompt: &str,
) -> AppState {
    std::fs::create_dir_all(workdir).unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider { requests })));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, runtime, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new(agent_name),
            model: ModelRef::new("fake"),
            system_prompt: system_prompt.to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn read_reference_state(workdir: &str, reference_dir: &str) -> AppState {
    std::fs::create_dir_all(workdir).unwrap();
    std::fs::create_dir_all(reference_dir).unwrap();
    std::fs::write(format!("{reference_dir}/guide.txt"), "reference body\n").unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "filePath": format!("{reference_dir}/guide.txt") }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = test_runtime(tools);
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, runtime, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "base system".to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = if body.is_null() {
        Body::empty()
    } else {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn create_session(app: axum::Router, workdir: &str) -> String {
    create_session_with_agent(app, workdir, "build").await
}

async fn create_session_with_agent(app: axum::Router, workdir: &str, agent: &str) -> String {
    let (status, body) = request_json(
        app,
        Method::POST,
        "/sessions",
        json!({"agent": agent, "model": "fake", "workdir": workdir}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["session"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn compat_prompt_system_includes_configured_reference_guidance() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let workdir = workdir();
    let app = router(state(&workdir, requests.clone()).await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": {
                    "path": "docs",
                    "description": "Project docs"
                },
                "hidden": {
                    "path": "hidden"
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session(app.clone(), &workdir).await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "read the docs"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let requests = requests.lock().unwrap();
    let system = requests[0].system.as_deref().unwrap();
    assert!(system.contains("base system"));
    assert!(system.contains("<available_references>"));
    assert!(system.contains("<name>docs</name>"));
    assert!(system.contains("<description>Project docs</description>"));
    assert!(!system.contains("<name>hidden</name>"));
}

#[tokio::test]
async fn compat_reference_directories_allow_external_tool_reads() {
    let reference_dir = workdir();
    let workdir = workdir();
    let app = router(read_reference_state(&workdir, &reference_dir).await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": { "path": reference_dir }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session(app.clone(), &workdir).await;
    let (status, _message) = request_json(
        app.clone(),
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "read the reference"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, messages) = request_json(
        app,
        Method::GET,
        &format!("/session/{session}/message"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tool = messages[1]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "tool" && part["tool"] == "read")
        .unwrap();
    assert_eq!(tool["state"]["status"], "completed");
    assert!(
        tool["state"]["output"]
            .as_str()
            .unwrap()
            .contains("reference body")
    );
}

#[tokio::test]
async fn compat_prompt_reference_guidance_uses_session_workdir() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_root = workdir();
    let session_root = workdir();
    let server_dir = child_dir(&server_root, "project");
    let session_dir = child_dir(&session_root, "project");
    let server_ref = sibling_dir(&server_root, "refs");
    let session_ref = sibling_dir(&session_root, "refs");
    let app = router(state(&server_dir, Arc::clone(&requests)).await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": { "path": "../refs", "description": "Session docs" }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session(app.clone(), &session_dir).await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "read session refs"}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let requests = requests.lock().unwrap();
    let system = requests[0].system.as_deref().unwrap();
    assert!(system.contains(session_ref.to_str().unwrap()));
    assert!(!system.contains(server_ref.to_str().unwrap()));
}

#[tokio::test]
async fn compat_reference_external_dirs_use_session_workdir() {
    let server_root = workdir();
    let session_root = workdir();
    let server_dir = child_dir(&server_root, "project");
    let session_dir = child_dir(&session_root, "project");
    let session_ref = sibling_dir(&session_root, "refs");
    let app = router(read_reference_state(&server_dir, session_ref.to_str().unwrap()).await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": { "path": "../refs" }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session(app.clone(), &session_dir).await;
    let (status, _message) = request_json(
        app.clone(),
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "read the session reference"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, messages) = request_json(
        app,
        Method::GET,
        &format!("/session/{session}/message"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tool = messages[1]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "tool" && part["tool"] == "read")
        .unwrap();
    assert_eq!(tool["state"]["status"], "completed");
    assert!(
        tool["state"]["output"]
            .as_str()
            .unwrap()
            .contains("reference body")
    );
}

/// End-to-end: recorded Bundle agent + reference guidance must compose in the
/// actual provider system prompt (not only `reference.rs` return values).
///
/// Owner contract: no legacy disk agent prompt/reasoning; non-empty Bundle
/// prompt replaces base; described references still appear exactly once.
#[tokio::test]
async fn compat_prompt_bundle_prompt_and_reference_guidance_parity() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let workdir = workdir();
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
    // Legacy disk agent file with a distinct marker (must not reach provider).
    std::fs::create_dir_all(format!("{workdir}/.opencode/agents")).unwrap();
    std::fs::write(
        format!("{workdir}/.opencode/agents/hya-main.md"),
        format!("---\noptions:\n  reasoningEffort: high\n---\n{LEGACY_PROMPT_MARKER}\n"),
    )
    .unwrap();

    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("hya-main").prompt(BUNDLE_PROMPT_MARKER),
            AgentFixture::main("build"),
            AgentFixture::subagent("general"),
            AgentFixture::subagent("compaction").prompt("compaction prompt"),
            AgentFixture::subagent("title").prompt("title prompt"),
            AgentFixture::subagent("summary").prompt("summary prompt"),
        ],
    );
    let app = router(
        state_with_runtime(
            &workdir,
            Arc::clone(&requests),
            runtime,
            "hya-main",
            BASE_PROMPT_MARKER,
        )
        .await,
    );

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": {
                    "path": "docs",
                    "description": "Project docs"
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session_with_agent(app.clone(), &workdir, "hya-main").await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "use references with bound agent"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let requests = requests.lock().unwrap();
    assert!(
        !requests.is_empty(),
        "expected at least one provider completion request"
    );
    let Some(system) = requests[0].system.as_deref() else {
        panic!("provider system prompt");
    };

    assert_eq!(
        system.matches(BUNDLE_PROMPT_MARKER).count(),
        1,
        "Bundle prompt must appear exactly once in provider system: {system}"
    );
    assert_eq!(
        system.matches("<available_references>").count(),
        1,
        "available_references block must appear exactly once: {system}"
    );
    assert_eq!(
        system.matches("</available_references>").count(),
        1,
        "available_references close must appear exactly once: {system}"
    );
    assert!(
        system.contains("<name>docs</name>"),
        "described reference name missing: {system}"
    );
    assert!(
        system.contains("<description>Project docs</description>"),
        "described reference description missing: {system}"
    );
    assert!(
        !system.contains(BASE_PROMPT_MARKER),
        "non-empty Bundle prompt must replace base prompt: {system}"
    );
    assert!(
        !system.contains(LEGACY_PROMPT_MARKER),
        "legacy disk agent prompt must not reach provider: {system}"
    );
}

fn write_workdir_agents(workdir: &str, body: &str) {
    std::fs::write(format!("{workdir}/AGENTS.md"), body).unwrap();
}

fn write_workdir_skill(workdir: &str, name: &str) {
    let skill_dir = PathBuf::from(workdir).join(".hya/skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} skill\n---\n{name} body\n"),
    )
    .unwrap();
}

/// a. Bundle Some replaces only agent_base; current AGENTS + references once, in order.
#[tokio::test]
async fn bundle_prompt_replaces_base_but_preserves_guidance_once_and_in_order() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let workdir = workdir();
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
    write_workdir_agents(&workdir, AGENTS_MARKER);
    write_workdir_skill(&workdir, "session-skill");

    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("hya-main").prompt(BUNDLE_PROMPT_MARKER),
            AgentFixture::main("build"),
            AgentFixture::subagent("general"),
            AgentFixture::subagent("compaction").prompt("compaction prompt"),
            AgentFixture::subagent("title").prompt("title prompt"),
            AgentFixture::subagent("summary").prompt("summary prompt"),
        ],
    );
    let app = router(
        state_with_runtime(
            &workdir,
            Arc::clone(&requests),
            runtime,
            "hya-main",
            BASE_PROMPT_MARKER,
        )
        .await,
    );

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": { "path": "docs", "description": REFERENCE_DESC_MARKER }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session_with_agent(app.clone(), &workdir, "hya-main").await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "compose order check"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let requests = requests.lock().unwrap();
    let Some(system) = requests[0].system.as_deref() else {
        panic!("system prompt");
    };
    let Some(bundle_pos) = system.find(BUNDLE_PROMPT_MARKER) else {
        panic!("bundle base missing");
    };
    let Some(agents_pos) = system.find(AGENTS_MARKER) else {
        panic!("workdir AGENTS.md project guidance missing");
    };
    let Some(refs_pos) = system.find("<available_references>") else {
        panic!("reference guidance missing");
    };
    let Some(skill_pos) = system.find("These skills are available on demand") else {
        panic!("skill section missing");
    };
    assert!(
        bundle_pos < agents_pos && agents_pos < refs_pos && refs_pos < skill_pos,
        "order must be Bundle base → AGENTS once → references once → skills: {system}"
    );
    assert_eq!(system.matches(BUNDLE_PROMPT_MARKER).count(), 1);
    assert_eq!(
        system.matches(AGENTS_MARKER).count(),
        1,
        "AGENTS marker must appear exactly once: {system}"
    );
    assert_eq!(system.matches("<available_references>").count(), 1);
    assert!(system.contains(REFERENCE_DESC_MARKER));
    assert!(system.contains("## Project context:"));
    assert!(!system.contains(BASE_PROMPT_MARKER));
}

/// b. Bundle prompt None: Harness base → Environment/project AGENTS → references, each once.
#[tokio::test]
async fn bundle_prompt_none_preserves_harness_base_then_guidance() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let workdir = workdir();
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
    write_workdir_agents(&workdir, AGENTS_MARKER);

    let tools = Arc::new(ToolRegistry::builtins());
    // No Bundle prompt: agent_base stays the Harness/server system prompt.
    let runtime = runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("hya-main"),
            AgentFixture::main("build"),
            AgentFixture::subagent("general"),
            AgentFixture::subagent("compaction").prompt("compaction prompt"),
            AgentFixture::subagent("title").prompt("title prompt"),
            AgentFixture::subagent("summary").prompt("summary prompt"),
        ],
    );
    let app = router(
        state_with_runtime(
            &workdir,
            Arc::clone(&requests),
            runtime,
            "hya-main",
            BASE_PROMPT_MARKER,
        )
        .await,
    );

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": { "path": "docs", "description": REFERENCE_DESC_MARKER }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session_with_agent(app.clone(), &workdir, "hya-main").await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "preserve harness base"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let requests = requests.lock().unwrap();
    let Some(system) = requests[0].system.as_deref() else {
        panic!("system prompt");
    };
    let Some(base_pos) = system.find(BASE_PROMPT_MARKER) else {
        panic!("harness base missing");
    };
    let Some(env_pos) = system.find("## Environment") else {
        panic!("Environment missing");
    };
    let Some(agents_pos) = system.find(AGENTS_MARKER) else {
        panic!("workdir AGENTS.md project guidance missing");
    };
    let Some(refs_pos) = system.find("<available_references>") else {
        panic!("reference guidance missing");
    };
    assert!(
        base_pos < env_pos && env_pos < agents_pos && agents_pos < refs_pos,
        "Harness base → Environment/AGENTS → references when Bundle prompt is None: {system}"
    );
    assert_eq!(system.matches(BASE_PROMPT_MARKER).count(), 1);
    assert_eq!(
        system.matches(AGENTS_MARKER).count(),
        1,
        "AGENTS marker must appear exactly once: {system}"
    );
    assert_eq!(system.matches("<available_references>").count(), 1);
    assert!(system.contains(REFERENCE_DESC_MARKER));
    assert!(!system.contains(BUNDLE_PROMPT_MARKER));
}

/// c. User/task input stays a separate user provider message, not system guidance.
#[tokio::test]
async fn user_input_is_separate_provider_message() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let workdir = workdir();
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
    let user_marker = "USER_TASK_TEXT_MUST_NOT_BE_IN_SYSTEM";

    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("hya-main").prompt(BUNDLE_PROMPT_MARKER),
            AgentFixture::main("build"),
            AgentFixture::subagent("general"),
            AgentFixture::subagent("compaction").prompt("compaction prompt"),
            AgentFixture::subagent("title").prompt("title prompt"),
            AgentFixture::subagent("summary").prompt("summary prompt"),
        ],
    );
    let app = router(
        state_with_runtime(
            &workdir,
            Arc::clone(&requests),
            runtime,
            "hya-main",
            BASE_PROMPT_MARKER,
        )
        .await,
    );

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": { "path": "docs", "description": "Project docs" }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session_with_agent(app.clone(), &workdir, "hya-main").await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": user_marker}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let requests = requests.lock().unwrap();
    let req = &requests[0];
    let Some(system) = req.system.as_deref() else {
        panic!("system prompt");
    };
    assert!(
        !system.contains(user_marker),
        "user input must not be folded into system: {system}"
    );
    assert!(system.contains("<available_references>"));

    let user_texts: Vec<String> = req
        .messages
        .iter()
        .filter_map(|message| match message {
            hya_proto::Message::User { parts, .. } => {
                let text = parts
                    .iter()
                    .filter_map(|part| match part {
                        hya_proto::Part::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                Some(text)
            }
            _ => None,
        })
        .collect();
    assert!(
        user_texts.iter().any(|text| text.contains(user_marker)),
        "user marker must appear in a user provider message: {user_texts:?}"
    );
}

/// d. Guidance is captured once per turn and reused across provider rounds.
///
/// Round 1 issues a tool call; before round 2 is recorded, a sync hook rewrites
/// workdir `AGENTS.md` from V1 → V2 (real rediscovery source). Both rounds must
/// retain V1 and exclude V2.
#[tokio::test]
async fn guidance_captured_once_across_provider_rounds() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let workdir = workdir();
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
    let guide_path = format!("{workdir}/docs/guide.txt");
    std::fs::write(&guide_path, "body\n").unwrap();
    let agents_path = format!("{workdir}/AGENTS.md");
    write_workdir_agents(&workdir, AGENTS_MARKER);

    let agents_path_for_hook = agents_path.clone();
    let tools = Arc::new(ToolRegistry::builtins());
    let providers = Arc::new(
        ProviderRouter::new().with(Arc::new(MultiRoundRecordingProvider {
            requests: Arc::clone(&requests),
            tool_path: guide_path,
            on_before_round2: Some(Arc::new(move || {
                assert!(
                    std::fs::write(&agents_path_for_hook, AGENTS_MARKER_V2).is_ok(),
                    "rewrite AGENTS.md before round 2"
                );
            })),
        })),
    );
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let runtime = runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("hya-main").prompt(BUNDLE_PROMPT_MARKER),
            AgentFixture::main("build"),
            AgentFixture::subagent("general"),
            AgentFixture::subagent("compaction").prompt("compaction prompt"),
            AgentFixture::subagent("title").prompt("title prompt"),
            AgentFixture::subagent("summary").prompt("summary prompt"),
        ],
    );
    let engine = SessionEngine::new(store, providers, runtime, perm, EventBus::default());
    let app = router(AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            system_prompt: BASE_PROMPT_MARKER.to_string(),
            workdir: workdir.clone().into(),
            reasoning: None,
        }),
    ));

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": {
                    "path": "docs",
                    "description": REFERENCE_DESC_MARKER
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let session = create_session_with_agent(app.clone(), &workdir, "hya-main").await;
    let (status, _message) = request_json(
        app,
        Method::POST,
        &format!("/session/{session}/message"),
        json!({"text": "multi-round guidance capture"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Source file was mutated after capture; on-disk content is V2.
    let on_disk = std::fs::read_to_string(&agents_path).unwrap();
    assert!(
        on_disk.contains(AGENTS_MARKER_V2),
        "AGENTS.md must be V2 after mid-turn rewrite: {on_disk}"
    );

    let recorded = requests.lock().unwrap();
    assert!(
        recorded.len() >= 2,
        "expected multi-round provider calls, got {}",
        recorded.len()
    );
    let Some(system_r1) = recorded[0].system.as_deref() else {
        panic!("r1 system");
    };
    let Some(system_r2) = recorded[1].system.as_deref() else {
        panic!("r2 system");
    };
    assert_eq!(
        system_r1, system_r2,
        "effective system must be reused across provider rounds"
    );
    assert!(
        system_r1.contains(AGENTS_MARKER),
        "captured AGENTS V1 missing: {system_r1}"
    );
    assert!(
        !system_r1.contains(AGENTS_MARKER_V2),
        "mid-turn AGENTS rewrite must not affect already-captured guidance: {system_r1}"
    );
    assert_eq!(system_r1.matches(AGENTS_MARKER).count(), 1);
    assert_eq!(system_r1.matches("<available_references>").count(), 1);
    assert_eq!(system_r1.matches(BUNDLE_PROMPT_MARKER).count(), 1);
}

struct MultiRoundRecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    tool_path: String,
    /// Invoked once when the second provider round starts (after tools).
    on_before_round2: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[async_trait]
impl Provider for MultiRoundRecordingProvider {
    fn id(&self) -> &str {
        "multi-round-recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        let round = {
            let mut guard = self.requests.lock().unwrap();
            let next = guard.len() + 1;
            if next == 2
                && let Some(hook) = &self.on_before_round2
            {
                // Mutate guidance *source* before this round's request is stored.
                // If core rediscovered, the recorded system would reflect V2.
                hook();
            }
            guard.push(req);
            next
        };
        if round == 1 {
            let part = hya_proto::PartId::new();
            let call = hya_proto::ToolCallId::new();
            let name = hya_proto::ToolName::new("read");
            Ok(Box::pin(stream::iter([
                Ok(Event::ToolCallRequested {
                    session,
                    message,
                    part,
                    call,
                    name,
                    input: json!({ "filePath": self.tool_path }),
                }),
                Ok(Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: FinishReason::ToolCalls,
                    tokens: None,
                }),
            ])))
        } else {
            Ok(Box::pin(stream::iter([Ok(Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
            })])))
        }
    }
}
