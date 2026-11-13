use hya_proto::{
    ActorClaim, ActorEpoch, Envelope, Event, FinishReason, MessageId, OwnerRunId, PartId,
    PartProjection, Role, RosterStatus, SessionId,
};
use sqlx::Row;
use uuid::Uuid;

use crate::{SessionStore, StoreError, decode_session_key};

/// Result of recovering (taking over) a resident actor claim after restart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveredActorClaim {
    /// New active claim after bumping the epoch for this owner run.
    pub claim: ActorClaim,
    /// Epoch that was active before takeover (for classifying interrupted work).
    pub previous_epoch: ActorEpoch,
}

/// A wake either admits new text or owns the directive already committed at registration.
#[derive(Debug)]
pub enum ResidentWorkPrompt {
    /// Admit one new coalesced user message.
    Text(String),
    /// First wake of a fresh episode, with no prior work or new text to append.
    /// The registered directive is still the transcript tail by construction.
    Fresh(MessageId),
    /// Reuse the initial directive committed at registration.
    Admitted {
        /// The registered user message owned by this turn.
        message: MessageId,
        /// Coalesced new mail, appended to that directive when nonempty.
        append: String,
    },
}

impl SessionStore {
    /// Reactivate an archived member, cut its context and append its first mail
    /// in one writer transaction. Authorization is rechecked here, including
    /// when another sender already revived the member. In that case only mail
    /// is appended and the returned claim is None.
    #[allow(clippy::too_many_arguments)]
    pub async fn revive_episode(
        &self,
        root: SessionId,
        owner: OwnerRunId,
        capacity: u64,
        parent_claim: Option<&ActorClaim>,
        actor: SessionId,
        from: String,
        to: hya_proto::MailEndpoint,
        kind: hya_proto::MailKind,
        body: String,
        context_marker: &str,
        policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
    ) -> Result<(Option<(ActorClaim, u64)>, Vec<Envelope>), StoreError> {
        use hya_proto::{MailEndpoint, SubagentMode, scope};
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = parent_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let mut projection = crate::replay_projection(&self.projections, &mut tx, root).await?;
        // Use the ordinary append's exact authorization and liveness rules.
        // MailRevivalRequired is returned before any append.
        let append = match &to {
            MailEndpoint::Handle(address) => {
                crate::mailbox::append_direct_mail_in_transaction(
                    &projection,
                    &mut tx,
                    root,
                    from.clone(),
                    address.clone(),
                    kind,
                    body.clone(),
                    policy,
                )
                .await
            }
            MailEndpoint::Channel(channel) => {
                let key = projection
                    .team
                    .resolve_channel(&from, channel)
                    .map_err(|error| StoreError::MailAuthorizationRejected {
                        sender: from.clone(),
                        channel: channel.clone(),
                        reason: error.to_string(),
                    })?;
                crate::mailbox::append_resolved_channel_mail(
                    &mut tx,
                    root,
                    from.clone(),
                    key,
                    kind,
                    body.clone(),
                    &projection,
                    policy,
                )
                .await
                .map(|(envelope, _)| envelope)
            }
        };
        let target = match append {
            Ok(envelope) => {
                tx.commit().await?;
                return Ok((None, vec![envelope]));
            }
            Err(StoreError::MailRevivalRequired { target, .. }) => target,
            Err(error) => return Err(error),
        };
        let archived = &projection.team.archived[&target];
        if archived.session != actor {
            return Err(StoreError::ActorClaimData(
                "revive actor changed during preparation".into(),
            ));
        }
        let agent_type = archived.agent_type.clone();
        let channel = projection
            .team
            .channels
            .iter()
            .find(|(_, state)| {
                state.kind == hya_proto::ChannelKind::Dm
                    && state.members.contains(&from)
                    && state.members.contains(&target)
            })
            .map(|(key, _)| key.clone())
            .ok_or_else(|| StoreError::MailAuthorizationRejected {
                sender: from.clone(),
                channel: target.clone(),
                reason: "archived member has no parent DM channel".into(),
            })?;
        let actor_projection = crate::replay_projection(&self.projections, &mut tx, actor).await?;
        let handoff =
            actor_projection.session.handoff.as_ref().ok_or_else(|| {
                StoreError::ActorClaimData("archived resident has no handoff".into())
            })?;
        let boundary = format!(
            "{context_marker}\nPRIOR HANDOFF (generation {})\n{}",
            handoff.generation, handoff.doc
        );
        let cursor = projection
            .team
            .inboxes
            .get(&target)
            .map_or(0, |inbox| inbox.len() as u64);
        check_capacity(&mut tx, root, 1, capacity).await?;
        let claim = Self::try_claim_new_in_transaction(&mut tx, root, actor, owner).await?;
        let message = MessageId::new();
        let part = PartId::new();
        let events = [
            Event::AgentRegistered {
                session: root,
                agent_session: actor,
                handle: scope::leaf(&target).into(),
                parent: Some(from.clone()),
                agent_type,
                mode: SubagentMode::Resident,
            },
            Event::AgentRestarted {
                session: root,
                handle: target,
                child: actor,
                epoch: claim.epoch,
            },
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
        ];
        let mut envelopes = Vec::new();
        for event in events {
            let session = event
                .session()
                .ok_or_else(|| StoreError::ActorClaimData("revive event has no session".into()))?;
            let envelope = crate::append_event_in_transaction(&mut tx, session, event).await?;
            if session == root {
                projection.apply(&envelope);
            }
            envelopes.push(envelope);
        }
        let (mail, _) = crate::mailbox::append_resolved_channel_mail(
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
        envelopes.push(mail);
        tx.commit().await?;
        Ok((Some((claim, cursor)), envelopes))
    }

    /// Deliver one coalesced wake and record its work boundary under one writer
    /// reservation. Hooks must finish before calling; publish only after commit.
    /// A claim-less wake belongs to the root lead.
    pub async fn begin_work(
        &self,
        claim: Option<&ActorClaim>,
        root: SessionId,
        handle: &str,
        inbox_through: u64,
        prompt: ResidentWorkPrompt,
        current_task: String,
    ) -> Result<(MessageId, Vec<Envelope>), StoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let session = claim.map_or(root, |claim| claim.actor_id);
        let mut events = Vec::new();
        let (admitted, text) = match prompt {
            ResidentWorkPrompt::Text(text) => (None, text),
            ResidentWorkPrompt::Fresh(message) => (Some(message), String::new()),
            ResidentWorkPrompt::Admitted { message, append } => {
                let projection =
                    crate::replay_projection(&self.projections, &mut tx, session).await?;
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
                                StoreError::ActorClaimData(
                                    "admitted resident prompt has no text".into(),
                                )
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
                    // Answered prompts are immutable. Settle the previous
                    // boundary before recording the replacement wake below.
                    let root_projection =
                        crate::replay_projection(&self.projections, &mut tx, root).await?;
                    if let Some(work) = root_projection
                        .team
                        .roster
                        .get(handle)
                        .and_then(|entry| entry.resident_work)
                    {
                        events.push(Event::ResidentWorkFinished {
                            session: root,
                            handle: handle.into(),
                            epoch: work.epoch,
                            inbox_through: work.inbox_through,
                        });
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
            events.push(Event::ResidentWorkStarted {
                session: root,
                actor_session: session,
                handle: handle.into(),
                epoch: claim.epoch,
                inbox_through,
            });
        } else {
            events.push(Event::MailConsumed {
                session: root,
                handle: handle.into(),
                through: inbox_through,
            });
        }
        events.push(Event::AgentActivityChanged {
            session: root,
            handle: handle.into(),
            status: RosterStatus::Busy,
            current_task: Some(current_task),
        });
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            let target = event
                .session()
                .ok_or_else(|| StoreError::ActorClaimData("wake event has no session".into()))?;
            envelopes.push(crate::append_event_in_transaction(&mut tx, target, event).await?);
        }
        tx.commit().await?;
        Ok((message, envelopes))
    }

    /// Commit a tool outcome and its optional team-mail acknowledgement together.
    /// The actor capability fences both logs; publish envelopes only after success.
    pub async fn commit_tool_result_with_mail(
        &self,
        claim: Option<&ActorClaim>,
        session: SessionId,
        result: Event,
        acknowledgement: Option<Event>,
    ) -> Result<Vec<Envelope>, StoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        let mut envelopes =
            vec![crate::append_event_in_transaction(&mut tx, session, result).await?];
        if let Some(acknowledgement) = acknowledgement {
            let root = acknowledgement.session().ok_or_else(|| {
                StoreError::ActorClaimData("mail acknowledgement has no session".into())
            })?;
            envelopes
                .push(crate::append_event_in_transaction(&mut tx, root, acknowledgement).await?);
        }
        tx.commit().await?;
        Ok(envelopes)
    }

    /// Session ids of resident actors currently in the `active` claim state.
    pub async fn active_actor_ids(&self) -> Result<Vec<SessionId>, StoreError> {
        let rows = sqlx::query(
            "SELECT actor_id FROM resident_actor_claim WHERE state = 'active' ORDER BY actor_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let actor_id: Vec<u8> = row.try_get("actor_id")?;
                decode_session_key(&actor_id)
            })
            .collect()
    }

    /// Claim a never-claimed or explicitly released actor.
    ///
    /// The upsert predicate is the concurrency gate: an active row cannot be
    /// overwritten, so simultaneous ordinary claims have exactly one winner.
    pub async fn try_claim_new(
        &self,
        root: SessionId,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<ActorClaim, StoreError> {
        let mut tx = self.begin_transaction().await?;
        let claim =
            Self::try_claim_new_in_transaction(&mut tx, root, actor_id, owner_run_id).await?;
        tx.commit().await?;
        Ok(claim)
    }

    /// Claim an actor as part of the caller's registration transaction.
    pub async fn try_claim_new_in_transaction(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        root: SessionId,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<ActorClaim, StoreError> {
        let row = sqlx::query(
            "INSERT INTO resident_actor_claim (actor_id, root_session_id, epoch, owner_run_id, state) \
             VALUES (?, ?, 1, ?, 'active') \
             ON CONFLICT(actor_id) DO UPDATE SET \
                 epoch = resident_actor_claim.epoch + 1, \
                 root_session_id = excluded.root_session_id, \
                 owner_run_id = excluded.owner_run_id, \
                 state = 'active' \
             WHERE resident_actor_claim.state = 'released' \
               AND resident_actor_claim.epoch < 9223372036854775807 \
             RETURNING actor_id, epoch, owner_run_id",
        )
        .bind(actor_id.storage_key())
        .bind(root.storage_key())
        .bind(owner_run_id.as_uuid().as_bytes().as_slice())
        .fetch_optional(&mut **tx)
        .await?;

        match row {
            Some(row) => decode_claim(row),
            None => Err(StoreError::ActorAlreadyClaimed { actor_id }),
        }
    }

    /// Register a complete batch and lazily register its root in one writer
    /// transaction. A failed fence, capacity check or write leaves no member.
    /// Returned envelopes are published only after commit.
    pub async fn register_episodes(
        &self,
        root: SessionId,
        owner: OwnerRunId,
        parent_claim: Option<&ActorClaim>,
        capacity: u64,
        registrations: Vec<EpisodeRegistration>,
    ) -> Result<Vec<(ActorClaim, Vec<Envelope>)>, StoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = parent_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }
        check_capacity(&mut tx, root, registrations.len() as u64, capacity).await?;
        let mut projection = crate::replay_projection(&self.projections, &mut tx, root).await?;
        let mut envelopes = Vec::new();
        if !projection.team.roster.contains_key(hya_proto::ROOT_HANDLE) {
            let event = Event::AgentRegistered {
                session: root,
                agent_session: root,
                handle: hya_proto::ROOT_HANDLE.into(),
                parent: None,
                agent_type: projection.session.agent.clone().ok_or_else(|| {
                    StoreError::ActorClaimData("registration root has no agent".into())
                })?,
                mode: hya_proto::SubagentMode::Transient,
            };
            let envelope = crate::append_event_in_transaction(&mut tx, root, event).await?;
            projection.apply(&envelope);
            envelopes.push(envelope);
        }
        let mut registered = Vec::with_capacity(registrations.len());
        for registration in registrations {
            let claim =
                Self::try_claim_new_in_transaction(&mut tx, root, registration.actor, owner)
                    .await?;
            for mut event in registration.events {
                if let Event::ChannelCreated {
                    channel,
                    kind: hya_proto::ChannelKind::Group,
                    unit,
                    ..
                } = &mut event
                    && let Some((existing, _)) =
                        projection.team.channels.iter().find(|(_, entry)| {
                            entry.kind == hya_proto::ChannelKind::Group && entry.unit == *unit
                        })
                {
                    channel.clone_from(existing);
                }
                let session = event.session().ok_or_else(|| {
                    StoreError::ActorClaimData("registration event has no session".into())
                })?;
                let envelope = crate::append_event_in_transaction(&mut tx, session, event).await?;
                if session == root {
                    projection.apply(&envelope);
                }
                envelopes.push(envelope);
            }
            registered.push((claim, std::mem::take(&mut envelopes)));
        }
        tx.commit().await?;
        Ok(registered)
    }

    /// Current capacity leases, including idle and recovered members.
    pub async fn live_member_count(&self, root: SessionId) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resident_actor_claim WHERE root_session_id = ? AND state = 'active'",
        ).bind(root.storage_key()).fetch_one(&self.pool).await?;
        Ok(count as u64)
    }

    /// Take over one active actor before runtime readiness.
    ///
    /// The single conditional update is the fence linearization point. The
    /// returned old epoch is derived from the successfully returned `N + 1`, so
    /// no read-before-write race exists.
    pub async fn recover_claim(
        &self,
        actor_id: SessionId,
        owner_run_id: OwnerRunId,
    ) -> Result<RecoveredActorClaim, StoreError> {
        let row = sqlx::query(
            "UPDATE resident_actor_claim SET \
                 epoch = epoch + 1, owner_run_id = ? \
             WHERE actor_id = ? AND state = 'active' \
               AND epoch < 9223372036854775807 \
             RETURNING actor_id, epoch, owner_run_id",
        )
        .bind(owner_run_id.as_uuid().as_bytes().as_slice())
        .bind(actor_id.storage_key())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Err(StoreError::ActorClaimUnavailable { actor_id });
        };
        let claim = decode_claim(row)?;
        let previous_epoch = ActorEpoch::from_storage(claim.epoch.get() - 1);
        Ok(RecoveredActorClaim {
            claim,
            previous_epoch,
        })
    }

    /// Point-check an execution capability against the current durable owner.
    pub async fn validate_actor_claim(&self, claim: &ActorClaim) -> Result<(), StoreError> {
        let current = sqlx::query(
            "SELECT actor_id, epoch, owner_run_id \
             FROM resident_actor_claim \
             WHERE actor_id = ? AND state = 'active'",
        )
        .bind(claim.actor_id.storage_key())
        .fetch_optional(&self.pool)
        .await?
        .map(decode_claim)
        .transpose()?;
        if current.as_ref() == Some(claim) {
            Ok(())
        } else {
            Err(StoreError::StaleActorClaim {
                actor_id: claim.actor_id,
            })
        }
    }

    /// Release only the exact current capability and its capacity lease.
    /// Repeating a matching release is idempotent; a newer owner remains fenced.
    pub async fn release_claim(&self, claim: &ActorClaim) -> Result<(), StoreError> {
        let epoch = i64::try_from(claim.epoch.get()).map_err(|_| {
            StoreError::ActorClaimData("actor epoch exceeds SQLite INTEGER range".to_string())
        })?;
        let mut tx = self.begin_transaction().await?;
        let current = sqlx::query(
            "UPDATE resident_actor_claim SET state = state \
             WHERE actor_id = ? AND epoch = ? AND owner_run_id = ? AND state = 'active'",
        )
        .bind(claim.actor_id.storage_key())
        .bind(epoch)
        .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
        if current.rows_affected() == 0 {
            let state: Option<String> = sqlx::query_scalar(
                "SELECT state FROM resident_actor_claim \
                 WHERE actor_id = ? AND epoch = ? AND owner_run_id = ?",
            )
            .bind(claim.actor_id.storage_key())
            .bind(epoch)
            .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
            .fetch_optional(&mut *tx)
            .await?;
            if state.as_deref() == Some("released") {
                tx.commit().await?;
                return Ok(());
            }
            tx.rollback().await?;
            return Err(StoreError::StaleActorClaim {
                actor_id: claim.actor_id,
            });
        }

        sqlx::query(
            "UPDATE resident_actor_claim SET state = 'released' \
             WHERE actor_id = ? AND epoch = ? AND owner_run_id = ? AND state = 'active'",
        )
        .bind(claim.actor_id.storage_key())
        .bind(epoch)
        .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Atomically fence and append one resident-owned canonical mutation.
    ///
    /// The no-op conditional `UPDATE` acquires SQLite's writer lock and validates
    /// the full capability tuple in the same transaction as the event append.
    /// The caller publishes the returned envelopes only after this transaction
    /// commits.
    pub async fn commit_resident_mutation(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, StoreError> {
        if events.iter().any(|event| event.session() != Some(session)) {
            return Err(StoreError::ActorClaimData(
                "resident mutation event session mismatch".to_string(),
            ));
        }

        let mut tx = self.begin_transaction().await?;
        fence_actor_claim(&mut tx, claim).await?;

        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            envelopes.push(crate::append_event_in_transaction(&mut tx, session, event).await?);
        }
        tx.commit().await?;
        Ok(envelopes)
    }
}

pub(crate) async fn fence_actor_claim(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claim: &ActorClaim,
) -> Result<(), StoreError> {
    let current = sqlx::query(
        "UPDATE resident_actor_claim SET state = state \
         WHERE actor_id = ? AND epoch = ? AND owner_run_id = ? AND state = 'active' \
         RETURNING actor_id",
    )
    .bind(claim.actor_id.storage_key())
    .bind(i64::try_from(claim.epoch.get()).map_err(|_| {
        StoreError::ActorClaimData("actor epoch exceeds SQLite INTEGER range".to_string())
    })?)
    .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
    .fetch_optional(&mut **tx)
    .await?;
    if current.is_some() {
        Ok(())
    } else {
        Err(StoreError::StaleActorClaim {
            actor_id: claim.actor_id,
        })
    }
}

fn decode_claim(row: sqlx::sqlite::SqliteRow) -> Result<ActorClaim, StoreError> {
    let actor_id: Vec<u8> = row.try_get("actor_id")?;
    let epoch: i64 = row.try_get("epoch")?;
    let owner_run_id: Vec<u8> = row.try_get("owner_run_id")?;
    let actor_id = decode_session_key(&actor_id)?;
    let epoch = u64::try_from(epoch)
        .ok()
        .filter(|value| *value > 0)
        .map(ActorEpoch::from_storage)
        .ok_or_else(|| StoreError::ActorClaimData("invalid actor epoch".to_string()))?;
    let owner_run_id = Uuid::from_slice(&owner_run_id)
        .map(OwnerRunId::from_storage)
        .map_err(|error| StoreError::ActorClaimData(format!("invalid owner run id: {error}")))?;
    Ok(ActorClaim {
        actor_id,
        epoch,
        owner_run_id,
    })
}

/// Prepared canonical events for one new episode in a registration batch.
pub struct EpisodeRegistration {
    /// Member session whose active claim is the capacity lease.
    pub actor: SessionId,
    /// Session, member, roster, channel and initial prompt events.
    pub events: Vec<Event>,
}

async fn check_capacity(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root: SessionId,
    requested: u64,
    limit: u64,
) -> Result<(), StoreError> {
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM resident_actor_claim WHERE root_session_id = ? AND state = 'active'",
    )
    .bind(root.storage_key())
    .fetch_one(&mut **tx)
    .await?;
    let live = live as u64;
    if requested > limit.saturating_sub(live) {
        return Err(StoreError::CapacityExceeded {
            root,
            live,
            requested,
            limit,
        });
    }
    Ok(())
}
