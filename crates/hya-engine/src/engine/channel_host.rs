//! Lightweight [`HostRuntime`] adapter backed by [`SessionEngine`]'s shared
//! store, bus, turn-gate, and reviver handles.
//!
//! Used by [`SessionEngine::try_new`] when wiring the default `hya/agent-channels`
//! native bundle (`ChannelUriSchemaHandler` and `UnreadMailSteerHook`) in
//! standalone engine tests; full `hya-app` runtimes install the complete
//! `AppHostRuntime` honoring `BuiltinBundleSelection`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};

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
    StagedMailAck, TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus,
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
pub(crate) struct EngineChannelsHostRuntime {
    pub(crate) store: SessionStore,
    pub(crate) bus: EventBus,
    pub(crate) runtime_owner: OwnerRunId,
    pub(crate) session_channel_policies: Arc<RwLock<HashMap<SessionId, ChannelPolicySnapshot>>>,
    pub(super) mail_steer_suppressed: TransientState,
    pub(crate) staged_mail_acks: Arc<RwLock<HashMap<SessionId, StagedMailAck>>>,
    pub(crate) reviver: Arc<RwLock<Option<Weak<ResidentSupervisor>>>>,
    pub(crate) turn_gate: Arc<TurnGate>,
}

impl EngineChannelsHostRuntime {
    fn supervisor(&self) -> Option<Arc<ResidentSupervisor>> {
        self.reviver
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
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
impl HostSessionStore for EngineChannelsHostRuntime {
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

    fn runtime_owner_id(&self) -> Option<OwnerRunId> {
        Some(self.runtime_owner)
    }
}

#[async_trait]
impl HostBundleStorage for EngineChannelsHostRuntime {
    fn get_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
    ) -> Result<Option<Value>, HostError> {
        Ok(get_transient(
            &self.mail_steer_suppressed,
            bundle,
            session,
            key,
        ))
    }

    fn set_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), HostError> {
        set_transient(&self.mail_steer_suppressed, bundle, session, key, value);
        Ok(())
    }

    fn subscribe_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
    ) -> Result<watch::Receiver<Option<Value>>, HostError> {
        Ok(subscribe_transient(
            &self.mail_steer_suppressed,
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
impl HostTurnRunner for EngineChannelsHostRuntime {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Err(HostError::Unavailable(
            "engine-channels host runtime".into(),
        ))
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
        _session: SessionId,
        _reason: Option<String>,
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn turn_status(&self, session: SessionId) -> Result<TurnStatus, HostError> {
        if self.turn_gate.is_active(session) {
            Ok(TurnStatus::Running)
        } else {
            Ok(TurnStatus::Idle)
        }
    }

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

    fn mail_mark(&self, session: SessionId) -> Option<u64> {
        self.turn_gate.mail_mark(session)
    }

    fn notify_mail_consumed(&self, session: SessionId, mark: u64, consumed_incoming: bool) {
        self.turn_gate
            .notify_mail_consumed(session, Some(mark), consumed_incoming);
    }

    fn is_member_stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        self.supervisor()
            .is_some_and(|sup| sup.stopped_by_user(root, handle))
    }

    // Adapter: removed in CHR-47.
    fn is_mail_steer_suppressed(&self, session: SessionId) -> bool {
        get_transient(
            &self.mail_steer_suppressed,
            STEER_STATE_BUNDLE,
            session,
            STEER_SUPPRESSION_KEY,
        ) == Some(Value::Bool(true))
    }

    fn stage_mail_steer_ack(&self, session: SessionId, ack: StagedMailAck) {
        self.staged_mail_acks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, ack);
    }

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
        let Some(supervisor) = self.supervisor() else {
            return Err(HostError::InvalidInput(
                "subagents are not supervised by this process".to_string(),
            ));
        };
        supervisor
            .archive_member_for_user(root, &child.to_string())
            .await
            .map_err(map_core_err)
    }

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
impl HostModelCaller for EngineChannelsHostRuntime {
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
impl HostEventBus for EngineChannelsHostRuntime {
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
impl HostCatalogReader for EngineChannelsHostRuntime {
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
impl HostExtensionRegistry for EngineChannelsHostRuntime {
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

impl HostRuntime for EngineChannelsHostRuntime {
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

fn map_core_err(error: crate::CoreError) -> HostError {
    match error {
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
}
