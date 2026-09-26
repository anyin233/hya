//! Subagent handles (0.41.0): `<prefix>-<operator>` leaves.
//!
//! The prefix is the sanitized `subagent_type` (the agent id) and the harness
//! appends one random Arknights operator name; a leaf is never reused within
//! a team (live or archived), and old counter handles (`scout-1`) from earlier
//! logs keep resolving.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

mod support;

use async_trait::async_trait;
use futures::stream;
use hya_core::handle_naming::{HandleRng, SplitMix64, operator_names};
use hya_core::{AgentSpec, CoreError, CreateSession, EventBus, ResidentSupervisor, SessionEngine};
use hya_proto::scope;
use hya_proto::{
    AgentName, Event, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef, SessionId,
    SubagentMode,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
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
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("ok".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

/// Always draws the same index: every spawn with one prefix collides.
struct Fixed(usize);

impl HandleRng for Fixed {
    fn pick(&mut self, bound: usize) -> usize {
        self.0.min(bound - 1)
    }
}

async fn engine_with(rng: Option<Box<dyn HandleRng>>) -> Arc<SessionEngine> {
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(StubProvider)));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    let engine = SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    );
    Arc::new(match rng {
        Some(rng) => engine.with_handle_rng(rng),
        None => engine,
    })
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

struct Team {
    engine: Arc<SessionEngine>,
    supervisor: Arc<ResidentSupervisor>,
    root: SessionId,
}

async fn team(rng: Option<Box<dyn HandleRng>>) -> Team {
    let engine = engine_with(rng).await;
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead = spec("build");
    let binding = engine.bind_runtime(&lead.workdir).unwrap();
    let resources = binding.agent_resource_policy("build").unwrap();
    supervisor
        .ensure_main(root, lead, (binding, Arc::from([]), resources), None, None)
        .await
        .unwrap();
    Team {
        engine,
        supervisor,
        root,
    }
}

/// Spawn `agent`; `subagent_type` is the agent id the `task` call named
/// (it may differ from the spec's name, e.g. under an inline overlay).
async fn spawn(
    team: &Team,
    parent: SessionId,
    agent: &str,
    subagent_type: Option<&str>,
) -> Result<(SessionId, String), CoreError> {
    let agent = spec(agent);
    let binding = team.engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("explore").unwrap();
    let resolved = (binding, Arc::from([]), resources, None);
    match subagent_type {
        Some(subagent_type) => {
            team.supervisor
                .spawn_resident_typed(
                    parent,
                    agent,
                    resolved,
                    "work".to_string(),
                    hya_core::TaskSpawnOrigin {
                        subagent_type: subagent_type.to_string(),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .await
        }
        None => {
            team.supervisor
                .spawn_resident(parent, agent, resolved, "work".to_string(), None, None)
                .await
        }
    }
}

/// `main/<prefix>-<one listed operator name>`.
fn assert_named(handle: &str, parent: &str, prefix: &str) {
    let leaf = handle
        .strip_prefix(&format!("{parent}/"))
        .unwrap_or_else(|| panic!("`{handle}` must live under `{parent}`"));
    assert!(!leaf.contains('/'), "{handle}");
    let name = leaf
        .strip_prefix(&format!("{prefix}-"))
        .unwrap_or_else(|| panic!("`{handle}` must start with `{prefix}-`"));
    assert!(
        operator_names().contains(&name),
        "`{name}` (from `{handle}`) must be one listed operator name"
    );
}

#[tokio::test]
async fn the_default_prefix_is_the_agent_id() {
    let team = team(None).await;
    let (_, handle) = spawn(&team, team.root, "explore", None).await.unwrap();
    assert_named(&handle, "main", "explore");
}

#[tokio::test]
async fn the_subagent_type_names_the_handle() {
    let team = team(None).await;
    // The spec's name is an inline overlay's; the handle follows the type.
    let (child, handle) = spawn(&team, team.root, "overlay", Some("scout"))
        .await
        .unwrap();
    assert_named(&handle, "main", "scout");
    // The minted handle is what the log recorded and what resolves back.
    let projection = team.engine.read_projection(team.root).await.unwrap();
    assert_eq!(projection.team.roster[&handle].session, child);
    assert_eq!(
        projection.team.canonical_member(scope::leaf(&handle)),
        handle,
        "the bare leaf resolves to the canonical handle"
    );
}

#[tokio::test]
async fn a_bundle_agent_id_is_sanitized_into_the_prefix() {
    let team = team(None).await;
    let (_, handle) = spawn(&team, team.root, "explore", Some("Acme_Scout.v2"))
        .await
        .unwrap();
    assert_named(&handle, "main", "acme-scout-v2");
    let (_, handle) = spawn(&team, team.root, "explore", Some("__"))
        .await
        .unwrap();
    assert_named(&handle, "main", "agent");
}

#[tokio::test]
async fn a_nested_member_keeps_its_parent_path() {
    let team = team(None).await;
    let (dev, dev_handle) = spawn(&team, team.root, "explore", Some("dev"))
        .await
        .unwrap();
    let (_, nested) = spawn(&team, dev, "general", None).await.unwrap();
    assert_named(&dev_handle, "main", "dev");
    assert_named(&nested, &dev_handle, "general");
}

#[tokio::test]
async fn an_archived_leaf_is_never_reused_and_its_mail_wakes_the_archived_member() {
    // Every draw picks the same operator name: the second `scout` collides.
    let team = team(Some(Box::new(Fixed(0)))).await;
    let (first, first_handle) = spawn(&team, team.root, "explore", Some("scout"))
        .await
        .unwrap();
    assert_eq!(first_handle, format!("main/scout-{}", operator_names()[0]));
    team.supervisor
        .archive_member(team.root, &first_handle, "done for now")
        .await
        .unwrap();

    let (second, second_handle) = spawn(&team, team.root, "explore", Some("scout"))
        .await
        .unwrap();
    assert_ne!(second, first);
    assert_ne!(
        second_handle, first_handle,
        "an archived member's leaf is never handed to a new member"
    );
    assert_eq!(
        second_handle,
        format!("main/scout-{}-{}", operator_names()[0], operator_names()[1]),
        "after the single-name retries the leaf falls back to two names"
    );

    // Mail to the first handle wakes the archived member, not the new one.
    team.engine
        .mail_send(
            team.root,
            MailEndpoint::Handle(first_handle.clone()),
            MailKind::Message,
            "resume".to_string(),
        )
        .await
        .unwrap();
    let projection = team.engine.read_projection(team.root).await.unwrap();
    assert_eq!(projection.team.roster[&first_handle].session, first);
    assert_eq!(projection.team.roster[&second_handle].session, second);
}

#[tokio::test]
async fn seeded_spawns_are_reproducible() {
    let names = |seed| async move {
        let team = team(Some(Box::new(SplitMix64::seeded(seed)))).await;
        let mut handles = Vec::new();
        for _ in 0..3 {
            handles.push(spawn(&team, team.root, "explore", None).await.unwrap().1);
        }
        handles
    };
    let first = names(11).await;
    assert_eq!(first, names(11).await);
    let unique: std::collections::BTreeSet<&String> = first.iter().collect();
    assert_eq!(unique.len(), 3, "{first:?}");
}

#[tokio::test]
async fn old_counter_handles_keep_resolving_next_to_new_names() {
    let team = team(None).await;
    // An earlier release registered `main/scout-1` (counter naming).
    let old = team
        .engine
        .create(CreateSession {
            parent: Some(team.root),
            agent: AgentName::new("scout"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    team.engine
        .store()
        .append_event(
            team.root,
            &Event::AgentRegistered {
                session: team.root,
                agent_session: old,
                handle: "scout-1".to_string(),
                parent: Some("main".to_string()),
                agent_type: AgentName::new("scout"),
                mode: SubagentMode::Resident,
            },
        )
        .await
        .unwrap();

    let (_, new_handle) = spawn(&team, team.root, "explore", Some("scout"))
        .await
        .unwrap();
    assert_named(&new_handle, "main", "scout");

    // Replay folds the recorded handles back; nothing re-derives them.
    let replayed =
        hya_proto::Projection::from_events(&team.engine.replay(team.root).await.unwrap());
    assert!(replayed.team.roster.contains_key("main/scout-1"));
    assert!(replayed.team.roster.contains_key(&new_handle));
    assert_eq!(replayed.team.roster["main/scout-1"].session, old);
    let projection = team.engine.read_projection(team.root).await.unwrap();
    assert_eq!(projection.team.canonical_member("scout-1"), "main/scout-1");
    // The bare old leaf still resolves to its canonical handle for mail (this
    // registration has no live resident in this process, so delivery is
    // refused — naming the resolved handle).
    let error = team
        .engine
        .mail_send(
            team.root,
            MailEndpoint::Handle("scout-1".to_string()),
            MailKind::Message,
            "still addressable".to_string(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("`main/scout-1`"), "{error}");
}
