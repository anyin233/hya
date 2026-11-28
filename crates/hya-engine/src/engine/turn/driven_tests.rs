#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hya_proto::{ActorClaim, ActorEpoch, AgentName, FinishReason, ModelRef, OwnerRunId, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_runtime_abi::BundleClaimRequest;
use hya_tool::{PermissionPlane, PermissionRules};
use tokio_util::sync::CancellationToken;

use super::{DrivenTurn, DrivenTurnEnd, FencedSidecars};
use crate::engine::{AgentSpec, SessionEngine};
use crate::error::CoreError;
use crate::sidecar::{BoundSidecarFactory, SidecarEnvironment, SidecarHandle, SidecarStart};
use crate::{CreateSession, EventBus};

#[derive(Default)]
struct Counts {
    started: AtomicUsize,
    stopped: AtomicUsize,
}

/// Starts sidecars that count their lifecycle; `lose` fires each loss token
/// as soon as the sidecar is ready.
struct FakeSidecars {
    counts: Arc<Counts>,
    lose: bool,
}

struct FakeHandle {
    counts: Arc<Counts>,
    loss: CancellationToken,
    lose: bool,
}

#[async_trait]
impl SidecarHandle for FakeHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        if self.lose {
            self.loss.cancel();
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.counts.stopped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }
}

#[async_trait]
impl BoundSidecarFactory for FakeSidecars {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        assert_eq!(start.lifecycle, crate::sidecar::SidecarLifecycle::Resident);
        self.counts.started.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeHandle {
            counts: Arc::clone(&self.counts),
            loss: CancellationToken::new(),
            lose: self.lose,
        }))
    }
}

struct FakeEnvironment(Arc<FakeSidecars>);

impl SidecarEnvironment for FakeEnvironment {
    fn factory_for(
        &self,
        _binding: &crate::TurnBinding,
        _stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Ok(Some(Arc::clone(&self.0) as Arc<dyn BoundSidecarFactory>))
    }
}

struct Fixture {
    engine: SessionEngine,
    child: SessionId,
    claim: ActorClaim,
    agent: AgentSpec,
    counts: Arc<Counts>,
}

async fn fixture(turns: usize, lose: bool) -> Fixture {
    let counts = Arc::new(Counts::default());
    let provider = FakeProvider::scripted_turns(
        (0..turns)
            .map(|_| vec![FakeStep::Text("ok".into())])
            .collect(),
    );
    let engine = SessionEngine::new(
        hya_store::SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        crate::test_support::runtime(hya_tool_loader::builtins()),
        PermissionPlane::new(PermissionRules::default()).0,
        EventBus::default(),
    )
    .with_sidecar_environment(Arc::new(FakeEnvironment(Arc::new(FakeSidecars {
        counts: Arc::clone(&counts),
        lose,
    }))));
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let create = |parent| CreateSession {
        parent,
        agent: agent.name.clone(),
        model: agent.model.clone(),
        workdir: ".".into(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    };
    let root = engine.create(create(None)).await.unwrap();
    let child = engine.create(create(Some(root))).await.unwrap();
    let owner = OwnerRunId::new();
    let lease = engine
        .host_runtime()
        .bundle_storage()
        .acquire_claim(BundleClaimRequest {
            bundle: hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
            resource_key: child.to_string(),
            owner_id: owner.as_uuid().to_string(),
            ttl_millis: 60_000,
            metadata: serde_json::json!({ "root_session_id": root.to_string() }),
        })
        .await
        .unwrap();
    let claim = ActorClaim {
        actor_id: child,
        epoch: ActorEpoch::from_storage(lease.epoch),
        owner_run_id: owner,
    };
    Fixture {
        engine,
        child,
        claim,
        agent,
        counts,
    }
}

async fn driven(fx: &Fixture, sidecars: &FencedSidecars) -> Result<DrivenTurnEnd, CoreError> {
    let binding = fx
        .engine
        .bind_session_runtime(fx.child, std::path::Path::new("."))
        .await
        .unwrap();
    let id = binding.agent_catalog().default_agent_id().to_string();
    let agents = fx.engine.agent_roster_for_binding(&binding, &id).unwrap();
    let resources = fx
        .engine
        .agent_resource_policy_for_binding(&binding, &id)
        .unwrap();
    let prompt = fx
        .engine
        .admit_user_prompt_with_binding(fx.child, "go".into(), &binding, &id)
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new(&id),
        ..fx.agent.clone()
    };
    fx.engine
        .run_driven_turn(
            fx.child,
            DrivenTurn {
                agent,
                binding,
                resolved: Some((agents, resources)),
                claim: Some(fx.claim),
                guidance: None,
                route: None,
                lease: None,
                admitted_prompt: Some(prompt),
            },
            sidecars,
            CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn a_fenced_session_keeps_one_sidecar_until_its_fence_is_released() {
    let fx = fixture(3, false).await;
    let sidecars = FencedSidecars::default();
    for _ in 0..2 {
        assert_eq!(
            driven(&fx, &sidecars).await.unwrap(),
            DrivenTurnEnd::Finished(FinishReason::Stop)
        );
    }
    assert_eq!(fx.counts.started.load(Ordering::SeqCst), 1);
    assert_eq!(fx.counts.stopped.load(Ordering::SeqCst), 0);

    sidecars.release(&fx.claim).await;
    assert_eq!(fx.counts.stopped.load(Ordering::SeqCst), 1);
    driven(&fx, &sidecars).await.unwrap();
    assert_eq!(fx.counts.started.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_lost_sidecar_fails_the_fenced_turn_and_is_replaced() {
    let fx = fixture(2, true).await;
    let sidecars = FencedSidecars::default();
    assert_eq!(
        driven(&fx, &sidecars).await.unwrap(),
        DrivenTurnEnd::SidecarLost
    );
    assert_eq!(
        driven(&fx, &sidecars).await.unwrap(),
        DrivenTurnEnd::SidecarLost
    );
    assert_eq!(fx.counts.started.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_superseded_fence_refuses_the_turn() {
    let mut fx = fixture(1, false).await;
    fx.claim.epoch = ActorEpoch::from_storage(fx.claim.epoch.get() + 1);
    let sidecars = FencedSidecars::default();
    assert!(matches!(
        driven(&fx, &sidecars).await,
        Err(CoreError::Store(
            hya_store::StoreError::StaleBundleClaim { .. }
        ))
    ));
    assert_eq!(fx.counts.started.load(Ordering::SeqCst), 0);
}

/// Records the depth `tool.visibility` and `model.stream.permit` see, and
/// when the stream permit is dropped.
struct DepthProbe(Arc<std::sync::Mutex<Vec<String>>>);

struct PermitDrop(Arc<std::sync::Mutex<Vec<String>>>);

impl Drop for PermitDrop {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("permit released".into());
    }
}

#[async_trait]
impl hya_runtime_abi::NativeHook for DepthProbe {
    fn id(&self) -> &str {
        "depth-probe"
    }

    fn hook_points(&self) -> &[hya_runtime_abi::HookPoint] {
        &[
            hya_runtime_abi::HookPoint::ToolVisibility,
            hya_runtime_abi::HookPoint::ModelStreamPermit,
        ]
    }

    async fn tool_visibility(
        &self,
        input: hya_runtime_abi::ToolVisibilityInput,
    ) -> Result<hya_runtime_abi::ToolVisibilityOutcome, hya_runtime_abi::HostError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("visibility depth {}", input.depth));
        Ok(hya_runtime_abi::ToolVisibilityOutcome::Preserve)
    }

    async fn model_stream_permit(
        &self,
        input: hya_runtime_abi::NativeStreamPermitInput,
    ) -> Result<Option<hya_runtime_abi::NativeStreamPermit>, hya_runtime_abi::HostError> {
        self.0.lock().unwrap().push(format!(
            "permit depth {} root {}",
            input.depth,
            input.root == input.session
        ));
        Ok(Some(hya_runtime_abi::NativeStreamPermit(Box::new(
            PermitDrop(Arc::clone(&self.0)),
        ))))
    }
}

#[tokio::test]
async fn child_turns_report_their_depth_and_release_stream_permits_per_round() {
    let mut fx = fixture(1, false).await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    fx.engine = fx
        .engine
        .clone()
        .with_hooks(Arc::new(DepthProbe(Arc::clone(&seen))));
    driven(&fx, &FencedSidecars::default()).await.unwrap();
    let seen = seen.lock().unwrap();
    let (visibility, permits): (Vec<_>, Vec<_>) = seen
        .iter()
        .map(String::as_str)
        .partition(|line| line.starts_with("visibility"));
    assert!(!visibility.is_empty());
    assert!(visibility.iter().all(|line| *line == "visibility depth 1"));
    assert_eq!(permits, ["permit depth 1 root false", "permit released"]);
}
