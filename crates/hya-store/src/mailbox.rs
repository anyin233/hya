use hya_proto::{
    ActorClaim, ActorEpoch, ArchiveReason, Envelope, Event, MailEndpoint, MailKind, MemberId,
    MemberRunStatus, Projection, ReportOutcome, RosterEntry, RosterStatus, SessionId, SubagentMode,
    scope,
};

use crate::{
    RecoveredActorClaim, SessionStore, StoreError, append_event_in_transaction,
    projection_cache::ProjectionCache, replay_projection, resident_claim::fence_actor_claim,
};

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveredResidentWork {
    Idle,
    Queued {
        inbox_cursor: u64,
    },
    AbortedRunning {
        inbox_cursor: u64,
        queued_after: bool,
    },
}

#[doc(hidden)]
pub struct RecoveredResidentOutcome {
    pub work: RecoveredResidentWork,
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
    pub expected_work: Option<(u64, Option<hya_proto::ResidentWorkProjection>)>,
}

/// Capability used for a terminal transaction.
#[derive(Clone, Copy, Debug)]
pub enum ResidentArchiveOwner<'a> {
    /// The episode ending itself under its current claim.
    Claim(&'a ActorClaim),
    /// End an orphan: the caller guarantees there is no in-memory episode.
    /// Requires the runtime-owning store and `check_gate: false`. Takes over
    /// the active claim and fences the previous owner atomically.
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

impl SessionStore {
    /// Authoritatively validate and append one direct-handle mail event under the
    /// SQLite writer lock. The caller publishes the returned envelope after this
    /// transaction commits.
    ///
    /// `from` is the sender's canonical path; `address` is the recipient exactly
    /// as the model wrote it — a relative leaf (`worker-1`) or a full path.
    ///
    /// **Gate order matters.** The scope check runs *before* the liveness checks,
    /// and an out-of-scope address fails identically to an unknown one. If
    /// liveness ran first, an agent could probe whether an out-of-scope teammate
    /// existed or was still running by reading which rejection came back.
    #[allow(clippy::too_many_arguments)]
    pub async fn append_direct_mail(
        &self,
        root: SessionId,
        from: String,
        address: String,
        kind: MailKind,
        body: String,
        actor_claim: Option<&ActorClaim>,
        policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
    ) -> Result<Envelope, StoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }

        let projection = replay_projection(&self.projections, &mut tx, root).await?;
        let envelope = append_direct_mail_in_transaction(
            &projection,
            &mut tx,
            root,
            from,
            address,
            kind,
            body,
            policy,
        )
        .await?;
        tx.commit().await?;
        Ok(envelope)
    }

    /// Append one harness-authored direct mail (`from` = [`scope::HARNESS_HANDLE`])
    /// to a live resident member under the writer lock.
    ///
    /// Harness mail bypasses the hierarchy reach rule — the harness is not an
    /// agent — but still only reaches a registered, live (non-terminal)
    /// resident that is not the team root.
    ///
    /// # Errors
    /// [`StoreError::MailAuthorizationRejected`] when `handle` is not such a member, or
    /// SQLite failures.
    pub async fn append_harness_mail(
        &self,
        root: SessionId,
        handle: &str,
        body: String,
    ) -> Result<Envelope, StoreError> {
        let mut tx = self.begin_transaction().await?;
        let projection = replay_projection(&self.projections, &mut tx, root).await?;
        let reject = |reason: String| StoreError::MailAuthorizationRejected {
            sender: scope::HARNESS_HANDLE.to_string(),
            channel: format!("direct:{handle}"),
            reason,
        };
        let Some(entry) = projection.team.roster.get(handle) else {
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
        let event = Event::MailSent {
            session: root,
            from: scope::HARNESS_HANDLE.to_string(),
            to: MailEndpoint::Handle(handle.to_string()),
            kind: MailKind::Message,
            body,
        };
        let envelope = append_event_in_transaction(&mut tx, root, event).await?;
        tx.commit().await?;
        Ok(envelope)
    }

    /// Append one channel-targeted mail under the writer lock; returns the
    /// envelope and recipient count.
    ///
    /// `channel` is the name as the model wrote it, without the leading `#`:
    /// `build` for the sender's own unit, `^build` for its home unit when it
    /// leads one. It is resolved to a unit-qualified key inside the transaction,
    /// which is what prevents a post from reaching another unit's channel of the
    /// same name.
    #[allow(clippy::too_many_arguments)]
    pub async fn append_channel_mail(
        &self,
        root: SessionId,
        from: String,
        channel: String,
        kind: MailKind,
        body: String,
        actor_claim: Option<&ActorClaim>,
        policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
    ) -> Result<(Envelope, usize), StoreError> {
        let mut tx = self.begin_transaction().await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }

        let projection = replay_projection(&self.projections, &mut tx, root).await?;
        let key = projection
            .team
            .resolve_channel(&from, &channel)
            .map_err(|error| StoreError::MailAuthorizationRejected {
                sender: from.clone(),
                channel: channel.clone(),
                reason: error.to_string(),
            })?;
        let (envelope, recipients) =
            append_resolved_channel_mail(&mut tx, root, from, key, kind, body, &projection, policy)
                .await?;
        tx.commit().await?;
        Ok((envelope, recipients))
    }

    /// Atomically recover one resident actor after its claim has been fenced.
    ///
    /// The writer transaction covers actor terminal effects,
    /// root activity, and work classification. The recovered claim remains
    /// active for the caller's subsequent continuation.
    #[doc(hidden)]
    pub async fn recover_resident_actor(
        &self,
        recovered: &RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<RecoveredResidentOutcome, StoreError> {
        // Fold outside the writer transaction so the transaction's folds of
        // the root and actor logs start from the cache.
        self.warm_projection(root).await?;
        self.warm_projection(recovered.claim.actor_id).await?;
        let mut tx = self.begin_transaction().await?;
        fence_actor_claim(&mut tx, &recovered.claim).await?;

        let projection = replay_projection(&self.projections, &mut tx, root).await?;
        let Some(entry) = projection.team.roster.get(handle) else {
            return Err(StoreError::ActorClaimData(
                "resident recovery roster mismatch".to_string(),
            ));
        };
        if entry.session != recovered.claim.actor_id || !entry.mode.is_resident() {
            return Err(StoreError::ActorClaimData(
                "resident recovery roster mismatch".to_string(),
            ));
        }

        let recovery_reason = "aborted by resident recovery";
        let inbox_len = projection
            .team
            .inboxes
            .get(handle)
            .map_or(0, |inbox| inbox.len() as u64);
        let mut envelopes = Vec::new();
        let work = if let Some(resident_work) = entry.resident_work {
            if resident_work.epoch > recovered.previous_epoch {
                return Err(StoreError::ActorClaimData(
                    "resident recovery work epoch mismatch".to_string(),
                ));
            }
            if entry.status == RosterStatus::Idle {
                RecoveredResidentWork::Queued {
                    inbox_cursor: entry.resident_cursor,
                }
            } else {
                envelopes.extend(
                    append_resident_effects_in_transaction(
                        &self.projections,
                        &mut tx,
                        recovered.claim.actor_id,
                        recovery_reason,
                    )
                    .await?,
                );
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        root,
                        Event::AgentActivityChanged {
                            session: root,
                            handle: handle.to_string(),
                            status: RosterStatus::Failed,
                            current_task: Some(recovery_reason.to_string()),
                        },
                    )
                    .await?,
                );
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        root,
                        Event::ResidentWorkFinished {
                            session: root,
                            handle: handle.into(),
                            epoch: resident_work.epoch,
                            inbox_through: resident_work.inbox_through,
                        },
                    )
                    .await?,
                );
                RecoveredResidentWork::AbortedRunning {
                    inbox_cursor: resident_work.inbox_through,
                    queued_after: inbox_len > resident_work.inbox_through,
                }
            }
        } else if inbox_len > entry.resident_cursor {
            RecoveredResidentWork::Queued {
                inbox_cursor: entry.resident_cursor,
            }
        } else {
            RecoveredResidentWork::Idle
        };

        tx.commit().await?;
        Ok(RecoveredResidentOutcome { work, envelopes })
    }

    /// Commit terminal effects, or only a refusal notice for a voluntary report.
    /// Publish returned envelopes only after success.
    pub async fn finalize_resident_archive(
        &self,
        owner: ResidentArchiveOwner<'_>,
        root: SessionId,
        handle: &str,
        archive: ResidentArchive,
    ) -> Result<(ResidentArchiveOutcome, Vec<Envelope>), StoreError> {
        let actor_id = archive.actor;
        let reason = archive.note.as_str();
        let member_status = archive.member_status;
        let mut tx = self.begin_transaction().await?;
        let owned_claim;
        let claim = match owner {
            ResidentArchiveOwner::Claim(claim) => claim,
            ResidentArchiveOwner::Takeover => {
                let owner_run_id = self
                    .runtime_owner
                    .claim
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .map(|claim| claim.owner)
                    .ok_or(StoreError::RuntimeOwnerClaimRequired)?;
                self.require_runtime_owner(owner_run_id)?;
                if archive.check_gate {
                    return Err(StoreError::ActorClaimData(
                        "voluntary report requires an episode claim".into(),
                    ));
                }
                let epoch: Option<i64> = sqlx::query_scalar(
                    "UPDATE resident_actor_claim SET epoch = epoch + 1, owner_run_id = ? \
                     WHERE actor_id = ? AND state = 'active' AND epoch < 9223372036854775807 \
                     RETURNING epoch",
                )
                .bind(owner_run_id.as_uuid().as_bytes().as_slice())
                .bind(actor_id.storage_key())
                .fetch_optional(&mut *tx)
                .await?;
                let Some(epoch) = epoch else {
                    let released: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM resident_actor_claim \
                         WHERE actor_id = ? AND state = 'released')",
                    )
                    .bind(actor_id.storage_key())
                    .fetch_one(&mut *tx)
                    .await?;
                    if released {
                        let projection =
                            replay_projection(&self.projections, &mut tx, root).await?;
                        if projection
                            .team
                            .archived
                            .get(handle)
                            .is_some_and(|entry| entry.session == actor_id)
                        {
                            tx.commit().await?;
                            return Ok((ResidentArchiveOutcome::Committed, Vec::new()));
                        }
                    }
                    return Err(StoreError::ActorClaimUnavailable { actor_id });
                };
                let epoch = epoch as u64;
                owned_claim = ActorClaim {
                    actor_id,
                    epoch: ActorEpoch::from_storage(epoch),
                    owner_run_id,
                };
                &owned_claim
            }
        };
        if let Err(error) = fence_actor_claim(&mut tx, claim).await {
            let stale = matches!(
                &error,
                StoreError::StaleActorClaim { actor_id } if *actor_id == claim.actor_id
            );
            if stale
                && released_resident_matches(&self.projections, &mut tx, claim, root, handle)
                    .await?
            {
                tx.commit().await?;
                return Ok((ResidentArchiveOutcome::Committed, Vec::new()));
            }
            return Err(error);
        }

        let projection = replay_projection(&self.projections, &mut tx, root).await?;
        let Some(entry) = projection.team.roster.get(handle) else {
            return Err(StoreError::ActorClaimData(
                "resident stop handle is not registered".to_string(),
            ));
        };
        if actor_id != claim.actor_id
            || entry.session != claim.actor_id
            || entry.mode != SubagentMode::Resident
        {
            return Err(StoreError::ActorClaimData(
                "resident stop handle does not match actor claim".to_string(),
            ));
        }

        let actor = replay_projection(&self.projections, &mut tx, claim.actor_id).await?;
        let generation = actor
            .session
            .handoff
            .as_ref()
            .map_or(1, |handoff| handoff.generation.saturating_add(1));
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
            let unread: Vec<_> = projection
                .team
                .unread_incoming_mail(handle, read_through)
                .collect();
            if !unread.is_empty() {
                let channels: std::collections::BTreeSet<_> = unread
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
            } else if let Some(child) = projection
                .team
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
                return Err(StoreError::MailboxRejected(reason));
            }
            let notice = append_event_in_transaction(
                &mut tx,
                root,
                Event::MailSent {
                    session: root,
                    from: scope::HARNESS_HANDLE.into(),
                    to: MailEndpoint::Handle(handle.into()),
                    kind: MailKind::Message,
                    body: format!(
                        "Your report was not accepted: {reason}. Read and answer it, then call report again."
                    ),
                },
            )
            .await?;
            tx.commit().await?;
            return Ok((ResidentArchiveOutcome::Refused, vec![notice]));
        }

        let mut envelopes = append_resident_effects_in_transaction(
            &self.projections,
            &mut tx,
            claim.actor_id,
            reason,
        )
        .await?;
        if archive.report.is_none() {
            envelopes.extend(
                close_parent_member_row_in_transaction(
                    &self.projections,
                    &mut tx,
                    claim.actor_id,
                    member_status,
                    reason,
                )
                .await?,
            );
        }

        let epoch = i64::try_from(claim.epoch.get()).map_err(|_| {
            StoreError::ActorClaimData("actor epoch exceeds SQLite INTEGER range".to_string())
        })?;
        let root_envelope = append_event_in_transaction(
            &mut tx,
            root,
            Event::AgentActivityChanged {
                session: root,
                handle: handle.to_string(),
                status: RosterStatus::Failed,
                current_task: Some(reason.to_string()),
            },
        )
        .await?;

        let released = sqlx::query(
            "UPDATE resident_actor_claim SET state = 'released' \
             WHERE actor_id = ? AND epoch = ? AND owner_run_id = ? AND state = 'active'",
        )
        .bind(claim.actor_id.storage_key())
        .bind(epoch)
        .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
        .execute(&mut *tx)
        .await?;
        if released.rows_affected() != 1 {
            return Err(StoreError::StaleActorClaim {
                actor_id: claim.actor_id,
            });
        }

        envelopes.push(root_envelope);
        if claim.actor_id != root {
            envelopes.push(
                append_event_in_transaction(
                    &mut tx,
                    claim.actor_id,
                    Event::HandoffCommitted {
                        session: claim.actor_id,
                        handle: handle.into(),
                        generation: archive.generation,
                        doc: archive.doc,
                        degraded: archive.degraded,
                    },
                )
                .await?,
            );
            if let Some((outcome, report)) = archive.report {
                let parent_path = scope::parent_path(handle).unwrap_or(scope::ROOT_HANDLE);
                let parent = projection
                    .team
                    .roster
                    .get(parent_path)
                    .map_or(root, |entry| entry.session);
                let parent_projection =
                    replay_projection(&self.projections, &mut tx, parent).await?;
                let member = parent_projection
                    .session
                    .members
                    .iter()
                    .find(|row| row.child == Some(claim.actor_id))
                    .map_or_else(MemberId::new, |row| row.member);
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        parent,
                        Event::SubagentReported {
                            session: parent,
                            member,
                            child: claim.actor_id,
                            handle: handle.into(),
                            outcome,
                            report: report.clone(),
                        },
                    )
                    .await?,
                );
                let target = projection
                    .team
                    .channels
                    .iter()
                    .find(|(_, channel)| {
                        channel.kind == hya_proto::ChannelKind::Dm
                            && channel.members.contains(handle)
                            && channel.members.contains(parent_path)
                    })
                    .map_or_else(
                        || MailEndpoint::Handle(parent_path.into()),
                        |(key, _)| MailEndpoint::Channel(key.clone()),
                    );
                envelopes.push(
                    append_event_in_transaction(
                        &mut tx,
                        root,
                        Event::MailSent {
                            session: root,
                            from: handle.into(),
                            to: target,
                            kind: MailKind::Message,
                            body: report,
                        },
                    )
                    .await?,
                );
            }
            envelopes.push(
                append_event_in_transaction(
                    &mut tx,
                    root,
                    Event::AgentArchived {
                        session: root,
                        handle: handle.into(),
                        child: claim.actor_id,
                        reason: archive.reason,
                    },
                )
                .await?,
            );
        }
        tx.commit().await?;
        Ok((ResidentArchiveOutcome::Committed, envelopes))
    }
}

fn resident_effect_terminal_events(
    actor: SessionId,
    projection: &Projection,
    reason: &str,
) -> Vec<Event> {
    // A stale claim means the owning process lost the actor: the same
    // process-death semantics as crash recovery, so member rows close.
    crate::recovery::open_turn_terminal_events(actor, projection, reason, "STALE_ACTOR_CLAIM", None)
}

async fn append_resident_effects_in_transaction(
    cache: &ProjectionCache,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor: SessionId,
    reason: &str,
) -> Result<Vec<Envelope>, StoreError> {
    let projection = replay_projection(cache, tx, actor).await?;
    let events = resident_effect_terminal_events(actor, &projection, reason);
    let mut envelopes = Vec::with_capacity(events.len());
    for event in events {
        envelopes.push(append_event_in_transaction(tx, actor, event).await?);
    }
    Ok(envelopes)
}

/// Close this episode's member row, including a revived actor that has not
/// started its first turn. The finalizer's claim/archive fence owns idempotency.
async fn close_parent_member_row_in_transaction(
    cache: &ProjectionCache,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor: SessionId,
    status: MemberRunStatus,
    reason: &str,
) -> Result<Vec<Envelope>, StoreError> {
    let actor_projection = replay_projection(cache, tx, actor).await?;
    let Some(parent) = actor_projection.session.parent else {
        return Ok(Vec::new());
    };
    let parent_projection = replay_projection(cache, tx, parent).await?;
    let mut envelopes = Vec::new();
    for row in &parent_projection.session.members {
        if row.child == Some(actor) {
            let event = Event::MemberFinished {
                session: parent,
                member: row.member,
                status,
                summary: reason.to_string(),
                child: Some(actor),
            };
            envelopes.push(append_event_in_transaction(tx, parent, event).await?);
        }
    }
    Ok(envelopes)
}

async fn released_resident_matches(
    cache: &ProjectionCache,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claim: &ActorClaim,
    root: SessionId,
    handle: &str,
) -> Result<bool, StoreError> {
    let state = sqlx::query_scalar::<_, String>(
        "SELECT state FROM resident_actor_claim \
         WHERE actor_id = ? AND epoch = ? AND owner_run_id = ?",
    )
    .bind(claim.actor_id.storage_key())
    .bind(i64::try_from(claim.epoch.get()).map_err(|_| {
        StoreError::ActorClaimData("actor epoch exceeds SQLite INTEGER range".to_string())
    })?)
    .bind(claim.owner_run_id.as_uuid().as_bytes().as_slice())
    .fetch_optional(&mut **tx)
    .await?;
    if state.as_deref() != Some("released") {
        return Ok(false);
    }

    let projection = replay_projection(cache, tx, root).await?;
    Ok(projection
        .team
        .archived
        .get(handle)
        .is_some_and(|entry| entry.session == claim.actor_id)
        || projection.team.roster.get(handle).is_some_and(|entry| {
            entry.session == claim.actor_id
                && entry.mode == SubagentMode::Resident
                && entry.status == RosterStatus::Failed
        }))
}

/// Count eligible subscribers of an already-resolved channel key and append the
/// post. Shared by the ordinary channel path and the announce path so both use
/// one eligibility rule and one append.
///
/// A group post is still appended when nobody is eligible: the channel keeps its own
/// ordered log independently of who happened to be alive at post time.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_resolved_channel_mail(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root: SessionId,
    from: String,
    key: String,
    kind: MailKind,
    body: String,
    projection: &Projection,
    policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
) -> Result<(Envelope, usize), StoreError> {
    let reject = |reason: &str| StoreError::MailAuthorizationRejected {
        sender: from.clone(),
        channel: key.clone(),
        reason: reason.into(),
    };
    let state = projection
        .team
        .channels
        .get(&key)
        .ok_or_else(|| reject("channel does not exist"))?;
    if !state.members.contains(&from) {
        return Err(reject("sender is not a channel member"));
    }
    let group = state.kind == hya_proto::ChannelKind::Group;
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
            if projection.team.archived.contains_key(peer)
                && !projection.team.roster.contains_key(peer)
            {
                if scope::parent_path(peer) == Some(from.as_str()) {
                    return Err(StoreError::MailRevivalRequired {
                        sender: from,
                        channel: key,
                        target: peer.clone(),
                    });
                }
                return Err(reject("archived recipient is not a direct child"));
            }
        }
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT actor_id FROM resident_actor_claim WHERE state = 'active' AND actor_id IN (",
        );
        {
            let mut ids = query.separated(", ");
            for member in &state.members {
                if let Some(entry) = projection.team.roster.get(member) {
                    ids.push_bind(entry.session.storage_key());
                }
            }
        }
        query.push(")");
        let active: std::collections::HashSet<Vec<u8>> = query
            .build_query_scalar()
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect();
        for member in &state.members {
            if !projection.team.mail_recipient_is_live(member) {
                return Err(reject("recipient is stopped or terminal"));
            }
            if let Some(entry) = projection.team.roster.get(member)
                && entry.session != root
                && entry.mode.is_resident()
                && !active.contains(&entry.session.storage_key())
            {
                return Err(reject("recipient has no active actor claim"));
            }
        }
    }
    let recipients = state
        .members
        .iter()
        .filter(|member| projection.team.mail_recipient_is_live(member))
        .count();
    let kind = if group { MailKind::Announcement } else { kind };

    let envelope = append_event_in_transaction(
        tx,
        root,
        Event::MailSent {
            session: root,
            from,
            to: MailEndpoint::Channel(key),
            kind,
            body,
        },
    )
    .await?;
    Ok((envelope, recipients))
}

async fn has_active_claim(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor_id: SessionId,
) -> Result<bool, StoreError> {
    Ok(sqlx::query(
        "SELECT 1 FROM resident_actor_claim WHERE actor_id = ? AND state = 'active' LIMIT 1",
    )
    .bind(actor_id.storage_key())
    .fetch_optional(&mut **tx)
    .await?
    .is_some())
}

async fn resident_member_is_eligible(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    entry: &RosterEntry,
) -> Result<bool, StoreError> {
    if !entry.accepts_mail() {
        return Ok(false);
    }
    has_active_claim(tx, entry.session).await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_direct_mail_in_transaction(
    projection: &Projection,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root: SessionId,
    from: String,
    address: String,
    kind: MailKind,
    body: String,
    policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
) -> Result<Envelope, StoreError> {
    let reject = |reason: String| StoreError::MailAuthorizationRejected {
        sender: from.clone(),
        channel: format!("direct:{address}"),
        reason,
    };
    if from != scope::ROOT_HANDLE && !projection.team.roster.contains_key(&from) {
        return Err(reject("sender is not a live team member".into()));
    }
    let handle = projection
        .team
        .resolve_in_scope(&from, &address)
        .or_else(|| {
            let canonical = if address.contains(scope::PATH_SEPARATOR) {
                address.clone()
            } else {
                scope::join_path(&from, &address)
            };
            projection
                .team
                .archived
                .get_key_value(&canonical)
                .and_then(|(handle, _)| {
                    (!projection.team.roster.contains_key(handle)
                        && scope::parent_path(handle) == Some(from.as_str()))
                    .then(|| handle.clone())
                })
        })
        .ok_or_else(|| {
            reject(format!(
                "`{address}` is not a teammate you can message; you may message \
                 your parent and the agents you lead — run `list_channel` to see them"
            ))
        })?;
    if policy.is_some_and(|policy| {
        let bits = if scope::parent_path(&from) == Some(handle.as_str()) {
            policy.dm_child
        } else {
            policy.dm_parent
        };
        bits & 1 == 0
    }) {
        return Err(reject("direction denied by policy".into()));
    }
    let Some(entry) = projection.team.roster.get(&handle) else {
        return Err(StoreError::MailRevivalRequired {
            sender: from,
            channel: format!("direct:{address}"),
            target: handle,
        });
    };
    if entry.session != root && entry.mode == SubagentMode::Transient {
        return Err(reject(format!("mail target `{handle}` is transient")));
    }
    if entry.session != root
        && entry.mode == SubagentMode::Resident
        && !resident_member_is_eligible(tx, entry).await?
    {
        return Err(reject(format!(
            "mail target `{handle}` is stopped or terminal"
        )));
    }

    let event = Event::MailSent {
        session: root,
        from,
        to: MailEndpoint::Handle(handle.to_string()),
        kind,
        body,
    };
    let envelope = append_event_in_transaction(tx, root, event).await?;
    Ok(envelope)
}
