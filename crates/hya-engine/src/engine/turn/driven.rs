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

/// Sidecars of fenced sessions, kept across their driven turns.
#[derive(Default)]
pub struct FencedSidecars {
    slots: Mutex<HashMap<SessionId, FencedSidecar>>,
}

impl FencedSidecars {
    fn take(&self, session: SessionId) -> Option<FencedSidecar> {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session)
    }

    fn put(&self, session: SessionId, sidecar: FencedSidecar) {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, sidecar);
    }

    /// Stop the sidecar `claim` started, if it is still kept.
    pub async fn release(&self, claim: &ActorClaim) {
        let released = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match slots.get(&claim.actor_id) {
                Some(slot) if slot.claim == *claim => slots.remove(&claim.actor_id),
                _ => None,
            }
        };
        if let Some(mut sidecar) = released
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
        self.validate_actor_claim(Some(claim)).await?;
        let sidecar = match sidecars.take(session) {
            Some(mut kept) if kept.claim != *claim || !kept.handle.is_healthy() => {
                let _ = kept.handle.terminate().await;
                self.start_fenced_sidecar(&binding, &agent, *claim, &cancel)
                    .await?
            }
            Some(kept) => Some(kept),
            None => {
                self.start_fenced_sidecar(&binding, &agent, *claim, &cancel)
                    .await?
            }
        };
        let tools = sidecar
            .as_ref()
            .map_or_else(|| Arc::from([]), |sidecar| Arc::clone(&sidecar.tools));
        let hooks = sidecar.as_ref().and_then(|sidecar| sidecar.hooks.clone());
        let loss = sidecar
            .as_ref()
            .and_then(|sidecar| sidecar.handle.loss_token());
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
                lease,
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
        match sidecar {
            Some(mut sidecar) if matches!(end, Ok(DrivenTurnEnd::SidecarLost)) => {
                let _ = sidecar.handle.terminate().await;
            }
            Some(sidecar) => sidecars.put(session, sidecar),
            None => {}
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
