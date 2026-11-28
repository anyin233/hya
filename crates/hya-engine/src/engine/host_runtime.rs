//! Lightweight [`HostRuntime`] adapter backed by [`SessionEngine`]'s shared
//! store, bus, turn-gate, and reviver handles.
//!
//! [`SessionEngine::host_runtime`] hands it to engine fixtures and to the
//! `hya-app` host runtime, which delegates its session-tree calls here.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock, Weak};

use async_trait::async_trait;
use hya_bundle::{Bundle, PreparedAgent};
use hya_proto::{
    ActorClaim, AgentName, ChannelPolicySnapshot, Envelope, Event, EventSeq, Message, ModelRef,
    OwnerRunId, Projection, SessionId, now_millis,
};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind, HostBundleStorage,
    HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry, HostModelCaller,
    HostRuntime, HostSessionStore, HostTurnRunner, MemberMailRevivalRequest, ModelCallRequest,
    ModelCallResponse, ModelStreamChunk, ModelWindowInfo, SessionListFilter, SessionSummaryMeta,
    SessionTreeArchive, TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus,
};
use hya_store::{BundleClaim, SessionStore};
use hya_tool::{AgentDef, SkillCatalogEntry};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::member_control::ArchiveReviver;
use super::turn_gate::TurnGate;
use crate::bus::EventBus;
use crate::resident::ResidentSupervisor;
use crate::store_bridge::SessionStoreExt as _;

pub(super) const STEER_STATE_BUNDLE: &str = "hya/agent-channels";
pub(super) const STEER_SUPPRESSION_KEY: &str = "steer_suppressed";

type TransientKey = (SessionId, String, String);
pub(super) type TransientState = Arc<RwLock<HashMap<TransientKey, watch::Sender<Option<Value>>>>>;

/// An extension event an after-hook staged for the tool result being committed.
pub(crate) struct StagedEvent {
    pub(crate) event: Event,
    pub(crate) on_commit: Option<Box<dyn FnOnce() + Send>>,
}

/// Staged events per session; a session has an entry only while its tool
/// result runs its after-hooks.
pub(super) type StagedEvents = Arc<Mutex<HashMap<SessionId, Vec<StagedEvent>>>>;

pub(super) fn stage_event(
    staged: &StagedEvents,
    during: SessionId,
    target: SessionId,
    bundle: &str,
    kind: &str,
    payload: Value,
    on_commit: Option<Box<dyn FnOnce() + Send>>,
) -> Result<(), HostError> {
    staged
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&during)
        .ok_or_else(|| {
            HostError::InvalidInput(format!(
                "session `{during}` has no tool result in its after-hooks"
            ))
        })?
        .push(StagedEvent {
            event: Event::Extension {
                session: target,
                bundle: bundle.to_string(),
                kind: kind.to_string(),
                payload,
            },
            on_commit,
        });
    Ok(())
}

pub(super) fn get_transient(
    state: &TransientState,
    bundle: &str,
    session: SessionId,
    key: &str,
) -> Option<Value> {
    state
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(session, bundle.to_owned(), key.to_owned()))
        .and_then(|slot| slot.borrow().clone())
}

pub(super) fn set_transient(
    state: &TransientState,
    bundle: &str,
    session: SessionId,
    key: &str,
    value: Option<Value>,
) {
    let mut entries = state
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (session, bundle.to_owned(), key.to_owned());
    let remove = value.is_none();
    match entries.entry(key) {
        std::collections::hash_map::Entry::Vacant(entry) if !remove => {
            entry.insert(watch::channel(value).0);
        }
        std::collections::hash_map::Entry::Occupied(entry) => {
            if remove && entry.get().receiver_count() == 0 {
                entry.remove();
            } else {
                entry.get().send_if_modified(|current| {
                    if *current == value {
                        return false;
                    }
                    *current = value;
                    true
                });
            }
        }
        std::collections::hash_map::Entry::Vacant(_) => {}
    }
}

pub(super) fn subscribe_transient(
    state: &TransientState,
    bundle: &str,
    session: SessionId,
    key: &str,
) -> watch::Receiver<Option<Value>> {
    state
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry((session, bundle.to_owned(), key.to_owned()))
        .or_insert_with(|| watch::channel(None).0)
        .subscribe()
}

/// Minimal [`HostRuntime`] exposing store, bus, bundle claims, and turn-runner
/// mailbox hooks directly from `SessionEngine`'s shared `Arc` fields.
pub(crate) struct EngineHostRuntime {
    pub(crate) store: SessionStore,
    pub(crate) bus: EventBus,
    pub(crate) runtime_owner: OwnerRunId,
    pub(crate) session_channel_policies: Arc<RwLock<HashMap<SessionId, ChannelPolicySnapshot>>>,
    pub(super) transient_state: TransientState,
    pub(super) staged_events: StagedEvents,
    pub(crate) reviver: Arc<RwLock<Option<Weak<ResidentSupervisor>>>>,
    pub(crate) turn_gate: Arc<TurnGate>,
}

struct SessionTreeSnapshot {
    root_exists: bool,
    children: HashMap<SessionId, Vec<SessionId>>,
    archived: HashSet<SessionId>,
}

impl SessionTreeSnapshot {
    fn children(&self, parent: SessionId) -> &[SessionId] {
        self.children.get(&parent).map_or(&[], Vec::as_slice)
    }
}

impl EngineHostRuntime {
    fn supervisor(&self) -> Option<Arc<ResidentSupervisor>> {
        self.reviver
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
    }

    async fn read_tree(&self, root: SessionId) -> Result<SessionTreeSnapshot, HostError> {
        let events = self
            .store
            .replay_tree(root)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))?;
        let mut tree = SessionTreeSnapshot {
            root_exists: false,
            children: HashMap::new(),
            archived: HashSet::new(),
        };
        for envelope in events {
            match envelope.event {
                Event::SessionCreated {
                    session, parent, ..
                } => {
                    tree.root_exists |= session == root;
                    if let Some(parent) = parent {
                        tree.children.entry(parent).or_default().push(session);
                    }
                }
                Event::SessionArchived { session, .. } => {
                    tree.archived.insert(session);
                }
                Event::SessionUnarchived { session } => {
                    tree.archived.remove(&session);
                }
                _ => {}
            }
        }
        Ok(tree)
    }

    async fn read_root_projection(&self, root: SessionId) -> Result<Arc<Projection>, HostError> {
        self.store
            .read_projection_shared(root)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))
    }

    fn supervisor_for_archive(&self) -> Result<Arc<ResidentSupervisor>, HostError> {
        self.supervisor().ok_or_else(|| {
            HostError::InvalidInput("subagents are not supervised by this process".into())
        })
    }

    async fn session_lineage(&self, session: SessionId) -> Result<(SessionId, u32), HostError> {
        let mut current = session;
        let mut depth: u32 = 0;
        for _ in 0..64 {
            let projection = self
                .store
                .read_projection_shared(current)
                .await
                .map_err(|e| HostError::Storage(e.to_string()))?;
            match projection.session.parent {
                Some(parent) if parent != current => {
                    current = parent;
                    depth = depth.saturating_add(1);
                }
                _ => break,
            }
        }
        Ok((current, depth))
    }
}

fn scoped_state_key(scope: &str, key: &str) -> String {
    if scope.is_empty() {
        key.to_string()
    } else {
        format!("{scope}:{key}")
    }
}

fn strip_scope_prefix<'a>(scope: &str, stored_key: &'a str) -> Option<&'a str> {
    if scope.is_empty() {
        Some(stored_key)
    } else {
        stored_key.strip_prefix(&format!("{scope}:"))
    }
}

#[async_trait]
impl HostSessionStore for EngineHostRuntime {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        self.store
            .read_projection_shared(session)
            .await
            .map(|projection| projection.last_seq > 0)
            .map_err(|e| HostError::Storage(e.to_string()))
    }

    async fn read_events(
        &self,
        session: SessionId,
        after_seq: Option<EventSeq>,
        limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut events = self
            .store
            .replay(session)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        if let Some(seq) = after_seq {
            events.retain(|env| env.seq > seq);
        }
        if let Some(limit) = limit {
            events.truncate(limit);
        }
        Ok(events)
    }

    async fn append_events(
        &self,
        session: SessionId,
        claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        match claim {
            Some(claim) => self
                .store
                .commit_resident_mutation(&claim, session, events)
                .await
                .map_err(map_core_err),
            None => self
                .store
                .append_events(session, events)
                .await
                .map_err(|e| HostError::Storage(e.to_string())),
        }
    }

    async fn append_extension_event(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        let event = Event::Extension {
            session,
            bundle: bundle.to_string(),
            kind: kind.to_string(),
            payload,
        };
        let mut appended = self.append_events(session, claim, vec![event]).await?;
        appended.pop().ok_or_else(|| {
            HostError::Internal("append_extension_event produced no envelope".into())
        })
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        self.store
            .read_projection_shared(session)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))
    }

    async fn read_extension_state(
        &self,
        session: SessionId,
        bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        let projection = self.read_projection(session).await?;
        Ok(projection.extensions.get(bundle).cloned())
    }

    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        let projection = self.read_projection(session).await?;
        if projection.last_seq == 0 {
            return Ok(None);
        }
        let s = &projection.session;
        Ok(Some(SessionSummaryMeta {
            session,
            parent: s.parent,
            agent: s.agent.clone(),
            model: s.model.clone(),
            workdir: s.workdir.clone(),
            title: s.title.clone(),
            project: None,
            archived: false,
            metadata: Value::Null,
            event_count: projection.last_seq,
        }))
    }

    async fn list_sessions(
        &self,
        _filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError> {
        Ok(Vec::new())
    }

    async fn append_batch(
        &self,
        fence: Option<ActorClaim>,
        expect: Vec<(SessionId, EventSeq)>,
        events: Vec<(SessionId, Event)>,
    ) -> Result<Vec<Envelope>, HostError> {
        let store_err = |error: hya_store::StoreError| map_core_err(error.into());
        let mut tx = self.store.begin_transaction().await.map_err(store_err)?;
        if let Some(claim) = &fence {
            crate::store_bridge::fence_actor_claim(&mut tx, claim)
                .await
                .map_err(store_err)?;
        }
        hya_store::expect_last_seqs_in_transaction(&mut tx, &expect)
            .await
            .map_err(store_err)?;
        let mut envelopes = Vec::with_capacity(events.len());
        for (session, event) in events {
            envelopes.push(
                hya_store::append_event_in_transaction(&mut tx, session, event)
                    .await
                    .map_err(store_err)?,
            );
        }
        tx.commit().await.map_err(store_err)?;
        for envelope in &envelopes {
            self.bus.publish(envelope.clone());
        }
        Ok(envelopes)
    }

    fn runtime_owner_id(&self) -> Option<OwnerRunId> {
        Some(self.runtime_owner)
    }

    fn stage_extension_event(
        &self,
        during: SessionId,
        target: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        on_commit: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<(), HostError> {
        stage_event(
            &self.staged_events,
            during,
            target,
            bundle,
            kind,
            payload,
            on_commit,
        )
    }
}

#[async_trait]
impl HostBundleStorage for EngineHostRuntime {
    fn get_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
    ) -> Result<Option<Value>, HostError> {
        Ok(get_transient(&self.transient_state, bundle, session, key))
    }

    fn set_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), HostError> {
        set_transient(&self.transient_state, bundle, session, key, value);
        Ok(())
    }

    fn subscribe_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
    ) -> Result<watch::Receiver<Option<Value>>, HostError> {
        Ok(subscribe_transient(
            &self.transient_state,
            bundle,
            session,
            key,
        ))
    }

    async fn get_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        let stored_key = scoped_state_key(scope, key);
        let row = self
            .store
            .get_bundle_state_entry(bundle, &stored_key)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        row.map(|row| {
            let value = serde_json::from_slice(&row.value)
                .map_err(|e| HostError::Storage(e.to_string()))?;
            Ok(BundleStateEntry {
                bundle: row.bundle,
                scope: scope.to_string(),
                key: key.to_string(),
                value,
                version: 1,
                updated_at_millis: row.updated_at,
            })
        })
        .transpose()
    }

    async fn put_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
        value: Value,
        _expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        let stored_key = scoped_state_key(scope, key);
        self.store
            .set_bundle_state_json(bundle, &stored_key, &value)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        Ok(BundleStateEntry {
            bundle: bundle.to_string(),
            scope: scope.to_string(),
            key: key.to_string(),
            value,
            version: 1,
            updated_at_millis: now_millis(),
        })
    }

    async fn delete_state(&self, bundle: &str, scope: &str, key: &str) -> Result<bool, HostError> {
        let stored_key = scoped_state_key(scope, key);
        self.store
            .delete_bundle_state(bundle, &stored_key)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))
    }

    async fn list_state(
        &self,
        bundle: &str,
        scope: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        let full_prefix = match (scope.is_empty(), prefix) {
            (true, p) => p.map(str::to_owned),
            (false, Some(p)) => Some(format!("{scope}:{p}")),
            (false, None) => Some(format!("{scope}:")),
        };
        let rows = self
            .store
            .list_bundle_state(bundle, full_prefix.as_deref())
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(unscoped_key) = strip_scope_prefix(scope, &row.key) else {
                continue;
            };
            let value = serde_json::from_slice(&row.value)
                .map_err(|e| HostError::Storage(e.to_string()))?;
            out.push(BundleStateEntry {
                bundle: row.bundle,
                scope: scope.to_string(),
                key: unscoped_key.to_string(),
                value,
                version: 1,
                updated_at_millis: row.updated_at,
            });
        }
        Ok(out)
    }

    async fn acquire_claim(
        &self,
        request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError> {
        let ttl = (request.ttl_millis > 0).then_some(request.ttl_millis);
        let row = self
            .store
            .acquire_bundle_claim(
                &request.bundle,
                &request.resource_key,
                &request.owner_id,
                ttl,
                Some(&request.metadata),
            )
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        Ok(claim_to_lease(row))
    }

    async fn renew_claim(
        &self,
        lease: &BundleClaimLease,
        ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        let claim = lease_to_claim(lease);
        let ttl = (ttl_millis > 0).then_some(ttl_millis);
        let row = self
            .store
            .renew_bundle_claim(&claim, ttl, None)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        Ok(claim_to_lease(row))
    }

    async fn release_claim(&self, lease: &BundleClaimLease) -> Result<(), HostError> {
        let claim = lease_to_claim(lease);
        self.store
            .release_bundle_claim(&claim)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))
    }

    async fn read_claim(
        &self,
        bundle: &str,
        resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError> {
        let row = self
            .store
            .get_bundle_claim_at(bundle, resource_key, now_millis())
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        Ok(row.map(claim_to_lease))
    }

    async fn list_claims(&self, bundle: &str) -> Result<Vec<BundleClaimLease>, HostError> {
        let rows = self
            .store
            .list_active_bundle_claims_at(bundle, now_millis())
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(claim_to_lease).collect())
    }
}

#[async_trait]
impl HostTurnRunner for EngineHostRuntime {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }

    async fn list_child_sessions(&self, parent: SessionId) -> Result<Vec<SessionId>, HostError> {
        Ok(self.read_tree(parent).await?.children(parent).to_vec())
    }

    async fn archive_session_tree(
        &self,
        session: SessionId,
    ) -> Result<SessionTreeArchive, HostError> {
        let (root, depth) = self.session_lineage(session).await?;
        let projection = self.read_root_projection(root).await?;
        let projection = hya_tool::extension::team_projection(&projection);
        let registered = projection
            .roster
            .values()
            .map(|entry| entry.session)
            .collect::<HashSet<_>>();
        let closed = projection
            .archived
            .values()
            .map(|entry| entry.session)
            .collect::<HashSet<_>>();
        if depth > 0 && (registered.contains(&session) || closed.contains(&session)) {
            let receipt = self
                .supervisor_for_archive()?
                .archive_member_for_user(root, &session.to_string())
                .await
                .map_err(map_core_err)?;
            let projection = self.read_root_projection(root).await?;
            let projection = hya_tool::extension::team_projection(&projection);
            let descendants = receipt
                .descendants
                .iter()
                .map(|handle| {
                    projection
                        .archived
                        .get(handle)
                        .map(|entry| entry.session)
                        .or_else(|| projection.roster.get(handle).map(|entry| entry.session))
                        .ok_or_else(|| {
                            HostError::Internal(format!(
                                "archived session for `{handle}` is missing"
                            ))
                        })
                })
                .collect::<Result<_, _>>()?;
            return Ok(SessionTreeArchive {
                cancelled_turn: receipt.cancelled_turn,
                descendants,
            });
        }

        let tree = self.read_tree(session).await?;
        if !tree.root_exists {
            return Err(HostError::NotFound(format!(
                "session `{session}` was not found"
            )));
        }
        let mut sessions = vec![session];
        let mut index = 0;
        while index < sessions.len() {
            sessions.extend_from_slice(tree.children(sessions[index]));
            index += 1;
        }

        let mut descendants = Vec::new();
        let mut events = Vec::new();
        let stamp = serde_json::Number::from(now_millis().max(1));
        // Walking in reverse visits descendants before their parent.
        for current in sessions.into_iter().rev() {
            if closed.contains(&current) || tree.archived.contains(&current) {
                continue;
            }
            if current != root && registered.contains(&current) {
                self.supervisor_for_archive()?
                    .archive_member_for_user(root, &current.to_string())
                    .await
                    .map_err(map_core_err)?;
            } else {
                events.push((
                    current,
                    Event::SessionArchived {
                        session: current,
                        archived: stamp.clone(),
                    },
                ));
            }
            if current != session {
                descendants.push(current);
            }
        }
        if !events.is_empty() {
            let envelopes = self
                .store
                .append_events_batch(events)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            for envelope in envelopes {
                self.bus.publish(envelope);
            }
        }
        Ok(SessionTreeArchive {
            cancelled_turn: false,
            descendants,
        })
    }

    async fn run_turn(
        &self,
        _request: TurnStartRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }

    async fn spawn_session_and_run(
        &self,
        _request: TurnSpawnRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }

    async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }

    async fn cancel_turn(
        &self,
        session: SessionId,
        reason: Option<String>,
    ) -> Result<bool, HostError> {
        Ok(self
            .turn_gate
            .cancel(session, finish_cause(reason.as_deref())))
    }

    async fn turn_status(&self, session: SessionId) -> Result<TurnStatus, HostError> {
        if self.turn_gate.is_active(session) {
            Ok(TurnStatus::Running)
        } else {
            Ok(TurnStatus::Idle)
        }
    }

    // Adapter: removed in CHR-47.
    async fn session_channel_policy(
        &self,
        session: SessionId,
    ) -> Result<Option<ChannelPolicySnapshot>, HostError> {
        Ok(self
            .session_channel_policies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .copied())
    }

    // Adapter: removed in CHR-47.
    fn mail_mark(&self, session: SessionId) -> Option<u64> {
        self.turn_gate.mail_mark(session)
    }

    // Adapter: removed in CHR-47.
    fn notify_mail_consumed(&self, session: SessionId, mark: u64, consumed_incoming: bool) {
        self.turn_gate
            .notify_mail_consumed(session, Some(mark), consumed_incoming);
    }

    // Adapter: removed in CHR-47.
    fn is_member_stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        self.supervisor()
            .is_some_and(|sup| sup.stopped_by_user(root, handle))
    }

    // Adapter: removed in CHR-47.
    async fn revive_archived_member_with_mail(
        &self,
        request: MemberMailRevivalRequest,
    ) -> Result<usize, HostError> {
        let Some(supervisor) = self.supervisor() else {
            return Err(HostError::Unavailable(format!(
                "archived member `{}` cannot be revived without a resident supervisor",
                request.target_handle
            )));
        };
        let policy = self
            .session_channel_policies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&request.from_session)
            .copied();
        supervisor
            .revive_episode(
                request.root,
                &request.from_handle,
                &request.target_handle,
                request.channel,
                request.kind,
                request.body,
                request.actor_claim.as_ref(),
                policy,
            )
            .await
            .map_err(map_core_err)
    }

    // Adapter: removed in CHR-47.
    async fn archive_subagent(
        &self,
        child: SessionId,
    ) -> Result<hya_tool::ArchiveReceipt, HostError> {
        let (root, depth) = self.session_lineage(child).await?;
        if depth == 0 {
            return Err(HostError::InvalidInput(
                "this session is not a subagent; use CancelTurn or CreateTurn on it directly"
                    .to_string(),
            ));
        }
        self.supervisor_for_archive()?;
        let projection = self.read_root_projection(root).await?;
        let target = crate::resident::resolve_member_target(
            &projection,
            root,
            hya_proto::ROOT_HANDLE,
            &child.to_string(),
        )
        .map_err(map_core_err)?;
        let archived = self.archive_session_tree(child).await?;
        let projection = self.read_root_projection(root).await?;
        let projection = hya_tool::extension::team_projection(&projection);
        let descendants = archived
            .descendants
            .into_iter()
            .map(|id| {
                projection
                    .archived
                    .values()
                    .find(|entry| entry.session == id)
                    .map(|entry| entry.handle.clone())
                    .or_else(|| {
                        projection
                            .roster
                            .values()
                            .find(|entry| entry.session == id)
                            .map(|entry| entry.handle.clone())
                    })
                    .ok_or_else(|| {
                        HostError::Internal(format!(
                            "archived session handle for `{id}` is missing"
                        ))
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(hya_tool::ArchiveReceipt {
            handle: target.handle,
            session: child,
            cancelled_turn: archived.cancelled_turn,
            descendants,
        })
    }

    // Adapter: removed in CHR-47.
    async fn steer_subagent(
        &self,
        child: SessionId,
        text: &str,
    ) -> Result<hya_tool::MailReceipt, HostError> {
        let body = text.trim();
        if body.is_empty() {
            return Err(HostError::InvalidInput(
                "a steer message needs text".to_string(),
            ));
        }
        let (root, depth) = self.session_lineage(child).await?;
        if depth == 0 {
            return Err(HostError::InvalidInput(
                "this session is not a subagent; use CancelTurn or CreateTurn on it directly"
                    .to_string(),
            ));
        }
        let projection = self
            .store
            .read_projection_shared(root)
            .await
            .map_err(|e| HostError::Storage(e.to_string()))?;
        let target = crate::resident::resolve_member_target(
            &projection,
            root,
            hya_proto::scope::ROOT_HANDLE,
            &child.to_string(),
        )
        .map_err(map_core_err)?;
        let parent =
            hya_proto::scope::parent_path(&target.handle).unwrap_or(hya_proto::scope::ROOT_HANDLE);
        let team = hya_tool::extension::team_projection(&projection);
        let parent_session = if parent == hya_proto::scope::ROOT_HANDLE {
            root
        } else {
            team.roster
                .get(parent)
                .map(|entry| entry.session)
                .ok_or_else(|| {
                    HostError::InvalidInput(format!(
                        "`{}`'s parent `{parent}` is archived; steer `{parent}` first",
                        target.handle
                    ))
                })?
        };
        let from = if parent_session == root {
            team.roster
                .values()
                .find(|entry| entry.session == root)
                .map_or_else(
                    || hya_proto::scope::ROOT_HANDLE.to_string(),
                    |entry| entry.handle.clone(),
                )
        } else {
            parent.to_string()
        };
        let channel = team.resolve_dm(&from, &target.handle).ok_or_else(|| {
            HostError::PermissionDenied(format!(
                "mail authorization rejected ({from} -> direct:{}): no DM channel with that target; run `list_channel` to see your channels",
                target.handle
            ))
        })?;
        drop(projection);
        let formatted = format!("{}{body}", super::member_control::USER_STEER_PREFIX);
        let append = self
            .store
            .append_channel_mail(
                root,
                None,
                from.clone(),
                channel.clone(),
                hya_proto::MailKind::Message,
                formatted.clone(),
                None,
            )
            .await;
        let (envelope, recipients) = match append {
            Ok((envelope, recipients)) => (Some(envelope), recipients),
            Err(crate::CoreError::MailRevivalRequired {
                target: target_handle,
                channel: dm_channel,
                ..
            }) => {
                let recipients = self
                    .revive_archived_member_with_mail(MemberMailRevivalRequest {
                        root,
                        from_session: parent_session,
                        from_handle: from.clone(),
                        target_handle,
                        channel: dm_channel,
                        kind: hya_proto::MailKind::Message,
                        body: formatted,
                        actor_claim: None,
                    })
                    .await?;
                (None, recipients)
            }
            Err(error) => return Err(map_core_err(error)),
        };
        if let Some(envelope) = envelope {
            self.bus.publish(envelope);
        }
        if let Some(supervisor) = self.supervisor() {
            supervisor.resume_for_user(child);
        }
        Ok(hya_tool::MailReceipt {
            from,
            to: hya_proto::MailEndpoint::Channel(channel),
            recipients,
        })
    }
}

#[async_trait]
impl HostModelCaller for EngineHostRuntime {
    async fn complete(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }

    async fn stream(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }

    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[Message],
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    async fn model_window(&self, _model: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostEventBus for EngineHostRuntime {
    fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
        self.bus.publish(envelope);
        Ok(())
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Ok(self.bus.subscribe())
    }

    async fn publish_extension(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), HostError> {
        self.bus.publish(Envelope {
            seq: EventSeq(0),
            ts_millis: now_millis(),
            event: Event::Extension {
                session,
                bundle: bundle.to_string(),
                kind: kind.to_string(),
                payload,
            },
        });
        Ok(())
    }
}

#[async_trait]
impl HostCatalogReader for EngineHostRuntime {
    async fn list_agents(&self, _session: Option<SessionId>) -> Result<Vec<AgentDef>, HostError> {
        Ok(Vec::new())
    }

    async fn get_agent(
        &self,
        _name: &AgentName,
        _session: Option<SessionId>,
    ) -> Result<Option<PreparedAgent>, HostError> {
        Ok(None)
    }

    async fn list_skills(
        &self,
        _session: Option<SessionId>,
    ) -> Result<Vec<SkillCatalogEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
        Ok(Vec::new())
    }

    async fn get_bundle(&self, _id: &str) -> Result<Option<Bundle>, HostError> {
        Ok(None)
    }

    async fn bundle_config(&self, _bundle_id: &str) -> Result<Value, HostError> {
        Ok(Value::Null)
    }

    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
}

#[async_trait]
impl HostExtensionRegistry for EngineHostRuntime {
    async fn list_extensions(
        &self,
        _kind: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn has_bundle(&self, _bundle_id: &str) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn invoke_bundle_api(
        &self,
        _invocation: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
    }
}

impl HostRuntime for EngineHostRuntime {
    fn session_store(&self) -> &dyn HostSessionStore {
        self
    }
    fn bundle_storage(&self) -> &dyn HostBundleStorage {
        self
    }
    fn turn_runner(&self) -> &dyn HostTurnRunner {
        self
    }
    fn model_caller(&self) -> &dyn HostModelCaller {
        self
    }
    fn event_bus(&self) -> &dyn HostEventBus {
        self
    }
    fn catalog_reader(&self) -> &dyn HostCatalogReader {
        self
    }
    fn extension_registry(&self) -> &dyn HostExtensionRegistry {
        self
    }
}

fn claim_to_lease(row: BundleClaim) -> BundleClaimLease {
    BundleClaimLease {
        bundle: row.bundle,
        resource_key: row.claim_key,
        owner_id: row.owner,
        epoch: row.epoch,
        expires_at_millis: row.expires_at.unwrap_or(i64::MAX),
        metadata: row.metadata,
    }
}

fn lease_to_claim(lease: &BundleClaimLease) -> BundleClaim {
    BundleClaim {
        bundle: lease.bundle.clone(),
        claim_key: lease.resource_key.clone(),
        owner: lease.owner_id.clone(),
        epoch: lease.epoch,
        expires_at: (lease.expires_at_millis != i64::MAX).then_some(lease.expires_at_millis),
        metadata: lease.metadata.clone(),
        acquired_at: 0,
        updated_at: 0,
    }
}

/// The [`FinishCause`](hya_proto::FinishCause) a `cancel_turn` reason names
/// on the wire (`archived`, `leader_failed`, …); other text records `other`
/// and no reason records `user_cancel`.
pub(crate) fn finish_cause(reason: Option<&str>) -> hya_proto::FinishCause {
    reason.map_or(hya_proto::FinishCause::UserCancel, |reason| {
        serde_json::from_value(Value::String(reason.to_string()))
            .unwrap_or(hya_proto::FinishCause::Other)
    })
}

fn map_core_err(error: crate::CoreError) -> HostError {
    match error {
        crate::CoreError::Store(hya_store::StoreError::StaleSessionSeq { .. }) => {
            HostError::Conflict(error.to_string())
        }
        crate::CoreError::CapacityExceeded { .. }
        | crate::CoreError::MailboxRejected(_)
        | crate::CoreError::MailRevivalRequired { .. } => HostError::Conflict(error.to_string()),
        crate::CoreError::MailAuthorizationRejected { .. } => {
            HostError::PermissionDenied(error.to_string())
        }
        crate::CoreError::Invalid(msg) => HostError::InvalidInput(msg),
        crate::CoreError::Store(hya_store::StoreError::StaleBundleClaim { .. }) => {
            HostError::StaleClaim(error.to_string())
        }
        other => HostError::Internal(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn session_tree_lists_direct_children_and_archives_the_subtree() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) =
            hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
        let engine = crate::SessionEngine::new(
            store,
            Arc::new(hya_provider::ProviderRouter::new()),
            crate::test_support::runtime(hya_tool_loader::builtins()),
            permission,
            EventBus::default(),
        );
        let create = |parent| {
            engine.create(crate::CreateSession {
                parent,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                project: None,
                workdir: "/tmp".into(),
                kind: hya_proto::SessionKind::Project,
            })
        };
        let root = create(None).await.unwrap();
        let child = create(Some(root)).await.unwrap();
        let leaf = create(Some(child)).await.unwrap();
        let sibling = create(Some(root)).await.unwrap();
        let host = engine.host_runtime();
        let turns = host.turn_runner();

        assert_eq!(
            turns.list_child_sessions(root).await.unwrap(),
            vec![child, sibling]
        );
        assert_eq!(turns.list_child_sessions(child).await.unwrap(), vec![leaf]);
        assert_eq!(
            turns.archive_session_tree(child).await.unwrap(),
            SessionTreeArchive {
                cancelled_turn: false,
                descendants: vec![leaf]
            },
        );
        for (session, archived) in [(root, false), (child, true), (leaf, true), (sibling, false)] {
            assert_eq!(
                engine
                    .read_projection_shared(session)
                    .await
                    .unwrap()
                    .session
                    .is_archived(),
                archived
            );
        }
        assert_eq!(
            turns.list_child_sessions(root).await.unwrap(),
            vec![child, sibling]
        );
        assert_eq!(
            turns.archive_session_tree(child).await.unwrap(),
            SessionTreeArchive::default()
        );
    }

    #[tokio::test]
    async fn archive_adapter_keeps_registered_handles_for_the_subtree() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (permission, _rx) =
            hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
        let engine = Arc::new(crate::SessionEngine::new(
            store,
            Arc::new(
                hya_provider::ProviderRouter::new()
                    .with(Arc::new(hya_provider::DevProvider::new())),
            ),
            crate::test_support::runtime(hya_tool_loader::builtins()),
            permission,
            EventBus::default(),
        ));
        let root = engine
            .create(crate::CreateSession {
                parent: None,
                agent: AgentName::new("hya-main"),
                model: ModelRef::new("hya/offline"),
                project: None,
                workdir: "/tmp".into(),
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        let owner = ResidentSupervisor::start(Arc::clone(&engine));
        let scout = crate::AgentSpec {
            name: AgentName::new("hya-scout"),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".into(),
            system_prompt: String::new(),
            reasoning: None,
        };
        let binding = engine.bind_runtime(&scout.workdir).unwrap();
        let resources = binding.agent_resource_policy(scout.name.as_str()).unwrap();
        let resolved = (binding, Arc::from([]), resources, None);
        let (child, handle, _) = owner
            .spawn_resident_parked(
                root,
                scout.clone(),
                resolved.clone(),
                "parent".into(),
                None,
                None,
            )
            .await
            .unwrap();
        let (leaf, leaf_handle, _) = owner
            .spawn_resident_parked(child, scout, resolved, "child".into(), None, None)
            .await
            .unwrap();

        let host = engine.host_runtime();
        let turns = host.turn_runner();
        assert_eq!(turns.list_child_sessions(child).await.unwrap(), vec![leaf]);
        let receipt = turns.archive_subagent(child).await.unwrap();
        assert_eq!(receipt.session, child);
        assert_eq!(receipt.handle, handle);
        assert_eq!(receipt.descendants, vec![leaf_handle]);
        assert!(!receipt.cancelled_turn);
        let error = turns.archive_subagent(child).await.unwrap_err();
        assert!(
            matches!(error, HostError::InvalidInput(reason) if reason.contains("already archived"))
        );
    }

    #[tokio::test]
    async fn transient_state_watches_only_its_bundle_session_and_key() {
        let state = TransientState::default();
        let session = SessionId::new();
        let mut changes =
            subscribe_transient(&state, STEER_STATE_BUNDLE, session, STEER_SUPPRESSION_KEY);
        assert!(changes.borrow_and_update().is_none());
        set_transient(
            &state,
            "other/bundle",
            session,
            STEER_SUPPRESSION_KEY,
            Some(Value::Bool(true)),
        );
        set_transient(
            &state,
            STEER_STATE_BUNDLE,
            SessionId::new(),
            STEER_SUPPRESSION_KEY,
            Some(Value::Bool(true)),
        );
        assert!(!changes.has_changed().unwrap());

        for value in [Some(Value::Bool(true)), None] {
            set_transient(
                &state,
                STEER_STATE_BUNDLE,
                session,
                STEER_SUPPRESSION_KEY,
                value.clone(),
            );
            assert!(changes.has_changed().unwrap());
            changes.changed().await.unwrap();
            assert_eq!(*changes.borrow(), value);
            assert_eq!(
                get_transient(&state, STEER_STATE_BUNDLE, session, STEER_SUPPRESSION_KEY),
                value,
            );
        }
    }

    fn test_engine(store: SessionStore) -> crate::SessionEngine {
        let (permission, _rx) =
            hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default());
        crate::SessionEngine::new(
            store,
            Arc::new(hya_provider::ProviderRouter::new()),
            crate::test_support::runtime(hya_tool_loader::builtins()),
            permission,
            EventBus::default(),
        )
    }

    fn ping(session: SessionId, bundle: &str) -> Event {
        Event::Extension {
            session,
            bundle: bundle.into(),
            kind: "ping".into(),
            payload: Value::Null,
        }
    }

    #[tokio::test]
    async fn append_batch_is_atomic_across_sessions_and_checks_expect_and_fence() {
        let engine = test_engine(SessionStore::connect_memory().await.unwrap());
        let host = engine.host_runtime();
        let store = host.session_store();
        let (a, b) = (SessionId::new(), SessionId::new());
        let first = store
            .append_events(a, None, vec![ping(a, "t/x")])
            .await
            .unwrap();
        let a_last = first[0].seq;

        let stale = store
            .append_batch(
                None,
                vec![(a, a_last), (b, EventSeq(7))],
                vec![(a, ping(a, "t/x")), (b, ping(b, "t/x"))],
            )
            .await;
        assert!(matches!(stale, Err(HostError::Conflict(_))), "{stale:?}");
        assert_eq!(store.read_events(b, None, None).await.unwrap().len(), 0);

        let owner = OwnerRunId::new();
        let lease = host
            .bundle_storage()
            .acquire_claim(hya_runtime_abi::BundleClaimRequest {
                bundle: hya_tool::extension::BUNDLE_MULTI_AGENT.into(),
                resource_key: b.to_string(),
                owner_id: owner.as_uuid().to_string(),
                ttl_millis: 60_000,
                metadata: Value::Null,
            })
            .await
            .unwrap();
        let fence = ActorClaim {
            actor_id: b,
            epoch: hya_proto::ActorEpoch::from_storage(lease.epoch),
            owner_run_id: owner,
        };
        let superseded = ActorClaim {
            epoch: hya_proto::ActorEpoch::from_storage(lease.epoch + 1),
            ..fence
        };
        let refused = store
            .append_batch(Some(superseded), Vec::new(), vec![(b, ping(b, "t/x"))])
            .await;
        assert!(
            matches!(refused, Err(HostError::StaleClaim(_))),
            "{refused:?}"
        );

        let mut live = engine.bus().subscribe();
        let appended = store
            .append_batch(
                Some(fence),
                vec![(a, a_last), (b, EventSeq(0))],
                vec![(a, ping(a, "t/x")), (b, ping(b, "t/x"))],
            )
            .await
            .unwrap();
        assert_eq!(appended.len(), 2);
        assert_eq!(store.read_events(a, None, None).await.unwrap().len(), 2);
        assert_eq!(store.read_events(b, None, None).await.unwrap().len(), 1);
        assert_eq!(live.recv().await.unwrap().seq, appended[0].seq);
        assert_eq!(live.recv().await.unwrap().seq, appended[1].seq);
    }

    #[tokio::test]
    async fn turn_control_reports_and_cancels_the_active_turn() {
        let engine = test_engine(SessionStore::connect_memory().await.unwrap());
        let host = engine.host_runtime();
        let session = SessionId::new();
        let runner = host.turn_runner();
        assert_eq!(runner.turn_status(session).await.unwrap(), TurnStatus::Idle);
        assert!(!runner.cancel_turn(session, None).await.unwrap());

        let lease = engine.try_begin_turn(session).unwrap();
        let cancel = lease.bind_cancel(&CancellationToken::new());
        assert_eq!(
            runner.turn_status(session).await.unwrap(),
            TurnStatus::Running
        );
        assert!(
            runner
                .cancel_turn(session, Some("archived".into()))
                .await
                .unwrap()
        );
        assert!(cancel.is_cancelled());
        assert_eq!(
            engine.turn_cancel_cause(session),
            Some(hya_proto::FinishCause::Archived)
        );
        assert_eq!(
            finish_cause(Some("no such cause")),
            hya_proto::FinishCause::Other
        );
    }

    /// Counts the events it folds, by owning bundle.
    struct CountingReducer {
        bundle: &'static str,
        observed: &'static [&'static str],
    }

    impl hya_runtime_abi::ProjectionReducer for CountingReducer {
        fn bundle_id(&self) -> &str {
            self.bundle
        }

        fn initial_state(&self) -> Value {
            serde_json::json!({})
        }

        fn apply(
            &self,
            state: &mut Value,
            event: &hya_runtime_abi::ExtensionEventContext,
        ) -> Result<(), HostError> {
            let count = state[&event.bundle].as_u64().unwrap_or(0);
            state[&event.bundle] = (count + 1).into();
            Ok(())
        }

        fn observed_bundles(&self) -> &[&str] {
            self.observed
        }
    }

    #[tokio::test]
    async fn observed_reducers_fold_other_bundles_events_into_their_own_state() {
        let owner = || -> Arc<dyn hya_runtime_abi::ProjectionReducer> {
            Arc::new(CountingReducer {
                bundle: "t/owner",
                observed: &[],
            })
        };
        let observer = || -> Arc<dyn hya_runtime_abi::ProjectionReducer> {
            Arc::new(CountingReducer {
                bundle: "t/observer",
                observed: &["t/owner", "t/absent"],
            })
        };
        // One call, and two calls in either order, fold the same state.
        for calls in [
            vec![vec![owner(), observer()]],
            vec![vec![owner()], vec![observer()]],
            vec![vec![observer()], vec![owner()]],
        ] {
            let engine = test_engine(SessionStore::connect_memory().await.unwrap());
            for reducers in calls {
                engine.register_native_projection_reducers(reducers);
            }
            let session = SessionId::new();
            let store = engine.host_runtime();
            let store = store.session_store();
            store
                .append_events(
                    session,
                    None,
                    vec![
                        ping(session, "t/owner"),
                        ping(session, "t/absent"),
                        ping(session, "t/observer"),
                    ],
                )
                .await
                .unwrap();
            let projection = store.read_projection(session).await.unwrap();
            assert_eq!(
                projection.extensions["t/owner"],
                serde_json::json!({"t/owner": 1})
            );
            assert_eq!(
                projection.extensions["t/observer"],
                serde_json::json!({"t/owner": 1, "t/absent": 1, "t/observer": 1})
            );
        }
    }
}
