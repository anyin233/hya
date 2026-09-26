//! Session revert: file snapshots recorded by write and bash, transcript
//! hiding, file restore (created → deleted, deleted → recreated), unrevert,
//! commit on the next prompt, and busy rejection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{AgentSpec, CreateSession, EventBus, RevertError, RevertTarget, SessionEngine};
use hya_proto::{
    AgentName, FileState, FinishReason, Message, MessageId, ModelRef, Part, Role, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-core-revert-{nanos}-{}-{}",
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical form: the tools resolve paths lexically against it.
    TempDir(dir.canonicalize().unwrap())
}

/// Records the user texts of every request, then plays the fake script.
struct Recording {
    inner: FakeProvider,
    users: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl Provider for Recording {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let users = req
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::User { parts, .. } => Some(
                    parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect();
        self.users.lock().unwrap().push(users);
        self.inner.stream(req, session, message).await
    }
}

struct Fixture {
    engine: SessionEngine,
    session: SessionId,
    agent: AgentSpec,
    users: Arc<Mutex<Vec<Vec<String>>>>,
}

async fn fixture(dir: &Path, script: Vec<Vec<FakeStep>>) -> Fixture {
    let users = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(Recording {
        inner: FakeProvider::scripted_turns(script),
        users: users.clone(),
    })));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Edit, "**", Mode::Allow),
        Rule::new(Action::Bash, "**", Mode::Allow),
        Rule::new(Action::Read, "**", Mode::Allow),
    ]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: dir.to_path_buf(),
        reasoning: None,
    };
    Fixture {
        engine,
        session,
        agent,
        users,
    }
}

/// One tool call, then a closing text round.
fn tool_turn(name: &str, input: serde_json::Value) -> Vec<Vec<FakeStep>> {
    vec![
        vec![
            FakeStep::ToolCall {
                name: name.to_string(),
                input,
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]
}

fn text_turn(text: &str) -> Vec<Vec<FakeStep>> {
    vec![vec![
        FakeStep::Text(text.to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]
}

impl Fixture {
    async fn prompt(&self, text: &str) -> MessageId {
        let user = self
            .engine
            .admit_user_prompt(self.session, text.to_string())
            .await
            .unwrap();
        self.engine
            .run_turn(self.session, &self.agent, CancellationToken::new())
            .await
            .unwrap();
        user
    }

    async fn visible(&self) -> Vec<MessageId> {
        self.engine
            .store()
            .read_projection(self.session)
            .await
            .unwrap()
            .session
            .messages
            .iter()
            .map(|message| message.id)
            .collect()
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[tokio::test]
async fn write_changes_are_recorded_and_reverted_then_redone() {
    let dir = tempdir();
    let a = dir.0.join("a.txt");
    let script = [
        tool_turn("write", json!({"path": "a.txt", "content": "one"})),
        tool_turn("write", json!({"path": "a.txt", "content": "two"})),
    ]
    .concat();
    let fx = fixture(&dir.0, script).await;
    let u1 = fx.prompt("first").await;
    let u2 = fx.prompt("second").await;
    assert_eq!(read(&a).as_deref(), Some("two"));
    let all = fx.visible().await;
    assert_eq!(all.len(), 4);

    // Each write recorded the file's state before it.
    let projection = fx.engine.store().read_projection(fx.session).await.unwrap();
    let changes: Vec<_> = projection
        .session
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| message.file_changes.clone())
        .collect();
    assert_eq!(changes[0].len(), 1);
    assert_eq!(changes[0][0].path, a.to_string_lossy());
    assert_eq!(changes[0][0].before, FileState::Absent);
    assert!(matches!(
        changes[1][0].before,
        FileState::Stored { size: 3, .. }
    ));

    // Revert the second turn: content goes back to the first write.
    let outcome = fx
        .engine
        .revert_session(fx.session, RevertTarget::Message(u2))
        .await
        .unwrap();
    assert_eq!(outcome.message, u2);
    assert_eq!(read(&a).as_deref(), Some("one"));
    assert_eq!(fx.visible().await, all[..2].to_vec());

    // `/undo` again reverts the previous user turn: created → deleted.
    let outcome = fx
        .engine
        .revert_session(fx.session, RevertTarget::LastUserMessage)
        .await
        .unwrap();
    assert_eq!(outcome.message, u1);
    assert!(!a.exists());
    assert!(fx.visible().await.is_empty());

    // Redo restores every hidden message and the latest content.
    fx.engine.unrevert_session(fx.session).await.unwrap();
    assert_eq!(read(&a).as_deref(), Some("two"));
    assert_eq!(fx.visible().await, all);
    let projection = fx.engine.store().read_projection(fx.session).await.unwrap();
    assert!(projection.session.revert.is_none());
}

#[tokio::test]
async fn bash_changes_in_a_git_work_tree_are_reverted() {
    let dir = tempdir();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(&dir.0)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(dir.0.join("orig.txt"), "orig").unwrap();
    git(&["add", "orig.txt"]);
    git(&["commit", "-q", "-m", "init"]);
    std::fs::write(dir.0.join("dirty.txt"), "d0").unwrap();
    let fx = fixture(&dir.0, Vec::new()).await;

    let (shell_message, finish) = fx
        .engine
        .run_shell(
            fx.session,
            &fx.agent,
            "rm orig.txt && printf new > made.txt && printf d1 > dirty.txt".to_string(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let projection = fx.engine.store().read_projection(fx.session).await.unwrap();
    let message = projection
        .session
        .messages
        .iter()
        .find(|message| message.id == shell_message)
        .unwrap();
    let mut paths: Vec<_> = message
        .file_changes
        .iter()
        .map(|change| {
            Path::new(&change.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["dirty.txt", "made.txt", "orig.txt"]);

    fx.engine
        .revert_session(fx.session, RevertTarget::LastUserMessage)
        .await
        .unwrap();
    assert_eq!(read(&dir.0.join("orig.txt")).as_deref(), Some("orig"));
    assert!(!dir.0.join("made.txt").exists());
    assert_eq!(read(&dir.0.join("dirty.txt")).as_deref(), Some("d0"));

    fx.engine.unrevert_session(fx.session).await.unwrap();
    assert!(!dir.0.join("orig.txt").exists());
    assert_eq!(read(&dir.0.join("made.txt")).as_deref(), Some("new"));
    assert_eq!(read(&dir.0.join("dirty.txt")).as_deref(), Some("d1"));
}

#[tokio::test]
async fn the_next_prompt_commits_the_revert_and_the_model_never_sees_it() {
    let dir = tempdir();
    let script = [text_turn("a"), text_turn("b"), text_turn("c")].concat();
    let fx = fixture(&dir.0, script).await;
    let u1 = fx.prompt("first").await;
    let u2 = fx.prompt("second").await;
    fx.engine
        .revert_session(fx.session, RevertTarget::Message(u2))
        .await
        .unwrap();

    let u3 = fx.prompt("third").await;

    let users = fx.users.lock().unwrap().clone();
    assert_eq!(users.last().unwrap(), &vec!["first", "third"]);
    let projection = fx.engine.store().read_projection(fx.session).await.unwrap();
    assert!(projection.session.revert.is_none());
    let visible = fx.visible().await;
    assert_eq!(visible.len(), 4);
    assert_eq!(visible[0], u1);
    assert_eq!(visible[2], u3);
    assert!(matches!(
        fx.engine.unrevert_session(fx.session).await,
        Err(RevertError::NoRevertPending)
    ));
}

#[tokio::test]
async fn revert_is_refused_while_a_turn_is_active() {
    let dir = tempdir();
    let fx = fixture(&dir.0, text_turn("a")).await;
    fx.prompt("first").await;
    let lease = fx.engine.try_begin_turn(fx.session).unwrap();

    let refused = fx
        .engine
        .revert_session(fx.session, RevertTarget::LastUserMessage)
        .await;
    assert!(matches!(refused, Err(RevertError::Busy)), "{refused:?}");
    drop(lease);
    fx.engine
        .revert_session(fx.session, RevertTarget::LastUserMessage)
        .await
        .unwrap();
    let lease = fx.engine.try_begin_turn(fx.session).unwrap();
    assert!(matches!(
        fx.engine.unrevert_session(fx.session).await,
        Err(RevertError::Busy)
    ));
    drop(lease);
}

#[tokio::test]
async fn revert_targets_are_validated() {
    let dir = tempdir();
    let fx = fixture(&dir.0, text_turn("a")).await;
    assert!(matches!(
        fx.engine
            .revert_session(fx.session, RevertTarget::LastUserMessage)
            .await,
        Err(RevertError::NothingToRevert)
    ));
    let u1 = fx.prompt("first").await;
    let visible = fx.visible().await;
    let assistant = visible[1];
    assert!(matches!(
        fx.engine
            .revert_session(fx.session, RevertTarget::Message(assistant))
            .await,
        Err(RevertError::NotUserMessage(id)) if id == assistant
    ));
    let unknown = MessageId::new();
    assert!(matches!(
        fx.engine
            .revert_session(fx.session, RevertTarget::Message(unknown))
            .await,
        Err(RevertError::MessageNotFound(id)) if id == unknown
    ));
    fx.engine
        .revert_session(fx.session, RevertTarget::Message(u1))
        .await
        .unwrap();
    assert!(matches!(
        fx.engine
            .revert_session(fx.session, RevertTarget::Message(u1))
            .await,
        Err(RevertError::AlreadyReverted(id)) if id == u1
    ));
}

#[tokio::test]
async fn oversized_prior_content_is_not_kept_and_not_restored() {
    let dir = tempdir();
    let big = dir.0.join("big.txt");
    let content = "x".repeat(3 * 1024 * 1024);
    std::fs::write(&big, &content).unwrap();
    let fx = fixture(
        &dir.0,
        tool_turn("write", json!({"path": "big.txt", "content": "small"})),
    )
    .await;
    fx.prompt("shrink").await;
    let projection = fx.engine.store().read_projection(fx.session).await.unwrap();
    let change = projection.session.messages[1].file_changes[0].clone();
    assert!(
        matches!(&change.before, FileState::Omitted { reason, .. } if reason == "too_large"),
        "{change:?}"
    );

    let outcome = fx
        .engine
        .revert_session(fx.session, RevertTarget::LastUserMessage)
        .await
        .unwrap();
    assert_eq!(read(&big).as_deref(), Some("small"));
    assert!(!outcome.files[0].restored.restorable());
}
