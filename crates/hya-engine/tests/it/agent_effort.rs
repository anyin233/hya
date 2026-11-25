//! An Agent's own default thinking effort, as the main agent sees it through
//! `list_agents` (the roster annotation) and as requests resolve it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::sync::Arc;

use hya_engine::{AgentEffortSource, EventBus, SessionEngine};
use hya_provider::{ProviderRouter, ReasoningEffort};
use hya_store::{OwnerRunId, SessionStore};
use hya_tool::{AgentDef, PermissionPlane, PermissionRules};

fn row(name: &str, authored: Option<&str>) -> AgentDef {
    AgentDef {
        name: name.to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: authored.map(str::to_string),
        effort_source: authored.map(|_| "authored".to_string()),
    }
}

#[tokio::test]
async fn roster_shows_runtime_over_configured_over_authored_agent_effort() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        store.clone(),
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        permission,
        EventBus::default(),
    );
    engine
        .runtime_registry()
        .publish_agent_effort_configuration(
            [
                ("scout".to_string(), ReasoningEffort::Low),
                ("review".to_string(), ReasoningEffort::Medium),
            ]
            .into_iter()
            .collect(),
        );
    store
        .upsert_agent_effort_preference(owner, "review", "high", 1)
        .await
        .unwrap();

    let roster: Arc<[AgentDef]> = vec![
        row("scout", Some("max")),
        row("review", Some("low")),
        row("deep", Some("xhigh")),
        row("general", None),
    ]
    .into();
    let annotated = engine.annotate_agent_efforts(roster, None).await.unwrap();
    let shown: Vec<_> = annotated
        .iter()
        .map(|agent| {
            (
                agent.name.as_str(),
                agent.effort.as_deref(),
                agent.effort_source.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("scout", Some("low"), Some("configured")),
            ("review", Some("high"), Some("preference")),
            ("deep", Some("xhigh"), Some("authored")),
            ("general", None, None),
        ]
    );

    assert_eq!(
        engine.agent_effort("review", None, None).await.unwrap(),
        Some((ReasoningEffort::High, AgentEffortSource::Preference))
    );
    assert_eq!(
        engine.agent_effort("general", None, None).await.unwrap(),
        None
    );
}

#[derive(Clone)]
struct RecordingEffortProvider {
    inner: Arc<hya_provider::FakeProvider>,
    seen: Arc<std::sync::Mutex<Vec<Option<ReasoningEffort>>>>,
}

#[async_trait::async_trait]
impl hya_provider::Provider for RecordingEffortProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, _model: &hya_proto::ModelRef) -> Option<hya_provider::Capabilities> {
        Some(hya_provider::Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            reasoning_request: true,
            ..hya_provider::Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: hya_provider::CompletionRequest,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
    ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
        self.seen.lock().unwrap().push(req.reasoning);
        self.inner.stream(req, session, message).await
    }
}

type HookBoxFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

struct BetweenRoundsTool {
    name: &'static str,
    action: Arc<dyn Fn() -> HookBoxFuture + Send + Sync>,
}

#[async_trait::async_trait]
impl hya_tool::Tool for BetweenRoundsTool {
    fn name(&self) -> &str {
        self.name
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        hya_proto::ToolSchema {
            name: hya_proto::ToolName::new(self.name),
            description: "between-round test hook".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        _ctx: &hya_tool::ToolCtx,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, hya_tool::ToolError> {
        (self.action)().await;
        Ok(serde_json::json!({ "ok": true }))
    }
}

fn scripted_two_round_engine(
    store: SessionStore,
    seen: Arc<std::sync::Mutex<Vec<Option<ReasoningEffort>>>>,
    hook_tool: BetweenRoundsTool,
) -> SessionEngine {
    let provider = RecordingEffortProvider {
        inner: Arc::new(hya_provider::FakeProvider::scripted_turns(vec![
            vec![
                hya_provider::FakeStep::ToolCall {
                    name: hook_tool.name.to_string(),
                    input: serde_json::json!({}),
                },
                hya_provider::FakeStep::Finish(hya_proto::FinishReason::ToolCalls),
            ],
            vec![
                hya_provider::FakeStep::Text("done".to_string()),
                hya_provider::FakeStep::Finish(hya_proto::FinishReason::Stop),
            ],
        ])),
        seen,
    };
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = hya_tool_loader::builtins();
    tools.register(Arc::new(hook_tool)).unwrap();
    let (permission, _asks) =
        PermissionPlane::new(PermissionRules::new(vec![hya_tool::Rule::new(
            hya_tool::Action::Tool,
            "*",
            hya_tool::Mode::Allow,
        )]));
    SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(tools)),
        permission,
        EventBus::default(),
    )
}

#[cfg(target_os = "linux")]
struct ShmWriteHold(std::fs::File);

#[cfg(target_os = "linux")]
impl ShmWriteHold {
    fn acquire(db: &std::path::Path) -> Self {
        use std::os::unix::io::AsRawFd;
        let shm_path = format!("{}-shm", db.display());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(shm_path)
            .unwrap();
        let lock = libc::flock {
            l_type: libc::F_WRLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: 120,
            l_len: 8,
            l_pid: 0,
        };
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) },
            0
        );
        Self(file)
    }
}

#[cfg(target_os = "linux")]
impl Drop for ShmWriteHold {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        let lock = libc::flock {
            l_type: libc::F_UNLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: 120,
            l_len: 8,
            l_pid: 0,
        };
        unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_OFD_SETLK, &lock) };
    }
}

#[tokio::test]
async fn turn_uses_changed_effort_preference_between_rounds_when_healthy() {
    let dir = support::TestDir::new("effort-healthy");
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    store
        .upsert_agent_effort_preference(owner, "hya-main", "low", 1)
        .await
        .unwrap();

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store_for_tool = store.clone();
    let hook = BetweenRoundsTool {
        name: "bump_effort",
        action: Arc::new(move || {
            let store = store_for_tool.clone();
            Box::pin(async move {
                store
                    .upsert_agent_effort_preference(owner, "hya-main", "high", 2)
                    .await
                    .unwrap();
            })
        }),
    };
    let engine = scripted_two_round_engine(store, Arc::clone(&seen), hook);
    let session = engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("fake"),
            workdir: dir.path().to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "check effort".to_string())
        .await
        .unwrap();
    let agent = hya_engine::AgentSpec {
        name: hya_proto::AgentName::new("hya-main"),
        model: hya_proto::ModelRef::new("fake"),
        system_prompt: "test".to_string(),
        workdir: dir.path().to_path_buf(),
        reasoning: None,
    };
    let finish = engine
        .run_turn(session, &agent, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, hya_proto::FinishReason::Stop);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Some(ReasoningEffort::Low), Some(ReasoningEffort::High)]
    );
}

#[cfg(target_os = "linux")]
struct ShmDir(std::path::PathBuf);

#[cfg(target_os = "linux")]
impl ShmDir {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("hya-effort-{tag}-{}", hya_proto::SessionId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(target_os = "linux")]
impl Drop for ShmDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn turn_reuses_round_one_effort_when_preference_read_fails_after_round_one() {
    let dir = ShmDir::new("effort-fallback");
    let db = dir.path().join("hya.db");
    let store = SessionStore::connect_with_write_timeout(
        db.to_str().unwrap(),
        std::time::Duration::from_millis(60),
    )
    .await
    .unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    store
        .upsert_agent_effort_preference(owner, "hya-main", "high", 1)
        .await
        .unwrap();
    store
        .upsert_model_effort_preference(owner, "fake", "model", "low", 1)
        .await
        .unwrap();

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stall_slot: Arc<std::sync::Mutex<Option<ShmWriteHold>>> =
        Arc::new(std::sync::Mutex::new(None));
    let stall_slot_for_tool = Arc::clone(&stall_slot);
    let db_for_tool = db.clone();
    let hook = BetweenRoundsTool {
        name: "stall_disk",
        action: Arc::new(move || {
            let slot = Arc::clone(&stall_slot_for_tool);
            let db = db_for_tool.clone();
            Box::pin(async move {
                *slot.lock().unwrap() = Some(ShmWriteHold::acquire(&db));
            })
        }),
    };
    let engine = scripted_two_round_engine(store.clone(), Arc::clone(&seen), hook);
    let workdir = dir.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let session = engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("fake/model"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "stall after round 1".to_string())
        .await
        .unwrap();
    // Warm the projection cache before round 1 starts.
    store.read_projection(session).await.unwrap();

    let agent = hya_engine::AgentSpec {
        name: hya_proto::AgentName::new("hya-main"),
        model: hya_proto::ModelRef::new("fake/model"),
        system_prompt: "test".to_string(),
        workdir,
        reasoning: None,
    };
    let finish = engine
        .run_turn(session, &agent, tokio_util::sync::CancellationToken::new())
        .await
        .expect("turn must reuse Round 1 effort on later read fault");
    assert_eq!(finish, hya_proto::FinishReason::Stop);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Some(ReasoningEffort::High), Some(ReasoningEffort::High)]
    );
    drop(stall_slot.lock().unwrap().take());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn turn_fails_when_first_preference_read_fails() {
    let dir = ShmDir::new("effort-first-fail");
    let db = dir.path().join("hya.db");
    let store = SessionStore::connect_with_write_timeout(
        db.to_str().unwrap(),
        std::time::Duration::from_millis(60),
    )
    .await
    .unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hook = BetweenRoundsTool {
        name: "noop",
        action: Arc::new(|| Box::pin(async {})),
    };
    let engine = scripted_two_round_engine(store.clone(), seen, hook);
    let workdir = dir.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let session = engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "first read fails".to_string())
        .await
        .unwrap();
    // Warm projection cache, then stall shm and trigger write degradation so projection reads
    // stay served from memory while the first preference SQL read hits `READ_TIMEOUT`.
    store.read_projection(session).await.unwrap();
    let _stall = ShmWriteHold::acquire(&db);
    store
        .append_event(
            session,
            &hya_proto::Event::SessionTitled {
                session,
                title: "degrade-writes".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store.health(),
        hya_store::StoreHealth::Degraded { .. }
    ));

    let agent = hya_engine::AgentSpec {
        name: hya_proto::AgentName::new("hya-main"),
        model: hya_proto::ModelRef::new("fake"),
        system_prompt: "test".to_string(),
        workdir,
        reasoning: None,
    };
    let err = engine
        .run_turn(session, &agent, tokio_util::sync::CancellationToken::new())
        .await
        .expect_err("first preference read failure must fail the turn");
    assert!(matches!(
        err,
        hya_engine::CoreError::Store(hya_store::StoreError::StorageTimeout { .. })
    ));
}

#[tokio::test]
async fn turn_propagates_non_storage_preference_error_between_rounds() {
    let dir = support::TestDir::new("effort-corrupt");
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    store
        .upsert_agent_effort_preference(owner, "hya-main", "low", 1)
        .await
        .unwrap();

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store_for_tool = store.clone();
    let hook = BetweenRoundsTool {
        name: "corrupt_effort",
        action: Arc::new(move || {
            let store = store_for_tool.clone();
            Box::pin(async move {
                let mut tx = store.begin_transaction().await.unwrap();
                sqlx::query(
                    "UPDATE agent_effort_preference SET effort = '' WHERE agent_id = 'hya-main'",
                )
                .execute(&mut *tx)
                .await
                .unwrap();
                tx.commit().await.unwrap();
            })
        }),
    };
    let engine = scripted_two_round_engine(store, Arc::clone(&seen), hook);
    let session = engine
        .create(hya_engine::CreateSession {
            parent: None,
            agent: hya_proto::AgentName::new("hya-main"),
            model: hya_proto::ModelRef::new("fake"),
            workdir: dir.path().to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "corrupt after round 1".to_string())
        .await
        .unwrap();
    let agent = hya_engine::AgentSpec {
        name: hya_proto::AgentName::new("hya-main"),
        model: hya_proto::ModelRef::new("fake"),
        system_prompt: "test".to_string(),
        workdir: dir.path().to_path_buf(),
        reasoning: None,
    };
    let err = engine
        .run_turn(session, &agent, tokio_util::sync::CancellationToken::new())
        .await
        .expect_err("non-storage InvalidPreferenceData error must propagate without fallback");
    assert!(matches!(
        err,
        hya_engine::CoreError::Store(hya_store::StoreError::InvalidPreferenceData { .. })
    ));
    assert_eq!(*seen.lock().unwrap(), vec![Some(ReasoningEffort::Low)]);
}
