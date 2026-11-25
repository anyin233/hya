//! Session revert/unrevert, per-round model usage attribution, bundle usage
//! tree reads, and calibrated token accounting.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use hya_engine::bundle_apis::session_usage_report;
use hya_engine::{
    AgentSpec, CalibratedTokenizer, EventBus, HostSessionReads, RevertError, RevertTarget,
    SessionEngine, StoreSessionReads, TokenAccounting, TokenAccountingMode, TokenSource, Tokenizer,
    UsageScope,
};
use hya_proto::{
    AgentName, Event, FileState, FinishReason, MemberId, Message, MessageId, ModelRef, Part,
    PartId, Role, SessionId, TokenUsage, ToolCallId, ToolName, ToolPartState, UsagePurpose,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter, ReasoningReplayPolicy,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule};
use serde_json::json;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Session revert / unrevert
// ---------------------------------------------------------------------------

struct RecordingProvider {
    inner: FakeProvider,
    users: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
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
            .filter_map(|m| match m {
                Message::User { parts, .. } => Some(
                    parts
                        .iter()
                        .filter_map(|p| match p {
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

struct RevertFixture {
    engine: SessionEngine,
    session: SessionId,
    agent: AgentSpec,
    users: Arc<Mutex<Vec<Vec<String>>>>,
}

async fn revert_fixture(dir: &Path, script: Vec<Vec<FakeStep>>) -> RevertFixture {
    let users = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        inner: FakeProvider::scripted_turns(script),
        users: Arc::clone(&users),
    });
    let engine = support::engine_with_provider(
        provider,
        PermissionRules::new(vec![
            Rule::new(Action::Edit, "**", Mode::Allow),
            Rule::new(Action::Bash, "**", Mode::Allow),
            Rule::new(Action::Read, "**", Mode::Allow),
        ]),
    )
    .await;
    let session = support::create_root_session(&engine, "hya-main", "fake", dir).await;
    let mut agent = support::agent_spec("hya-main", "fake", dir);
    agent.system_prompt = "x".into();
    RevertFixture {
        engine,
        session,
        agent,
        users,
    }
}

fn tool_turn(name: &str, input: serde_json::Value) -> Vec<Vec<FakeStep>> {
    vec![
        vec![
            FakeStep::ToolCall {
                name: name.into(),
                input,
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]
}

fn text_turn(text: &str) -> Vec<Vec<FakeStep>> {
    vec![vec![
        FakeStep::Text(text.into()),
        FakeStep::Finish(FinishReason::Stop),
    ]]
}

impl RevertFixture {
    async fn prompt(&self, text: &str) -> MessageId {
        let user = self
            .engine
            .admit_user_prompt(self.session, text.into())
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
            .map(|m| m.id)
            .collect()
    }
}

fn read_opt(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn git_cmd(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[tokio::test]
async fn write_revert_redo_and_prompt_commit() {
    let dir = support::TestDir::new("revert-write");
    let canon = dir.path().canonicalize().unwrap();
    let a = canon.join("a.txt");
    let script = [
        tool_turn("write", json!({"path": "a.txt", "content": "one"})),
        tool_turn("write", json!({"path": "a.txt", "content": "two"})),
        text_turn("three"),
    ]
    .concat();
    let fx = revert_fixture(&canon, script).await;
    let u1 = fx.prompt("first").await;
    let u2 = fx.prompt("second").await;
    assert_eq!(read_opt(&a).as_deref(), Some("two"));
    let all = fx.visible().await;
    assert_eq!(all.len(), 4);

    let projection = fx.engine.store().read_projection(fx.session).await.unwrap();
    let changes: Vec<_> = projection
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .map(|m| m.file_changes.clone())
        .collect();
    assert_eq!(changes[0][0].path, a.to_string_lossy());
    assert_eq!(changes[0][0].before, FileState::Absent);
    assert!(matches!(
        changes[1][0].before,
        FileState::Stored { size: 3, .. }
    ));
    assert_eq!(projection.session.files[a.to_str().unwrap()].size, Some(3));
    let events = fx.engine.replay(fx.session).await.unwrap();
    assert_eq!(
        events
            .windows(2)
            .filter(|pair| matches!(
                (&pair[0].event, &pair[1].event),
                (
                    Event::ToolResult { .. },
                    Event::FilesObserved {
                        external: false,
                        ..
                    }
                )
            ))
            .count(),
        2
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.event, Event::FilesObserved { external: true, .. }))
    );

    // Revert u2 -> "one"; revert u1 -> deleted; unrevert -> "two".
    let outcome = fx
        .engine
        .revert_session(fx.session, RevertTarget::Message(u2))
        .await
        .unwrap();
    assert_eq!(outcome.message, u2);
    assert_eq!(read_opt(&a).as_deref(), Some("one"));
    assert_eq!(fx.visible().await, all[..2].to_vec());

    let outcome = fx
        .engine
        .revert_session(fx.session, RevertTarget::LastUserMessage)
        .await
        .unwrap();
    assert_eq!(outcome.message, u1);
    assert!(!a.exists());
    assert!(fx.visible().await.is_empty());

    fx.engine.unrevert_session(fx.session).await.unwrap();
    assert_eq!(read_opt(&a).as_deref(), Some("two"));
    assert_eq!(fx.visible().await, all);
    assert!(
        fx.engine
            .store()
            .read_projection(fx.session)
            .await
            .unwrap()
            .session
            .revert
            .is_none()
    );

    // Revert u2 and send a new prompt: commits the revert and hides u2 from the model.
    fx.engine
        .revert_session(fx.session, RevertTarget::Message(u2))
        .await
        .unwrap();
    let u3 = fx.prompt("third").await;
    assert_eq!(
        fx.users.lock().unwrap().last().unwrap(),
        &["first", "third"]
    );
    assert_eq!(fx.visible().await[0], u1);
    assert_eq!(fx.visible().await[2], u3);
    assert!(matches!(
        fx.engine.unrevert_session(fx.session).await,
        Err(RevertError::NoRevertPending)
    ));
}

#[tokio::test]
async fn bash_git_and_concurrent_edit_revert_and_redo() {
    // 1. Direct bash deletes, creates, and modifies tracked/untracked files in a git repo.
    let dir = support::TestDir::new("revert-git");
    let canon = dir.path().canonicalize().unwrap();
    git_cmd(&canon, &["init", "-q"]);
    std::fs::write(canon.join("orig.txt"), "orig").unwrap();
    git_cmd(&canon, &["add", "orig.txt"]);
    git_cmd(&canon, &["commit", "-q", "-m", "init"]);
    std::fs::write(canon.join("dirty.txt"), "d0").unwrap();
    let fx = revert_fixture(&canon, Vec::new()).await;

    let (shell_msg, finish) = fx
        .engine
        .run_shell(
            fx.session,
            &fx.agent,
            "rm orig.txt && printf new > made.txt && printf d1 > dirty.txt".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let projection = fx.engine.store().read_projection(fx.session).await.unwrap();
    let msg = projection
        .session
        .messages
        .iter()
        .find(|m| m.id == shell_msg)
        .unwrap();
    let mut paths: Vec<_> = msg
        .file_changes
        .iter()
        .map(|c| {
            Path::new(&c.path)
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
    assert_eq!(read_opt(&canon.join("orig.txt")).as_deref(), Some("orig"));
    assert!(!canon.join("made.txt").exists());
    assert_eq!(read_opt(&canon.join("dirty.txt")).as_deref(), Some("d0"));

    fx.engine.unrevert_session(fx.session).await.unwrap();
    assert!(!canon.join("orig.txt").exists());
    assert_eq!(read_opt(&canon.join("made.txt")).as_deref(), Some("new"));
    assert_eq!(read_opt(&canon.join("dirty.txt")).as_deref(), Some("d1"));

    // 2. Concurrent external edit during bash is observed, reverted, and redone.
    let dir2 = support::TestDir::new("revert-concurrent");
    let canon2 = dir2.path().canonicalize().unwrap();
    git_cmd(&canon2, &["init", "-q"]);
    let file = canon2.join("user.txt");
    std::fs::write(&file, "original").unwrap();
    git_cmd(&canon2, &["add", "user.txt"]);
    git_cmd(&canon2, &["commit", "-q", "-m", "init"]);
    let fx2 = revert_fixture(&canon2, Vec::new()).await;

    let shell = fx2.engine.run_shell(
        fx2.session,
        &fx2.agent,
        "touch ready; while [ ! -f release ]; do sleep 0.01; done".into(),
        CancellationToken::new(),
    );
    let external = async {
        assert!(
            support::poll_until(std::time::Duration::from_secs(5), || async {
                canon2.join("ready").exists()
            })
            .await
        );
        std::fs::write(&file, "user edited during command").unwrap();
        std::fs::write(canon2.join("release"), "").unwrap();
    };
    let (res, ()) = tokio::join!(shell, external);
    res.unwrap();
    let outcome = fx2
        .engine
        .revert_session(fx2.session, RevertTarget::LastUserMessage)
        .await
        .unwrap();
    let restored = outcome
        .files
        .iter()
        .find(|c| c.path == file.to_string_lossy())
        .expect("observed file reported");
    assert!(restored.observed);
    assert_eq!(read_opt(&file).as_deref(), Some("original"));
    fx2.engine.unrevert_session(fx2.session).await.unwrap();
    assert_eq!(
        read_opt(&file).as_deref(),
        Some("user edited during command")
    );
}

#[tokio::test]
async fn revert_validation_busy_oversized_and_read_timeout() {
    // 1. Target validation + busy refusal.
    let dir = support::TestDir::new("revert-val");
    let canon = dir.path().canonicalize().unwrap();
    let fx = revert_fixture(&canon, text_turn("a")).await;
    assert!(matches!(
        fx.engine
            .revert_session(fx.session, RevertTarget::LastUserMessage)
            .await,
        Err(RevertError::NothingToRevert)
    ));
    let u1 = fx.prompt("first").await;
    let assistant = fx.visible().await[1];
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
    let lease = fx.engine.try_begin_turn(fx.session).unwrap();
    assert!(matches!(
        fx.engine
            .revert_session(fx.session, RevertTarget::LastUserMessage)
            .await,
        Err(RevertError::Busy)
    ));
    drop(lease);
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
    let lease = fx.engine.try_begin_turn(fx.session).unwrap();
    assert!(matches!(
        fx.engine.unrevert_session(fx.session).await,
        Err(RevertError::Busy)
    ));
    drop(lease);

    // 2. Oversized prior file is omitted and not restored.
    let dir_big = support::TestDir::new("revert-big");
    let canon_big = dir_big.path().canonicalize().unwrap();
    let big = canon_big.join("big.txt");
    std::fs::write(&big, "x".repeat(3 * 1024 * 1024)).unwrap();
    let fx_big = revert_fixture(
        &canon_big,
        tool_turn("write", json!({"path": "big.txt", "content": "small"})),
    )
    .await;
    fx_big.prompt("shrink").await;
    let proj = fx_big
        .engine
        .store()
        .read_projection(fx_big.session)
        .await
        .unwrap();
    assert!(matches!(
        &proj.session.messages[1].file_changes[0].before,
        FileState::Omitted { reason, .. } if reason == "too_large"
    ));
    let outcome = fx_big
        .engine
        .revert_session(fx_big.session, RevertTarget::LastUserMessage)
        .await
        .unwrap();
    assert_eq!(read_opt(&big).as_deref(), Some("small"));
    assert!(!outcome.files[0].restored.restorable());

    // 3. Healthy revert budget scan exceeding read_timeout still succeeds.
    let db_dir = support::TestDir::new("revert-timeout");
    let workdir = db_dir.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let workdir = workdir.canonicalize().unwrap();
    let file = workdir.join("a.txt");
    std::fs::write(&file, "before").unwrap();
    let db = db_dir.path().join("hya.db");
    let db_str = db.to_str().unwrap();
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Edit, "**", Mode::Allow),
        Rule::new(Action::Bash, "**", Mode::Allow),
        Rule::new(Action::Read, "**", Mode::Allow),
    ]));
    let session = {
        let store = SessionStore::connect(db_str).await.unwrap();
        let engine = SessionEngine::new(
            store,
            Arc::new(
                ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(tool_turn(
                    "write",
                    json!({"path": "a.txt", "content": "after"}),
                )))),
            ),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm.clone(),
            EventBus::default(),
        );
        let session = support::create_root_session(&engine, "hya-main", "fake", &workdir).await;
        engine
            .admit_user_prompt(session, "overwrite".into())
            .await
            .unwrap();
        engine
            .run_turn(
                session,
                &support::agent_spec("hya-main", "fake", &workdir),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut tx = engine.store().begin_transaction().await.unwrap();
        sqlx::query(
            "WITH RECURSIVE cnt(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM cnt LIMIT 60000) \
             INSERT INTO file_blob (session_id, hash, size, content) \
             SELECT ?, printf('pad_%060d', x), 100, zeroblob(400) FROM cnt",
        )
        .bind(session.storage_key())
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        session
    };
    let read_timeout = std::time::Duration::from_millis(2);
    let slow_store = SessionStore::connect_with_timeouts(
        db_str,
        std::time::Duration::from_secs(5),
        read_timeout,
    )
    .await
    .unwrap();
    let engine = SessionEngine::new(
        slow_store,
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        perm,
        EventBus::default(),
    );
    let outcome = engine
        .revert_session(session, RevertTarget::LastUserMessage)
        .await
        .expect("healthy revert must not fail at READ_TIMEOUT");
    assert_eq!(outcome.files.len(), 1);
    assert_eq!(read_opt(&file).as_deref(), Some("before"));
}

// ---------------------------------------------------------------------------
// Per-round usage attribution & bundle usage reads
// ---------------------------------------------------------------------------

enum UsageOutcome {
    PreStreamFailure,
    Tool(String, TokenUsage),
    Text(TokenUsage),
    UsageThenError(TokenUsage, ProviderError),
    CancelAndHang(CancellationToken),
}

struct UsageProvider {
    claimed: &'static str,
    outcomes: Mutex<VecDeque<UsageOutcome>>,
}

impl UsageProvider {
    fn new(claimed: &'static str, outcomes: Vec<UsageOutcome>) -> Self {
        Self {
            claimed,
            outcomes: Mutex::new(outcomes.into()),
        }
    }
}

#[async_trait]
impl Provider for UsageProvider {
    fn id(&self) -> &str {
        "usage"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == self.claimed).then(|| Capabilities {
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
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(UsageOutcome::PreStreamFailure);
        let wrap = |events: Vec<Event>| -> EventStream {
            Box::pin(stream::iter(events.into_iter().map(Ok)))
        };
        match outcome {
            UsageOutcome::PreStreamFailure => Err(ProviderError::Transport("reset".into())),
            UsageOutcome::Tool(path, usage) => Ok(wrap(FakeProvider::materialize(
                &[
                    FakeStep::ToolCall {
                        name: "read".into(),
                        input: json!({ "path": path }),
                    },
                    FakeStep::Usage(usage),
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                session,
                message,
            ))),
            UsageOutcome::Text(usage) => Ok(wrap(FakeProvider::materialize(
                &[
                    FakeStep::Text("done".into()),
                    FakeStep::Usage(usage),
                    FakeStep::Finish(FinishReason::Stop),
                ],
                session,
                message,
            ))),
            UsageOutcome::UsageThenError(usage, error) => Ok(Box::pin(stream::iter([
                Ok(Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: FinishReason::Stop,
                    tokens: Some(usage),
                    cause: None,
                }),
                Err(error),
            ]))),
            UsageOutcome::CancelAndHang(cancel) => {
                cancel.cancel();
                Ok(Box::pin(stream::pending()))
            }
        }
    }
}

fn sample_usage(input: u64, output: u64, reasoning: Option<u64>) -> TokenUsage {
    TokenUsage {
        input,
        output,
        reasoning: reasoning.unwrap_or(0),
        cache_read: 3,
        cache_write: 2,
        reasoning_unknown: reasoning.is_none(),
    }
}

async fn usage_fixture(
    script: impl FnOnce(String) -> Vec<UsageProvider>,
    fallbacks: Option<HashMap<ModelRef, Vec<ModelRef>>>,
) -> (support::TestDir, SessionEngine, SessionId, AgentSpec) {
    let dir = support::TestDir::new("usage-attr");
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "alpha").unwrap();
    let router = script(file.to_string_lossy().into_owned())
        .into_iter()
        .fold(ProviderRouter::new(), |r, p| r.with(Arc::new(p)));
    let mut engine = support::engine_with_router_and_runtime(
        Arc::new(router),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        PermissionRules::new(vec![Rule::new(Action::Read, "/**", Mode::Allow)]),
    )
    .await;
    if let Some(fallbacks) = fallbacks {
        engine = engine.with_model_fallbacks(fallbacks);
    }
    let session = support::create_root_session(&engine, "build", "model-a", dir.path()).await;
    engine
        .admit_user_prompt(session, "read the file".into())
        .await
        .unwrap();
    let agent = support::agent_spec("build", "model-a", dir.path());
    (dir, engine, session, agent)
}

async fn usage_records(
    engine: &SessionEngine,
    session: SessionId,
) -> Vec<(
    Option<MessageId>,
    Option<u32>,
    ModelRef,
    UsagePurpose,
    TokenUsage,
)> {
    engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::UsageRecorded {
                message,
                step,
                model,
                purpose,
                tokens,
                ..
            } => Some((message, step, model, purpose, tokens)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn usage_attribution_across_fallback_retry_cancel_and_error() {
    let a = ModelRef::new("model-a");
    let b = ModelRef::new("model-b");
    let fb = || Some(HashMap::from([(a.clone(), vec![a.clone(), b.clone()])]));

    // 1. Multi-round turn across primary + fallback attributes each round to its serving model.
    let round_a = sample_usage(100, 20, None);
    let round_b = sample_usage(40, 9, Some(4));
    let (_dir, engine, session, agent) = usage_fixture(
        |path| {
            vec![
                UsageProvider::new(
                    "model-a",
                    vec![
                        UsageOutcome::Tool(path, round_a),
                        UsageOutcome::PreStreamFailure,
                    ],
                ),
                UsageProvider::new("model-b", vec![UsageOutcome::Text(round_b)]),
            ]
        },
        fb(),
    )
    .await;
    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let records = usage_records(&engine, session).await;
    assert_eq!(records.len(), 2);
    let msg_id = records[0].0.expect("turn round message");
    assert_eq!(
        (records[0].1, &records[0].2, records[0].3, records[0].4),
        (Some(0), &a, UsagePurpose::Turn, round_a)
    );
    assert_eq!(
        (records[1].0, records[1].1, &records[1].2, records[1].4),
        (Some(msg_id), Some(1), &b, round_b)
    );
    let proj = engine.read_projection(session).await.unwrap();
    assert_eq!(proj.session.usage.by_model[&a].output, 20);
    assert_eq!(proj.session.usage.by_model[&a].reasoning_unknown_output, 20);
    assert_eq!(
        proj.session.usage.by_model[&b]
            .output_split()
            .visible_exact(),
        Some(5)
    );
    let mut sum = round_a;
    sum.add(round_b);
    assert_eq!(
        proj.session
            .messages
            .iter()
            .find(|m| m.id == msg_id)
            .unwrap()
            .tokens,
        Some(sum)
    );

    // 2. Billed retry stays pinned on the fallback model that served the interrupted stream.
    let first = sample_usage(40, 9, Some(4));
    let next = sample_usage(20, 6, Some(1));
    let (_dir, engine, session, agent) = usage_fixture(
        |_| {
            vec![
                UsageProvider::new(
                    "model-a",
                    vec![UsageOutcome::PreStreamFailure, UsageOutcome::Text(next)],
                ),
                UsageProvider::new(
                    "model-b",
                    vec![
                        UsageOutcome::UsageThenError(
                            first,
                            ProviderError::Decode("unexpected EOF in response body".into()),
                        ),
                        UsageOutcome::Text(next),
                    ],
                ),
            ]
        },
        fb(),
    )
    .await;
    assert_eq!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let records = usage_records(&engine, session).await;
    let billed: Vec<_> = records.iter().map(|(_, s, m, _, t)| (*s, m, *t)).collect();
    assert_eq!(billed, [(Some(0), &b, first), (Some(1), &b, next)]);
    assert_eq!(
        engine
            .read_projection(session)
            .await
            .unwrap()
            .session
            .usage
            .by_model[&b]
            .output,
        first.output + next.output
    );

    // 3. Cancelled turn keeps usage of earlier billed rounds; errored round keeps received usage.
    let cancel = CancellationToken::new();
    let round_c = sample_usage(70, 11, None);
    let hang = cancel.clone();
    let (_dir, engine, session, agent) = usage_fixture(
        |path| {
            vec![UsageProvider::new(
                "model-a",
                vec![
                    UsageOutcome::Tool(path, round_c),
                    UsageOutcome::CancelAndHang(hang),
                ],
            )]
        },
        None,
    )
    .await;
    assert_eq!(
        engine.run_turn(session, &agent, cancel).await.unwrap(),
        FinishReason::Cancelled
    );
    let proj = engine.read_projection(session).await.unwrap();
    let assistant = proj
        .session
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(assistant.finish, Some(FinishReason::Cancelled));
    assert_eq!(assistant.tokens, None);
    assert_eq!(proj.session.usage.by_model[&a].output, 11);

    let round_e = sample_usage(55, 6, Some(0));
    let (_dir, engine, session, agent) = usage_fixture(
        |_| {
            vec![UsageProvider::new(
                "model-a",
                vec![UsageOutcome::UsageThenError(
                    round_e,
                    ProviderError::Decode("invalid usage frame".into()),
                )],
            )]
        },
        None,
    )
    .await;
    assert!(
        engine
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .is_err()
    );
    let proj = engine.read_projection(session).await.unwrap();
    assert_eq!(proj.session.usage.total().input, 55);
    assert_eq!(
        proj.session
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .unwrap()
            .finish,
        Some(FinishReason::Error)
    );
}

#[tokio::test]
async fn usage_projection_records_reported_and_never_estimates_missing() {
    let dir = support::TestDir::new("usage-proj");
    let engine = support::engine_with_provider(
        Arc::new(FakeProvider::scripted(vec![
            FakeStep::Text("measured answer".into()),
            FakeStep::Usage(TokenUsage {
                input: 120,
                output: 34,
                reasoning: 0,
                cache_read: 10,
                cache_write: 5,
                reasoning_unknown: false,
            }),
            FakeStep::Finish(FinishReason::Stop),
        ])),
        PermissionRules::default(),
    )
    .await;
    let session = support::create_root_session(&engine, "hya-main", "fake", dir.path()).await;
    engine
        .admit_user_prompt(session, "hi".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(
                session,
                &support::agent_spec("hya-main", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let proj = engine.read_projection(session).await.unwrap();
    let totals = proj.session.usage.by_model[&ModelRef::new("fake")];
    assert_eq!(
        (
            totals.input,
            totals.cache_read,
            totals.cache_write,
            totals.prompt(),
            totals.output,
            totals.rounds,
            totals.legacy_messages
        ),
        (120, 10, 5, 135, 34, 1, 0)
    );
    assert_eq!(proj.session.usage.by_purpose[&UsagePurpose::Turn], totals);

    for model in ["qwen/family-test", "somecustom/mystery-9b"] {
        let engine = support::engine_with_provider(
            Arc::new(FakeProvider::scripted(vec![
                FakeStep::Text("plain answer".into()),
                FakeStep::Finish(FinishReason::Stop),
            ])),
            PermissionRules::default(),
        )
        .await;
        let session = support::create_root_session(&engine, "hya-main", model, dir.path()).await;
        engine
            .admit_user_prompt(session, "hi".into())
            .await
            .unwrap();
        engine
            .run_turn(
                session,
                &support::agent_spec("hya-main", model, dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let proj = engine.read_projection(session).await.unwrap();
        assert!(proj.session.usage.is_empty(), "{model}");
    }
}

#[tokio::test]
async fn bundle_usage_tree_scope_folds_descendants_and_merges_totals() {
    let store = SessionStore::connect_memory().await.unwrap();
    let (root, child, grandchild) = (SessionId::new(), SessionId::new(), SessionId::new());
    for (session, parent, agent) in [
        (root, None, "build"),
        (child, Some(root), "explore"),
        (grandchild, Some(child), "explore"),
    ] {
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent,
                    agent: AgentName::new(agent),
                    model: ModelRef::new("fake/model"),
                    workdir: "/tmp".into(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();
    }
    for (parent, c) in [(root, child), (child, grandchild)] {
        store
            .append_event(
                parent,
                &Event::MemberSpawned {
                    session: parent,
                    member: MemberId::new(),
                    child: Some(c),
                    subagent_type: AgentName::new("explore"),
                    description: "look".into(),
                    depth: 1,
                    directive: String::new(),
                    tool_call: None,
                },
            )
            .await
            .unwrap();
    }
    let tok = |input, output, reasoning, unknown| TokenUsage {
        input,
        output,
        reasoning,
        reasoning_unknown: unknown,
        ..TokenUsage::default()
    };
    for (session, model, tokens) in [
        (root, "fake/model", tok(10, 5, 2, false)),
        (child, "fake/model", tok(7, 3, 0, true)),
        (grandchild, "other/model", tok(1, 1, 1, false)),
    ] {
        store
            .append_event(
                session,
                &Event::UsageRecorded {
                    session,
                    message: None,
                    step: None,
                    model: ModelRef::new(model),
                    purpose: UsagePurpose::Turn,
                    tokens,
                },
            )
            .await
            .unwrap();
    }

    let report = session_usage_report(&store, root, UsageScope::Tree)
        .await
        .unwrap();
    let rows: Vec<_> = report.sessions.iter().map(|r| r.session).collect();
    assert_eq!(rows, [root, child, grandchild]);
    let val = serde_json::to_value(&report).unwrap();
    assert_eq!(val["scope"], "tree");
    assert_eq!(val["total"]["by_model"]["fake/model"]["input"], 17);
    assert_eq!(val["total"]["by_model"]["fake/model"]["output"], 8);
    assert_eq!(
        val["total"]["by_model"]["fake/model"]["split"],
        json!({"thinking": 2, "visible": 3, "unknown": 3})
    );
    assert_eq!(val["total"]["total"]["output"], 9);

    let single = StoreSessionReads::new(store.clone())
        .session_usage(child, UsageScope::Session)
        .await
        .unwrap();
    assert_eq!(
        (single.sessions.len(), single.total.total.totals.input),
        (1, 7)
    );
    let from_leaf = session_usage_report(&store, grandchild, UsageScope::Root)
        .await
        .unwrap();
    assert_eq!((from_leaf.root, from_leaf.sessions.len()), (root, 3));
    let subtree = session_usage_report(&store, child, UsageScope::Tree)
        .await
        .unwrap();
    assert_eq!(subtree.sessions.len(), 2);
    assert!(
        session_usage_report(&store, SessionId::new(), UsageScope::Tree)
            .await
            .is_err()
    );
}

// ---------------------------------------------------------------------------
// Calibrated token accounting & reasoning replay parity
// ---------------------------------------------------------------------------

const FIXTURES: &[(&str, &str, usize)] = &[
    (
        "rust_code",
        "pub fn resolved_threshold(cfg: &CompactionConfig, max_context: Option<u32>) -> usize {\n    let Some(window) = max_context.filter(|w| *w > 0) else {\n        return cfg.token_threshold;\n    };\n    if !(cfg.context_fraction > 0.0 && cfg.context_fraction <= 1.0) {\n        return cfg.token_threshold;\n    }\n    let scaled = f64::from(window) * f64::from(cfg.context_fraction);\n    (scaled as usize).max(MIN_RESOLVED_THRESHOLD)\n}\n",
        113,
    ),
    (
        "json_blob",
        "{\n  \"session\": \"01JQ7X8Z9ABCDEFGHJKMNPQRST\",\n  \"messages\": [\n    {\n      \"role\": \"user\",\n      \"tokens\": {\n        \"input\": 18422,\n        \"output\": 512,\n        \"cache_read\": 16384\n      }\n    },\n    {\n      \"role\": \"assistant\",\n      \"finish\": \"tool_calls\",\n      \"tools\": [\n        \"read\",\n        \"grep\",\n        \"bash\"\n      ]\n    }\n  ],\n  \"paths\": [\n    \"crates/hya-core/src/compaction.rs\",\n    \"crates/hya-tool/src/handle/artifact.rs\"\n  ]\n}",
        143,
    ),
    (
        "markdown_prose",
        "## Compaction ladder\n\nThe turn loop walks a fixed escalation order, cheapest and most recoverable\nfirst, and stops at the first rung that brings the transcript under the\ntrigger. Spilling tool output is lossless because the body moves to an\n`artifact://` handle and only the pointer stays in the transcript.\n",
        65,
    ),
    (
        "chinese",
        "本次我们开始开发上下文管理功能，首先阅读当前仓库，分析系统如何管理上下文。压缩工具的实现完全参考分级设计进行，使用多个工具分级依次尝试，从而实现当高效压缩工具有效时直接调用高效的压缩工具。",
        64,
    ),
    (
        "mixed_cjk",
        "接下来我们需要为 hya 引入类似 omp 的 artifact:// 等路径工具，即引入包括 skill:// 在内的这类基于 url 的索引工具，替换现有的索引工具，为 hya 带来统一化的外部工具调用体验。",
        59,
    ),
    (
        "indented_code",
        "impl Tool for ShellTool {\n    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {\n        let args: ShellArgs = serde_json::from_value(input)?;\n        let artifacts = ctx.handles.artifacts();\n        let mut sink = OutputSink::new(artifacts.clone());\n        for chunk in stream {\n            sink.push(&chunk)?;\n        }\n        Ok(sink.finish()?)\n    }\n}\n",
        93,
    ),
    (
        "numeric_table",
        "threshold=150000 reserve=16384 window=200000 fraction=0.75\ninput=18422 output=512 cache_read=16384 cache_write=0\n1234567890 987654321 42 3.14159265358979 0xDEADBEEF\n",
        60,
    ),
    (
        "log_output",
        "error[E0433]: failed to resolve: use of undeclared crate or module `handle`\n  --> crates/hya-tool/src/shell.rs:354:41\n   |\n354 |         let artifact_root = normalize(&absolutize(&ctx.workdir));\n   |                                         ^^^^^^^^^^ not found in this scope\nwarning: unused import: `std::sync::Arc`\n",
        78,
    ),
];

const BLOB_FIXTURES: &[(&str, &str, usize)] = &[
    (
        "base64_attachment",
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
        52,
    ),
    (
        "hex_digest_table",
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n",
        80,
    ),
    (
        "long_identifier_run",
        "averyveryverylongunbrokenidentifiernamethatkeepsgoingwithoutanyseparators",
        18,
    ),
];

fn rel_err(predicted: usize, truth: usize) -> f64 {
    (predicted as f64 - truth as f64) / truth as f64
}

#[test]
fn calibrated_tokenizer_accuracy_safety_and_blobs() {
    let tok = CalibratedTokenizer;
    assert_eq!((tok.count_text(""), tok.name()), (0, "calibrated"));
    assert!(tok.count_text("a") >= 1);

    let (mut cal_tot, mut base_tot, mut cal_max, mut base_max) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (name, text, truth) in FIXTURES {
        let err = rel_err(tok.count_text(text), *truth);
        let base = rel_err(text.len() / 4, *truth);
        assert!(err.abs() <= 0.20 && err > -0.15, "{name}: {err:+.3}");
        if *name == "json_blob" || *name == "numeric_table" {
            assert!(base < -0.20 && err.abs() < base.abs(), "{name}");
        }
        cal_tot += err.abs();
        base_tot += base.abs();
        cal_max = cal_max.max(err.abs());
        base_max = base_max.max(base.abs());
    }
    assert!(cal_tot < base_tot && cal_max < base_max);

    for (name, text, truth) in BLOB_FIXTURES {
        let err = rel_err(tok.count_text(text), *truth);
        assert!(err > -0.25 && err < 1.0, "{name}: {err:+.3}");
        let base = rel_err(text.len() / 4, *truth);
        if base < -0.25 {
            assert!(err.abs() < base.abs(), "{name}");
        }
    }
}

fn user_msg(text: &str) -> Message {
    Message::User {
        id: MessageId::new(),
        parts: vec![Part::Text {
            id: PartId::new(),
            text: text.into(),
        }],
    }
}

fn assistant_msg(parts: Vec<Part>, tokens: Option<TokenUsage>) -> Message {
    Message::Assistant {
        id: MessageId::new(),
        agent: AgentName::new("build"),
        model: ModelRef::new("test/model"),
        parts,
        finish: Some(FinishReason::Stop),
        tokens,
        last_round: None,
        last_step_start: None,
    }
}

#[test]
fn token_accounting_modes_cache_anchor_and_tool_payloads() {
    let make = |reported| {
        vec![
            user_msg(&"x".repeat(4_000)),
            assistant_msg(
                vec![Part::Text {
                    id: PartId::new(),
                    text: "ok".into(),
                }],
                reported,
            ),
        ]
    };
    let auto = TokenAccounting::new(TokenAccountingMode::Auto);
    let est = auto.estimate(&make(None));
    let plausible = TokenUsage {
        input: est as u64,
        output: 10,
        ..TokenUsage::default()
    };
    let implausible = TokenUsage {
        input: 3,
        output: 1,
        ..TokenUsage::default()
    };
    assert_eq!(
        auto.tokens_in_use(&make(None), true).source,
        TokenSource::Estimate
    );
    assert_eq!(
        auto.tokens_in_use(&make(Some(plausible)), false).source,
        TokenSource::Estimate
    );
    assert_eq!(
        auto.tokens_in_use(&make(Some(implausible)), true).source,
        TokenSource::Estimate
    );
    assert_eq!(
        auto.tokens_in_use(&make(Some(plausible)), true).source,
        TokenSource::Provider
    );

    let prov = TokenAccounting::new(TokenAccountingMode::Provider);
    let p_count = prov.tokens_in_use(&make(Some(implausible)), true);
    assert_eq!((p_count.source, p_count.tokens), (TokenSource::Provider, 3));
    assert_eq!(
        prov.tokens_in_use(
            &make(Some(TokenUsage {
                input: 500,
                cache_read: 1_000,
                cache_write: 700,
                ..TokenUsage::default()
            })),
            true,
        )
        .tokens,
        2_200
    );

    let est_mode = TokenAccounting::new(TokenAccountingMode::Estimate);
    let e_count = est_mode.tokens_in_use(
        &make(Some(TokenUsage {
            input: 999_999,
            ..TokenUsage::default()
        })),
        true,
    );
    assert_eq!(e_count.source, TokenSource::Estimate);
    assert!(e_count.tokens < 999_999);

    // Latest-round anchor beats cumulative message tokens.
    let mut messages = make(Some(TokenUsage {
        input: 50_000,
        output: 1,
        ..TokenUsage::default()
    }));
    if let Message::Assistant { last_round, .. } = &mut messages[1] {
        *last_round = Some(TokenUsage {
            input: 120,
            cache_read: 30,
            cache_write: 20,
            ..TokenUsage::default()
        });
    }
    messages.push(user_msg("appended after the last round"));
    let appended = prov.estimate(&messages[2..]);
    let anchored = prov.tokens_in_use(&messages, true);
    assert_eq!(
        (anchored.source, anchored.tokens),
        (TokenSource::Provider, 170 + appended)
    );

    // Mode parse round-trip and tool payload estimation.
    for mode in [
        TokenAccountingMode::Auto,
        TokenAccountingMode::Provider,
        TokenAccountingMode::Estimate,
    ] {
        assert_eq!(TokenAccountingMode::parse(mode.as_str()), Some(mode));
    }
    assert_eq!(
        TokenAccountingMode::parse("  AUTO "),
        Some(TokenAccountingMode::Auto)
    );
    assert_eq!(TokenAccountingMode::parse("nonsense"), None);

    let tool_only = vec![assistant_msg(
        vec![Part::Tool {
            id: PartId::new(),
            call_id: ToolCallId::new(),
            name: ToolName::new("grep"),
            state: ToolPartState::Completed {
                input: json!({ "pattern": "fn main" }),
                output: serde_json::Value::String("hit\n".repeat(500)),
                time_ms: 4,
            },
            provider_data: None,
        }],
        None,
    )];
    assert!(est_mode.estimate(&tool_only) > 400);
}

#[test]
fn reasoning_replay_estimation_matches_route_policies_and_anthropic_encoder() {
    use hya_provider::{AnthropicMessagesProtocol, Protocol};
    let reasoning = |text: &str, signed: bool| Part::Reasoning {
        id: PartId::new(),
        text: text.into(),
        provider_data: signed.then(|| json!({"type": "thinking", "signature": "sig"})),
    };
    let chat = |text: &str| Part::Reasoning {
        id: PartId::new(),
        text: text.into(),
        provider_data: Some(json!({"type": "reasoning_content"})),
    };
    let fixture = || {
        vec![
            user_msg("q1"),
            assistant_msg(vec![reasoning(&"o".repeat(400), true)], None),
            user_msg("q2"),
            assistant_msg(
                vec![
                    reasoning(&"c".repeat(400), true),
                    reasoning(&"u".repeat(400), false),
                ],
                None,
            ),
        ]
    };
    let accounting = TokenAccounting::new(TokenAccountingMode::Estimate);
    let messages = fixture();
    let none = accounting.estimate_with_reasoning_policy(&messages, ReasoningReplayPolicy::None);
    assert_eq!(none, accounting.estimate(&[user_msg("q1"), user_msg("q2")]));

    let anthropic = accounting
        .estimate_with_reasoning_policy(&messages, ReasoningReplayPolicy::SignedCurrentTurn);
    let current_signed =
        accounting.estimate(&[assistant_msg(vec![reasoning(&"c".repeat(400), true)], None)]);
    assert_eq!(anthropic, none + current_signed);
    assert!(accounting.estimate(&messages) > anthropic);

    let mut with_chat = fixture();
    with_chat.insert(2, assistant_msg(vec![chat(&"p".repeat(400))], None));
    with_chat.push(assistant_msg(vec![chat(&"n".repeat(400))], None));
    let chat_replayed = accounting.estimate(&[
        assistant_msg(vec![chat(&"p".repeat(400))], None),
        assistant_msg(vec![chat(&"n".repeat(400))], None),
    ]);
    assert_eq!(
        accounting
            .estimate_with_reasoning_policy(&with_chat, ReasoningReplayPolicy::ReasoningContent),
        accounting.estimate_with_reasoning_policy(&with_chat, ReasoningReplayPolicy::None)
            + chat_replayed
    );

    // Anthropic encoder and estimator agree on replayed thinking blocks.
    let turn_start = hya_provider::current_turn_start(&messages);
    let counted: Vec<String> = messages
        .iter()
        .enumerate()
        .flat_map(|(i, m)| match m {
            Message::Assistant { parts, .. } => parts
                .iter()
                .filter(|p| ReasoningReplayPolicy::SignedCurrentTurn.replays(turn_start, i, p))
                .filter_map(|p| match p {
                    Part::Reasoning { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    let body = AnthropicMessagesProtocol
        .encode(&CompletionRequest {
            model: ModelRef::new("test/model"),
            system: None,
            messages,
            tools: Vec::new(),
            temperature: None,
            max_output_tokens: Some(64),
            reasoning: None,
            headers: Default::default(),
        })
        .unwrap();
    let emitted: Vec<String> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "thinking")
        .map(|b| b["thinking"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(emitted, counted);
    assert_eq!(emitted, vec!["c".repeat(400)]);
}
