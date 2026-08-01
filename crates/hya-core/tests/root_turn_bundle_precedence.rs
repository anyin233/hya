#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Root/main turn Bundle precedence for Commit 2.
//!
//! The stable agent definition for an existing root session is taken exactly
//! from the turn's captured TurnBinding catalog. Bundle model/category defaults
//! and prepared workdirs are not per-turn overrides; session model/workdir win.

mod support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::{AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, Role};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
    ReasoningEffort,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use tokio_util::sync::CancellationToken;

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

struct CaptureProvider {
    requests: Mutex<Vec<CompletionRequest>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    fn id(&self) -> &str {
        "capture"
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
        request: CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
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

#[derive(Clone)]
struct AgentFixture {
    stable_id: String,
    prompt: Option<String>,
    model: Option<String>,
    category: Option<String>,
    reasoning: Option<String>,
    workdir: Option<String>,
}

impl AgentFixture {
    fn new(stable_id: impl Into<String>) -> Self {
        Self {
            stable_id: stable_id.into(),
            prompt: None,
            model: None,
            category: None,
            reasoning: None,
            workdir: None,
        }
    }

    fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    fn reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    fn workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = Some(workdir.into());
        self
    }
}

fn catalog(agents: &[AgentFixture]) -> Arc<BundleCatalog> {
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/root-turn-precedence".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: agents
            .iter()
            .map(|agent| PreparedAgent {
                local_id: agent.stable_id.clone(),
                stable_id: AgentName::new(&agent.stable_id),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: agent.prompt.clone(),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy {
                    model: agent.model.clone(),
                    category: agent.category.clone(),
                    reasoning: agent.reasoning.clone(),
                },
                workdir: agent.workdir.clone(),
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view: ResourceView::default(),
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
            })
            .collect(),
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    Arc::new(BundleCatalog::from_prepared(&[bundle]).expect("valid precedence catalog"))
}

fn write_skill(root: &Path, name: &str) {
    let dir = root.join(".hya/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} skill\n---\n{name} body\n"),
    )
    .unwrap();
}

async fn engine_with(catalog: Arc<BundleCatalog>, provider: Arc<CaptureProvider>) -> SessionEngine {
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        ToolRegistry::builtins().snapshot(),
        catalog,
    ));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        runtime,
        permission,
        EventBus::default(),
    )
}

const BASE_MARKER: &str = "ROOT_TURN_BASE_PROMPT_MARKER";
const AGENTS_MARKER: &str = "ROOT_TURN_AGENTS_CONTEXT_MARKER";
const BUNDLE_PROMPT: &str = "EXACT_BUNDLE_PROMPT_FOR_EXPLORE";

fn composed_base(workdir: PathBuf) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("base-caller"),
        model: ModelRef::new("base-model"),
        system_prompt: [
            BASE_MARKER,
            "You are the composed base agent.",
            "",
            "## Project context: AGENTS.md",
            AGENTS_MARKER,
            "",
            "## Environment",
            "- cwd: /composed",
        ]
        .join("\n"),
        workdir,
        reasoning: Some(ReasoningEffort::Low),
    }
}

fn skill_header_count(system: &str) -> usize {
    system
        .matches("These skills are available on demand; read the named SKILL.md when relevant:")
        .count()
}

#[tokio::test]
async fn root_turn_missing_definition_fails_closed_without_general_fallback() {
    let home = support::TestDir::new("root-missing-home");
    let workdir = support::TestDir::new("root-missing-def");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("general")
            .model("bundle-general-model")
            .category("quick")]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("ghost"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "should not run".to_string())
        .await
        .unwrap();

    let err = engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .expect_err("missing root definition must fail closed");
    assert!(
        err.to_string().contains("AGENT_DEFINITION_MISSING"),
        "expected AGENT_DEFINITION_MISSING, got {err}"
    );
    assert!(
        provider.requests.lock().unwrap().is_empty(),
        "must not synthesize or fall back to general for a missing root definition"
    );
}

#[tokio::test]
async fn root_turn_prompt_none_preserves_composed_base_and_appends_skills_once() {
    let home = support::TestDir::new("root-prompt-none-home");
    let workdir = support::TestDir::new("root-prompt-none");
    let _home = HomeGuard::set(home.path());
    write_skill(workdir.path(), "session-skill");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("build")
            .model("bundle-default-model")
            .category("deep")
            .workdir("/bundle/must-not-win")]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "preserve base".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &composed_base(PathBuf::from("/agent/base")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let system = requests[0].system.as_deref().expect("system prompt");
    assert!(
        system.contains(BASE_MARKER),
        "prompt=None must preserve the composed base prompt: {system}"
    );
    assert!(
        system.contains(AGENTS_MARKER),
        "prompt=None must not erase AGENTS/context composition: {system}"
    );
    assert_eq!(skill_header_count(system), 1, "skill section once");
    assert!(system.contains("session-skill"));
    assert!(
        system.find(BASE_MARKER).unwrap()
            < system.find("These skills are available on demand").unwrap(),
        "skills append after preserved base"
    );
    assert!(
        !system.contains(BUNDLE_PROMPT),
        "prompt=None must not inject a Bundle prompt body"
    );
    assert_eq!(requests[0].model.as_str(), "session-model");
}

#[tokio::test]
async fn root_turn_bundle_prompt_replaces_base_then_appends_skills_once() {
    let home = support::TestDir::new("root-prompt-replace-home");
    let workdir = support::TestDir::new("root-prompt-replace");
    let _home = HomeGuard::set(home.path());
    write_skill(workdir.path(), "session-skill");
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("explore").prompt(BUNDLE_PROMPT)]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("explore"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "replace base".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let requests = provider.requests.lock().unwrap();
    let system = requests[0].system.as_deref().expect("system prompt");
    assert!(
        system.starts_with(BUNDLE_PROMPT),
        "non-empty Bundle prompt replaces the base once: {system}"
    );
    assert!(
        !system.contains(BASE_MARKER),
        "replaced base must not remain: {system}"
    );
    assert!(
        !system.contains(AGENTS_MARKER),
        "replaced base composition must not remain: {system}"
    );
    assert_eq!(skill_header_count(system), 1);
    assert!(system.contains("session-skill"));
    assert_eq!(
        system.matches(BUNDLE_PROMPT).count(),
        1,
        "bundle prompt applied exactly once"
    );
}

#[tokio::test]
async fn root_turn_session_model_and_model_switched_win_over_base_and_bundle_defaults() {
    let home = support::TestDir::new("root-session-model-home");
    let workdir = support::TestDir::new("root-session-model");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[AgentFixture::new("build")
            .model("bundle-default-model")
            .category("ultrabrain")]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("created-session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "use session model".to_string())
        .await
        .unwrap();

    engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    {
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests[0].model.as_str(), "created-session-model");
        assert_ne!(requests[0].model.as_str(), "base-model");
        assert_ne!(requests[0].model.as_str(), "bundle-default-model");
    }

    engine
        .switch_model(session, ModelRef::new("switched-model"))
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "use switched model".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].model.as_str(), "switched-model");
    assert!(
        !requests.iter().any(|req| {
            req.model.as_str() == "bundle-default-model" || req.model.as_str() == "base-model"
        }),
        "Bundle model/category and base AgentSpec must not override persisted session model"
    );
}

#[tokio::test]
async fn root_turn_bundle_reasoning_override_and_absent_preserves_base() {
    let home = support::TestDir::new("root-reasoning-home");
    let workdir = support::TestDir::new("root-reasoning");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[
            AgentFixture::new("with-reasoning").reasoning("high"),
            AgentFixture::new("no-reasoning"),
        ]),
        provider.clone(),
    )
    .await;

    let override_session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("with-reasoning"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(override_session, "override reasoning".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            override_session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let preserve_session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("no-reasoning"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(preserve_session, "preserve reasoning".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            preserve_session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].reasoning, Some(ReasoningEffort::High));
    assert_eq!(
        requests[1].reasoning,
        Some(ReasoningEffort::Low),
        "absent Bundle reasoning must preserve the base AgentSpec reasoning"
    );
}

#[tokio::test]
async fn root_turn_session_workdir_wins_over_bundle_and_base_workdir() {
    let home = support::TestDir::new("root-workdir-home");
    let session_dir = support::TestDir::new("root-session-workdir");
    let bundle_dir = support::TestDir::new("root-bundle-workdir");
    let agent_dir = support::TestDir::new("root-agent-workdir");
    let _home = HomeGuard::set(home.path());
    write_skill(session_dir.path(), "from-session");
    write_skill(bundle_dir.path(), "from-bundle");
    write_skill(agent_dir.path(), "from-agent");

    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(
        catalog(&[
            AgentFixture::new("build").workdir(bundle_dir.path().to_string_lossy().into_owned())
        ]),
        provider.clone(),
    )
    .await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: session_dir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "session workdir".to_string())
        .await
        .unwrap();

    let finish = engine
        .run_turn(
            session,
            &composed_base(agent_dir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let requests = provider.requests.lock().unwrap();
    let system = requests[0].system.as_deref().expect("system prompt");
    assert!(
        system.contains("from-session"),
        "skills must come from persisted session workdir: {system}"
    );
    assert!(
        !system.contains("from-bundle"),
        "Bundle prepared workdir must not redirect an existing root turn: {system}"
    );
    assert!(
        !system.contains("from-agent"),
        "base AgentSpec workdir must not redirect when session workdir is set: {system}"
    );
    assert_eq!(skill_header_count(system), 1);
}

#[tokio::test]
async fn root_turn_records_one_turn_binding() {
    let home = support::TestDir::new("root-one-binding-home");
    let workdir = support::TestDir::new("root-one-binding");
    let _home = HomeGuard::set(home.path());
    let provider = Arc::new(CaptureProvider {
        requests: Mutex::new(Vec::new()),
    });
    let engine = engine_with(catalog(&[AgentFixture::new("build")]), provider).await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: workdir.path().to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "one binding".to_string())
        .await
        .unwrap();
    engine
        .run_turn(
            session,
            &composed_base(workdir.path().to_path_buf()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let bindings = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter(|envelope| matches!(envelope.event, Event::TurnBindingRecorded { .. }))
        .count();
    assert_eq!(
        bindings, 1,
        "root turn must capture exactly one TurnBinding"
    );
}
