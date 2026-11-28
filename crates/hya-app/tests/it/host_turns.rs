//! Bundle-driven turns (`HostTurnRunner::run_turn`) and agent model
//! resolution (`HostCatalogReader::resolve_agent_model`) on the app host.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_app::{
    BuiltSessionEngine, InvocationPolicy, agent_with_model, build_session_engine_pure,
    config::EngineSettings, open_store,
};
use hya_engine::CreateSession;
use hya_proto::{ActorClaim, ActorEpoch, AgentName, ModelRef, OwnerRunId, SessionId, SessionKind};
use hya_provider::{DevProvider, ProviderRouter};
use hya_runtime_abi::{
    AgentModelRequest, BundleClaimLease, BundleClaimRequest, HostError, HostRuntime, TurnStart,
    TurnStartRequest,
};
use tokio_util::sync::CancellationToken;

struct Fixture {
    built: BuiltSessionEngine,
    host: Arc<dyn HostRuntime>,
    root: SessionId,
    child: SessionId,
}

async fn fixture() -> Fixture {
    let mut agent = agent_with_model("hya/offline", None);
    agent.workdir = std::env::temp_dir();
    let built = build_session_engine_pure(
        open_store("").await.unwrap(),
        ProviderRouter::new().with(Arc::new(DevProvider::new())),
        &agent,
        BTreeMap::new(),
        Vec::new(),
        (InvocationPolicy::default(), EngineSettings::default()),
    )
    .await
    .unwrap();
    let create = |parent| CreateSession {
        parent,
        agent: agent.name.clone(),
        model: agent.model.clone(),
        workdir: agent.workdir.to_string_lossy().into_owned(),
        project: None,
        kind: SessionKind::Project,
    };
    let root = built.engine().create(create(None)).await.unwrap();
    let child = built.engine().create(create(Some(root))).await.unwrap();
    let host = built.host_runtime();
    Fixture {
        built,
        host,
        root,
        child,
    }
}

async fn fence(host: &dyn HostRuntime, session: SessionId, root: SessionId) -> BundleClaimLease {
    host.bundle_storage()
        .acquire_claim(BundleClaimRequest {
            bundle: hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
            resource_key: session.to_string(),
            owner_id: OwnerRunId::new().as_uuid().to_string(),
            ttl_millis: 60_000,
            metadata: serde_json::json!({ "root_session_id": root.to_string() }),
        })
        .await
        .unwrap()
}

fn as_claim(lease: &BundleClaimLease) -> ActorClaim {
    ActorClaim {
        actor_id: lease.resource_key.parse().unwrap(),
        epoch: ActorEpoch::from_storage(lease.epoch),
        owner_run_id: serde_json::from_value(lease.owner_id.clone().into()).unwrap(),
    }
}

fn prompted(session: SessionId, claim: Option<ActorClaim>, start: TurnStart) -> TurnStartRequest {
    TurnStartRequest {
        session,
        prompt: Some("go".into()),
        agent: None,
        model: None,
        system_override: None,
        metadata: None,
        actor_claim: claim,
        guidance: None,
        route: None,
        start,
    }
}

async fn log_len(fx: &Fixture, session: SessionId) -> usize {
    fx.host
        .session_store()
        .read_events(session, None, None)
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_superseded_fence_refuses_the_turn_without_admitting_its_prompt() {
    let mut fx = fixture().await;
    let first = fence(fx.host.as_ref(), fx.child, fx.root).await;
    fx.host
        .bundle_storage()
        .release_claim(&first)
        .await
        .unwrap();
    let _current = fence(fx.host.as_ref(), fx.child, fx.root).await;
    let before = log_len(&fx, fx.child).await;

    let refused = fx
        .host
        .turn_runner()
        .run_turn(
            prompted(fx.child, Some(as_claim(&first)), TurnStart::Always),
            CancellationToken::new(),
        )
        .await;

    assert!(
        matches!(refused, Err(HostError::StaleClaim(_))),
        "{refused:?}"
    );
    assert_eq!(log_len(&fx, fx.child).await, before);
    fx.built.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_fenced_turn_needs_a_parent_session() {
    let mut fx = fixture().await;
    let lease = fence(fx.host.as_ref(), fx.root, fx.root).await;
    let before = log_len(&fx, fx.root).await;

    let refused = fx
        .host
        .turn_runner()
        .run_turn(
            prompted(fx.root, Some(as_claim(&lease)), TurnStart::Always),
            CancellationToken::new(),
        )
        .await;

    assert!(
        matches!(refused, Err(HostError::InvalidInput(_))),
        "{refused:?}"
    );
    assert_eq!(log_len(&fx, fx.root).await, before);
    fx.built.shutdown().await.unwrap();
}

#[tokio::test]
async fn if_idle_on_a_busy_session_is_a_conflict_that_admits_nothing() {
    let mut fx = fixture().await;
    let lease = fence(fx.host.as_ref(), fx.child, fx.root).await;
    let before = log_len(&fx, fx.child).await;
    let _active = fx.built.engine().try_begin_turn(fx.child).unwrap();

    let refused = fx
        .host
        .turn_runner()
        .run_turn(
            prompted(fx.child, Some(as_claim(&lease)), TurnStart::IfIdle),
            CancellationToken::new(),
        )
        .await;

    assert!(
        matches!(refused, Err(HostError::Conflict(_))),
        "{refused:?}"
    );
    assert_eq!(log_len(&fx, fx.child).await, before);
    fx.built.shutdown().await.unwrap();
}

#[tokio::test]
async fn resolve_agent_model_follows_the_task_spawn_precedence() {
    let mut fx = fixture().await;
    let resolve = |agent: &str, model: Option<&str>, effort: Option<&str>| {
        fx.host
            .catalog_reader()
            .resolve_agent_model(AgentModelRequest {
                parent: fx.root,
                agent: AgentName::new(agent),
                model: model.map(ModelRef::new),
                category: None,
                effort: effort.map(str::to_string),
            })
    };
    // Base model, then the request's model matched against the catalog,
    // then the request's effort over a model's `#effort` suffix.
    assert_eq!(
        resolve("hya-scout", None, None).await.unwrap(),
        ModelRef::new("hya/offline")
    );
    assert_eq!(
        resolve("hya-scout", Some("offline#low"), None)
            .await
            .unwrap(),
        ModelRef::new("hya/offline#low")
    );
    assert_eq!(
        resolve("hya-scout", Some("offline#low"), Some("high"))
            .await
            .unwrap(),
        ModelRef::new("hya/offline#high")
    );
    assert!(matches!(
        resolve("no-such-agent", None, None).await,
        Err(HostError::InvalidInput(_))
    ));
    fx.built.shutdown().await.unwrap();
}
