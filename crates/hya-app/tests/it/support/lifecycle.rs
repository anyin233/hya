//! Real app composition for lifecycle regressions. No resident registration,
//! terminal store operations, claim manipulation, or turn-lease manipulation.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use hya_app::{BuiltSessionEngine, InvocationPolicy, build_session_engine_pure};
use hya_engine::{AgentSpec, CreateSession, SessionEngine};
use hya_proto::{
    AgentName, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef, PartProjection,
    SessionId, ToolPartState,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::PermissionModel;
use serde_json::{Value, json};
use sqlx::{Connection, SqliteConnection};
use tokio::sync::{Semaphore, broadcast};
use tokio_util::sync::CancellationToken;

pub const BOUND: Duration = Duration::from_secs(11);
pub const HANDOFF: &str = "1. Goal\nComplete the assigned work\n2. Current state\nReady\n3. Files and code\nNone\n4. Decisions\nNone\n5. Pending\nNone\n6. Next step\nAwait new work";

/// Counting permits retain both arrival and release signals even if the test
/// observes them after the production task has reached the control point.
pub struct Gate {
    entered: Semaphore,
    release: Semaphore,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}

impl Gate {
    pub async fn reached(&self) {
        self.entered.acquire().await.unwrap().forget();
    }
    pub async fn arrived(&self) {
        tokio::time::timeout(BOUND, self.reached())
            .await
            .expect("control point not reached");
    }

    pub fn release(&self) {
        self.release.add_permits(1);
    }

    async fn block(&self) {
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
    }
}

pub struct Round {
    pub steps: Vec<FakeStep>,
    pub gate: Option<Arc<Gate>>,
    pub stream_gate: Option<Arc<Gate>>,
    pub error: Option<ProviderError>,
}

impl Round {
    pub fn text(text: &str) -> Self {
        Self {
            steps: vec![
                FakeStep::Text(text.into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            gate: None,
            stream_gate: None,
            error: None,
        }
    }

    /// A member's answer cut by the length limit: the member stays live with
    /// `text` as its current state, where a complete final answer would be
    /// delivered as its report.
    pub fn unfinished(text: &str) -> Self {
        Self {
            steps: vec![
                FakeStep::Text(text.into()),
                FakeStep::Finish(FinishReason::Length),
            ],
            gate: None,
            stream_gate: None,
            error: None,
        }
    }

    pub fn tool(name: &str, input: Value) -> Self {
        Self {
            steps: vec![
                FakeStep::ToolCall {
                    name: name.into(),
                    input,
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            gate: None,
            stream_gate: None,
            error: None,
        }
    }

    pub fn gated(mut self, gate: Arc<Gate>) -> Self {
        self.gate = Some(gate);
        self
    }
    pub fn stream_gated(mut self, gate: Arc<Gate>) -> Self {
        self.stream_gate = Some(gate);
        self
    }

    pub fn failure(note: &str) -> Self {
        Self {
            steps: Vec::new(),
            gate: None,
            stream_gate: None,
            error: Some(ProviderError::Incompatible(note.into())),
        }
    }
}

#[derive(Clone)]
pub struct Request {
    pub session: SessionId,
    pub completion: CompletionRequest,
}

pub struct ScriptedProvider {
    scripts: Mutex<BTreeMap<String, VecDeque<Round>>>,
    script_sessions: Mutex<BTreeMap<String, SessionId>>,
    pub requests: broadcast::Sender<Request>,
    pub handoffs: Mutex<VecDeque<Round>>,
    pub unscripted: Mutex<VecDeque<Round>>,
    pub captured: Mutex<Vec<Request>>,
}

impl Default for ScriptedProvider {
    fn default() -> Self {
        Self {
            scripts: Mutex::default(),
            script_sessions: Mutex::default(),
            requests: broadcast::channel(256).0,
            handoffs: Mutex::default(),
            unscripted: Mutex::default(),
            captured: Mutex::default(),
        }
    }
}

impl ScriptedProvider {
    /// A unique directive selects its own script, so main synthesis and handoff
    /// calls cannot consume a member's or a test command's next round.
    pub fn script(&self, rounds: Vec<Round>) -> String {
        let marker = format!("M4SCRIPT{}.", MessageId::new());
        self.scripts
            .lock()
            .unwrap()
            .insert(marker.clone(), rounds.into());
        marker
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        Some(b"m4-lifecycle-script-v1".to_vec())
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        // ModelSummarizer requests have no tools. The app builder's real
        // summarizer is gated here, rather than replaced by a test seam.
        let round = if request.tools.is_empty() {
            self.handoffs
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Round::text(HANDOFF))
        } else {
            let mut scripts = self.scripts.lock().unwrap();
            let sessions = self.script_sessions.lock().unwrap();
            request
                .messages
                .iter()
                .rev()
                .filter(|message| matches!(message, hya_proto::Message::User { .. }))
                .find_map(|message| {
                    let encoded = serde_json::to_string(message).unwrap();
                    scripts
                        .iter_mut()
                        .filter(|(marker, _)| {
                            encoded.contains(marker.as_str())
                                && sessions
                                    .get(marker.as_str())
                                    .is_none_or(|owner| *owner == session)
                        })
                        .max_by_key(|(marker, _)| encoded.rfind(marker.as_str()))
                        .map(|(_, rounds)| {
                            rounds
                                .pop_front()
                                .unwrap_or_else(|| Round::unfinished("settled"))
                        })
                })
                .unwrap_or_else(|| {
                    self.unscripted
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| Round::text("main settled"))
                })
        };
        let captured = Request {
            session,
            completion: request,
        };
        self.captured.lock().unwrap().push(captured.clone());
        let _ = self.requests.send(captured);
        if let Some(gate) = round.gate {
            gate.block().await;
        }
        if let Some(error) = round.error {
            return Err(error);
        }
        let mut events = FakeProvider::materialize(&round.steps, session, message);
        if let Some(gate) = round.stream_gate {
            // Hold TextEnd as well as Finish: cancellation must flush the
            // partial text, rather than testing text already persisted at End.
            let tail = events.split_off(events.len() - 2);
            assert!(matches!(tail[0], hya_proto::Event::TextEnd { .. }));
            return Ok(Box::pin(
                stream::iter(events.into_iter().map(Ok)).chain(
                    stream::once(async move {
                        gate.block().await;
                        stream::iter(tail.into_iter().map(Ok))
                    })
                    .flatten(),
                ),
            ));
        }
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

pub struct Harness {
    pub built: BuiltSessionEngine,
    pub engine: Arc<SessionEngine>,
    pub provider: Arc<ScriptedProvider>,
    pub agent: AgentSpec,
    pub root: SessionId,
    pub dir: PathBuf,
}

impl Harness {
    pub async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("hya-m4-{}", SessionId::new()));
        std::fs::create_dir(&dir).unwrap();
        Self::open(dir, None, Vec::new()).await
    }

    pub async fn open(
        dir: PathBuf,
        root: Option<SessionId>,
        plugins: Vec<hya_plugin::config::PluginSpec>,
    ) -> Self {
        Self::open_with_base_prompt(dir, root, plugins, String::new()).await
    }

    pub async fn with_base_prompt(prompt: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hya-m4-{}", SessionId::new()));
        std::fs::create_dir(&dir).unwrap();
        Self::open_with_base_prompt(dir, None, Vec::new(), prompt.into()).await
    }

    async fn open_with_base_prompt(
        dir: PathBuf,
        root: Option<SessionId>,
        plugins: Vec<hya_plugin::config::PluginSpec>,
        system_prompt: String,
    ) -> Self {
        let store = SessionStore::connect(dir.join("events.db").to_str().unwrap())
            .await
            .unwrap();
        let provider = Arc::new(ScriptedProvider::default());
        let agent = AgentSpec {
            name: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            system_prompt,
            workdir: dir.clone(),
            reasoning: None,
        };
        let built = build_session_engine_pure(
            store,
            ProviderRouter::new().with(provider.clone()),
            &agent,
            BTreeMap::new(),
            plugins,
            (
                InvocationPolicy::default().with_model(PermissionModel::Danger),
                hya_app::config::EngineSettings::default(),
            ),
        )
        .await
        .unwrap();
        let engine = built.engine();
        let root = match root {
            Some(root) => root,
            None => engine
                .create(CreateSession {
                    parent: None,
                    agent: agent.name.clone(),
                    model: agent.model.clone(),
                    workdir: dir.to_string_lossy().into_owned(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                })
                .await
                .unwrap(),
        };
        Self {
            built,
            engine,
            provider,
            agent,
            root,
            dir,
        }
    }

    /// Execute the actual dynamically loaded tool inside a root model turn.
    /// Returns its durable result (including errors) rather than a supervisor reply.
    pub async fn tool(&self, name: &str, input: Value) -> ToolPartState {
        let marker = self.provider.script(vec![Round::tool(name, input)]);
        self.engine
            .admit_user_prompt(self.root, marker)
            .await
            .unwrap();
        tokio::time::timeout(
            BOUND,
            self.engine
                .run_turn(self.root, &self.agent, CancellationToken::new()),
        )
        .await
        .expect("root tool round timed out")
        .unwrap();
        let projection = self.engine.read_projection(self.root).await.unwrap();
        projection
            .session
            .messages
            .iter()
            .rev()
            .flat_map(|m| m.parts.iter().rev())
            .find_map(|part| match part {
                PartProjection::Tool {
                    name: tool, state, ..
                } if tool.as_str() == name => Some(state.clone()),
                _ => None,
            })
            .expect("model round did not execute requested tool")
    }

    pub async fn start_tool(
        &self,
        name: &str,
        input: Value,
    ) -> tokio::task::JoinHandle<Result<FinishReason, hya_engine::CoreError>> {
        let marker = self.provider.script(vec![Round::tool(name, input)]);
        self.engine
            .admit_user_prompt(self.root, marker)
            .await
            .unwrap();
        let engine = self.engine.clone();
        let agent = self.agent.clone();
        let root = self.root;
        tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        })
    }

    pub async fn spawn(&self, rounds: Vec<Round>) -> (SessionId, String) {
        self.spawn_agent("hya-task", rounds).await
    }

    pub async fn spawn_agent(&self, agent: &str, rounds: Vec<Round>) -> (SessionId, String) {
        let directive = self.provider.script(rounds);
        let result = self.tool("task", json!({"description": "lifecycle member", "prompt": directive, "subagent_type": agent, "model": "fake"})).await;
        assert!(
            matches!(result, ToolPartState::Completed { .. }),
            "task failed: {result:?}"
        );
        let projection = self.engine.read_projection(self.root).await.unwrap();
        let members = hya_tool::extension::members_projection(&projection);
        let member = members.last().unwrap();
        let child = member.child.unwrap();
        let team = hya_tool::extension::team_projection(&projection);
        let handle = team
            .roster
            .iter()
            .find(|(_, entry)| entry.session == child)
            .map(|(h, _)| h.clone())
            .or_else(|| {
                team.archived
                    .iter()
                    .find(|(_, entry)| entry.session == child)
                    .map(|(h, _)| h.clone())
            })
            .unwrap();
        (child, handle)
    }

    /// Use the public spawn boundary with a controlled activation sidecar.
    pub async fn spawn_with_sidecar(
        &self,
        rounds: Vec<Round>,
        sidecar: Arc<dyn hya_engine::BoundSidecarFactory>,
    ) -> Result<(SessionId, String), hya_engine::CoreError> {
        let binding = self
            .engine
            .bind_session_runtime(self.root, &self.dir)
            .await?;
        let agents = self.engine.agent_roster_for_binding(&binding, "hya-main")?;
        let resources = self
            .engine
            .agent_resource_policy_for_binding(&binding, "hya-main")?;
        let supervisor = self.built.resident_supervisor();
        supervisor
            .ensure_main(
                self.root,
                self.agent.clone(),
                (binding.clone(), agents, resources),
                None,
                None,
            )
            .await?;
        let agent = self
            .engine
            .agent_spec_for_binding(&binding, &self.agent, "hya-task")?;
        let agents = self.engine.agent_roster_for_binding(&binding, "hya-task")?;
        let resources = self
            .engine
            .agent_resource_policy_for_binding(&binding, "hya-task")?;
        supervisor
            .spawn_resident(
                self.root,
                agent,
                (binding, agents, resources, Some(sidecar)),
                self.provider.script(rounds),
                None,
                None,
            )
            .await
    }

    pub async fn mail(&self, handle: &str, body: String) -> Result<(), hya_engine::CoreError> {
        // Real DM mail appears in both peers' histories. Bind its script to
        // the recipient so a concurrent main synthesis cannot steal the round.
        let projection = self.engine.read_projection(self.root).await.unwrap();
        let team = hya_tool::extension::team_projection(&projection);
        let target = match MailEndpoint::parse(handle) {
            MailEndpoint::Handle(path) => Some(path),
            MailEndpoint::Channel(channel) => team
                .channels
                .get(&channel)
                .and_then(|channel| {
                    channel
                        .members
                        .iter()
                        .find(|member| member.as_str() != "main")
                })
                .cloned(),
        };
        if let Some(target) = target {
            let session = team
                .roster
                .get(&target)
                .map(|entry| entry.session)
                .or_else(|| team.archived.get(&target).map(|entry| entry.session));
            if let Some(session) = session {
                let scripts = self.provider.scripts.lock().unwrap();
                let mut sessions = self.provider.script_sessions.lock().unwrap();
                for marker in scripts
                    .keys()
                    .filter(|marker| body.contains(marker.as_str()))
                {
                    sessions.insert(marker.clone(), session);
                }
            }
        }
        self.engine
            .mail_send(
                self.root,
                MailEndpoint::parse(handle),
                MailKind::Message,
                body,
            )
            .await
            .map(|_| ())
    }

    /// Conditions inspect durable projections, and wake only on published
    /// envelopes. Subscribe before checking to avoid missing a commit.
    pub async fn until(
        &self,
        session: SessionId,
        predicate: impl Fn(&hya_proto::Projection) -> bool,
    ) {
        let mut events = self.engine.bus().subscribe();
        let result = tokio::time::timeout(BOUND, async {
            loop {
                if predicate(&self.engine.read_projection(session).await.unwrap()) {
                    break;
                }
                events.recv().await.unwrap();
            }
        })
        .await;
        if result.is_err() {
            panic!(
                "expected durable lifecycle event did not arrive; projection: {:?}; sessions: {:?}",
                self.engine.read_projection(session).await.unwrap(),
                self.engine.store().list_sessions().await.unwrap()
            );
        }
    }

    pub async fn archived(&self, handle: &str) {
        self.until(self.root, |p| {
            hya_tool::extension::team_projection(p)
                .archived
                .contains_key(handle)
        })
        .await;
    }

    pub async fn idle(&self, child: SessionId, handle: &str) {
        self.until(child, |p| {
            p.session
                .messages
                .last()
                .is_some_and(|m| m.role == hya_proto::Role::Assistant && m.finish.is_some())
        })
        .await;
        self.until(self.root, |p| {
            hya_tool::extension::team_projection(p)
                .roster
                .get(handle)
                .is_some_and(|r| r.status == hya_proto::RosterStatus::Idle)
        })
        .await;
    }

    pub async fn trigger(&self, tag: &str) {
        assert!(matches!(tag, "agent_archived" | "agent_restarted"));
        let mut db = SqliteConnection::connect(&format!(
            "sqlite://{}",
            self.dir.join("events.db").display()
        ))
        .await
        .unwrap();
        sqlx::query(&format!("CREATE TRIGGER m4_reject BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.kind') = '{tag}' BEGIN SELECT RAISE(ABORT, 'M4 injected {tag}'); END")).execute(&mut db).await.unwrap();
    }

    pub async fn heal(&self) {
        let mut db = SqliteConnection::connect(&format!(
            "sqlite://{}",
            self.dir.join("events.db").display()
        ))
        .await
        .unwrap();
        sqlx::query("DROP TRIGGER m4_reject")
            .execute(&mut db)
            .await
            .unwrap();
    }

    pub async fn claim_epoch(&self, child: SessionId) -> i64 {
        let mut db = SqliteConnection::connect(&format!(
            "sqlite://{}",
            self.dir.join("events.db").display()
        ))
        .await
        .unwrap();
        sqlx::query_scalar(
            "SELECT epoch FROM bundle_claim WHERE bundle = 'hya/multi-agent' AND claim_key = ?",
        )
        .bind(child.to_string())
        .fetch_one(&mut db)
        .await
        .unwrap()
    }

    pub async fn shutdown(&mut self) {
        tokio::time::timeout(BOUND, self.built.shutdown())
            .await
            .expect("runtime shutdown exceeded ending bound")
            .unwrap();
    }

    /// Run the Workflow `m4` and return its raw `/command` `started` reply.
    pub async fn workflow(&self, nodes: &str, graph: &str) -> serde_json::Value {
        let directory = self.dir.join(".hya/workflows");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("m4.hya.md"), format!("---\nkind: Workflow\nname: m4\ndescription: Lifecycle fixture\nnodes:\n{nodes}---\nflowchart TD\n{graph}\n")).unwrap();
        let outcome = tokio::time::timeout(
            BOUND,
            self.built
                .engine()
                .invoke_bundle_api(hya_engine::BundleApiCall {
                    bundle: "hya/workflow".to_owned(),
                    method: hya_engine::ApiMethod::Post,
                    session: Some(self.root),
                    path: "/command".to_owned(),
                    query: BTreeMap::new(),
                    body: serde_json::json!({
                        "run": {
                            "name": "m4",
                            "delivery": "finished"
                        }
                    }),
                }),
        )
        .await
        .expect("Workflow exceeded fixture deadline")
        .unwrap();
        assert_eq!(outcome.status, 200, "{}", outcome.body);
        outcome.body["started"].clone()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn output(state: &ToolPartState) -> &Value {
    match state {
        ToolPartState::Completed { output, .. } => output,
        _ => panic!("tool failed: {state:?}"),
    }
}

pub async fn request_for(
    rx: &mut broadcast::Receiver<Request>,
    session: SessionId,
) -> CompletionRequest {
    tokio::time::timeout(BOUND, async {
        loop {
            let request = rx.recv().await.unwrap();
            if request.session == session {
                return request.completion;
            }
        }
    })
    .await
    .expect("provider request not observed")
}

/// Config environment is read by the real builder. Re-exec one test instead
/// of mutating process environment while Tokio/plugin threads are running.
pub fn isolated_config(test: &str, entries: &[(&str, &str)]) -> bool {
    if std::env::var("HYA_M4_CONFIG_CHILD").as_deref() == Ok(test) {
        return false;
    }
    let result = std::process::Command::new("timeout")
        .arg("45")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("resident_lifecycle::{test}"),
            "--include-ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("HYA_M4_CONFIG_CHILD", test)
        .envs(entries.iter().copied())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "isolated configuration failed ({:?}):\n{}\n{}",
        result.status.code(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    true
}

/// This branch runs only in a disposable copy of this integration-test binary.
/// The parent kills it after the socket handshake, giving startup recovery an
/// actual crashed runtime instead of manually editing claims or resume rows.
pub async fn crash_seed_if_requested(test: &str, queue_main: bool) {
    use tokio::io::AsyncWriteExt;
    if std::env::var("HYA_M4_CRASH_SEED").as_deref() != Ok(test) {
        return;
    }
    let h = Harness::open(
        PathBuf::from(std::env::var_os("HYA_M4_CRASH_DIR").unwrap()),
        None,
        Vec::new(),
    )
    .await;
    if queue_main {
        let gate = Arc::new(Gate::default());
        let first = h.provider.script(vec![Round::unfinished("A before crash")]);
        let second = h.provider.script(vec![Round::unfinished("B before crash")]);
        let marker = h.provider.script(vec![
            Round::tool("task", json!({"tasks": [
                {"description":"A", "prompt":first, "subagent_type":"hya-task", "model":"fake"},
                {"description":"B", "prompt":second, "subagent_type":"hya-task", "model":"fake"},
            ]})),
            Round::text("ROOT_BEFORE_CRASH").stream_gated(gate.clone()),
        ]);
        h.engine.admit_user_prompt(h.root, marker).await.unwrap();
        let engine = h.engine.clone();
        let agent = h.agent.clone();
        let root = h.root;
        let turn = tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        });
        gate.arrived().await;
        let team =
            hya_tool::extension::team_projection(&h.engine.read_projection(h.root).await.unwrap());
        let members = team
            .roster
            .iter()
            .filter(|(_, r)| r.session != h.root)
            .map(|(handle, r)| (r.session, handle.clone()))
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 2);
        for (child, handle) in &members {
            h.idle(*child, handle).await;
        }
        h.engine
            .mail_send(
                members[0].0,
                MailEndpoint::parse("main"),
                MailKind::Message,
                "QUEUED_BEFORE_RESTORE".into(),
            )
            .await
            .unwrap();
        let state = hya_server::AppState::new(h.engine.clone(), Arc::new(h.agent.clone()))
            .with_auto_title(false);
        state
            .stop_for_restart(Duration::from_secs(5))
            .await
            .unwrap();
        turn.await.unwrap().unwrap();
        assert!(
            h.engine
                .store()
                .list_pending_resumes()
                .await
                .unwrap()
                .iter()
                .any(|row| row.session == h.root),
            "stop_for_restart must seed the root's restore obligation"
        );
    } else {
        let (a, first) = h.spawn(vec![Round::unfinished("A before crash")]).await;
        h.idle(a, &first).await;
        let (b, second) = h.spawn(vec![Round::unfinished("B before crash")]).await;
        h.idle(b, &second).await;
    }
    let mut ready = tokio::net::TcpStream::connect(std::env::var("HYA_M4_CRASH_READY").unwrap())
        .await
        .unwrap();
    ready
        .write_all(format!("{}\n", h.root).as_bytes())
        .await
        .unwrap();
    std::future::pending::<()>().await;
}

struct CrashChild(std::process::Child);

impl Drop for CrashChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub async fn crashed_team(test: &str) -> Harness {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let dir = std::env::temp_dir().join(format!("hya-m4-crash-{}", SessionId::new()));
    std::fs::create_dir(&dir).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let log = std::fs::File::create(dir.join("seed.log")).unwrap();
    let mut child = CrashChild(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("resident_lifecycle::{test}"),
                "--include-ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("HYA_M4_CRASH_SEED", test)
            .env("HYA_M4_CRASH_DIR", &dir)
            .env(
                "HYA_M4_CRASH_READY",
                listener.local_addr().unwrap().to_string(),
            )
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    std::fs::write(dir.join("seed.pid"), child.0.id().to_string()).unwrap();
    let root = tokio::time::timeout(Duration::from_secs(20), async {
        let (socket, _) = listener.accept().await.unwrap();
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).await.unwrap();
        line.trim().parse::<SessionId>().unwrap()
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "crash fixture did not become ready: {}",
            std::fs::read_to_string(dir.join("seed.log")).unwrap()
        )
    });
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    drop(child);
    Harness::open(dir, Some(root), Vec::new()).await
}

/// The public installed-Bundle route supplies a real executable sidecar.
pub async fn install_sidecar_bundle() {
    // Bundle processes have a cleared environment, including PATH.
    let python = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = serde_json::to_string(String::from_utf8(python.stdout).unwrap().trim()).unwrap();
    let control =
        PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()).join("m4-sidecar-control.json");
    let control = serde_json::to_string(&control.to_string_lossy()).unwrap();
    let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "m4-sidecar",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.hya.md",
                br#"---
kind: Bundle
schema_version: 2
identity: { id: hya/m4-sidecar, version: 1.0.0, publisher: hya-tests }
runtime:
  kind: js
  mode: managed
  entry: extensions/runtime.js
resources:
  tools:
    - { id: echo, path: extensions/runtime.js }
agents:
  - id: m4-sidecar-worker
    role: subagent
    resource_view:
      allow:
        - bundle:hya/m4-sidecar/tool/echo
        - harness:tool/report
---
You are the lifecycle sidecar worker.
"#,
            ),
            hya_bundle::SourceFile::new(
                "extensions/runtime.js",
                r#"import { connect } from "node:net";
const controlFile = CONTROL_FILE;
async function controlProcess() {
  if (!(await Bun.file(controlFile).exists())) return;
  const config = await Bun.file(controlFile).json();
  if (config.refuse_start) throw new Error("M4 sidecar start failure");
  const socket = connect({ host: "127.0.0.1", port: config.port });
  await new Promise((resolve, reject) => {
    socket.once("connect", resolve);
    socket.once("error", reject);
  });
  socket.write(JSON.stringify({ role: "main", pid: process.pid, path: import.meta.url }) + "\n");
  socket.on("data", () => process.exit(17));
  socket.on("end", () => process.exit(0));
  if (config.hold_stderr) {
    const script = String.raw`import json,os,socket,sys
s=socket.create_connection(('127.0.0.1',int(sys.argv[1])))
s.sendall((json.dumps({'role':'stderr','pid':os.getpid(),'path':sys.argv[2]})+'\n').encode())
s.recv(1024)
s.close()
`;
    Bun.spawn([PYTHON_COMMAND, "-c", script, String(config.port), import.meta.url], {
      stdin: "ignore", stdout: "ignore", stderr: "inherit",
    });
  }
}
export default {
  id: "runtime",
  server: async () => {
    await controlProcess();
    return { tool: { echo: {
    description: "M4 sidecar echo",
    args: { type: "object", properties: {} },
    execute: async () => "sidecar echo",
    } } };
  },
};
"#
                .replace("CONTROL_FILE", &control)
                .replace("PYTHON_COMMAND", &python)
                .into_bytes(),
            ),
        ],
    ))
    .unwrap();
    let path = hya_app::bundle_registry_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let registry = hya_app::bundle_registry::BundleRegistry::connect(path.to_str().unwrap())
        .await
        .unwrap();
    registry
        .install(
            &[],
            hya_app::bundle_registry::NamespaceInstallPolicy::DenyConflicts,
            hya_app::bundle_registry::BundleInstallCandidate {
                source_digest: [53; 32],
                prepared_digest: prepared.digest().to_string(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
}

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn isolated_sidecar_config(test: &str) -> bool {
    isolated_sidecar_config_with(test, &[])
}

pub fn isolated_sidecar_config_with(test: &str, overrides: &[(&str, &str)]) -> bool {
    if std::env::var("HYA_M4_CONFIG_CHILD").as_deref() == Ok(test) {
        return false;
    }
    let directory =
        TestDirectory(std::env::temp_dir().join(format!("hya-m4-sidecar-{}", SessionId::new())));
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.to_str().unwrap();
    let mut entries = vec![("XDG_DATA_HOME", path), ("XDG_CONFIG_HOME", path)];
    entries.extend_from_slice(overrides);
    isolated_config(test, &entries)
}

/// Controls actual activation processes through a socket, not a private
/// supervisor handle. The optional descendant retains the captured stderr pipe
/// after its Bun parent dies, making force-termination genuinely non-cooperative.
pub struct SidecarControl {
    listener: tokio::net::TcpListener,
    sockets: Vec<(String, tokio::net::TcpStream)>,
    greetings: Vec<Value>,
    hold_stderr: bool,
}

impl SidecarControl {
    pub async fn new(hold_stderr: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config =
            json!({"port":listener.local_addr().unwrap().port(),"hold_stderr":hold_stderr});
        let path = PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
            .join("m4-sidecar-control.json");
        std::fs::write(path, config.to_string()).unwrap();
        Self {
            listener,
            sockets: Vec::new(),
            greetings: Vec::new(),
            hold_stderr,
        }
    }

    pub async fn started(&mut self) {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let result = tokio::time::timeout(BOUND, async {
            while !self.sockets.iter().any(|(role, _)| role == "main")
                || (self.hold_stderr && !self.sockets.iter().any(|(role, _)| role == "stderr"))
            {
                let (socket, _) = self.listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut line = String::new();
                socket.read_line(&mut line).await.unwrap();
                let hello: Value = serde_json::from_str(&line).unwrap();
                let role = hello["role"].as_str().unwrap();
                let role = if hello["path"].as_str().unwrap().contains("/activations/") {
                    role.to_string()
                } else {
                    format!("global-{role}")
                };
                let pid = hello["pid"].as_u64().unwrap();
                let data = PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap());
                std::fs::write(data.join(format!("m4-{role}-{pid}.pid")), pid.to_string()).unwrap();
                self.greetings.push(hello);
                self.sockets.push((role, socket.into_inner()));
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "activation sidecar control did not connect: {:?}",
            self.greetings
        );
    }

    pub async fn die(&mut self) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let index = self
            .sockets
            .iter()
            .position(|(role, _)| role == "main")
            .unwrap();
        let (_, mut socket) = self.sockets.remove(index);
        socket.write_all(b"die\n").await.unwrap();
        tokio::time::timeout(BOUND, socket.read_to_end(&mut Vec::new()))
            .await
            .unwrap()
            .unwrap();
    }

    pub async fn shutdown(&mut self) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (_, mut socket) in self.sockets.drain(..) {
            let _ = socket.shutdown().await;
            let ended = tokio::time::timeout(BOUND, socket.read_to_end(&mut Vec::new()))
                .await
                .expect("controlled sidecar did not exit");
            if let Err(error) = ended {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            }
        }
    }
}

/// Sidecar loss and handle disposal observed through the public activation seam.
#[derive(Clone)]
pub struct LossSidecar {
    pub loss: tokio_util::sync::CancellationToken,
    pub starts: Arc<std::sync::atomic::AtomicUsize>,
    pub drops: Arc<std::sync::atomic::AtomicUsize>,
    pub dropped: Arc<tokio::sync::Notify>,
}

struct LossHandle {
    probe: LossSidecar,
    loss: tokio_util::sync::CancellationToken,
}

impl Drop for LossHandle {
    fn drop(&mut self) {
        self.probe
            .drops
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.probe.dropped.notify_one();
    }
}

#[async_trait]
impl hya_engine::BoundSidecarFactory for LossSidecar {
    async fn start(
        &self,
        _: hya_engine::SidecarStart,
    ) -> Result<Box<dyn hya_engine::SidecarHandle>, hya_engine::CoreError> {
        let first = self
            .starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 0;
        Ok(Box::new(LossHandle {
            probe: self.clone(),
            loss: if first {
                self.loss.clone()
            } else {
                tokio_util::sync::CancellationToken::new()
            },
        }))
    }
}

#[async_trait]
impl hya_engine::SidecarHandle for LossHandle {
    async fn ready(&mut self) -> Result<(), hya_engine::CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), hya_engine::CoreError> {
        Ok(())
    }
    fn loss_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        Some(self.loss.clone())
    }
}
