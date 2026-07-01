#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, PartProjection, Role, ToolPartState};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static ENV_LOCK: AtomicBool = AtomicBool::new(false);

struct HomeGuard {
    previous: Option<OsString>,
}

impl HomeGuard {
    fn set(home: &Path) -> Self {
        while ENV_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
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
        ENV_LOCK.store(false, Ordering::Release);
    }
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-core-workdir-test-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_skill(root: &Path, name: &str, description: &str, body: &str) {
    let dir = root.join(".hya/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

fn agent(workdir: PathBuf) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "base prompt".to_string(),
        workdir,
        reasoning: None,
    }
}

async fn engine(
    provider_router: Arc<ProviderRouter>,
    tools: Arc<ToolRegistry>,
    rules: PermissionRules,
) -> SessionEngine {
    let store = SessionStore::connect_memory().await.unwrap();
    let (permission, _rx) = PermissionPlane::new(rules);
    SessionEngine::new(
        store,
        provider_router,
        tools,
        permission,
        EventBus::default(),
    )
}

struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait::async_trait]
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
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
            },
        )])))
    }
}

fn bash_rules() -> PermissionRules {
    PermissionRules::new(vec![Rule::new(Action::Bash, "**", Mode::Allow)])
}

fn completed_shell_output(projection: &hya_proto::Projection) -> String {
    projection
        .session
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .find_map(|part| match part {
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { input, output, .. },
                ..
            } if name.as_str() == "shell" && input["command"].as_str() == Some("pwd") => {
                output["output"].as_str().map(str::to_string)
            }
            _ => None,
        })
        .expect("completed shell pwd output")
}

#[tokio::test]
async fn run_turn_injects_one_ordered_skill_section_from_session_workdir_not_agent_workdir() {
    let home_dir = tempdir();
    let agent_dir = tempdir();
    let session_dir = tempdir();
    let _home = HomeGuard::set(&home_dir);
    write_skill(&agent_dir, "agent-only", "Agent skill", "agent body\n");
    write_skill(&session_dir, "b-session", "B skill", "b body\n");
    write_skill(&session_dir, "a-session", "A skill", "a body\n");

    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider {
        requests: Arc::clone(&requests),
    })));
    let engine = engine(
        provider_router,
        Arc::new(ToolRegistry::builtins()),
        PermissionRules::default(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: session_dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "record prompt".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(session, &agent(agent_dir), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    let requests = requests.lock().unwrap();
    let system = requests[0].system.as_deref().unwrap();
    assert_eq!(
        system
            .matches("These skills are available on demand; read the named SKILL.md when relevant:")
            .count(),
        1
    );
    assert!(!system.contains("Available skills"));
    assert!(
        system.find("base prompt").unwrap()
            < system.find("These skills are available on demand").unwrap()
    );
    assert!(
        system.find("- a-session: A skill").unwrap() < system.find("- b-session: B skill").unwrap()
    );
    assert!(system.contains("- a-session: A skill"));
    assert!(system.contains("- b-session: B skill"));
    assert!(!system.contains("agent-only"));
    assert!(!system.contains("Agent skill"));
}

#[tokio::test]
async fn run_turn_shell_tool_uses_session_workdir_not_agent_workdir() {
    let home_dir = tempdir();
    let agent_dir = tempdir();
    let session_dir = tempdir();
    let _home = HomeGuard::set(&home_dir);
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "shell".to_string(),
                input: json!({ "command": "pwd" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let engine = engine(
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(ToolRegistry::builtins()),
        bash_rules(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: session_dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "pwd please".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(session, &agent(agent_dir.clone()), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    let projection = engine.store().read_projection(session).await.unwrap();
    let out = completed_shell_output(&projection);
    assert!(
        out.contains(
            std::fs::canonicalize(&session_dir)
                .unwrap()
                .to_str()
                .unwrap()
        ),
        "output was {out:?}"
    );
    assert!(
        !out.contains(std::fs::canonicalize(&agent_dir).unwrap().to_str().unwrap()),
        "output was {out:?}"
    );
}

#[tokio::test]
async fn run_shell_uses_session_workdir_not_agent_workdir() {
    let home_dir = tempdir();
    let agent_dir = tempdir();
    let session_dir = tempdir();
    let _home = HomeGuard::set(&home_dir);
    let engine = engine(
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![])))),
        Arc::new(ToolRegistry::builtins()),
        bash_rules(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: session_dir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();

    let (_assistant, finish) = engine
        .run_shell(
            session,
            &agent(agent_dir.clone()),
            "pwd".to_string(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(finish, FinishReason::Stop);
    let projection = engine.store().read_projection(session).await.unwrap();
    let out = completed_shell_output(&projection);
    assert!(
        out.contains(
            std::fs::canonicalize(&session_dir)
                .unwrap()
                .to_str()
                .unwrap()
        ),
        "output was {out:?}"
    );
    assert!(
        !out.contains(std::fs::canonicalize(&agent_dir).unwrap().to_str().unwrap()),
        "output was {out:?}"
    );
}
