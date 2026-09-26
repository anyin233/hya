//! Coordination tools (0.41.0) are allocated by the harness when an agent
//! starts, regardless of its bundle `resource_view`: `report` for subagents,
//! `wait` for every agent, `task`/`archive` with spawn rights, and the channel
//! mail tools (`list_channel`, `send`, `read channel://`) when the channel
//! family is loaded.
//!
//! Reproduces the real run where `hya-extra/scout` (a narrow view without mail
//! tools) was mailed by its lead mid-turn: every `report` was rejected for
//! unread mail it had no way to read, so it could never finish.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
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
    AgentName, ArchiveReason, ChannelKind, Event, FinishReason, MailEndpoint, MailKind, MessageId,
    ModelRef, SessionId,
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
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const BUNDLE: &str = "kind: AgentSetBundle
identity: { id: acme/coordination, version: 1.0.0, publisher: acme }
agents:
  - id: lead
    role: main
    can_spawn: [scout]
    resource_view: { allow: [harness:tool/grep] }
  - id: scout
    role: subagent
    resource_view: { allow: [harness:tool/grep, harness:tool/glob] }
";

/// The lead finishes at once. The scout parks its first round until the test
/// mailed it, then reports; on a rejected report it follows the `read
/// channel://…` call the gate error names, then reports again.
#[derive(Default)]
struct ScriptProvider {
    root: Mutex<Option<SessionId>>,
    requests: Mutex<Vec<(SessionId, CompletionRequest)>>,
    scout_started: Notify,
    mail_sent: Notify,
}

fn tool_calls_so_far(request: &CompletionRequest) -> usize {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            hya_proto::Message::Assistant { parts, .. } => Some(
                parts
                    .iter()
                    .filter(|part| matches!(part, hya_proto::Part::Tool { .. }))
                    .count(),
            ),
            _ => None,
        })
        .sum()
}

/// The `channel://…` spelling named by the latest gate error in the transcript.
fn named_channel_read(request: &CompletionRequest) -> String {
    let transcript = serde_json::to_string(&request.messages).unwrap();
    let start = transcript
        .rfind("channel://")
        .unwrap_or_else(|| panic!("the gate error must name a channel read: {transcript}"));
    transcript[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '-' | '?' | '='))
        .collect()
}

#[async_trait]
impl Provider for ScriptProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: false,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests
            .lock()
            .unwrap()
            .push((session, request.clone()));
        let is_root = *self.root.lock().unwrap() == Some(session);
        let steps = if is_root {
            vec![
                FakeStep::Text("lead done".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        } else {
            match tool_calls_so_far(&request) {
                0 => {
                    self.scout_started.notify_one();
                    self.mail_sent.notified().await;
                    vec![
                        FakeStep::ToolCall {
                            name: "report".to_string(),
                            input: json!({"result": "early findings"}),
                        },
                        FakeStep::Finish(FinishReason::ToolCalls),
                    ]
                }
                1 => vec![
                    FakeStep::ToolCall {
                        name: "read".to_string(),
                        input: json!({"path": named_channel_read(&request)}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                2 => vec![
                    FakeStep::ToolCall {
                        name: "report".to_string(),
                        input: json!({"result": "SCOUT_FINAL_REPORT"}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                _ => vec![
                    FakeStep::Text("reported".to_string()),
                    FakeStep::Finish(FinishReason::Stop),
                ],
            }
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

fn tool_names(request: &CompletionRequest) -> BTreeSet<String> {
    request
        .tools
        .iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect()
}

#[tokio::test]
async fn a_narrow_bundle_member_reads_its_dm_and_reports_after_its_lead_mails_it() {
    let source = hya_bundle::BundleSource::new(
        "coordination",
        vec![hya_bundle::SourceFile::new("bundle.yaml", BUNDLE)],
    );
    let prepared = hya_bundle::prepare_package(source).unwrap();
    let bundles = hya_bundle::BundleCatalog::from_prepared(prepared.bundles()).unwrap();
    let agents = AgentCatalog::new(Arc::new(bundles)).unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        Arc::new(agents),
    ));
    let provider = Arc::new(ScriptProvider::default());
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

    // Root: the bundle `lead` (role main, can_spawn [scout], narrow view).
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

    // Member: the bundle `scout` (no spawn rights, narrow view without read).
    let scout_policy = binding.agent_resource_policy("scout").unwrap();
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            spec("scout"),
            (binding.clone(), Arc::from([]), scout_policy, None),
            "find the marker".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), provider.scout_started.notified())
        .await
        .expect("scout's first round must start");
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "status check: where is the marker?".to_string(),
        )
        .await
        .unwrap();
    provider.mail_sent.notify_one();

    let mut archived = None;
    for _ in 0..500 {
        let projection = engine.read_projection(root).await.unwrap();
        if let Some(entry) = projection.team.archived.get(&handle) {
            archived = Some(entry.reason);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let child_log = engine.replay(child).await.unwrap();
    let tool_errors: Vec<String> = child_log
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolError { message_text, .. } => Some(message_text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        archived,
        Some(ArchiveReason::Reported),
        "the scout must report and be archived; tool errors: {tool_errors:?}"
    );

    // The gate error named the DM channel and the exact read call.
    let projection = engine.read_projection(root).await.unwrap();
    let dm = projection
        .team
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == ChannelKind::Dm && channel.members.iter().any(|m| m == &handle)
        })
        .map(|(id, _)| id.clone())
        .expect("registration minted the scout's DM");
    let gate = tool_errors
        .iter()
        .find(|text| text.contains("unread mail"))
        .expect("the first report hit the unread-mail gate");
    assert!(
        gate.contains(&format!("#{dm}")) && gate.contains(&format!("read channel://{dm}")),
        "the gate error must name the channel and the read call: {gate}"
    );
    assert!(
        projection.team.inboxes["main"]
            .iter()
            .any(|mail| mail.body.contains("SCOUT_FINAL_REPORT")),
        "the final report must reach the lead"
    );

    // Tool allocation: harness-owned coordination tools on both narrow views.
    let requests = provider.requests.lock().unwrap().clone();
    let (_, lead_request) = requests
        .iter()
        .find(|(session, _)| *session == root)
        .unwrap();
    let lead_tools = tool_names(lead_request);
    for expected in [
        "grep",
        "wait",
        "task",
        "archive",
        "send",
        "list_channel",
        "read",
    ] {
        assert!(
            lead_tools.contains(expected),
            "lead lacks `{expected}`: {lead_tools:?}"
        );
    }
    for absent in ["report", "bash", "glob"] {
        assert!(
            !lead_tools.contains(absent),
            "lead must not see `{absent}`: {lead_tools:?}"
        );
    }
    let lead_system = lead_request.system.as_deref().unwrap_or_default();
    assert!(
        lead_system.contains("NEVER call `report`") && lead_system.contains("`task`"),
        "the lead's quick reference covers its own tools: {lead_system}"
    );

    let (_, scout_request) = requests
        .iter()
        .find(|(session, _)| *session == child)
        .unwrap();
    let scout_tools = tool_names(scout_request);
    for expected in [
        "grep",
        "glob",
        "report",
        "wait",
        "send",
        "list_channel",
        "read",
    ] {
        assert!(
            scout_tools.contains(expected),
            "scout lacks `{expected}`: {scout_tools:?}"
        );
    }
    for absent in ["task", "archive", "bash"] {
        assert!(
            !scout_tools.contains(absent),
            "scout without spawn rights must not see `{absent}`: {scout_tools:?}"
        );
    }
    let scout_system = scout_request.system.as_deref().unwrap_or_default();
    assert!(
        scout_system.contains("`report`") && !scout_system.contains("`task`"),
        "the scout's quick reference lists only its own tools: {scout_system}"
    );
    assert!(
        !scout_system.contains("NEVER call `report`"),
        "a subagent must never be told not to report: {scout_system}"
    );
}
