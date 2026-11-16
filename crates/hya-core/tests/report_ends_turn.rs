//! An accepted `report` ends the member's turn (0.41.0 real-task fix).
//!
//! Real run 5: a member whose `report` was accepted kept taking model rounds
//! and re-reporting ("Acknowledged closure…") — 43 reports in one turn for one
//! implementer, 64 for the reviewer. The engine now ends the turn right after
//! the tool round in which a report is accepted (no further model call), a
//! second `report` in that turn is rejected, and a member woken by mail after
//! its report runs a new episode that may report once more.
//!
//! A member that ends its turn with a final answer and no `report` reports
//! that answer (REPORT H8): the same acceptance path, outcome `done`. An empty
//! or cut-off answer, or unread mail at the turn's end, leaves it idle.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hya_core::{
    AgentCatalog, AgentSpec, CreateSession, EventBus, ResidentSupervisor, RuntimeRegistry,
    SessionEngine, SubagentGovernor, SubagentLimits, run_lifecycle_service, run_mailbox_service,
};
use hya_proto::{
    AgentName, ArchiveReason, Event, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef,
    Role, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, LifecyclePlane, MailboxPlane, Mode, PermissionPlane, PermissionRules, Rule,
    ToolRegistry,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const BUNDLE: &str = "kind: Bundle\nidentity: { id: acme/report-turn, version: 1.0.0, publisher: acme }\nagents:\n  - id: lead\n    role: main\n    can_spawn: [scout]\n    resource_view: { allow: [harness:tool/grep] }\n  - id: scout\n    role: subagent\n    resource_view: { allow: [harness:tool/grep, harness:tool/glob] }\n";

/// What a member does in every model round: the script receives the member's
/// 0-based model-round number and returns that round's steps.
type MemberScript = dyn Fn(usize) -> Vec<FakeStep> + Send + Sync;

/// The lead finishes at once; every member round follows `script`.
struct ScriptProvider {
    root: Mutex<Option<SessionId>>,
    member_rounds: Mutex<HashMap<SessionId, usize>>,
    script: Box<MemberScript>,
    engine: OnceLock<Weak<SessionEngine>>,
    /// The lead mails the member while its first model round streams, so the
    /// mail is still unread when that round ends the turn.
    mail_in_first_round: AtomicBool,
}

impl ScriptProvider {
    fn new(script: impl Fn(usize) -> Vec<FakeStep> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            root: Mutex::new(None),
            member_rounds: Mutex::new(HashMap::new()),
            script: Box::new(script),
            engine: OnceLock::new(),
            mail_in_first_round: AtomicBool::new(false),
        })
    }

    fn rounds(&self, session: SessionId) -> usize {
        self.member_rounds
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(0)
    }
}

#[async_trait]
impl Provider for ScriptProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let is_root = *self.root.lock().unwrap() == Some(session);
        let steps = if is_root {
            vec![
                FakeStep::Text("lead done".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        } else {
            let round = {
                let mut rounds = self.member_rounds.lock().unwrap();
                let count = rounds.entry(session).or_default();
                *count += 1;
                *count - 1
            };
            if round == 0 && self.mail_in_first_round.load(Ordering::SeqCst) {
                let engine = self.engine.get().and_then(Weak::upgrade).unwrap();
                let root = self.root.lock().unwrap().unwrap();
                let handle = engine
                    .read_projection(root)
                    .await
                    .unwrap()
                    .team
                    .roster
                    .values()
                    .find(|entry| entry.session == session)
                    .unwrap()
                    .handle
                    .clone();
                engine
                    .mail_send(
                        root,
                        MailEndpoint::Handle(handle),
                        MailKind::Message,
                        "also check the tests".to_string(),
                    )
                    .await
                    .unwrap();
            }
            (self.script)(round)
        };
        let events = FakeProvider::materialize(&steps, session, message);
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

fn spec(name: &str) -> AgentSpec {
    AgentSpec {
        name: AgentName::new(name),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("."),
        reasoning: None,
    }
}

fn report_call(result: &str) -> FakeStep {
    FakeStep::ToolCall {
        name: "report".to_string(),
        input: json!({ "result": result }),
    }
}

struct Team {
    engine: Arc<SessionEngine>,
    supervisor: Arc<ResidentSupervisor>,
    root: SessionId,
    binding: hya_core::TurnBinding,
}

async fn team(provider: Arc<ScriptProvider>) -> Team {
    let source = hya_bundle::BundleSource::new(
        "report-turn",
        vec![hya_bundle::SourceFile::new("bundle.yaml", BUNDLE)],
    );
    let prepared = hya_bundle::prepare_package(source).unwrap();
    let bundles = hya_bundle::BundleCatalog::from_prepared(prepared.bundles()).unwrap();
    let agents = AgentCatalog::new(Arc::new(bundles)).unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        Arc::new(agents),
    ));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![Rule {
        action: Action::Read,
        resource_pattern: "*".to_string(),
        mode: Mode::Allow,
    }]));
    let (mailbox, mailbox_rx) = MailboxPlane::new();
    let (lifecycle, lifecycle_rx) = LifecyclePlane::new();
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(provider.clone())),
            runtime,
            permission,
            EventBus::default(),
        )
        .with_mailbox(mailbox)
        .with_lifecycle(lifecycle)
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    tokio::spawn(run_mailbox_service(
        engine.clone(),
        mailbox_rx,
        tokio_util::sync::CancellationToken::new(),
    ));
    let supervisor = ResidentSupervisor::start(engine.clone());
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        supervisor.clone(),
        lifecycle_rx,
        tokio_util::sync::CancellationToken::new(),
    ));
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("lead"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    *provider.root.lock().unwrap() = Some(root);
    provider.engine.set(Arc::downgrade(&engine)).unwrap();
    let binding = engine.bind_runtime(&PathBuf::from(".")).unwrap();
    let lead_policy = binding.agent_resource_policy("lead").unwrap();
    supervisor
        .ensure_main(
            root,
            spec("lead"),
            (binding.clone(), Arc::from([]), lead_policy),
            None,
            None,
        )
        .await
        .unwrap();
    engine
        .admit_user_prompt(root, "coordinate".to_string())
        .await
        .unwrap();
    engine
        .run_turn_with_external_dirs_and_guidance(
            root,
            &spec("lead"),
            CancellationToken::new(),
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    Team {
        engine,
        supervisor,
        root,
        binding,
    }
}

async fn spawn_scout(team: &Team) -> (SessionId, String) {
    let policy = team.binding.agent_resource_policy("scout").unwrap();
    team.supervisor
        .spawn_resident(
            team.root,
            spec("scout"),
            (team.binding.clone(), Arc::from([]), policy, None),
            "find the marker".to_string(),
            None,
            None,
        )
        .await
        .unwrap()
}

/// Wait until `handle` is archived with a report, `reports` reports landed on
/// the lead's log, and the member's slot settled.
async fn wait_reported(team: &Team, handle: &str, reports: usize) {
    for _ in 0..500 {
        let projection = team.engine.read_projection(team.root).await.unwrap();
        if projection
            .team
            .archived
            .get(handle)
            .is_some_and(|entry| entry.reason == ArchiveReason::Reported)
            && reported(team).await.len() >= reports
        {
            // Let any (wrongly) continuing turn take its next rounds.
            tokio::time::sleep(Duration::from_millis(200)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("`{handle}` never reported and archived ({reports} report(s) expected)");
}

/// Every `SubagentReported` body on the lead's log, in order.
async fn reported(team: &Team) -> Vec<String> {
    team.engine
        .replay(team.root)
        .await
        .unwrap()
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::SubagentReported { report, .. } => Some(report.clone()),
            _ => None,
        })
        .collect()
}

struct MemberLog {
    results: Vec<String>,
    errors: Vec<String>,
    finishes: Vec<(FinishReason, Option<hya_proto::FinishCause>)>,
}

async fn member_log(team: &Team, child: SessionId) -> MemberLog {
    let log = team.engine.replay(child).await.unwrap();
    let mut out = MemberLog {
        results: Vec::new(),
        errors: Vec::new(),
        finishes: Vec::new(),
    };
    for envelope in &log {
        match &envelope.event {
            Event::ToolResult { output, .. } => out.results.push(output.to_string()),
            Event::ToolError { message_text, .. } => out.errors.push(message_text.clone()),
            Event::MessageFinished {
                role: Role::Assistant,
                finish,
                cause,
                ..
            } => out.finishes.push((*finish, *cause)),
            _ => {}
        }
    }
    out
}

#[tokio::test]
async fn an_accepted_report_ends_the_turn_without_another_model_round() {
    // The model would report forever, round after round.
    let provider = ScriptProvider::new(|_| {
        vec![
            report_call("Acknowledged closure."),
            FakeStep::Finish(FinishReason::ToolCalls),
        ]
    });
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, &handle, 1).await;

    assert_eq!(
        provider.rounds(child),
        1,
        "no model round may follow the round in which the report was accepted"
    );
    assert_eq!(
        reported(&team).await,
        vec!["Acknowledged closure.".to_string()]
    );
    let log = member_log(&team, child).await;
    assert_eq!(
        log.finishes,
        vec![(FinishReason::Stop, None)],
        "exactly one MessageFinished, a plain stop"
    );
    assert_eq!(log.results.len(), 1, "{:?}", log.results);
    assert!(
        log.results[0].contains("Report submitted; it takes effect when this turn ends."),
        "the result must distinguish submission from archive: {}",
        log.results[0]
    );
    assert!(
        log.results[0].contains("you will receive a harness notice and must report again"),
        "{}",
        log.results[0]
    );
}

#[tokio::test]
async fn other_tool_calls_in_the_report_round_complete_then_the_turn_ends() {
    let provider = ScriptProvider::new(|_| {
        vec![
            FakeStep::ToolCall {
                name: "grep".to_string(),
                input: json!({ "pattern": "zzz_no_such_marker_zzz", "path": "src" }),
            },
            report_call("found nothing"),
            FakeStep::ToolCall {
                name: "glob".to_string(),
                input: json!({ "pattern": "*.toml" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ]
    });
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, &handle, 1).await;

    assert_eq!(provider.rounds(child), 1);
    let log = member_log(&team, child).await;
    assert_eq!(
        log.results.len() + log.errors.len(),
        3,
        "every tool call of the round is recorded: results {:?} errors {:?}",
        log.results,
        log.errors
    );
    assert!(
        log.results
            .iter()
            .any(|text| text.contains("Report submitted")),
        "{:?}",
        log.results
    );
    assert_eq!(log.finishes, vec![(FinishReason::Stop, None)]);
    assert_eq!(reported(&team).await, vec!["found nothing".to_string()]);
}

#[tokio::test]
async fn a_second_report_in_the_same_episode_is_rejected() {
    let provider = ScriptProvider::new(|_| {
        vec![
            report_call("first report"),
            report_call("second report"),
            FakeStep::Finish(FinishReason::ToolCalls),
        ]
    });
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, &handle, 1).await;

    assert_eq!(provider.rounds(child), 1);
    let log = member_log(&team, child).await;
    assert_eq!(log.results.len(), 1, "{:?}", log.results);
    assert_eq!(log.errors.len(), 1, "{:?}", log.errors);
    assert!(
        log.errors[0].contains("already accepted")
            && log.errors[0].contains("do not call `report` again"),
        "the rejection must be actionable: {}",
        log.errors[0]
    );
    assert_eq!(
        reported(&team).await,
        vec!["first report".to_string()],
        "exactly one SubagentReported, carrying the accepted report"
    );
}

#[tokio::test]
async fn a_member_woken_after_its_report_can_report_once_more() {
    // One model round per episode, so the round number is the episode.
    let provider = ScriptProvider::new(|round| {
        vec![
            report_call(&format!("report of episode {round}")),
            FakeStep::Finish(FinishReason::ToolCalls),
        ]
    });
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, &handle, 1).await;

    team.engine
        .mail_send(
            team.root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "follow-up: also check the docs".to_string(),
        )
        .await
        .unwrap();
    wait_reported(&team, &handle, 2).await;

    assert_eq!(
        provider.rounds(child),
        2,
        "one model round per episode, none after either accepted report"
    );
    assert_eq!(
        reported(&team).await,
        vec![
            "report of episode 0".to_string(),
            "report of episode 1".to_string()
        ]
    );
    let log = member_log(&team, child).await;
    assert_eq!(
        log.finishes,
        vec![(FinishReason::Stop, None), (FinishReason::Stop, None)]
    );
    assert!(log.errors.is_empty(), "{:?}", log.errors);
}

fn answer(text: &str, finish: FinishReason) -> Vec<FakeStep> {
    let mut steps = Vec::new();
    if !text.is_empty() {
        steps.push(FakeStep::Text(text.to_string()));
    }
    steps.push(FakeStep::Finish(finish));
    steps
}

/// Wait until the member's first turn ended and its slot settled, then assert
/// it is still live (idle) with no report delivered.
async fn assert_idle_unreported(team: &Team, child: SessionId, handle: &str) {
    for _ in 0..500 {
        if !member_log(team, child).await.finishes.is_empty()
            && team
                .engine
                .read_projection(team.root)
                .await
                .unwrap()
                .team
                .roster
                .get(handle)
                .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let projection = team.engine.read_projection(team.root).await.unwrap();
    assert!(
        projection.team.roster.contains_key(handle),
        "`{handle}` must stay live: {:?}",
        projection
            .team
            .archived
            .get(handle)
            .map(|entry| entry.reason)
    );
    assert!(
        reported(team).await.is_empty(),
        "{:?}",
        reported(team).await
    );
}

#[tokio::test]
async fn a_final_answer_without_report_is_delivered_as_the_report() {
    let provider = ScriptProvider::new(|round| {
        answer(&format!("answer of episode {round}"), FinishReason::Stop)
    });
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, &handle, 1).await;

    assert_eq!(provider.rounds(child), 1);
    assert_eq!(
        reported(&team).await,
        vec!["answer of episode 0".to_string()]
    );
    let log = team.engine.replay(team.root).await.unwrap();
    assert!(
        log.iter().any(|envelope| matches!(
            &envelope.event,
            Event::SubagentReported { child: reporter, outcome: hya_proto::ReportOutcome::Done, .. }
                if *reporter == child
        )),
        "the answer is reported with outcome done"
    );

    // Mail from the parent wakes it for a new episode, which reports again.
    team.engine
        .mail_send(
            team.root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "follow-up: also check the docs".to_string(),
        )
        .await
        .unwrap();
    wait_reported(&team, &handle, 2).await;
    assert_eq!(provider.rounds(child), 2);
    assert_eq!(
        reported(&team).await,
        vec![
            "answer of episode 0".to_string(),
            "answer of episode 1".to_string()
        ]
    );
}

#[tokio::test]
async fn an_empty_final_answer_leaves_the_member_idle() {
    let provider = ScriptProvider::new(|_| answer("", FinishReason::Stop));
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    assert_idle_unreported(&team, child, &handle).await;
}

#[tokio::test]
async fn an_answer_cut_by_the_length_limit_leaves_the_member_idle() {
    let provider = ScriptProvider::new(|_| answer("the marker is in", FinishReason::Length));
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    assert_idle_unreported(&team, child, &handle).await;
    assert_eq!(
        member_log(&team, child).await.finishes,
        vec![(FinishReason::Length, None)]
    );
}

#[tokio::test]
async fn unread_mail_at_the_end_of_the_turn_blocks_the_answer_until_it_is_handled() {
    let provider = ScriptProvider::new(|round| {
        answer(&format!("answer of round {round}"), FinishReason::Stop)
    });
    provider.mail_in_first_round.store(true, Ordering::SeqCst);
    let team = team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, &handle, 1).await;

    assert_eq!(
        provider.rounds(child),
        2,
        "the unread mail wakes a second turn instead of the first answer being reported"
    );
    assert_eq!(reported(&team).await, vec!["answer of round 1".to_string()]);
}
