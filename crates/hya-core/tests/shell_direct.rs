//! Integration tests for `hya-core`: shell direct.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_core::{
    AgentSpec, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CreateSession, EventBus, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, SessionEngine, TextCompleteInput, TextCompleteOutcome,
    ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, ModelRef, PartProjection, Role, ToolPartState,
};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    Action, Decision, ExactSubject, InvocationPolicy, Mode, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, RememberScope, Rule, ToolRegistry,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
/// Remove a temporary workspace when a test exits, including assertion failures.
struct TempDirGuard(PathBuf);

impl TempDirGuard {
    /// Retain a workspace path for best-effort test cleanup.
    fn new(path: &Path) -> Self {
        Self(path.to_path_buf())
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-core-shell-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn direct_shell_runs_command_and_records_tool_part() {
    // Given
    let dir = tempdir();
    let _cleanup = TempDirGuard::new(&dir);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };

    // When
    let (assistant, finish) = engine
        .run_shell(
            session,
            &agent,
            "python3 -c 'import sys; sys.stdout.write(\"direct-shell-ok\" + \"A\" * 51201)'"
                .to_string(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(finish, FinishReason::Stop);
    let projection = engine.store().read_projection(session).await.unwrap();
    assert!(projection.session.messages.iter().any(|message| {
        message.role == Role::User
            && message.parts.iter().any(|part| {
                matches!(
                    part,
                    PartProjection::Text { text, .. }
                        if text == "The following tool was executed by the user"
                )
            })
    }));
    let assistant_message = projection
        .session
        .messages
        .iter()
        .find(|message| message.id == assistant)
        .expect("assistant shell message");
    assert_eq!(assistant_message.role, Role::Assistant);
    let generation = assistant_message
        .config_generation
        .expect("direct shell runtime binding");
    assert_eq!(assistant_message.finish, Some(FinishReason::Stop));
    assert!(assistant_message.parts.iter().any(|part| {
        matches!(
            part,
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "bash" && output["output"].as_str().unwrap().contains("direct-shell-ok")
        )
    }));
    let output_path = assistant_message
        .parts
        .iter()
        .find_map(|part| match part {
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "bash" => output["metadata"]["outputPath"].as_str(),
            _ => None,
        })
        .expect("durable Bash result must retain its complete artifact path");
    assert!(Path::new(output_path).is_file());
    let bindings = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::TurnBindingRecorded {
                message,
                generation,
                ..
            } if message == assistant => Some(generation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(bindings, vec![generation]);
}

#[tokio::test]
async fn direct_shell_authorizes_once_with_call_correlation() {
    let dir = tempdir();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, mut asks) = PermissionPlane::new_with_policy(
        PermissionRules::default(),
        InvocationPolicy::compile(PermissionModel::Default, Vec::new()).unwrap(),
    );
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir,
        reasoning: None,
    };
    let runner = engine.clone();
    let task = tokio::spawn(async move {
        runner
            .run_shell(
                session,
                &agent,
                "printf direct-policy".to_string(),
                CancellationToken::new(),
            )
            .await
    });

    let request = tokio::time::timeout(std::time::Duration::from_secs(1), asks.recv())
        .await
        .expect("permission request timeout")
        .expect("permission request");
    let correlation = (request.session, request.message_id, request.call_id);
    let remember = request.remember.clone();
    request.reply.send(Decision::AllowOnce).unwrap();
    let (message, finish) = task.await.unwrap().unwrap();

    assert_eq!(finish, FinishReason::Stop);
    assert_eq!(correlation.0, Some(session));
    assert_eq!(correlation.1, Some(message));
    assert!(correlation.2.is_some());
    assert_eq!(
        remember,
        RememberScope::Exact(ExactSubject::new(
            PermissionTarget::Command,
            "printf direct-policy",
        ))
    );
    assert!(asks.try_recv().is_err(), "direct shell must prompt once");
}

struct OversizedDirectResultHook;

#[async_trait::async_trait]
impl HookDispatcher for OversizedDirectResultHook {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, _input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: ToolOutcomeNative::Ok {
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
        }
    }
}

#[tokio::test]
async fn direct_shell_caps_oversized_post_hook_coding_envelope() {
    // Given: a direct Bash call whose after-hook returns an oversized envelope.
    let dir = tempdir();
    let _cleanup = TempDirGuard::new(&dir);
    let artifact_root = dir.join(".hya/tool-output");
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_hooks(Arc::new(OversizedDirectResultHook));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir.clone(),
        reasoning: None,
    };

    // When: the direct shell path persists the after-hook result.
    let (assistant, finish) = tokio::time::timeout(
        Duration::from_secs(8),
        engine.run_shell(
            session,
            &agent,
            "python3 -c 'import sys; sys.stdout.write(\"A\" * 51201)'".to_string(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("direct shell cap test exceeded its outer guard")
    .unwrap();

    // Then: persistence keeps a bounded structured coding envelope.
    assert_eq!(finish, FinishReason::Stop);
    let projection = engine.store().read_projection(session).await.unwrap();
    let assistant_message = projection
        .session
        .messages
        .iter()
        .find(|message| message.id == assistant)
        .expect("assistant shell message");
    let output = assistant_message
        .parts
        .iter()
        .find_map(|part| match part {
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "bash" => Some(output),
            _ => None,
        })
        .expect("direct Bash result");
    let serialized = serde_json::to_vec(output).unwrap();
    assert!(
        serialized.len() <= 256 * 1024,
        "direct coding result exceeded the envelope bound: {} bytes",
        serialized.len()
    );
    assert!(output["title"].as_str().unwrap().len() <= 512);
    assert!(output["output"].as_str().unwrap().len() <= 50 * 1024);
    assert_eq!(output["metadata"]["truncated"], true);
    assert_eq!(output["metadata"]["titleTruncated"], true);
    assert_eq!(output["metadata"]["outputTruncated"], true);
    assert!(output["metadata"].get("outputPath").is_none());
    assert!(output["metadata"].get("unexpected").is_none());
    let mut artifacts = tokio::fs::read_dir(&artifact_root).await.unwrap();
    assert!(
        artifacts.next_entry().await.unwrap().is_none(),
        "a direct after-hook rewrite must remove the pre-hook Bash artifact"
    );
}
