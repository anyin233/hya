use hya_proto::{
    ActorClaim, Envelope, Event, EventSeq, FinishReason, MailEndpoint, MailKind, MemberRunStatus,
    PartProjection, Projection, Role, RosterEntry, RosterStatus, SessionId, SubagentMode,
    ToolPartState, now_millis,
};
use sqlx::Row;

use crate::{
    AdmissionRecord, RecoveredActorClaim, SessionStore, StoreError,
    admission::{abort_recovered_actor_admissions_in_transaction, decode_record},
    resident_claim::fence_actor_claim,
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
    pub admissions: Vec<AdmissionRecord>,
}

impl SessionStore {
    /// Authoritatively validate and append one direct-handle mail event under the
    /// SQLite writer lock. The caller publishes the returned envelope after this
    /// transaction commits.
    pub async fn append_direct_mail(
        &self,
        root: SessionId,
        from: String,
        handle: String,
        kind: MailKind,
        body: String,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<Envelope, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }

        let projection = replay_projection(&mut tx, root).await?;
        let Some(entry) = projection.team.roster.get(&handle) else {
            return Err(StoreError::MailboxRejected(format!(
                "unknown mail target `{handle}`"
            )));
        };
        if entry.session != root && entry.mode == SubagentMode::Transient {
            return Err(StoreError::MailboxRejected(format!(
                "mail target `{handle}` is transient"
            )));
        }
        if entry.session != root
            && entry.mode == SubagentMode::Resident
            && !resident_member_is_eligible(&mut tx, entry).await?
        {
            return Err(StoreError::MailboxRejected(format!(
                "mail target `{handle}` is stopped or terminal"
            )));
        }

        let event = Event::MailSent {
            session: root,
            from,
            to: MailEndpoint::Handle(handle),
            kind,
            body,
        };
        let envelope = append_event_in_transaction(&mut tx, root, event).await?;
        tx.commit().await?;
        Ok(envelope)
    }

    pub async fn append_channel_mail(
        &self,
        root: SessionId,
        from: String,
        channel: String,
        kind: MailKind,
        body: String,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<(Envelope, usize), StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(claim) = actor_claim {
            fence_actor_claim(&mut tx, claim).await?;
        }

        let projection = replay_projection(&mut tx, root).await?;
        let mut recipients = 0;
        if let Some(channel_state) = projection.team.channels.get(&channel) {
            for member in &channel_state.members {
                let eligible = match projection.team.roster.get(member) {
                    Some(entry) if entry.mode == SubagentMode::Resident => {
                        resident_member_is_eligible(&mut tx, entry).await?
                    }
                    _ => true,
                };
                if eligible {
                    recipients += 1;
                }
            }
        }

        let envelope = append_event_in_transaction(
            &mut tx,
            root,
            Event::MailSent {
                session: root,
                from,
                to: MailEndpoint::Channel(channel),
                kind,
                body,
            },
        )
        .await?;
        tx.commit().await?;
        Ok((envelope, recipients))
    }

    /// Atomically recover one resident actor after its claim has been fenced.
    ///
    /// The writer transaction covers admission aborts, actor terminal effects,
    /// root activity, and work classification. The recovered claim remains
    /// active for the caller's subsequent continuation.
    #[doc(hidden)]
    pub async fn recover_resident_actor(
        &self,
        recovered: &RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<RecoveredResidentOutcome, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        fence_actor_claim(&mut tx, &recovered.claim).await?;

        let projection = replay_projection(&mut tx, root).await?;
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

        let admission_reason = "resident actor takeover";
        let recovery_reason = "aborted by resident recovery";
        let admissions =
            abort_recovered_actor_admissions_in_transaction(&mut tx, recovered, admission_reason)
                .await?;
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
            envelopes.extend(
                append_resident_effects_in_transaction(
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
            RecoveredResidentWork::AbortedRunning {
                inbox_cursor: resident_work.inbox_through,
                queued_after: inbox_len > resident_work.inbox_through,
            }
        } else {
            let actor_projection = replay_projection(&mut tx, recovered.claim.actor_id).await?;
            let pending_user_turn = actor_projection
                .session
                .messages
                .last()
                .is_some_and(|message| message.role == Role::User);
            if inbox_len > entry.resident_cursor
                || (pending_user_turn && entry.status == RosterStatus::Idle)
            {
                RecoveredResidentWork::Queued {
                    inbox_cursor: entry.resident_cursor,
                }
            } else {
                RecoveredResidentWork::Idle
            }
        };

        tx.commit().await?;
        Ok(RecoveredResidentOutcome {
            work,
            envelopes,
            admissions,
        })
    }

    pub async fn finalize_resident_stop(
        &self,
        claim: &ActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<(Vec<Envelope>, Vec<AdmissionRecord>), StoreError> {
        self.finalize_resident_failure(claim, root, handle, "resident stopped")
            .await
    }

    /// Atomically terminalize a resident actor with the supplied reason and
    /// release its claim. The stop path delegates here so activation failures
    /// use the same rollback and idempotency boundary.
    #[doc(hidden)]
    pub async fn finalize_resident_failure(
        &self,
        claim: &ActorClaim,
        root: SessionId,
        handle: &str,
        reason: &str,
    ) -> Result<(Vec<Envelope>, Vec<AdmissionRecord>), StoreError> {
        self.finalize_resident_with_reason(claim, root, handle, reason)
            .await
    }

    async fn finalize_resident_with_reason(
        &self,
        claim: &ActorClaim,
        root: SessionId,
        handle: &str,
        reason: &str,
    ) -> Result<(Vec<Envelope>, Vec<AdmissionRecord>), StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Err(error) = fence_actor_claim(&mut tx, claim).await {
            let stale = matches!(
                &error,
                StoreError::StaleActorClaim { actor_id } if *actor_id == claim.actor_id
            );
            if stale && released_resident_matches(&mut tx, claim, root, handle).await? {
                tx.commit().await?;
                return Ok((Vec::new(), Vec::new()));
            }
            return Err(error);
        }

        let projection = replay_projection(&mut tx, root).await?;
        let Some(entry) = projection.team.roster.get(handle) else {
            return Err(StoreError::ActorClaimData(
                "resident stop handle is not registered".to_string(),
            ));
        };
        if entry.session != claim.actor_id || entry.mode != SubagentMode::Resident {
            return Err(StoreError::ActorClaimData(
                "resident stop handle does not match actor claim".to_string(),
            ));
        }

        let mut envelopes =
            append_resident_effects_in_transaction(&mut tx, claim.actor_id, reason).await?;

        let epoch = i64::try_from(claim.epoch.get()).map_err(|_| {
            StoreError::ActorClaimData("actor epoch exceeds SQLite INTEGER range".to_string())
        })?;
        let rows = sqlx::query(
            "UPDATE admission_journal \
             SET state = 'aborted', \
                 logical_released = CASE WHEN state = 'started' THEN 1 ELSE logical_released END, \
                 terminal_reason = ?, updated_at = ? \
             WHERE actor_id = ? AND actor_epoch = ? AND state IN ('accepted', 'started') \
             RETURNING operation_id, source_tool_call_id, root_session_id, request_fingerprint, \
                       state, admission_units, logical_released, terminal_reason, created_at, updated_at, \
                       actor_id, actor_epoch",
        )
        .bind(reason)
        .bind(now_millis())
        .bind(claim.actor_id.storage_key())
        .bind(epoch)
        .fetch_all(&mut *tx)
        .await?;
        let admissions = rows
            .into_iter()
            .map(decode_record)
            .collect::<Result<Vec<_>, _>>()?;

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

        tx.commit().await?;
        envelopes.push(root_envelope);
        Ok((envelopes, admissions))
    }
}

fn resident_effect_terminal_events(
    actor: SessionId,
    projection: &Projection,
    reason: &str,
) -> Vec<Event> {
    let mut events = Vec::new();
    for member in &projection.session.members {
        if matches!(
            member.status,
            MemberRunStatus::Spawning | MemberRunStatus::Running
        ) {
            events.push(Event::MemberFinished {
                session: actor,
                member: member.member,
                status: MemberRunStatus::Cancelled,
                summary: reason.to_string(),
                child: member.child,
            });
        }
    }
    for message in &projection.session.messages {
        if message.role != Role::Assistant || message.finish.is_some() {
            continue;
        }
        for part in &message.parts {
            if let PartProjection::Tool {
                id,
                call,
                state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                ..
            } = part
            {
                events.push(Event::ToolError {
                    session: actor,
                    message: message.id,
                    part: *id,
                    call: *call,
                    message_text: reason.to_string(),
                    value: Some(serde_json::json!({
                        "code": "STALE_ACTOR_CLAIM",
                    })),
                });
            }
        }
        events.push(Event::MessageFinished {
            session: actor,
            message: message.id,
            role: Role::Assistant,
            finish: FinishReason::Cancelled,
            tokens: None,
        });
    }
    events
}

async fn append_resident_effects_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    actor: SessionId,
    reason: &str,
) -> Result<Vec<Envelope>, StoreError> {
    let projection = replay_projection(tx, actor).await?;
    let events = resident_effect_terminal_events(actor, &projection, reason);
    let mut envelopes = Vec::with_capacity(events.len());
    for event in events {
        envelopes.push(append_event_in_transaction(tx, actor, event).await?);
    }
    Ok(envelopes)
}

async fn released_resident_matches(
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

    let projection = replay_projection(tx, root).await?;
    Ok(projection.team.roster.get(handle).is_some_and(|entry| {
        entry.session == claim.actor_id
            && entry.mode == SubagentMode::Resident
            && entry.status == RosterStatus::Failed
    }))
}

async fn append_event_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: SessionId,
    event: Event,
) -> Result<Envelope, StoreError> {
    let ts_millis = now_millis();
    let payload = serde_json::to_string(&event)?;
    let row = sqlx::query(
        "INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?) RETURNING seq",
    )
    .bind(session.storage_key())
    .bind(payload)
    .bind(ts_millis)
    .fetch_one(&mut **tx)
    .await?;
    let seq: i64 = row.try_get("seq")?;
    Ok(Envelope {
        seq: EventSeq(seq.max(0) as u64),
        ts_millis,
        event,
    })
}

async fn replay_projection(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: SessionId,
) -> Result<Projection, StoreError> {
    let rows =
        sqlx::query("SELECT seq, ts, payload FROM event_log WHERE session_id = ? ORDER BY seq")
            .bind(session.storage_key())
            .fetch_all(&mut **tx)
            .await?;
    let mut envelopes = Vec::with_capacity(rows.len());
    for row in rows {
        let seq: i64 = row.try_get("seq")?;
        let ts_millis: i64 = row.try_get("ts")?;
        let payload: String = row.try_get("payload")?;
        let event: Event = serde_json::from_str(&payload)?;
        envelopes.push(Envelope {
            seq: EventSeq(seq.max(0) as u64),
            ts_millis,
            event,
        });
    }
    Ok(Projection::from_events(&envelopes))
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
    if matches!(entry.status, RosterStatus::Done | RosterStatus::Failed) {
        return Ok(false);
    }
    has_active_claim(tx, entry.session).await
}
