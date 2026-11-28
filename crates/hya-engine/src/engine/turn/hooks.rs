//! Per-turn extensions to the kernel's round callbacks.

use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{ActorClaim, MessageId, ModelRef, SessionId};
use hya_provider::{CompletionRequest, EventStream, ProviderRouter};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

use super::SessionEngine;
use super::route::{ExplicitTurnRoute, core_route_failure_class, provider_route_failure_class};
use crate::hooks::{HookPoint, NativeStreamPermit, NativeStreamPermitInput};
use crate::{CoreError, kernel::Failure};

pub(super) struct OpenRequest<'a> {
    pub(super) router: &'a ProviderRouter,
    pub(super) request: &'a CompletionRequest,
    pub(super) session: SessionId,
    pub(super) message: MessageId,
}

pub(super) type Opened = (EventStream, ModelRef);

#[async_trait]
pub(super) trait TurnHook: Send {
    async fn before_round(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn before_open(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn open(&mut self, _input: &OpenRequest<'_>) -> Result<Option<Opened>, CoreError> {
        Ok(None)
    }

    fn failed(
        &mut self,
        _failure: &Failure,
        _retry: &mut Option<Duration>,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    fn collected(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

/// The engine's own turn hook entries. `Route` (a request's explicit model
/// route) is core provider routing; `StreamPermit` and `Claim` are temporary
/// adapters for the actor extensions the engine still wires.
pub(super) fn entries<'a>(
    engine: &'a SessionEngine,
    session: SessionId,
    claim: Option<&'a ActorClaim>,
    route: Option<&'a ExplicitTurnRoute>,
    cancel: &'a CancellationToken,
    depth: u32,
) -> Vec<Box<dyn TurnHook + 'a>> {
    let mut entries: Vec<Box<dyn TurnHook>> = Vec::new();
    if let Some(route) = route {
        entries.push(Box::new(Route {
            engine,
            route,
            cancel,
            candidate: None,
        }));
    }
    if let Some(governor) = &engine.governor {
        entries.push(Box::new(StreamPermit {
            governor,
            reserved: depth == 0,
            permit: None,
        }));
    }
    entries.push(Box::new(HookPermit {
        engine,
        session,
        depth,
        permits: None,
    }));
    if let Some(claim) = claim {
        entries.push(Box::new(Claim { engine, claim }));
    }
    entries
}

/// Resolve the advertised delivery tool. The legacy report capability stays
/// here until its bundle can declare it directly.
pub(super) fn delivery_tool<'a>(
    engine: &SessionEngine,
    binding: &crate::TurnBinding,
    agent: &super::super::AgentSpec,
    resources: &crate::runtime_registry::CompiledResourceView,
    parent: Option<SessionId>,
    offered: &'a [hya_proto::ToolSchema],
) -> Result<Option<&'a str>, CoreError> {
    for schema in offered {
        let name = schema.name.as_str();
        let terminal = resources
            .resolve_tool(name)
            .is_some_and(|tool| tool.tool.ends_turn())
            || (name == "report"
                && crate::lifecycle::report_available(
                    engine.lifecycle.is_connected(),
                    binding,
                    &agent.name,
                    parent,
                )?);
        if terminal {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

struct Claim<'a> {
    engine: &'a SessionEngine,
    claim: &'a ActorClaim,
}

#[async_trait]
impl TurnHook for Claim<'_> {
    async fn before_round(&mut self) -> Result<(), CoreError> {
        self.engine.validate_actor_claim(Some(self.claim)).await
    }

    async fn before_open(&mut self) -> Result<(), CoreError> {
        self.engine.validate_actor_claim(Some(self.claim)).await
    }
}

struct StreamPermit<'a> {
    governor: &'a super::super::StreamGovernor,
    reserved: bool,
    permit: Option<OwnedSemaphorePermit>,
}

#[async_trait]
impl TurnHook for StreamPermit<'_> {
    async fn before_open(&mut self) -> Result<(), CoreError> {
        if self.permit.is_none() {
            self.permit = if self.reserved {
                self.governor.acquire_reserved_stream().await
            } else {
                self.governor.acquire_general_stream().await
            };
        }
        Ok(())
    }

    fn failed(
        &mut self,
        _failure: &Failure,
        retry: &mut Option<Duration>,
    ) -> Result<(), CoreError> {
        if retry.is_some() {
            self.permit = None;
        }
        Ok(())
    }

    fn collected(&mut self) -> Result<(), CoreError> {
        // Tool calls can wait for work that needs its own stream permit.
        self.permit = None;
        Ok(())
    }
}

/// The `model.stream.permit` guards of the hooks active for the turn, held
/// from opening a stream until it is collected or the round waits to retry.
struct HookPermit<'a> {
    engine: &'a SessionEngine,
    session: SessionId,
    depth: u32,
    permits: Option<Vec<NativeStreamPermit>>,
}

#[async_trait]
impl TurnHook for HookPermit<'_> {
    async fn before_open(&mut self) -> Result<(), CoreError> {
        if self.permits.is_some() {
            return Ok(());
        }
        let Some(hooks) = self
            .engine
            .active_hook_dispatcher(self.session)
            .filter(|hooks| hooks.handles(HookPoint::ModelStreamPermit))
        else {
            return Ok(());
        };
        let (root, _) = self.engine.session_lineage(self.session).await?;
        self.permits = Some(
            hooks
                .model_stream_permit(NativeStreamPermitInput {
                    session: self.session,
                    root,
                    depth: self.depth,
                })
                .await,
        );
        Ok(())
    }

    fn failed(
        &mut self,
        _failure: &Failure,
        retry: &mut Option<Duration>,
    ) -> Result<(), CoreError> {
        if retry.is_some() {
            self.permits = None;
        }
        Ok(())
    }

    fn collected(&mut self) -> Result<(), CoreError> {
        self.permits = None;
        Ok(())
    }
}

struct Route<'a> {
    engine: &'a SessionEngine,
    route: &'a ExplicitTurnRoute,
    cancel: &'a CancellationToken,
    candidate: Option<usize>,
}

#[async_trait]
impl TurnHook for Route<'_> {
    async fn open(&mut self, input: &OpenRequest<'_>) -> Result<Option<Opened>, CoreError> {
        match self
            .engine
            .stream_with_explicit_route(
                input.router,
                input.request,
                input.session,
                input.message,
                self.route,
                self.candidate,
            )
            .await
        {
            Ok((stream, index)) => {
                self.candidate = Some(index);
                Ok(Some((
                    stream,
                    self.route.route().candidates[index].model.clone(),
                )))
            }
            Err(error) => {
                if !self.cancel.is_cancelled() {
                    self.route
                        .finalize(Some(provider_route_failure_class(&error)))?;
                }
                Err(error.into())
            }
        }
    }

    fn failed(&mut self, failure: &Failure, retry: &mut Option<Duration>) -> Result<(), CoreError> {
        if !failure.step_started {
            // Opening already exhausted or recorded the explicit route.
            *retry = None;
        } else if retry.is_none() {
            self.route
                .finalize(Some(core_route_failure_class(&failure.error, self.cancel)))?;
        }
        Ok(())
    }

    fn collected(&mut self) -> Result<(), CoreError> {
        self.route.finalize(None)?;
        self.candidate = None;
        Ok(())
    }
}
