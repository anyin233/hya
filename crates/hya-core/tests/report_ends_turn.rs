//! An accepted `report` ends the member's turn (0.41.0 real-task fix).
//!
//! Real run 5: a member whose `report` was accepted kept taking model rounds
//! and re-reporting ("Acknowledged closure…") — 43 reports in one turn for one
//! implementer, 64 for the reviewer. The engine now ends the turn right after
//! the tool round in which a report is accepted (no further model call), a
//! second `report` in that turn is rejected, and a member woken by mail after
//! its report runs a new episode that may report once more.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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

const BUNDLE: &str = "kind: AgentSetBundle
identity: { id: acme/report-turn, version: 1.0.0, publisher: acme }
agents:
  - id: lead
    role: main
    can_spawn: [scout]
    resource_view: { allow: [harness:tool/grep] }
  - id: scout
    role: subagent
    resource_view: { allow: [harness:tool/grep, harness:tool/glob] }
";

/// What a member does in every model round: the script receives the member's
/// 0-based model-round number and returns that round's steps.
type MemberScript = dyn Fn(usize) -> Vec<FakeStep> + Send + Sync;

/// The lead finishes at once; every member round follows `script`.
struct ScriptProvider {
    root: Mutex<Option<SessionId>>,
    member_rounds: Mutex<HashMap<SessionId, usize>>,
    script: Box<MemberScript>,
}

impl ScriptProvider {
    fn new(script: impl Fn(usize) -> Vec<FakeStep> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            root: Mutex::new(None),
            member_rounds: Mutex::new(HashMap::new()),
            script: Box::new(script),
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
            parallel_tool_calls: true,
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
    tokio::spawn(run_mailbox_service(engine.clone(), mailbox_rx));
    let supervisor = ResidentSupervisor::start(engine.clone());
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        supervisor.clone(),
        lifecycle_rx,
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
        log.results[0].contains("Report accepted; your turn ends now."),
        "the acceptance text must not invite continuation: {}",
        log.results[0]
    );
    assert!(
        !log.results[0].contains("when this turn completes"),
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
            .any(|text| text.contains("Report accepted")),
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
