//! Captured per-session lifecycle/event hook chains and how they follow the
//! session's current catalog scope binding.
//!
//! Outside a turn, a session's bundle and scope-plugin hooks (`session.start`,
//! `session.end`, live events) go to the chain captured from its last bind.
//! During a turn the turn's own activation chain takes precedence for events.
//!
//! **Contract.**
//! - *Own bind swaps.* Every bind of a session through
//!   [`SessionEngine::bind_session_runtime`] (admission, turns and their
//!   round rebinds, shell, summary/title, resident activation, …) and every
//!   explicit capture replaces the session's captured chain with the bound
//!   snapshot's chain for the session's agent. The swap is one map write:
//!   an event published before it goes to the old chain, after it to the new
//!   one, never to both or neither. It happens at bind time, before the turn
//!   (or admission) publishes anything from that binding.
//! - *Identity, not generation, decides.* Entries are compared by the
//!   retained source dispatcher (its process) and the owner-bundle filter.
//!   A rebind whose entries are unchanged (a base-only publish that kept the
//!   processes) does nothing. Otherwise dispatchers new to the session get
//!   `session.start` right after the swap; dispatchers kept across the swap
//!   get nothing; dropped ones get no `session.end` (the session did not end)
//!   and are released, so a retired process exits once no binding or scope
//!   overlay holds it.
//! - *Idle sessions never pin a retired process.* When any bind of scope `K`
//!   yields a new generation, the captured chains of other sessions in `K`
//!   whose entries differ from `K`'s current snapshot are released
//!   (dropped, identities remembered); they recapture at their own next bind,
//!   where only processes new to them get `session.start`.
//! - *Invalidation and eviction release at once.* When scope `K` is
//!   invalidated or evicted, every captured chain from `K` is released
//!   immediately.
//! - A released session's out-of-turn events and lifecycle hooks reach no
//!   bundle/scope hooks until its next bind. Process-wide (config) hooks are
//!   separate and unaffected by all of this.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use hya_proto::{ConfigGeneration, SessionId};

use super::SessionEngine;
use crate::catalog_scope::ScopeKey;
use crate::hooks::{HookChain, HookDispatcher, SessionLifecycleInput};
use crate::runtime_registry::{BoundAgentHook, TurnBinding};

/// Identity of one captured chain entry. The `Weak` pins the allocation
/// (never the process), so a later dispatcher cannot reuse its address.
struct CapturedIdentity {
    source: Weak<dyn HookDispatcher>,
    hook_ids: Option<Arc<[String]>>,
}

impl CapturedIdentity {
    fn of(hook: &BoundAgentHook) -> Self {
        Self {
            source: Arc::downgrade(&hook.source),
            hook_ids: hook.hook_ids.clone(),
        }
    }

    fn matches(&self, hook: &BoundAgentHook) -> bool {
        std::ptr::addr_eq(self.source.as_ptr(), Arc::as_ptr(&hook.source))
            && self.hook_ids == hook.hook_ids
    }
}

/// One session's captured hooks and where they came from.
pub(crate) struct CapturedSessionHooks {
    /// `None` when there are no entries or the chain was released.
    chain: Option<Arc<dyn HookDispatcher>>,
    /// Identities of the entries last captured (kept after a release).
    members: Vec<CapturedIdentity>,
    /// Stable agent id the chain was resolved for.
    agent: String,
    scope: ScopeKey,
    generation: ConfigGeneration,
}

pub(crate) type SessionHookMap = HashMap<SessionId, CapturedSessionHooks>;

fn same_members(members: &[CapturedIdentity], hooks: &[BoundAgentHook]) -> bool {
    members.len() == hooks.len()
        && members
            .iter()
            .zip(hooks)
            .all(|(member, hook)| member.matches(hook))
}

impl SessionEngine {
    fn session_hook_map(&self) -> std::sync::RwLockWriteGuard<'_, SessionHookMap> {
        self.session_bundle_hooks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The session's current captured chain, if any.
    pub(crate) fn captured_session_hooks(
        &self,
        session: SessionId,
    ) -> Option<Arc<dyn HookDispatcher>> {
        self.session_bundle_hooks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .and_then(|captured| captured.chain.clone())
    }

    /// Forget the session's captured hooks (its `session.end`), returning
    /// the chain so the caller can notify it.
    pub(crate) fn remove_captured_session_hooks(
        &self,
        session: SessionId,
    ) -> Option<Arc<dyn HookDispatcher>> {
        self.session_hook_map()
            .remove(&session)
            .and_then(|captured| captured.chain)
    }

    /// Replace `session`'s captured chain with `binding`'s chain for
    /// `stable_agent_id` (see the module contract), starting dispatchers new
    /// to the session.
    pub(crate) async fn swap_session_hooks(
        &self,
        session: SessionId,
        binding: &TurnBinding,
        stable_agent_id: &str,
    ) {
        let hooks = binding.bound_hooks_for_agent(stable_agent_id);
        let scope = binding.scope().key();
        let generation = binding.generation();
        let (retired, fresh) = {
            let mut map = self.session_hook_map();
            if let Some(previous) = map.get_mut(&session)
                && previous.chain.is_some() == !hooks.is_empty()
                && previous.agent == stable_agent_id
                && same_members(&previous.members, &hooks)
            {
                previous.scope = scope;
                previous.generation = generation;
                return;
            }
            let previous = map.get(&session);
            let fresh = hooks
                .iter()
                .filter(|hook| {
                    previous.as_ref().is_none_or(|previous| {
                        !previous.members.iter().any(|member| member.matches(hook))
                    })
                })
                .map(|hook| Arc::clone(&hook.dispatcher))
                .collect::<Vec<_>>();
            let chain = (!hooks.is_empty()).then(|| {
                Arc::new(HookChain::new(
                    hooks
                        .iter()
                        .map(|hook| Arc::clone(&hook.dispatcher))
                        .collect(),
                )) as Arc<dyn HookDispatcher>
            });
            let captured = CapturedSessionHooks {
                chain,
                members: hooks.iter().map(CapturedIdentity::of).collect(),
                agent: stable_agent_id.to_string(),
                scope,
                generation,
            };
            let retired = map.insert(session, captured);
            (retired, fresh)
        };
        // Release the old chain (possibly the last handle on a process)
        // outside the lock.
        drop(retired);
        if !fresh.is_empty() {
            HookChain::new(fresh)
                .session_start(SessionLifecycleInput { session })
                .await;
        }
    }

    /// After a bind of `binding`'s scope: swap `own`'s captured chain (when
    /// it has one) and release other sessions' chains in that scope whose
    /// entries the new snapshot retired.
    pub(crate) async fn follow_scope_session_hooks(
        &self,
        binding: &TurnBinding,
        own: Option<SessionId>,
    ) {
        let scope = binding.scope().key();
        let generation = binding.generation();
        let (released, own_agent) = {
            let mut map = self.session_hook_map();
            let mut released = Vec::new();
            for (session, captured) in map.iter_mut() {
                if Some(*session) == own
                    || captured.scope != scope
                    || captured.generation == generation
                    || captured.chain.is_none()
                {
                    continue;
                }
                let hooks = binding.bound_hooks_for_agent(&captured.agent);
                if same_members(&captured.members, &hooks) {
                    captured.generation = generation;
                } else {
                    released.push(captured.chain.take());
                }
            }
            let own_agent = own.and_then(|own| map.get(&own).map(|c| c.agent.clone()));
            (released, own_agent)
        };
        drop(released);
        if let (Some(own), Some(agent)) = (own, own_agent) {
            self.swap_session_hooks(own, binding, &agent).await;
        }
    }

    /// Release every captured chain from scope `key` (invalidated or
    /// evicted) at once.
    pub(crate) fn release_scope_session_hooks(&self, key: &ScopeKey) {
        let released = self
            .session_hook_map()
            .values_mut()
            .filter(|captured| captured.scope == *key)
            .filter_map(|captured| captured.chain.take())
            .collect::<Vec<_>>();
        drop(released);
    }
}
