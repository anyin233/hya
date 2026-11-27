//! Domain store operations over `hya-store` v2 (`bundle_claim` + `Event::Extension`).
//!
//! Replaces the v1 domain-specific `resident_claim`, `mailbox`, and `workflow`
//! modules that were removed from `hya-store` in `CHR-16`. All concurrency
//! fencing, atomic lease accounting, and transactional event appends use
//! `hya_store::SessionStore` transactions and `bundle_claim` directly.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use hya_proto::{
    ActorClaim, ActorEpoch, ChannelKind, Envelope, Event, FinishCause, FinishReason, MailEndpoint,
    MailKind, MemberRunStatus, MessageId, OwnerRunId, PartId, PartProjection, Projection, Role,
    RosterStatus, SessionId, SubagentMode, ToolCallId, ToolPartState, WorkflowRunId, now_millis,
    scope,
};
use hya_store::{
    BundleClaim, SessionStore, StoreError, StoreTransaction, append_event_in_transaction,
    open_turn_terminal_events,
};
use hya_tool::extension::{
    BUNDLE_MULTI_AGENT, BUNDLE_WORKFLOW, ExtensionEvent, ResidentWorkProjection, RosterEntry,
    handoff_projection, members_projection, projection_reducers, team_projection,
    workflow_projection,
};
use hya_tool::mailbox::ChannelPolicySnapshot;
use hya_tool::workflow_plane::WorkflowRunStatus;
use hya_tool::{ArchiveReason, ReportOutcome};
use serde_json::{Value, json};

use crate::error::CoreError;

const CLAIM_BUNDLE: &str = BUNDLE_MULTI_AGENT;

/// Result of recovering (taking over) a resident actor claim after restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredActorClaim {
    /// New active claim after bumping the epoch for this owner run.
    pub claim: ActorClaim,
    /// Epoch that was active before takeover (for classifying interrupted work).
    pub previous_epoch: ActorEpoch,
}

/// A wake admits new mail or resumes work retained by user cancellation.
#[derive(Debug)]
pub enum ResidentWorkPrompt {
    /// Admit one new coalesced user message.
    Text(String),
    /// Resume an unfinished admitted wake after user cancellation.
    Resume {
        /// The unfinished user message owned by this turn.
        message: MessageId,
        /// Coalesced new mail, appended only if the prompt is still unanswered.
        append: String,
    },
}

/// Prepared canonical events for one new episode in a registration batch.
pub struct EpisodeRegistration {
    /// Member session whose active claim is the capacity lease.
    pub actor: SessionId,
    /// Session, member, roster, channel and optional directive-mail events.
    pub events: Vec<Event>,
}

/// Durable recovery disposition for a resident actor's in-flight work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveredResidentWork {
    /// No pending mail or active work.
    Idle,
    /// Unread mail is queued at `inbox_cursor`.
    Queued {
        /// Current inbox cursor.
        inbox_cursor: u64,
    },
    /// Active work was interrupted on the prior run.
    AbortedRunning {
        /// Inbox boundary of the aborted turn.
        inbox_cursor: u64,
        /// Whether more mail arrived past `inbox_cursor`.
        queued_after: bool,
    },
}

/// Result of recovering a resident actor's durable work state.
pub struct RecoveredResidentOutcome {
    /// Classified work disposition.
    pub work: RecoveredResidentWork,
    /// Envelopes committed during recovery.
    pub envelopes: Vec<Envelope>,
}

/// Prepared terminal effects; generation fences the handoff episode.
pub struct ResidentArchive {
    /// Resident session being archived.
    pub actor: SessionId,
    /// Next handoff generation.
    pub generation: u32,
    /// Prepared handoff document.
    pub doc: String,
    /// Whether the handoff is deterministic.
    pub degraded: bool,
    /// Archive classification.
    pub reason: ArchiveReason,
    /// Human-readable terminal reason used in cancellation and member closure.
    pub note: String,
    /// Terminal status of the parent member row, supplied by the caller.
    pub member_status: MemberRunStatus,
    /// Optional outcome and report sent to the parent.
    pub report: Option<(ReportOutcome, String)>,
    /// Recheck unread mail and live children for a voluntary report.
    pub check_gate: bool,
    /// Cursor and active work observed before preparing the handoff.
    pub expected_work: Option<(u64, Option<ResidentWorkProjection>)>,
}

/// Capability used for a terminal transaction.
#[derive(Clone, Copy, Debug)]
pub enum ResidentArchiveOwner<'a> {
    /// The episode ending itself under its current claim.
    Claim(&'a ActorClaim),
    /// End an orphan: takes over the active claim and fences the previous owner atomically.
    Takeover,
}

/// Durable result of a terminal transaction, distinct from a store failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentArchiveOutcome {
    /// Terminal effects committed (or were already committed for this claim).
    Committed,
    /// Only a harness notice committed; the episode remains live.
    Refused,
}

/// Result of atomically admitting one durable Workflow run.
#[derive(Debug, PartialEq)]
pub enum WorkflowAdmissionOutcome {
    /// The start event was appended by this request.
    Admitted(Box<Envelope>),
    /// This run id was already admitted with the same immutable request hash.
    Existing,
    /// This run id was already admitted with different immutable request data.
    Conflict,
    /// Another run currently owns the Session.
    Busy {
        /// Active run identity.
        run: WorkflowRunId,
    },
}

/// Result of atomically changing one Session's Workflow selection.
#[derive(Debug, PartialEq)]
pub enum WorkflowSelectionOutcome {
    /// The selection event was appended by this request.
    Selected(Box<Envelope>),
    /// A run currently owns the Session, so selection did not change.
    Busy {
        /// Active run identity.
        run: WorkflowRunId,
    },
}

fn owner_to_string(owner: OwnerRunId) -> String {
    owner.as_uuid().to_string()
}

fn parse_owner(raw: &str) -> Result<OwnerRunId, CoreError> {
    serde_json::from_value(Value::String(raw.to_owned())).map_err(|err| {
        CoreError::Store(StoreError::BundleClaimData(format!(
            "invalid owner_run_id `{raw}`: {err}"
        )))
    })
}

fn claim_to_bundle(claim: &ActorClaim) -> BundleClaim {
    BundleClaim {
        bundle: CLAIM_BUNDLE.to_owned(),
        claim_key: claim.actor_id.to_string(),
        owner: owner_to_string(claim.owner_run_id),
        epoch: claim.epoch.get(),
        expires_at: None,
        metadata: Value::Null,
        acquired_at: 0,
        updated_at: 0,
    }
}

fn bundle_to_claim(claim: &BundleClaim) -> Result<ActorClaim, CoreError> {
    let actor_id: SessionId = claim.claim_key.parse().map_err(|err| {
        CoreError::Store(StoreError::BundleClaimData(format!(
            "invalid actor_id `{}`: {err}",
            claim.claim_key
        )))
    })?;
    let owner_run_id = parse_owner(&claim.owner)?;
    if claim.epoch == 0 {
        return Err(CoreError::Store(StoreError::BundleClaimData(
            "actor epoch must be >= 1".to_owned(),
        )));
    }
    Ok(ActorClaim {
        actor_id,
        epoch: ActorEpoch::from_storage(claim.epoch),
        owner_run_id,
    })
}

fn claim_metadata(root: SessionId) -> Value {
    json!({ "root_session_id": root.to_string() })
}

fn claim_root_id(claim: &BundleClaim) -> Option<SessionId> {
    claim
        .metadata
        .get("root_session_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
}

pub(crate) async fn fence_actor_claim(
    tx: &mut StoreTransaction<'_>,
    claim: &ActorClaim,
) -> Result<(), StoreError> {
    SessionStore::fence_bundle_claim_in_tx_at(tx, &claim_to_bundle(claim), now_millis()).await
}

async fn check_capacity(
    tx: &mut StoreTransaction<'_>,
    root: SessionId,
    requested: u64,
    limit: u64,
) -> Result<(), CoreError> {
    let claims =
        SessionStore::list_active_bundle_claims_in_tx_at(tx, CLAIM_BUNDLE, now_millis()).await?;
    let live = claims
        .iter()
        .filter(|c| claim_root_id(c) == Some(root))
        .count() as u64;
    if requested > limit.saturating_sub(live) {
        return Err(CoreError::CapacityExceeded { requested, limit });
    }
    Ok(())
}

async fn try_claim_new_in_tx(
    tx: &mut StoreTransaction<'_>,
    root: SessionId,
    actor_id: SessionId,
    owner: OwnerRunId,
) -> Result<ActorClaim, CoreError> {
    let bundle_claim = SessionStore::acquire_bundle_claim_in_tx_at(
        tx,
        CLAIM_BUNDLE,
        &actor_id.to_string(),
        &owner_to_string(owner),
        None,
        Some(&claim_metadata(root)),
        now_millis(),
    )
    .await?;
    bundle_to_claim(&bundle_claim)
}

async fn has_active_claim(
    tx: &mut StoreTransaction<'_>,
    actor_id: SessionId,
) -> Result<bool, StoreError> {
    Ok(SessionStore::get_bundle_claim_in_tx_at(
        tx,
        CLAIM_BUNDLE,
        &actor_id.to_string(),
        now_millis(),
    )
    .await?
    .is_some())
}

async fn resident_member_is_eligible(
    tx: &mut StoreTransaction<'_>,
    entry: &RosterEntry,
) -> Result<bool, StoreError> {
    if !entry.accepts_mail() {
        return Ok(false);
    }
    has_active_claim(tx, entry.session).await
}

#[allow(clippy::too_many_arguments)]
async fn append_resolved_channel_mail(
    tx: &mut StoreTransaction<'_>,
    root: SessionId,
    from: String,
    key: String,
    kind: MailKind,
    body: String,
    projection: &Projection,
    policy: Option<ChannelPolicySnapshot>,
) -> Result<(Envelope, usize), CoreError> {
    let team = team_projection(projection);
    let reject = |reason: &str| CoreError::MailAuthorizationRejected {
        sender: from.clone(),
        channel: key.clone(),
        reason: reason.into(),
    };
    if from != scope::ROOT_HANDLE && !team.roster.contains_key(&from) {
        return Err(reject("sender is not on the live roster"));
    }
    let state = team
        .channels
        .get(&key)
        .ok_or_else(|| reject("channel does not exist"))?;
    if !state.members.contains(&from) {
        return Err(reject("sender is not a channel member"));
    }
    let group = state.kind == ChannelKind::Group;
    if group && state.unit.as_deref() != Some(from.as_str()) {
        return Err(reject("only the unit leader may announce"));
    }
    let bits = policy.map(|policy| {
        if group {
            policy.unit_leader
        } else if state
            .members
            .iter()
            .any(|peer| scope::parent_path(peer) == Some(from.as_str()))
        {
            policy.dm_parent
        } else {
            policy.dm_child
        }
    });
    if bits.is_some_and(|bits| bits & 1 == 0) {
        return Err(reject("direction denied by policy"));
    }
    if !group {
        for peer in state.members.iter().filter(|peer| **peer != from) {
            if team.archived.contains_key(peer) && !team.roster.contains_key(peer) {
                if scope::parent_path(peer) == Some(from.as_str()) {
                    return Err(CoreError::MailRevivalRequired {
                        sender: from,
                        channel: key,
                        target: peer.clone(),
                    });
                }
                return Err(reject("archived recipient is not a direct child"));
            }
        }
        let active_claims =
            SessionStore::list_active_bundle_claims_in_tx_at(tx, CLAIM_BUNDLE, now_millis())
                .await?;
        let active: HashSet<String> = active_claims.into_iter().map(|c| c.claim_key).collect();
        for member in &state.members {
            if !team.mail_recipient_is_live(member) {
                return Err(reject("recipient is stopped or terminal"));
            }
            if let Some(entry) = team.roster.get(member)
                && entry.session != root
                && entry.mode.is_resident()
                && !active.contains(&entry.session.to_string())
            {
                return Err(reject("recipient has no active actor claim"));
            }
        }
    }
    let recipients = state
        .members
        .iter()
        .filter(|member| team.mail_recipient_is_live(member))
        .count();
    let kind = if group { MailKind::Announcement } else { kind };

    let envelope = append_event_in_transaction(
        tx,
        root,
        ExtensionEvent::MailSent {
            session: root,
            from,
            to: MailEndpoint::Channel(key),
            kind,
            body,
        }
        .into_event(),
    )
    .await?;
    Ok((envelope, recipients))
}

fn resident_effect_terminal_events(
    actor: SessionId,
    projection: &Projection,
    reason: &str,
) -> Vec<Event> {
    let mut events = Vec::new();
    let open_calls: Vec<ToolCallId> = projection
        .session
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant && message.finish.is_none())
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            PartProjection::Tool {
                call,
                state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                ..
            } => Some(*call),
            _ => None,
        })
        .collect();
    for member in members_projection(projection) {
        if matches!(
            member.status,
            MemberRunStatus::Spawning | MemberRunStatus::Running
        ) && member
            .tool_call
            .is_none_or(|call| open_calls.contains(&call))
        {
            events.push(
                ExtensionEvent::MemberFinished {
                    session: actor,
                    member: member.member,
                    status: MemberRunStatus::Cancelled,
                    summary: reason.to_string(),
                    child: member.child,
                }
                .into_event(),
            );
        }
    }
    events.extend(open_turn_terminal_events(
        actor,
        projection,
        reason,
        "STALE_ACTOR_CLAIM",
        None,
    ));
    events
}

async fn append_resident_effects_in_transaction(
    store: &SessionStore,
    tx: &mut StoreTransaction<'_>,
    actor: SessionId,
    reason: &str,
) -> Result<Vec<Envelope>, StoreError> {
    let projection = store.replay_projection_in_transaction(tx, actor).await?;
    let events = resident_effect_terminal_events(actor, &projection, reason);
    let mut envelopes = Vec::with_capacity(events.len());
    for event in events {
        envelopes.push(append_event_in_transaction(tx, actor, event).await?);
    }
    Ok(envelopes)
}

async fn close_parent_member_row_in_transaction(
    store: &SessionStore,
    tx: &mut StoreTransaction<'_>,
    actor: SessionId,
    status: MemberRunStatus,
    reason: &str,
) -> Result<Vec<Envelope>, StoreError> {
    let actor_projection = store.replay_projection_in_transaction(tx, actor).await?;
    let Some(parent) = actor_projection.session.parent else {
        return Ok(Vec::new());
    };
    let parent_projection = store.replay_projection_in_transaction(tx, parent).await?;
    let mut envelopes = Vec::new();
    for row in members_projection(&parent_projection) {
        if row.child == Some(actor) {
            let event = ExtensionEvent::MemberFinished {
                session: parent,
                member: row.member,
                status,
                summary: reason.to_string(),
                child: Some(actor),
            }
            .into_event();
            envelopes.push(append_event_in_transaction(tx, parent, event).await?);
        }
    }
    Ok(envelopes)
}

async fn released_resident_matches(
    store: &SessionStore,
    tx: &mut StoreTransaction<'_>,
    claim: &ActorClaim,
    root: SessionId,
    handle: &str,
) -> Result<bool, StoreError> {
    let released = SessionStore::is_bundle_claim_released_in_tx(
        tx,
        CLAIM_BUNDLE,
        &claim.actor_id.to_string(),
        &owner_to_string(claim.owner_run_id),
        claim.epoch.get(),
    )
    .await?;
    if !released {
        return Ok(false);
    }
    let projection = store.replay_projection_in_transaction(tx, root).await?;
    Ok(team_projection(&projection)
        .archived
        .get(handle)
        .is_some_and(|entry| entry.session == claim.actor_id))
}

/// Extension methods on [`SessionStore`] implementing multi-agent, mailbox, and workflow
/// transactional operations over `hya-store` v2 primitives.
#[allow(async_fn_in_trait, clippy::too_many_arguments)]
pub trait SessionStoreExt {
    /// Reactivate an archived member, cut its context and append its first mail in one tx.
    async fn revive_episode(
        &self,
        root: SessionId,
        owner: OwnerRunId,
        capacity: u64,
        parent_claim: Option<&ActorClaim>,
        actor: SessionId,
        from: String,
        channel: String,
        kind: MailKind,
        body: String,
        context_marker: &str,
        policy: Option<ChannelPolicySnapshot>,
    ) -> Result<(Option<(ActorClaim, u64)>, Vec<Envelope>, usize), CoreError>;

    /// Deliver one coalesced wake and record its work boundary under one writer reservation.
    async fn begin_work(
        &self,
        claim: Option<&ActorClaim>,
        root: SessionId,
        handle: &str,
        inbox_through: u64,
        prompt: ResidentWorkPrompt,
        current_task: String,
    ) -> Result<(MessageId, Vec<Envelope>), CoreError>;

    /// Commit a tool outcome, its file observations, and optional team-mail acknowledgement together.
    async fn commit_tool_result_with_mail(
        &self,
        claim: Option<&ActorClaim>,
        session: SessionId,
        events: Vec<Event>,
        acknowledgement: Option<Event>,
    ) -> Result<Vec<Envelope>, CoreError>;

    /// Session ids of resident actors currently in the `active` claim state.
    async fn active_actor_ids(&self) -> Result<Vec<SessionId>, CoreError>;

    /// Claim a never-claimed or explicitly released actor.
    async fn try_claim_new(
        &self,
        root: SessionId,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<ActorClaim, CoreError>;

    /// Register a complete batch and lazily register its root in one writer transaction.
    async fn register_episodes(
        &self,
        root: SessionId,
        owner: OwnerRunId,
        parent_claim: Option<&ActorClaim>,
        capacity: u64,
        registrations: Vec<EpisodeRegistration>,
    ) -> Result<Vec<(ActorClaim, Vec<Envelope>)>, CoreError>;

    /// Current capacity leases, including idle and recovered members.
    async fn live_member_count(&self, root: SessionId) -> Result<u64, CoreError>;

    /// Take over one active actor before runtime readiness.
    async fn recover_claim(
        &self,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<RecoveredActorClaim, CoreError>;

    /// Point-check an execution capability against the current durable owner.
    async fn validate_actor_claim(&self, claim: &ActorClaim) -> Result<(), CoreError>;

    /// Release only the exact current capability and its capacity lease.
    async fn release_claim(&self, claim: &ActorClaim) -> Result<(), CoreError>;

    /// Atomically fence and append one resident-owned canonical mutation.
    async fn commit_resident_mutation(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, CoreError>;

    /// Append one harness-authored direct mail to a live resident member.
    async fn append_harness_mail(
        &self,
        root: SessionId,
        handle: &str,
        body: String,
    ) -> Result<Envelope, CoreError>;

    /// Append one channel-targeted mail under the writer lock.
    async fn append_channel_mail(
        &self,
        root: SessionId,
        claim: Option<&ActorClaim>,
        from: String,
        channel: String,
        kind: MailKind,
        body: String,
        policy: Option<ChannelPolicySnapshot>,
    ) -> Result<(Envelope, usize), CoreError>;

    /// Recover durable resident actor state after restart.
    async fn recover_resident_actor(
        &self,
        recovered: &RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<RecoveredResidentOutcome, CoreError>;

    /// Commit terminal effects, or only a refusal notice for a voluntary report.
    async fn finalize_resident_archive(
        &self,
        owner: ResidentArchiveOwner<'_>,
        root: SessionId,
        handle: &str,
        archive: ResidentArchive,
    ) -> Result<(ResidentArchiveOutcome, Vec<Envelope>), CoreError>;

    /// Atomically fence, deduplicate, exclude, and append one Workflow run start.
    async fn admit_workflow_run(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        event: Event,
    ) -> Result<WorkflowAdmissionOutcome, CoreError>;

    /// Atomically reject selection during an active run and append otherwise.
    async fn select_workflow(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        event: Event,
    ) -> Result<WorkflowSelectionOutcome, CoreError>;

    /// Mark every persisted nonterminal Workflow run as interrupted.
    async fn recover_nonterminal_workflows(
        &self,
        current_owner: OwnerRunId,
        reason: &str,
    ) -> Result<Vec<Envelope>, CoreError>;

    /// Append a pending restart resume prompt including interrupted subagent handles.
    async fn take_restart_resume_with_subagents(
        &self,
        session: SessionId,
        owner: OwnerRunId,
    ) -> Result<Option<(MessageId, Vec<Envelope>)>, CoreError>;
}

impl SessionStoreExt for SessionStore {
    async fn revive_episode(
        &self,
        root: SessionId,
        owner: OwnerRunId,
        capacity: u64,
        parent_claim: Option<&ActorClaim>,
        actor: SessionId,
        from: String,
        channel: String,
        kind: MailKind,
        body: String,
        context_marker: &str,
        policy: Option<ChannelPolicySnapshot>,
    ) -> Result<(Option<(ActorClaim, u64)>, Vec<Envelope>, usize), CoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = parent_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let mut projection = self.replay_projection_in_transaction(&mut tx, root).await?;
        let append = append_resolved_channel_mail(
            &mut tx,
            root,
            from.clone(),
            channel.clone(),
            kind,
            body.clone(),
            &projection,
            policy,
        )
        .await;
        let target = match append {
            Ok((envelope, recipients)) => {
                tx.commit().await?;
                return Ok((None, vec![envelope], recipients));
            }
            Err(CoreError::MailRevivalRequired { target, .. }) => target,
            Err(error) => return Err(error),
        };
        let team = team_projection(&projection);
        let archived = team.archived.get(&target).ok_or_else(|| {
            CoreError::Store(StoreError::BundleClaimData(
                "revive target missing from archive".into(),
            ))
        })?;
        if archived.session != actor {
            return Err(CoreError::Store(StoreError::BundleClaimData(
                "revive actor changed during preparation".into(),
            )));
        }
        let agent_type = archived.agent_type.clone();
        let actor_projection = self
            .replay_projection_in_transaction(&mut tx, actor)
            .await?;
        let handoff = handoff_projection(&actor_projection).ok_or_else(|| {
            CoreError::Store(StoreError::BundleClaimData(
                "archived resident has no handoff".into(),
            ))
        })?;
        let boundary = format!(
            "{context_marker}\nPRIOR HANDOFF (generation {})\n{}",
            handoff.generation, handoff.doc
        );
        let cursor = team
            .inboxes
            .get(&target)
            .map_or(0, |inbox| inbox.len() as u64);
        check_capacity(&mut tx, root, 1, capacity).await?;
        let claim = try_claim_new_in_tx(&mut tx, root, actor, owner).await?;
        let message = MessageId::new();
        let part = PartId::new();
        let events = [
            ExtensionEvent::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: scope::leaf(&target).into(),
                parent: Some(from.clone()),
                agent_type,
                mode: SubagentMode::Resident,
            }
            .into_event(),
            ExtensionEvent::AgentRestarted {
                session: root,
                handle: target,
                child: actor,
                epoch: claim.epoch,
            }
            .into_event(),
            Event::MessageStarted {
                session: actor,
                message,
                role: Role::System,
                agent: None,
                model: None,
            },
            Event::TextStart {
                session: actor,
                message,
                part,
            },
            Event::TextDelta {
                session: actor,
                message,
                part,
                delta: boundary,
            },
            Event::TextEnd {
                session: actor,
                message,
                part,
            },
            Event::MessageFinished {
                session: actor,
                message,
                role: Role::System,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
            ExtensionEvent::MailSent {
                session: root,
                from,
                to: MailEndpoint::Channel(channel),
                kind,
                body,
            }
            .into_event(),
        ];
        let reducers = projection_reducers();
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            let session = event.session().ok_or_else(|| {
                CoreError::Store(StoreError::BundleClaimData(
                    "revival event has no session".into(),
                ))
            })?;
            let envelope = append_event_in_transaction(&mut tx, session, event).await?;
            if session == root {
                Arc::make_mut(&mut projection).apply_with(&envelope, &reducers);
            }
            envelopes.push(envelope);
        }
        tx.commit().await?;
        Ok((Some((claim, cursor)), envelopes, 1))
    }

    async fn begin_work(
        &self,
        claim: Option<&ActorClaim>,
        root: SessionId,
        handle: &str,
        inbox_through: u64,
        prompt: ResidentWorkPrompt,
        current_task: String,
    ) -> Result<(MessageId, Vec<Envelope>), CoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let session = claim.map_or(root, |claim| claim.actor_id);
        let mut events = Vec::new();
        let (admitted, text) = match prompt {
            ResidentWorkPrompt::Text(text) => (None, text),
            ResidentWorkPrompt::Resume { message, append } => {
                let projection = self
                    .replay_projection_in_transaction(&mut tx, session)
                    .await?;
                let tail = projection
                    .session
                    .messages
                    .iter()
                    .rev()
                    .find(|entry| entry.role != Role::Assistant || !entry.parts.is_empty());
                if tail.is_some_and(|entry| entry.id == message) {
                    if !append.is_empty() {
                        let (part, text) = tail
                            .and_then(|entry| {
                                entry.parts.iter().find_map(|part| match part {
                                    PartProjection::Text { id, text, .. } => Some((*id, text)),
                                    _ => None,
                                })
                            })
                            .ok_or_else(|| {
                                CoreError::Store(StoreError::BundleClaimData(
                                    "admitted resident prompt has no text".into(),
                                ))
                            })?;
                        events.push(Event::TextReplace {
                            session,
                            message,
                            part,
                            text: format!("{text}\n\n{append}"),
                        });
                    }
                    (Some(message), String::new())
                } else {
                    let root_projection =
                        self.replay_projection_in_transaction(&mut tx, root).await?;
                    let team = team_projection(&root_projection);
                    if let Some(work) = team
                        .roster
                        .get(handle)
                        .and_then(|entry| entry.resident_work)
                    {
                        events.push(
                            ExtensionEvent::ResidentWorkFinished {
                                session: root,
                                handle: handle.into(),
                                epoch: work.epoch,
                                inbox_through: work.inbox_through,
                            }
                            .into_event(),
                        );
                    }
                    (None, append)
                }
            }
        };
        let message = if let Some(message) = admitted {
            message
        } else {
            let message = MessageId::new();
            let part = PartId::new();
            events.extend([
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session,
                    message,
                    part,
                },
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: text,
                },
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
            ]);
            message
        };
        if let Some(claim) = claim {
            events.push(
                ExtensionEvent::ResidentWorkStarted {
                    session: root,
                    actor_session: session,
                    handle: handle.into(),
                    epoch: claim.epoch,
                    inbox_through,
                }
                .into_event(),
            );
        } else {
            events.push(
                ExtensionEvent::MailConsumed {
                    session: root,
                    handle: handle.into(),
                    through: inbox_through,
                }
                .into_event(),
            );
        }
        events.push(
            ExtensionEvent::AgentActivityChanged {
                session: root,
                handle: handle.into(),
                status: RosterStatus::Busy,
                current_task: Some(current_task),
            }
            .into_event(),
        );
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            let target = event.session().ok_or_else(|| {
                CoreError::Store(StoreError::BundleClaimData(
                    "wake event has no session".into(),
                ))
            })?;
            envelopes.push(append_event_in_transaction(&mut tx, target, event).await?);
        }
        tx.commit().await?;
        Ok((message, envelopes))
    }

    async fn commit_tool_result_with_mail(
        &self,
        claim: Option<&ActorClaim>,
        session: SessionId,
        events: Vec<Event>,
        acknowledgement: Option<Event>,
    ) -> Result<Vec<Envelope>, CoreError> {
        if claim.is_none() {
            let mut batch: Vec<(SessionId, Event)> =
                events.into_iter().map(|ev| (session, ev)).collect();
            if let Some(ack) = acknowledgement {
                let root = ack.session().ok_or_else(|| {
                    CoreError::Store(StoreError::BundleClaimData(
                        "mail acknowledgement has no session".into(),
                    ))
                })?;
                batch.push((root, ack));
            }
            return Ok(self.append_events_batch(batch).await?);
        }
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let mut envelopes =
            Vec::with_capacity(events.len() + usize::from(acknowledgement.is_some()));
        for event in events {
            envelopes.push(append_event_in_transaction(&mut tx, session, event).await?);
        }
        if let Some(acknowledgement) = acknowledgement {
            let root = acknowledgement.session().ok_or_else(|| {
                CoreError::Store(StoreError::BundleClaimData(
                    "mail acknowledgement has no session".into(),
                ))
            })?;
            envelopes.push(append_event_in_transaction(&mut tx, root, acknowledgement).await?);
        }
        tx.commit().await?;
        Ok(envelopes)
    }

    async fn active_actor_ids(&self) -> Result<Vec<SessionId>, CoreError> {
        let claims = self.list_active_bundle_claims(CLAIM_BUNDLE).await?;
        claims
            .into_iter()
            .map(|c| {
                c.claim_key.parse().map_err(|err| {
                    CoreError::Store(StoreError::BundleClaimData(format!(
                        "invalid actor_id `{}`: {err}",
                        c.claim_key
                    )))
                })
            })
            .collect()
    }

    async fn try_claim_new(
        &self,
        root: SessionId,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<ActorClaim, CoreError> {
        let mut tx = self.begin_transaction().await?;
        let claim = try_claim_new_in_tx(&mut tx, root, actor_id, owner_run_id).await?;
        tx.commit().await?;
        Ok(claim)
    }

    async fn register_episodes(
        &self,
        root: SessionId,
        owner: OwnerRunId,
        parent_claim: Option<&ActorClaim>,
        capacity: u64,
        registrations: Vec<EpisodeRegistration>,
    ) -> Result<Vec<(ActorClaim, Vec<Envelope>)>, CoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = parent_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        check_capacity(&mut tx, root, registrations.len() as u64, capacity).await?;
        let mut projection = self.replay_projection_in_transaction(&mut tx, root).await?;
        let reducers = projection_reducers();
        let mut envelopes = Vec::new();
        if !team_projection(&projection)
            .roster
            .contains_key(scope::ROOT_HANDLE)
        {
            let event = ExtensionEvent::AgentRegistered {
                session: root,
                agent_session: root,
                handle: scope::ROOT_HANDLE.into(),
                parent: None,
                agent_type: projection.session.agent.clone().ok_or_else(|| {
                    CoreError::Store(StoreError::BundleClaimData(
                        "registration root has no agent".into(),
                    ))
                })?,
                mode: SubagentMode::Transient,
            }
            .into_event();
            let envelope = append_event_in_transaction(&mut tx, root, event).await?;
            Arc::make_mut(&mut projection).apply_with(&envelope, &reducers);
            envelopes.push(envelope);
        }
        let mut registered = Vec::with_capacity(registrations.len());
        for registration in registrations {
            let claim = try_claim_new_in_tx(&mut tx, root, registration.actor, owner).await?;
            for mut event in registration.events {
                if let Some(ExtensionEvent::ChannelCreated {
                    session,
                    channel: _,
                    kind: ChannelKind::Group,
                    unit,
                    members,
                }) = ExtensionEvent::from_event(&event)
                {
                    let team = team_projection(&projection);
                    if let Some((existing, _)) = team
                        .channels
                        .iter()
                        .find(|(_, entry)| entry.kind == ChannelKind::Group && entry.unit == unit)
                    {
                        event = ExtensionEvent::ChannelCreated {
                            session,
                            channel: existing.clone(),
                            kind: ChannelKind::Group,
                            unit,
                            members,
                        }
                        .into_event();
                    }
                }
                let session = event.session().ok_or_else(|| {
                    CoreError::Store(StoreError::BundleClaimData(
                        "registration event has no session".into(),
                    ))
                })?;
                let envelope = append_event_in_transaction(&mut tx, session, event).await?;
                if session == root {
                    Arc::make_mut(&mut projection).apply_with(&envelope, &reducers);
                }
                envelopes.push(envelope);
            }
            registered.push((claim, std::mem::take(&mut envelopes)));
        }
        tx.commit().await?;
        Ok(registered)
    }

    async fn live_member_count(&self, root: SessionId) -> Result<u64, CoreError> {
        let claims = self.list_active_bundle_claims(CLAIM_BUNDLE).await?;
        Ok(claims
            .iter()
            .filter(|c| claim_root_id(c) == Some(root))
            .count() as u64)
    }

    async fn recover_claim(
        &self,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<RecoveredActorClaim, CoreError> {
        let (bundle_claim, previous_epoch) = self
            .takeover_bundle_claim(
                CLAIM_BUNDLE,
                &actor_id.to_string(),
                &owner_to_string(owner_run_id),
                owner_run_id,
                None,
                None,
            )
            .await?;
        let claim = bundle_to_claim(&bundle_claim)?;
        Ok(RecoveredActorClaim {
            claim,
            previous_epoch: ActorEpoch::from_storage(previous_epoch),
        })
    }

    async fn validate_actor_claim(&self, claim: &ActorClaim) -> Result<(), CoreError> {
        Ok(self.validate_bundle_claim(&claim_to_bundle(claim)).await?)
    }

    async fn release_claim(&self, claim: &ActorClaim) -> Result<(), CoreError> {
        Ok(self.release_bundle_claim(&claim_to_bundle(claim)).await?)
    }

    async fn commit_resident_mutation(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, CoreError> {
        if events.iter().any(|event| event.session() != Some(session)) {
            return Err(CoreError::Store(StoreError::BundleClaimData(
                "resident mutation event session mismatch".to_string(),
            )));
        }
        let mut tx = self.begin_transaction().await?;
        fence_actor_claim(&mut tx, claim).await?;

        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            envelopes.push(append_event_in_transaction(&mut tx, session, event).await?);
        }
        tx.commit().await?;
        Ok(envelopes)
    }

    async fn append_harness_mail(
        &self,
        root: SessionId,
        handle: &str,
        body: String,
    ) -> Result<Envelope, CoreError> {
        let mut tx = self.begin_transaction().await?;
        let projection = self.replay_projection_in_transaction(&mut tx, root).await?;
        let team = team_projection(&projection);
        let reject = |reason: String| CoreError::MailAuthorizationRejected {
            sender: scope::HARNESS_HANDLE.to_string(),
            channel: format!("direct:{handle}"),
            reason,
        };
        let Some(entry) = team.roster.get(handle) else {
            return Err(reject(format!("unknown mail target `{handle}`")));
        };
        if entry.session == root
            || entry.mode != SubagentMode::Resident
            || !resident_member_is_eligible(&mut tx, entry).await?
        {
            return Err(reject(format!(
                "mail target `{handle}` is not a live resident member"
            )));
        }
        let event = ExtensionEvent::MailSent {
            session: root,
            from: scope::HARNESS_HANDLE.to_string(),
            to: MailEndpoint::Handle(handle.to_string()),
            kind: MailKind::Message,
            body,
        }
        .into_event();
        let envelope = append_event_in_transaction(&mut tx, root, event).await?;
        tx.commit().await?;
        Ok(envelope)
    }

    async fn append_channel_mail(
        &self,
        root: SessionId,
        claim: Option<&ActorClaim>,
        from: String,
        channel: String,
        kind: MailKind,
        body: String,
        policy: Option<ChannelPolicySnapshot>,
    ) -> Result<(Envelope, usize), CoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let projection = self.replay_projection_in_transaction(&mut tx, root).await?;
        let (envelope, recipients) = append_resolved_channel_mail(
            &mut tx,
            root,
            from,
            channel,
            kind,
            body,
            &projection,
            policy,
        )
        .await?;
        tx.commit().await?;
        Ok((envelope, recipients))
    }

    async fn recover_resident_actor(
        &self,
        recovered: &RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<RecoveredResidentOutcome, CoreError> {
        self.warm_projection(root).await?;
        self.warm_projection(recovered.claim.actor_id).await?;
        let mut tx = self.begin_transaction().await?;
        fence_actor_claim(&mut tx, &recovered.claim).await?;

        let projection = self.replay_projection_in_transaction(&mut tx, root).await?;
        let team = team_projection(&projection);
        let Some(entry) = team.roster.get(handle) else {
            return Err(CoreError::Store(StoreError::BundleClaimData(
                "resident recovery roster mismatch".to_string(),
            )));
        };
        if entry.session != recovered.claim.actor_id || !entry.mode.is_resident() {
            return Err(CoreError::Store(StoreError::BundleClaimData(
                "resident recovery roster mismatch".to_string(),
            )));
        }

        let recovery_reason = "aborted by resident recovery";
        let mut envelopes = Vec::new();
        let work = if let Some(resident_work) = entry.resident_work {
            if resident_work.epoch > recovered.previous_epoch {
                return Err(CoreError::Store(StoreError::BundleClaimData(
                    "resident recovery work epoch mismatch".to_string(),
                )));
            }
            if entry.status == RosterStatus::Idle {
                RecoveredResidentWork::Queued {
                    inbox_cursor: entry.resident_cursor,
                }
            } else {
                let actor = self
                    .replay_projection_in_transaction(&mut tx, recovered.claim.actor_id)
                    .await?;
                let actor_id = recovered.claim.actor_id;
                let mut events = resident_effect_terminal_events(actor_id, &actor, recovery_reason);
                let tail = actor.session.messages.iter().rev().find(|message| {
                    message.parts.iter().any(|part| match part {
                        PartProjection::Text { text, .. } => !text.is_empty(),
                        PartProjection::Reasoning { .. } => false,
                        PartProjection::Tool { .. } => true,
                    })
                });
                if tail.is_some_and(|message| message.role == Role::User) {
                    let message = MessageId::new();
                    let part = PartId::new();
                    events.extend([
                        Event::MessageStarted {
                            session: actor_id,
                            message,
                            role: Role::Assistant,
                            agent: None,
                            model: None,
                        },
                        Event::TextStart {
                            session: actor_id,
                            message,
                            part,
                        },
                        Event::TextDelta {
                            session: actor_id,
                            message,
                            part,
                            delta: "Interrupted before this work completed.".into(),
                        },
                        Event::TextEnd {
                            session: actor_id,
                            message,
                            part,
                        },
                        Event::MessageFinished {
                            session: actor_id,
                            message,
                            role: Role::Assistant,
                            finish: FinishReason::Cancelled,
                            tokens: None,
                            cause: Some(FinishCause::Interrupted),
                        },
                    ]);
                }
                for event in events {
                    envelopes.push(append_event_in_transaction(&mut tx, actor_id, event).await?);
                }
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        root,
                        ExtensionEvent::AgentActivityChanged {
                            session: root,
                            handle: handle.to_string(),
                            status: RosterStatus::Failed,
                            current_task: Some(recovery_reason.to_string()),
                        }
                        .into_event(),
                    )
                    .await?,
                );
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        root,
                        ExtensionEvent::ResidentWorkFinished {
                            session: root,
                            handle: handle.into(),
                            epoch: resident_work.epoch,
                            inbox_through: resident_work.inbox_through,
                        }
                        .into_event(),
                    )
                    .await?,
                );
                RecoveredResidentWork::AbortedRunning {
                    inbox_cursor: resident_work.inbox_through,
                    queued_after: team
                        .unread_incoming_mail(handle, resident_work.inbox_through)
                        .next()
                        .is_some(),
                }
            }
        } else if team
            .unread_incoming_mail(handle, entry.resident_cursor)
            .next()
            .is_some()
        {
            RecoveredResidentWork::Queued {
                inbox_cursor: entry.resident_cursor,
            }
        } else {
            RecoveredResidentWork::Idle
        };

        tx.commit().await?;
        Ok(RecoveredResidentOutcome { work, envelopes })
    }

    async fn finalize_resident_archive(
        &self,
        owner: ResidentArchiveOwner<'_>,
        root: SessionId,
        handle: &str,
        archive: ResidentArchive,
    ) -> Result<(ResidentArchiveOutcome, Vec<Envelope>), CoreError> {
        let actor_id = archive.actor;
        let reason = archive.note.as_str();
        let member_status = archive.member_status;
        let mut tx = self.begin_transaction().await?;
        let owned_claim;
        let claim = match owner {
            ResidentArchiveOwner::Claim(claim) => claim,
            ResidentArchiveOwner::Takeover => {
                let owner_run_id = self.current_runtime_owner()?;
                self.require_runtime_owner(owner_run_id)?;
                if archive.check_gate {
                    return Err(CoreError::Store(StoreError::BundleClaimData(
                        "voluntary report requires an episode claim".into(),
                    )));
                }
                match SessionStore::takeover_bundle_claim_in_tx_at(
                    &mut tx,
                    CLAIM_BUNDLE,
                    &actor_id.to_string(),
                    &owner_to_string(owner_run_id),
                    None,
                    None,
                    now_millis(),
                )
                .await
                {
                    Ok((bundle_claim, _)) => {
                        owned_claim = bundle_to_claim(&bundle_claim)?;
                        &owned_claim
                    }
                    Err(StoreError::BundleClaimUnavailable { .. }) => {
                        let projection =
                            self.replay_projection_in_transaction(&mut tx, root).await?;
                        if team_projection(&projection)
                            .archived
                            .get(handle)
                            .is_some_and(|entry| entry.session == actor_id)
                        {
                            tx.commit().await?;
                            return Ok((ResidentArchiveOutcome::Committed, Vec::new()));
                        }
                        return Err(CoreError::Store(StoreError::BundleClaimUnavailable {
                            bundle: CLAIM_BUNDLE.to_owned(),
                            claim_key: actor_id.to_string(),
                        }));
                    }
                    Err(error) => return Err(CoreError::Store(error)),
                }
            }
        };
        if let Err(error) = fence_actor_claim(&mut tx, claim).await {
            if matches!(error, StoreError::StaleBundleClaim { .. })
                && released_resident_matches(self, &mut tx, claim, root, handle).await?
            {
                tx.commit().await?;
                return Ok((ResidentArchiveOutcome::Committed, Vec::new()));
            }
            return Err(CoreError::Store(error));
        }

        let projection = self.replay_projection_in_transaction(&mut tx, root).await?;
        let team = team_projection(&projection);
        let Some(entry) = team.roster.get(handle) else {
            return Err(CoreError::Store(StoreError::BundleClaimData(
                "resident stop handle is not registered".to_string(),
            )));
        };
        if actor_id != claim.actor_id
            || entry.session != claim.actor_id
            || entry.mode != SubagentMode::Resident
        {
            return Err(CoreError::Store(StoreError::BundleClaimData(
                "resident stop handle does not match actor claim".to_string(),
            )));
        }

        let actor = self
            .replay_projection_in_transaction(&mut tx, claim.actor_id)
            .await?;
        let generation =
            handoff_projection(&actor).map_or(1, |handoff| handoff.generation.saturating_add(1));
        let mut refusal = if archive
            .expected_work
            .is_some_and(|work| work != (entry.resident_cursor, entry.resident_work))
            || archive.generation != generation
        {
            Some("your episode changed during handoff".to_string())
        } else {
            None
        };
        if archive.check_gate && refusal.is_none() {
            let read_through = entry.resident_work.map_or(entry.resident_cursor, |work| {
                entry.resident_cursor.max(work.inbox_through)
            });
            let unread: Vec<_> = team.unread_incoming_mail(handle, read_through).collect();
            if !unread.is_empty() {
                let channels: BTreeSet<_> = unread
                    .iter()
                    .map(|mail| match &mail.to {
                        MailEndpoint::Channel(channel) => format!("#{channel}"),
                        MailEndpoint::Handle(_) => format!("your inbox (`{handle}`)"),
                    })
                    .collect();
                refusal = Some(format!(
                    "{} unread mail on {}",
                    unread.len(),
                    channels.into_iter().collect::<Vec<_>>().join(", ")
                ));
            } else if let Some(child) = team
                .roster
                .keys()
                .find(|path| scope::parent_path(path) == Some(handle))
            {
                refusal = Some(format!(
                    "live subagent `{child}`; answer or archive it first"
                ));
            }
        }
        if let Some(reason) = refusal {
            if !archive.check_gate {
                return Err(CoreError::MailboxRejected(reason));
            }
            let notice = append_event_in_transaction(
                &mut tx,
                root,
                ExtensionEvent::MailSent {
                    session: root,
                    from: scope::HARNESS_HANDLE.into(),
                    to: MailEndpoint::Handle(handle.into()),
                    kind: MailKind::Message,
                    body: format!(
                        "Your report was not accepted: {reason}. Read and answer it, then call report again."
                    ),
                }
                .into_event(),
            )
            .await?;
            tx.commit().await?;
            return Ok((ResidentArchiveOutcome::Refused, vec![notice]));
        }

        let mut envelopes =
            append_resident_effects_in_transaction(self, &mut tx, claim.actor_id, reason).await?;
        if archive.report.is_none() {
            envelopes.extend(
                close_parent_member_row_in_transaction(
                    self,
                    &mut tx,
                    claim.actor_id,
                    member_status,
                    reason,
                )
                .await?,
            );
        }

        let root_envelope = append_event_in_transaction(
            &mut tx,
            root,
            ExtensionEvent::AgentActivityChanged {
                session: root,
                handle: handle.to_string(),
                status: match member_status {
                    MemberRunStatus::Done => RosterStatus::Done,
                    MemberRunStatus::Spawning
                    | MemberRunStatus::Running
                    | MemberRunStatus::Failed
                    | MemberRunStatus::Cancelled => RosterStatus::Failed,
                },
                current_task: Some(reason.to_string()),
            }
            .into_event(),
        )
        .await?;

        SessionStore::release_bundle_claim_in_tx_at(&mut tx, &claim_to_bundle(claim), now_millis())
            .await?;

        envelopes.push(root_envelope);
        if claim.actor_id != root {
            envelopes.push(
                append_event_in_transaction(
                    &mut tx,
                    claim.actor_id,
                    ExtensionEvent::HandoffCommitted {
                        session: claim.actor_id,
                        handle: handle.into(),
                        generation: archive.generation,
                        doc: archive.doc,
                        degraded: archive.degraded,
                    }
                    .into_event(),
                )
                .await?,
            );
            if let Some((outcome, report)) = archive.report {
                let parent_path = scope::parent_path(handle).unwrap_or(scope::ROOT_HANDLE);
                let parent = team
                    .roster
                    .get(parent_path)
                    .map_or(root, |entry| entry.session);
                let parent_projection = self
                    .replay_projection_in_transaction(&mut tx, parent)
                    .await?;
                let member = members_projection(&parent_projection)
                    .iter()
                    .find(|row| row.child == Some(claim.actor_id))
                    .map(|row| row.member)
                    .ok_or_else(|| {
                        CoreError::Store(StoreError::BundleClaimData(
                            "reporting member row missing on parent".into(),
                        ))
                    })?;
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        parent,
                        ExtensionEvent::SubagentReported {
                            session: parent,
                            member,
                            child: claim.actor_id,
                            handle: handle.into(),
                            outcome,
                            report: report.clone(),
                        }
                        .into_event(),
                    )
                    .await?,
                );
                let target = team
                    .channels
                    .iter()
                    .find(|(_, channel)| {
                        channel.kind == ChannelKind::Dm
                            && channel.members.contains(handle)
                            && channel.members.contains(parent_path)
                    })
                    .map(|(key, _)| MailEndpoint::Channel(key.clone()))
                    .ok_or_else(|| {
                        CoreError::Store(StoreError::BundleClaimData(
                            "reporting member has no parent DM channel".into(),
                        ))
                    })?;
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        root,
                        ExtensionEvent::MailSent {
                            session: root,
                            from: handle.into(),
                            to: target,
                            kind: MailKind::Message,
                            body: report,
                        }
                        .into_event(),
                    )
                    .await?,
                );
            }
            envelopes.push(
                append_event_in_transaction(
                    &mut tx,
                    root,
                    ExtensionEvent::AgentArchived {
                        session: root,
                        handle: handle.into(),
                        child: claim.actor_id,
                        reason: archive.reason,
                    }
                    .into_event(),
                )
                .await?,
            );
        }
        tx.commit().await?;
        Ok((ResidentArchiveOutcome::Committed, envelopes))
    }

    async fn admit_workflow_run(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        event: Event,
    ) -> Result<WorkflowAdmissionOutcome, CoreError> {
        let (run, request_hash) = match ExtensionEvent::from_event(&event) {
            Some(ExtensionEvent::WorkflowRunStarted {
                session: event_session,
                run,
                request_hash,
                ..
            }) if event_session == session => (run, request_hash),
            _ => {
                return Err(CoreError::Invalid(
                    "run admission requires a matching WorkflowRunStarted event".to_string(),
                ));
            }
        };
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let events = SessionStore::replay_in_transaction(&mut tx, session).await?;
        if let Some(existing_hash) =
            events.iter().find_map(
                |envelope| match ExtensionEvent::from_event(&envelope.event) {
                    Some(ExtensionEvent::WorkflowRunStarted {
                        run: existing,
                        request_hash,
                        ..
                    }) if existing == run => Some(request_hash),
                    _ => None,
                },
            )
        {
            tx.commit().await?;
            return Ok(if existing_hash == request_hash {
                WorkflowAdmissionOutcome::Existing
            } else {
                WorkflowAdmissionOutcome::Conflict
            });
        }
        let projection = Projection::from_events_with(&events, &projection_reducers());
        if let Some(active) = workflow_projection(&projection).and_then(|wf| wf.run)
            && active.status == WorkflowRunStatus::Running
        {
            let run = active.id;
            tx.commit().await?;
            return Ok(WorkflowAdmissionOutcome::Busy { run });
        }
        let envelope = append_event_in_transaction(&mut tx, session, event).await?;
        tx.commit().await?;
        Ok(WorkflowAdmissionOutcome::Admitted(Box::new(envelope)))
    }

    async fn select_workflow(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        event: Event,
    ) -> Result<WorkflowSelectionOutcome, CoreError> {
        if !matches!(
            ExtensionEvent::from_event(&event),
            Some(ExtensionEvent::WorkflowSelected { session: event_session, .. }) if event_session == session
        ) {
            return Err(CoreError::Invalid(
                "selection requires a matching WorkflowSelected event".to_string(),
            ));
        }
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let projection = self
            .replay_projection_in_transaction(&mut tx, session)
            .await?;
        if let Some(active) = workflow_projection(&projection).and_then(|wf| wf.run)
            && active.status == WorkflowRunStatus::Running
        {
            let run = active.id;
            tx.commit().await?;
            return Ok(WorkflowSelectionOutcome::Busy { run });
        }
        let envelope = append_event_in_transaction(&mut tx, session, event).await?;
        tx.commit().await?;
        Ok(WorkflowSelectionOutcome::Selected(Box::new(envelope)))
    }

    async fn recover_nonterminal_workflows(
        &self,
        current_owner: OwnerRunId,
        reason: &str,
    ) -> Result<Vec<Envelope>, CoreError> {
        self.require_runtime_owner(current_owner)?;
        let mut tx = self.begin_transaction().await?;
        let sessions = SessionStore::find_sessions_with_extension_event(
            &mut tx,
            BUNDLE_WORKFLOW,
            "workflow_run_started",
        )
        .await?;
        let mut recovered = Vec::new();
        for session in sessions {
            let projection = self
                .replay_projection_in_transaction(&mut tx, session)
                .await?;
            let Some(run) = workflow_projection(&projection).and_then(|wf| wf.run) else {
                continue;
            };
            if run.status != WorkflowRunStatus::Running || run.owner == current_owner {
                continue;
            }
            recovered.push(
                append_event_in_transaction(
                    &mut tx,
                    session,
                    ExtensionEvent::WorkflowRunFinished {
                        session,
                        run: run.id,
                        status: WorkflowRunStatus::Interrupted,
                        error: Some(reason.chars().take(2_048).collect()),
                    }
                    .into_event(),
                )
                .await?,
            );
        }
        tx.commit().await?;
        Ok(recovered)
    }

    async fn take_restart_resume_with_subagents(
        &self,
        session: SessionId,
        owner: OwnerRunId,
    ) -> Result<Option<(MessageId, Vec<Envelope>)>, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        let team = team_projection(&projection);
        let candidates = team
            .roster
            .values()
            .map(|entry| (entry.handle.clone(), entry.session))
            .chain(
                team.archived
                    .values()
                    .filter(|entry| entry.reason == ArchiveReason::Shutdown)
                    .map(|entry| (entry.handle.clone(), entry.session)),
            )
            .collect::<Vec<_>>();
        drop(projection);

        let mut children = Vec::new();
        for (handle, child) in candidates {
            if child == session {
                continue;
            }
            let child_proj = self.read_projection_shared(child).await?;
            if child_proj
                .session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == Role::Assistant)
                .is_some_and(|message| {
                    message.finish == Some(FinishReason::Cancelled)
                        && message.cause == Some(FinishCause::Shutdown)
                })
            {
                children.push(handle);
            }
        }
        let suffix = (!children.is_empty()).then(|| {
            format!(
                "\n\nInterrupted subagents: {}. Their recovery is managed by the supervisor.",
                children.join(", ")
            )
        });
        Ok(self
            .take_restart_resume_with_suffix(session, owner, suffix.as_deref())
            .await?)
    }
}
