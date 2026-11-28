//! Direct shell execution, prompt media/file attachments, and session-vs-agent
//! workdir precedence (using local skill discovery with zero `$HOME` mutation).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_engine::hooks::{NativeToolAfterInput, NativeToolAfterOutcome, NativeToolResult};
use hya_engine::{AgentSpec, EventBus, NativeHook, SessionEngine};
use hya_proto::{
    Event, FinishReason, Message, MessageId, ModelRef, Part, PartProjection, Role, SessionId,
    ToolPartState,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, HttpProvider, Provider,
    ProviderError, ProviderKind, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, Rule,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

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
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

#[derive(Default)]
struct OversizedDirectResultHook {
    captured_output_path: std::sync::Mutex<Option<String>>,
}

#[async_trait]
impl NativeHook for OversizedDirectResultHook {
    fn id(&self) -> &str {
        "OversizedDirectResultHook"
    }
    fn hook_points(&self) -> &[hya_runtime_abi::HookPoint] {
        &[hya_runtime_abi::HookPoint::ToolExecuteAfter]
    }

    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, hya_runtime_abi::HostError> {
        if let NativeToolResult::Ok { output, .. } = &input.result
            && let Some(path) = output["metadata"]["outputPath"].as_str()
        {
            *self.captured_output_path.lock().unwrap() = Some(path.to_string());
        }
        Ok(NativeToolAfterOutcome::Continue {
            result: NativeToolResult::Ok {
                output: json!({
                    "title": "T".repeat(4096),
                    "output": "O".repeat(300 * 1024),
                    "metadata": {
                        "display": { "text": "D".repeat(100 * 1024) },
                        "unexpected": "U".repeat(100 * 1024),
                        "outputPath": "not-owned-by-bash.txt",
                    }
                }),
                time_ms: 0,
            },
        })
    }
}

async fn policy_shell_engine(
    rules: PermissionRules,
    policy: InvocationPolicy,
) -> (
    SessionEngine,
    tokio::sync::mpsc::UnboundedReceiver<hya_tool::AskRequest>,
    SessionId,
    AgentSpec,
    support::TestDir,
) {
    let dir = support::TestDir::new("shell-policy");
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, asks) = PermissionPlane::new_with_policy(rules, policy);
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::builtin_test_runtime().runtime,
        permission,
        EventBus::default(),
    );
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    let mut agent = support::agent_spec("hya-main", "fake", dir.path());
    agent.system_prompt = "x".into();
    (engine, asks, session, agent, dir)
}

async fn shell_part_state(
    engine: &SessionEngine,
    session: SessionId,
    message: MessageId,
) -> ToolPartState {
    let projection = engine.store().read_projection(session).await.unwrap();
    projection
        .session
        .messages
        .iter()
        .find(|m| m.id == message)
        .and_then(|m| {
            m.parts.iter().find_map(|p| match p {
                PartProjection::Tool { name, state, .. } if name.as_str() == "bash" => {
                    Some(state.clone())
                }
                _ => None,
            })
        })
        .expect("direct shell tool part")
}

#[tokio::test]
async fn direct_shell_runs_command_records_artifact_and_binding() {
    let dir = support::TestDir::new("shell-direct");
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted(vec![])),
        PermissionRules::new(vec![Rule::new(Action::Bash, "**", Mode::Allow)]),
    )
    .await;
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    let agent = support::agent_spec("hya-main", "fake", dir.path());

    let (assistant, finish) = engine
        .run_shell(
            session,
            &agent,
            "python3 -c 'import sys; sys.stdout.write(\"direct-shell-ok\" + \"A\" * 51201)'".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    let projection = engine.store().read_projection(session).await.unwrap();
    assert!(projection.session.messages.iter().any(|m| {
        m.role == Role::User
            && m.parts.iter().any(|p| {
                matches!(p, PartProjection::Text { text, .. } if text == "The following tool was executed by the user")
            })
    }));
    let msg = projection
        .session
        .messages
        .iter()
        .find(|m| m.id == assistant)
        .expect("assistant shell message");
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(msg.finish, Some(FinishReason::Stop));
    let generation = msg.config_generation.expect("direct shell binding");
    let output = msg
        .parts
        .iter()
        .find_map(|p| match p {
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "bash" => Some(output),
            _ => None,
        })
        .expect("completed bash output");
    assert!(
        output["output"]
            .as_str()
            .unwrap()
            .contains("direct-shell-ok")
    );
    let artifact = output["metadata"]["outputPath"]
        .as_str()
        .expect("durable Bash artifact path");
    assert!(Path::new(artifact).is_file());
    let bindings: Vec<_> = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::TurnBindingRecorded {
                message,
                generation,
                ..
            } if message == assistant => Some(generation),
            _ => None,
        })
        .collect();
    assert_eq!(bindings, vec![generation]);
}

#[tokio::test]
async fn direct_shell_bypasses_ask_but_honours_explicit_deny_rules() {
    // 1. Default policy would ask; user's direct shell runs unprompted.
    let (engine, mut asks, session, agent, _dir) = policy_shell_engine(
        PermissionRules::default(),
        InvocationPolicy::compile(PermissionModel::Default, Vec::new()).unwrap(),
    )
    .await;
    let (message, finish) = tokio::time::timeout(
        Duration::from_secs(5),
        engine.run_shell(
            session,
            &agent,
            "printf direct-policy".into(),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    assert!(asks.try_recv().is_err());
    assert!(matches!(
        shell_part_state(&engine, session, message).await,
        ToolPartState::Completed { output, .. } if output["output"].as_str().unwrap().contains("direct-policy")
    ));

    // 2. Both explicit InvocationRule Command Deny and PermissionRules Bash Deny block.
    for (rules, policy) in [
        (
            PermissionRules::default(),
            InvocationPolicy::compile(
                PermissionModel::Default,
                vec![InvocationRule::new(
                    PermissionTarget::Command,
                    "^printf ",
                    Mode::Deny,
                )],
            )
            .unwrap(),
        ),
        (
            PermissionRules::new(vec![Rule::new(Action::Bash, "printf *", Mode::Deny)]),
            InvocationPolicy::compile(PermissionModel::Default, Vec::new()).unwrap(),
        ),
    ] {
        let (engine, mut asks, session, agent, _dir) = policy_shell_engine(rules, policy).await;
        let (message, finish) = tokio::time::timeout(
            Duration::from_secs(5),
            engine.run_shell(
                session,
                &agent,
                "printf denied".into(),
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(finish, FinishReason::Error);
        assert!(asks.try_recv().is_err());
        assert!(matches!(
            shell_part_state(&engine, session, message).await,
            ToolPartState::Error { .. }
        ));
    }
}

#[tokio::test]
async fn direct_shell_caps_oversized_post_hook_coding_envelope() {
    let dir = support::TestDir::new("shell-cap");
    let hook = Arc::new(OversizedDirectResultHook::default());
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted(vec![])),
        PermissionRules::new(vec![Rule::new(Action::Bash, "**", Mode::Allow)]),
    )
    .await
    .with_hooks(Arc::clone(&hook) as Arc<dyn NativeHook>);
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    let agent = support::agent_spec("hya-main", "fake", dir.path());

    let (assistant, finish) = tokio::time::timeout(
        Duration::from_secs(8),
        engine.run_shell(
            session,
            &agent,
            "python3 -c 'import sys; sys.stdout.write(\"A\" * 51201)'".into(),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let ToolPartState::Completed { output, .. } =
        shell_part_state(&engine, session, assistant).await
    else {
        panic!("expected completed direct shell output");
    };
    assert!(serde_json::to_vec(&output).unwrap().len() <= 256 * 1024);
    assert!(output["title"].as_str().unwrap().len() <= 512);
    assert!(output["output"].as_str().unwrap().len() <= 50 * 1024);
    assert_eq!(output["metadata"]["truncated"], true);
    assert_eq!(output["metadata"]["titleTruncated"], true);
    assert_eq!(output["metadata"]["outputTruncated"], true);
    assert!(output["metadata"].get("outputPath").is_none());
    assert!(output["metadata"].get("unexpected").is_none());
    let spilled = hook
        .captured_output_path
        .lock()
        .unwrap()
        .clone()
        .expect("direct bash must have spilled oversized output before hook rewrite");
    assert!(
        !Path::new(&spilled).exists(),
        "unpublished bash spill artifact must be deleted: {spilled}"
    );
}

#[tokio::test]
async fn prompt_media_and_file_context_replay_correctly() {
    // 1. Compat data-URI image attachment replays as Part::Media.
    let dir = support::TestDir::new("attach-media");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let engine = support::engine_with_provider(
        Arc::new(RecordingProvider {
            requests: Arc::clone(&requests),
        }),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    let message = engine
        .admit_user_prompt(session, "inspect attached image".into())
        .await
        .unwrap();
    engine
        .record_user_prompt_context(
            session,
            message,
            vec![json!({
                "uri": "data:image/png;base64,aGVsbG8=",
                "mime": "image/png",
                "name": "pixel.png",
                "description": "tiny fixture",
            })],
            Vec::new(),
        )
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &support::agent_spec("build", "fake", dir.path()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    {
        let requests = requests.lock().unwrap();
        let Message::User { parts, .. } = &requests[0].messages[0] else {
            panic!("expected user message");
        };
        assert!(parts.iter().any(|part| matches!(
            part,
            Part::Media { media_type, data, filename, .. }
                if media_type == "image/png"
                    && data == "data:image/png;base64,aGVsbG8="
                    && filename.as_deref() == Some("pixel.png")
        )));
    }

    // 2. Text/directory/resource references stay non-media with native guidance.
    let dir = support::TestDir::new("attach-file");
    let text_path = dir.path().join("notes.txt");
    std::fs::write(&text_path, "reference body").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let engine = support::engine_with_provider(
        Arc::new(RecordingProvider {
            requests: Arc::clone(&requests),
        }),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "build", "fake", dir.path()).await;
    let message = engine
        .admit_user_prompt(session, "Read @notes.txt".into())
        .await
        .unwrap();
    engine
        .record_user_prompt_context(
            session,
            message,
            vec![
                json!({
                    "type": "file",
                    "mime": "text/plain",
                    "filename": "notes.txt",
                    "url": format!("file://{}", text_path.to_string_lossy()),
                    "source": {
                        "type": "file",
                        "path": text_path.to_string_lossy(),
                        "text": {"value": "@notes.txt", "start": 5, "end": 15}
                    }
                }),
                json!({
                    "type": "file",
                    "mime": "application/x-directory",
                    "filename": "reference",
                    "url": format!("file://{}", dir.path().display()),
                    "source": {"type": "file", "path": dir.path().to_string_lossy()}
                }),
                json!({
                    "type": "file",
                    "mime": "application/json",
                    "filename": "resource",
                    "url": "luna://test/resource",
                    "source": {"type": "resource", "clientName": "fixture", "uri": "luna://test/resource"}
                }),
            ],
            Vec::new(),
        )
        .await
        .unwrap();
    let mut agent = support::agent_spec("build", "fake", dir.path());
    agent.system_prompt = "base system".into();
    engine
        .run_turn_with_external_dirs_and_guidance(
            session,
            &agent,
            CancellationToken::new(),
            &[dir.path().to_path_buf()],
            Some(Arc::from("NATIVE_GUIDANCE_MARKER")),
            None,
            None,
        )
        .await
        .unwrap();
    let requests = requests.lock().unwrap();
    assert!(
        requests[0]
            .system
            .as_deref()
            .is_some_and(|s| s.contains("NATIVE_GUIDANCE_MARKER"))
    );
    let Message::User { parts, .. } = &requests[0].messages[0] else {
        panic!("expected user message");
    };
    assert!(parts.iter().all(|p| !matches!(p, Part::Media { .. })));
    assert!(
        parts
            .iter()
            .any(|p| matches!(p, Part::Text { text, .. } if text == "Read @notes.txt"))
    );
}

async fn start_responses_capture_server(
    request_count: usize,
) -> (String, Arc<AsyncMutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AsyncMutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    tokio::spawn(async move {
        for _ in 0..request_count {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            let header_end = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + content_length {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&chunk[..read]);
            }
            let body =
                serde_json::from_slice::<Value>(&bytes[header_end..header_end + content_length])
                    .unwrap();
            captured.lock().await.push(body);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\ndata: [DONE]\n\n",
                )
                .await
                .unwrap();
        }
    });
    (format!("http://{addr}/v1"), requests)
}

#[tokio::test]
async fn image_attachment_reaches_responses_and_replays_in_order() {
    let dir = support::TestDir::new("attach-responses");
    let (base_url, requests) = start_responses_capture_server(2).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiResponse,
        &base_url,
        Some("test-token".into()),
        ["gpt-5.6-luna".to_string()],
    )
    .unwrap();
    let engine =
        support::engine_with_provider(Arc::new(provider), PermissionRules::default()).await;
    let agent = support::agent_spec("build", "openai/gpt-5.6-luna", dir.path());
    let session =
        support::create_root_session(&engine, "build", "openai/gpt-5.6-luna", dir.path()).await;
    let first = engine
        .admit_user_prompt(session, "inspect attached image".into())
        .await
        .unwrap();
    let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=";
    engine
        .record_user_prompt_context(
            session,
            first,
            vec![json!({
                "type": "file",
                "url": image,
                "mime": "image/png",
                "filename": "red.png",
            })],
            Vec::new(),
        )
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "follow up".into())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    let expected = json!([
        {"type": "input_text", "text": "inspect attached image"},
        {"type": "input_image", "image_url": image},
    ]);
    assert_eq!(requests[0]["input"][0]["role"], "user");
    assert_eq!(requests[0]["input"][0]["content"], expected);
    assert_eq!(requests[1]["input"][0]["content"], expected);
    assert_eq!(
        requests[1]["input"][1],
        json!({"role": "user", "content": "follow up"})
    );
}

#[tokio::test]
async fn session_workdir_wins_over_agent_workdir_for_skills() {
    let agent_dir = support::TestDir::new("workdir-agent");
    let session_dir = support::TestDir::new("workdir-session");
    agent_dir.write_skill_with("agent-only", "Agent skill", "agent body\n");
    session_dir.write_skill_with("b-session", "B skill", "b body\n");
    session_dir.write_skill_with("a-session", "A skill", "a body\n");

    let requests = Arc::new(Mutex::new(Vec::new()));
    let engine = support::engine_with_provider(
        Arc::new(RecordingProvider {
            requests: Arc::clone(&requests),
        }),
        PermissionRules::default(),
    )
    .await;
    let session =
        support::create_root_session(&engine, "hya-main", "fake", session_dir.path()).await;
    engine
        .admit_user_prompt(session, "record prompt".into())
        .await
        .unwrap();
    let mut agent = support::agent_spec("hya-main", "fake", agent_dir.path());
    agent.system_prompt = "base prompt".into();

    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let requests = requests.lock().unwrap();
    let system = requests[0].system.as_deref().unwrap();
    assert_eq!(
        system
            .matches("Skills found at session start; use the skill tool to load a current skill:")
            .count(),
        1
    );
    assert!(!system.contains("Available skills"));
    assert!(
        system.find("You are hya-main").unwrap()
            < system.find("Skills found at session start").unwrap()
    );
    assert!(system.find("- a-session").unwrap() < system.find("- b-session").unwrap());
    assert!(system.contains("a-session") && system.contains("A skill"));
    assert!(system.contains("b-session") && system.contains("B skill"));
    assert!(!system.contains("agent-only") && !system.contains("Agent skill"));
}

#[tokio::test]
async fn session_workdir_wins_over_agent_workdir_for_tool_and_direct_shell() {
    let agent_dir = support::TestDir::new("workdir-bash-agent");
    let session_dir = support::TestDir::new("workdir-bash-session");
    let session_canon = std::fs::canonicalize(session_dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let agent_canon = std::fs::canonicalize(agent_dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let bash_rules = || PermissionRules::new(vec![Rule::new(Action::Bash, "**", Mode::Allow)]);

    // 1. Model `bash` tool runs in session_dir.
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "bash".into(),
                input: json!({ "command": "pwd" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let engine = support::engine_with_provider(Arc::new(provider), bash_rules()).await;
    let session =
        support::create_root_session(&engine, "hya-main", "fake", session_dir.path()).await;
    engine
        .admit_user_prompt(session, "pwd please".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("hya-main", "fake", agent_dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let projection = engine.store().read_projection(session).await.unwrap();
    let out = projection
        .session
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .find_map(|p| match p {
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { input, output, .. },
                ..
            } if name.as_str() == "bash" && input["command"].as_str() == Some("pwd") => {
                output["output"].as_str().map(str::to_string)
            }
            _ => None,
        })
        .expect("tool pwd output");
    assert!(out.contains(&session_canon) && !out.contains(&agent_canon));

    // 2. Direct `run_shell` runs in session_dir.
    let engine =
        support::engine_with_provider(Arc::new(FakeProvider::scripted(vec![])), bash_rules()).await;
    let session =
        support::create_root_session(&engine, "hya-main", "fake", session_dir.path()).await;
    let (msg, finish) = engine
        .run_shell(
            session,
            &support::agent_spec("hya-main", "fake", agent_dir.path()),
            "pwd".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let ToolPartState::Completed { output, .. } = shell_part_state(&engine, session, msg).await
    else {
        panic!("expected completed shell pwd");
    };
    let direct_out = output["output"].as_str().unwrap();
    assert!(direct_out.contains(&session_canon) && !direct_out.contains(&agent_canon));
}
