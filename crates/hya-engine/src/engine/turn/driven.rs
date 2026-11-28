//! Turns a bundle drives through `HostTurnRunner::run_turn`.
//!
//! A fenced turn runs a resolved child runtime and keeps one sidecar for its
//! session across turns, until the fence that started it is released or
//! replaced. An unfenced turn runs on the session's own binding.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hya_proto::{ActorClaim, FinishReason, MessageId, SessionId};
use hya_tool::{AgentDef, ResolvedTool};
use tokio_util::sync::CancellationToken;

use super::{
    ExplicitTurnRoute, SessionEngine, TurnActivation, TurnLease, TurnRequestContext,
    scope_activation_hooks,
};
use crate::error::CoreError;
use crate::hooks::HookChain;
use crate::sidecar::{SidecarHandle, SidecarStart};
use crate::{AgentResourcePolicy, AgentSpec, TurnBinding};

/// One bundle-driven turn, resolved by the host.
pub struct DrivenTurn {
    /// Agent spec the turn runs.
    pub agent: AgentSpec,
    /// Binding the turn's resources compile under.
    pub binding: TurnBinding,
    /// For a fenced turn, the child roster and resource policy resolved
    /// from its parent's binding.
    pub resolved: Option<(Arc<[AgentDef]>, AgentResourcePolicy)>,
    /// Fence of the turn.
    pub claim: Option<ActorClaim>,
    /// Guidance layer composed into the system prompt.
    pub guidance: Option<Arc<str>>,
    /// Explicit model route.
    pub route: Option<ExplicitTurnRoute>,
    /// The session's turn lease when the caller took it; otherwise the turn
    /// queues behind an active one.
    pub lease: Option<TurnLease>,
    /// The prompt the caller admitted for this turn, if any.
    pub admitted_prompt: Option<MessageId>,
}

/// How a driven turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrivenTurnEnd {
    /// The turn finished with this reason.
    Finished(FinishReason),
    /// The session's sidecar was lost; the turn was cancelled.
    SidecarLost,
}

struct FencedSidecar {
    claim: ActorClaim,
    handle: Box<dyn SidecarHandle>,
    tools: Arc<[ResolvedTool]>,
    hooks: Option<Arc<HookChain>>,
}

impl FencedSidecar {
    /// What a turn uses: the sidecar's tools, hooks, and loss token.
    fn share(&self) -> SharedSidecar {
        (
            Arc::clone(&self.tools),
            self.hooks.clone(),
            self.handle.loss_token(),
        )
    }
}

type SharedSidecar = (
    Arc<[ResolvedTool]>,
    Option<Arc<HookChain>>,
    Option<CancellationToken>,
);

/// Sidecars of fenced sessions, kept across their driven turns.
#[derive(Default)]
pub struct FencedSidecars {
    slots: Mutex<HashMap<SessionId, FencedSidecar>>,
}

impl FencedSidecars {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, FencedSidecar>> {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Remove `session`'s kept sidecar when another fence started it or it
    /// is unhealthy.
    fn take_stale(&self, session: SessionId, claim: &ActorClaim) -> Option<FencedSidecar> {
        let mut slots = self.lock();
        match slots.get(&session) {
            Some(kept) if kept.claim != *claim || !kept.handle.is_healthy() => {
                slots.remove(&session)
            }
            _ => None,
        }
    }

    /// Remove the sidecar `claim` started, if it is still kept.
    fn take(&self, claim: &ActorClaim) -> Option<FencedSidecar> {
        let mut slots = self.lock();
        match slots.get(&claim.actor_id) {
            Some(kept) if kept.claim == *claim => slots.remove(&claim.actor_id),
            _ => None,
        }
    }

    /// Stop the sidecar `claim` started, if it is still kept.
    pub async fn release(&self, claim: &ActorClaim) {
        if let Some(mut sidecar) = self.take(claim)
            && let Err(error) = sidecar.handle.shutdown().await
        {
            tracing::warn!(session = %claim.actor_id, %error, "fenced sidecar did not stop");
        }
    }
}

impl SessionEngine {
    /// Run one bundle-driven turn on `session`.
    ///
    /// # Errors
    /// Fence, store, sidecar start, and turn failures.
    pub async fn run_driven_turn(
        &self,
        session: SessionId,
        turn: DrivenTurn,
        sidecars: &FencedSidecars,
        cancel: CancellationToken,
    ) -> Result<DrivenTurnEnd, CoreError> {
        let DrivenTurn {
            agent,
            binding,
            resolved,
            claim,
            guidance,
            route,
            lease,
            admitted_prompt,
        } = turn;
        let (Some(claim), Some((agents, resources))) = (claim, resolved) else {
            return self
                .run_turn_with_external_dirs_and_claim(
                    session,
                    &agent,
                    TurnActivation::Bound(binding),
                    TurnRequestContext {
                        lease,
                        ..TurnRequestContext::new(cancel, &[], guidance, claim.as_ref(), route)
                            .with_admitted_prompt(admitted_prompt)
                    },
                )
                .await
                .map(DrivenTurnEnd::Finished);
        };
        let claim = &claim;
        // The lease comes first so concurrent turns on the session take
        // turns with its one kept sidecar.
        let lease = match lease {
            Some(lease) => lease,
            None => match self.begin_turn(session, &cancel).await? {
                Some(lease) => lease,
                None => {
                    return self
                        .end_queued_turn(session, Some(claim), admitted_prompt.is_some(), &cancel)
                        .await
                        .map(DrivenTurnEnd::Finished);
                }
            },
        };
        self.validate_actor_claim(Some(claim)).await?;
        if let Some(mut stale) = sidecars.take_stale(session, claim) {
            let _ = stale.handle.terminate().await;
        }
        // The sidecar stays kept while turns use it, so `release` reaches it
        // mid-turn; only the lease holder inserts it.
        let kept = sidecars.lock().get(&session).map(FencedSidecar::share);
        let kept = match kept {
            Some(kept) => Some(kept),
            None => self
                .start_fenced_sidecar(&binding, &agent, *claim, &cancel)
                .await?
                .map(|sidecar| {
                    let shared = sidecar.share();
                    sidecars.lock().insert(session, sidecar);
                    shared
                }),
        };
        let (tools, hooks, loss) = kept.unwrap_or_else(|| (Arc::from([]), None, None));
        let turn_cancel = cancel.child_token();
        let run = self.run_turn_with_external_dirs_and_claim(
            session,
            &agent,
            TurnActivation::Resolved {
                binding: binding.clone(),
                agents,
                resources,
                sidecar_tools: tools,
            },
            TurnRequestContext {
                lease: Some(lease),
                ..TurnRequestContext::new(turn_cancel.clone(), &[], guidance, Some(claim), route)
                    .with_admitted_prompt(admitted_prompt)
            },
        );
        let mut run = Box::pin(async move {
            match hooks {
                Some(hooks) => scope_activation_hooks(session, hooks, run).await,
                None => run.await,
            }
        });
        let lost = async {
            match &loss {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let end = tokio::select! {
            biased;
            () = lost => {
                turn_cancel.cancel();
                let _ = run.await;
                Ok(DrivenTurnEnd::SidecarLost)
            }
            outcome = &mut run => outcome.map(DrivenTurnEnd::Finished),
        };
        if matches!(end, Ok(DrivenTurnEnd::SidecarLost))
            && let Some(mut lost) = sidecars.take(claim)
        {
            let _ = lost.handle.terminate().await;
        }
        end
    }

    async fn start_fenced_sidecar(
        &self,
        binding: &TurnBinding,
        agent: &AgentSpec,
        claim: ActorClaim,
        cancel: &CancellationToken,
    ) -> Result<Option<FencedSidecar>, CoreError> {
        let Some(environment) = self.sidecar_environment.as_ref() else {
            return Ok(None);
        };
        let Some(factory) = environment.factory_for(binding, agent.name.as_str())? else {
            return Ok(None);
        };
        let mut handle = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(CoreError::Cancelled),
            started = factory.start(SidecarStart::persistent()) => started?,
        };
        let ready = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(CoreError::Cancelled),
            ready = handle.ready() => ready,
        };
        if let Err(error) = ready {
            let _ = handle.terminate().await;
            return Err(error);
        }
        Ok(Some(FencedSidecar {
            claim,
            tools: handle.tool_bindings(),
            hooks: handle
                .hook_dispatcher()
                .map(crate::bundle_hooks::restricted_sidecar_hooks),
            handle,
        }))
    }
}
