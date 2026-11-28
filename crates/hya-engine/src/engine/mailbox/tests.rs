#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::{AgentName, ModelRef, OwnerRunId};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};

use hya_proto::{MailEndpoint, MailKind};
use hya_tool::MailReceipt;

use super::*;
use crate::AgentSpec;
use crate::bus::EventBus;
use crate::engine::{CreateSession, MailPreAppendGate, SessionEngine};
use crate::resident::ResidentSupervisor;

mod recovery;
mod send;
mod view;

fn local_test_runtime() -> Arc<crate::RuntimeRegistry> {
    tests_support::runtime_with_resident()
}

mod tests_support {
    use super::*;

    pub(super) fn runtime_with_resident() -> Arc<crate::RuntimeRegistry> {
        runtime_with_extra(Vec::new())
    }

    pub(super) fn runtime_with_channel_restriction(channels: &str) -> Arc<crate::RuntimeRegistry> {
        let source = hya_bundle::BundleSource::new(
            "channel-restriction",
            vec![hya_bundle::SourceFile::new(
                "bundle.yaml",
                format!(
                    "schema_version: 2\nkind: Bundle\nidentity: {{ id: acme/channel-restriction, version: 1.0.0, publisher: acme }}\nchannels:\n{channels}\n"
                ),
            )],
        );
        let prepared = hya_bundle::prepare_package(source).expect("channel restriction");
        runtime_with_extra(prepared.bundles().to_vec())
    }

    fn runtime_with_extra(mut extra: Vec<hya_bundle::Bundle>) -> Arc<crate::RuntimeRegistry> {
        let bundle = hya_bundle::Bundle {
            check: None,
            format_version: 4,
            identity: hya_bundle::BundleIdentity {
                id: "hya/mailbox-unit-tests-resident".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            digest: "test-only-resident".to_string(),
            agents: ["resident"]
                .into_iter()
                .map(|id| hya_bundle::PreparedAgent {
                    private: false,
                    id: AgentName::new(id),
                    description: None,
                    role: hya_bundle::AgentRole::Subagent,
                    color: None,
                    prompt: None,
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: hya_bundle::ModelPolicy::default(),
                    workdir: None,
                    resource_view: hya_bundle::ResourceView::default(),
                    can_spawn: Vec::new(),
                    hook_refs: Vec::new(),
                    advertise: Vec::new(),
                    handoff: hya_bundle::HandoffPolicy::Model,
                })
                .collect(),
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
            tui: None,

            workflow: None,
            channels: vec![],
            schemas: vec![],
            process: None,
            apis: vec![],
            commands: Vec::new(),
            runtime: None,
            permission_modes: vec![],
            cli: Vec::new(),
            requires: Vec::new(),
            default_agent: None,
            overrides: None,
        };
        extra.push(bundle);
        let catalog = hya_bundle::BundleCatalog::from_prepared(&extra).expect("catalog valid");
        let catalog = crate::AgentCatalog::new(Arc::new(catalog)).expect("agent catalog valid");
        Arc::new(crate::RuntimeRegistry::new(
            hya_tool_loader::builtins(),
            Arc::new(catalog),
        ))
    }
}

async fn engine() -> SessionEngine {
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new());
    let runtime = local_test_runtime();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    SessionEngine::new(store, router, runtime, permission, EventBus::default())
}

async fn engine_with_runtime(runtime: Arc<crate::RuntimeRegistry>) -> SessionEngine {
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    SessionEngine::new(store, router, runtime, permission, EventBus::default())
}

fn assert_mail_rejected(
    result: Result<MailReceipt, CoreError>,
    expected_sender: &str,
    expected_channel: &str,
) {
    assert!(
        matches!(result,
            Err(CoreError::MailAuthorizationRejected {
                ref sender, ref channel, ..
            }) if sender == expected_sender && channel == expected_channel),
        "expected rejection for {expected_sender} to {expected_channel}: {result:?}"
    );
}

async fn root_team(engine: &SessionEngine) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap()
}

/// One agent in a scoped test org: its session, its canonical path, and the
/// claim that keeps it eligible to receive direct mail.
struct Agent {
    session: SessionId,
    path: String,
    _claim: hya_proto::ActorClaim,
}

/// Register a resident child of `parent`, mirroring what the spawn path does:
/// an `AgentRegistered` carrying the real parent, plus the auto-join of that
/// unit's reserved announce channel.
async fn register_child(
    engine: &SessionEngine,
    root: SessionId,
    parent_session: SessionId,
    parent_path: &str,
    leaf: &str,
) -> Agent {
    let session = engine
        .create(CreateSession {
            parent: Some(parent_session),
            agent: AgentName::new("worker"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(std::path::Path::new(".")).unwrap();
    engine
        .capture_session_bundle_hooks(session, &binding, "worker")
        .await;
    // Mirror the supervisor's registration-time minting: one group channel
    // per unit (leader + children), reused across siblings.
    let existing_group =
        hya_tool::extension::team_projection(&engine.read_projection(root).await.unwrap())
            .channels
            .iter()
            .find(|(_, channel)| {
                channel.kind == hya_proto::ChannelKind::Group
                    && channel.unit.as_deref() == Some(parent_path)
            })
            .map(|(key, _)| key.clone());
    let child_path = scope::join_path(parent_path, leaf);
    match existing_group {
        Some(group) => {
            engine
                .emit_for_actor(
                    None,
                    root,
                    hya_tool::extension::ExtensionEvent::ChannelCreated {
                        session: root,
                        channel: group,
                        kind: hya_proto::ChannelKind::Group,
                        unit: Some(parent_path.into()),
                        members: vec![child_path.clone()],
                    }
                    .into_event(),
                )
                .await
                .unwrap();
        }
        None => {
            let group = hya_proto::mint_channel_id(hya_proto::ChannelKind::Group);
            engine
                .emit_for_actor(
                    None,
                    root,
                    hya_tool::extension::ExtensionEvent::ChannelCreated {
                        session: root,
                        channel: group.clone(),
                        kind: hya_proto::ChannelKind::Group,
                        unit: Some(parent_path.to_string()),
                        members: vec![parent_path.to_string(), child_path.clone()],
                    }
                    .into_event(),
                )
                .await
                .unwrap();
            let _ = group;
        }
    }
    engine
        .emit_for_actor(
            None,
            root,
            hya_tool::extension::ExtensionEvent::AgentRegistered {
                session: root,
                agent_session: session,
                handle: leaf.to_string(),
                parent: Some(parent_path.to_string()),
                agent_type: AgentName::new("worker"),
                mode: SubagentMode::Resident,
            }
            .into_event(),
        )
        .await
        .unwrap();
    mint_dm(engine, root, parent_path, &child_path).await;
    use crate::store_bridge::SessionStoreExt as _;
    let claim = engine
        .store()
        .try_claim_new(session, session, OwnerRunId::new())
        .await
        .unwrap();
    Agent {
        session,
        path: child_path,
        _claim: claim,
    }
}

/// The standard two-unit org for the scope tests:
///
/// ```text
/// main
/// ├── lead-1 ── worker-1, worker-2
/// └── lead-2 ── worker-1      <- same leaf as lead-1's, a different agent
/// ```
struct Org {
    root: SessionId,
    lead_1: Agent,
    lead_2: Agent,
    worker_1: Agent,
    worker_2: Agent,
    other_worker: Agent,
}

async fn org(engine: &SessionEngine) -> Org {
    let root = root_team(engine).await;
    engine.ensure_root_registered(root).await.unwrap();
    let lead_1 = register_child(engine, root, root, "main", "lead-1").await;
    let lead_2 = register_child(engine, root, root, "main", "lead-2").await;
    let worker_1 = register_child(
        engine,
        root,
        lead_1.session,
        &lead_1.path.clone(),
        "worker-1",
    )
    .await;
    let worker_2 = register_child(
        engine,
        root,
        lead_1.session,
        &lead_1.path.clone(),
        "worker-2",
    )
    .await;
    let other_worker = register_child(
        engine,
        root,
        lead_2.session,
        &lead_2.path.clone(),
        "worker-1",
    )
    .await;
    Org {
        root,
        lead_1,
        lead_2,
        worker_1,
        worker_2,
        other_worker,
    }
}

/// Bodies delivered to `path`'s inbox, in order.
async fn inbox(engine: &SessionEngine, root: SessionId, path: &str) -> Vec<String> {
    let projection = engine.read_projection(root).await.unwrap();
    let team = hya_tool::extension::team_projection(&projection);
    team.inboxes
        .get(path)
        .map(|inbox| {
            inbox
                .iter()
                .map(|index| team.mail[*index].body.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Mint the DM channel the resident supervisor creates at registration
/// time: one direct line between `parent_path` and `child_path`.
async fn mint_dm(engine: &SessionEngine, root: SessionId, parent_path: &str, child_path: &str) {
    if dm_channel_between(
        &engine.read_projection(root).await.unwrap(),
        parent_path,
        child_path,
    )
    .is_some()
    {
        return;
    }
    let channel = hya_proto::mint_channel_id(hya_proto::ChannelKind::Dm);
    engine
        .emit_for_actor(
            None,
            root,
            hya_tool::extension::ExtensionEvent::ChannelCreated {
                session: root,
                channel: channel.clone(),
                kind: hya_proto::ChannelKind::Dm,
                unit: None,
                members: vec![parent_path.to_string(), child_path.to_string()],
            }
            .into_event(),
        )
        .await
        .unwrap();
}
