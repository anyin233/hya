use super::*;

async fn initialize_episode(
    team: &TeamActor,
    claim: &ActorClaim,
    cancel: &CancellationToken,
    init: EpisodeInit,
) -> Result<(), CoreError> {
    let EpisodeInit {
        binding,
        agent,
        parent,
        envelopes,
        published,
        ready,
    } = init;
    // No child hook or turn may overtake the committed parent registration.
    let _ = published.await;
    let session = claim.actor_id;
    let created = envelopes.iter().any(|envelope| {
        matches!(envelope.event,
        Event::SessionCreated { session: id, .. } if id == session)
    });
    let hooks = async {
        if created {
            team.engine.notify_session_start(session).await;
        }
        for hooks in team
            .engine
            .replace_session_hooks(session, &binding, agent.as_str())
        {
            hooks.session_start(SessionLifecycleInput { session }).await;
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {},
        () = hooks => {},
    }
    // Only child-log events wait for session-start hooks. Cancellation still
    // publishes them before the episode can write its terminal events.
    for envelope in envelopes {
        team.engine.publish_envelope(envelope);
    }
    let initialize = async {
        if created && let Some(hooks) = team.engine.session_hook_dispatcher(parent) {
            hooks
                .agent_spawn(AgentSpawnInput {
                    parent: team.root,
                    child: session,
                })
                .await;
        }
        Ok::<(), CoreError>(())
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        result = initialize => result,
    };
    let result = if cancel.is_cancelled() {
        Err(CoreError::Cancelled)
    } else {
        result
    };
    let _ = ready.send(result.clone());
    result
}

pub(super) async fn episode_task(
    team: Arc<TeamActor>,
    session: SessionId,
    task_claim: Option<ActorClaim>,
    notify: Arc<Notify>,
    cancel: CancellationToken,
    init: Option<EpisodeInit>,
) {
    let mut sidecar_handle: Option<Box<dyn SidecarHandle>> = None;
    let mut sidecar_tools: Arc<[ResolvedTool]> = Arc::from([]);
    let mut sidecar_hooks: Option<Arc<dyn HookDispatcher>> = None;
    if let Some(init) = init {
        let Some(claim) = task_claim.as_ref() else {
            return;
        };
        let result = initialize_episode(&team, claim, &cancel, init).await;
        if let Err(error) = result {
            if cancel.is_cancelled()
                && team
                    .lock()
                    .residents
                    .get(&session)
                    .is_none_or(|slot| slot.ending.is_none())
            {
                team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
            }
            finish_episode(
                &team,
                claim,
                Ending::Failed(format!("resident initialization failed: {error}")),
                &mut sidecar_handle,
                None,
            )
            .await;
            return;
        }
    }
    loop {
        if cancel.is_cancelled() && !team.lock().phase.killed() {
            let requested = team
                .lock()
                .residents
                .get(&session)
                .is_some_and(|slot| slot.ending.is_some());
            if !requested {
                team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
            }
        }
        if !team.owns_slot(session, task_claim.as_ref()) {
            if let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                tracing::warn!(%session, %error, "stale episode sidecar did not stop");
            }
            return;
        }
        let ending = team
            .lock()
            .residents
            .get(&session)
            .and_then(|slot| slot.ending.clone());
        if let Some(ending) = ending {
            if let Some(claim) = task_claim.as_ref() {
                finish_episode(&team, claim, ending, &mut sidecar_handle, None).await;
            } else {
                team.complete_episode(session, None, Ok(()));
            }
            return;
        }
        if let (Some(claim), Some(pending)) = (
            task_claim.as_ref(),
            team.pending_report(session, task_claim.as_ref()),
        ) {
            let ending = Ending::Reported(pending);
            match commit_episode_ending(&team, claim, &ending, None).await {
                Err(CoreError::Cancelled) => {
                    if team
                        .lock()
                        .residents
                        .get(&session)
                        .is_none_or(|slot| slot.ending.is_none())
                    {
                        team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
                    }
                    continue;
                }
                Ok((hya_store::ResidentArchiveOutcome::Refused, _)) => {
                    let mut state = team.lock();
                    if let Some(slot) = state
                        .residents
                        .get_mut(&session)
                        .filter(|slot| slot.claim == task_claim)
                    {
                        slot.report = None;
                    }
                }
                result => {
                    finish_episode(
                        &team,
                        claim,
                        ending,
                        &mut sidecar_handle,
                        Some(result.map(|(_, finalization)| finalization)),
                    )
                    .await;
                    return;
                }
            }
        }
        if let Some(loss_token) = sidecar_handle
            .as_ref()
            .and_then(|handle| handle.loss_token())
        {
            tokio::select! {
                biased;
                _ = loss_token.cancelled() => {
                    sidecar_tools = Arc::from([]);
                    sidecar_hooks = None;
                    if let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                        tracing::warn!(%session, %error, "idle sidecar did not stop");
                    }
                    continue;
                }
                _ = cancel.cancelled() => continue,
                _ = notify.notified() => {}
            }
        } else {
            tokio::select! {
                _ = cancel.cancelled() => continue,
                _ = notify.notified() => {}
            }
        }
        loop {
            match team.next_action(session, task_claim.as_ref()).await {
                Action::Run(plan) => {
                    let handle = plan.handle.clone();
                    let claim = plan.claim;
                    let is_main = plan.is_main;
                    if sidecar_handle
                        .as_ref()
                        .is_some_and(|cached| !cached.is_healthy())
                    {
                        sidecar_tools = Arc::from([]);
                        sidecar_hooks = None;
                        if let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                            tracing::warn!(%session, %error, "idle sidecar did not stop");
                        }
                    }
                    if sidecar_handle.is_none()
                        && let Some(factory) = plan.sidecar_factory.clone()
                    {
                        let cancel = plan.cancel.clone();
                        let started = tokio::select! {
                            biased;
                            _ = cancel.cancelled() => Err(CoreError::Cancelled),
                            result = factory.start(SidecarStart::resident()) => result,
                        };
                        let activation = match started {
                            Ok(mut handle) => {
                                let result = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => Err(CoreError::Cancelled),
                                    result = handle.ready() => result,
                                };
                                sidecar_handle = Some(handle);
                                result
                            }
                            Err(error) => Err(error),
                        };
                        if let Err(error) = activation {
                            drop(plan);
                            let Some(claim) = claim.as_ref() else {
                                return;
                            };
                            finish_episode(
                                &team,
                                claim,
                                Ending::Failed(format!("sidecar start failed: {error}")),
                                &mut sidecar_handle,
                                None,
                            )
                            .await;
                            return;
                        }
                        if let Some(handle) = sidecar_handle.as_ref() {
                            sidecar_tools = handle.tool_bindings();
                            sidecar_hooks = handle
                                .hook_dispatcher()
                                .map(crate::bundle_hooks::restricted_sidecar_hooks);
                        }
                    }
                    let loss_token = sidecar_handle
                        .as_ref()
                        .and_then(|handle| handle.loss_token());
                    let turn_cancel = plan.cancel.clone();
                    let turn = team.run_one_turn(session, *plan, Arc::clone(&sidecar_tools));
                    let hooks = sidecar_hooks.clone();
                    let mut run = Box::pin(async move {
                        match hooks {
                            Some(hooks) => scope_activation_hooks(session, hooks, turn).await,
                            None => turn.await,
                        }
                    });
                    let loss = async {
                        match loss_token {
                            Some(token) => token.cancelled().await,
                            None => std::future::pending().await,
                        }
                    };
                    let mut sidecar_lost = false;
                    let mut loss_interrupted_turn = false;
                    let result = tokio::select! {
                        biased;
                        () = loss => {
                            sidecar_lost = true;
                            loss_interrupted_turn = team.engine.turn_active(session)
                                && team.engine.turn_cancel_cause(session).is_none()
                                && !team.lock().residents.get(&session).is_some_and(|slot| slot.user_stopped);
                            turn_cancel.cancel();
                            None
                        }
                        _ = cancel.cancelled() => None,
                        result = &mut run => Some(result),
                    };
                    let turn_result = match result {
                        Some(result) => result,
                        None => {
                            // Poll the own turn before any ending writes: it can
                            // hold the connection those writes would need.
                            match tokio::time::timeout(DRAIN_DEADLINE, &mut run).await {
                                Ok(result) => result,
                                Err(_) => {
                                    let cause = team
                                        .lock()
                                        .residents
                                        .get(&session)
                                        .and_then(|slot| slot.ending.as_ref().map(Ending::cause))
                                        .unwrap_or(if sidecar_lost {
                                            FinishCause::Other
                                        } else {
                                            FinishCause::Shutdown
                                        });
                                    let close = async {
                                        team.engine
                                            .close_harness_turn(claim.as_ref(), session, cause)
                                            .await?;
                                        Ok::<(), CoreError>(())
                                    };
                                    // Closing shares the turn's message lock. Keep
                                    // polling its owner until close finishes.
                                    tokio::select! {
                                        result = &mut run => result,
                                        result = close => {
                                            if let Err(error) = result {
                                                tracing::error!(%session, ?claim, %error, "episode straggler close failed");
                                            }
                                            Err(CoreError::Cancelled)
                                        }
                                    }
                                }
                            }
                        }
                    };
                    drop(run);
                    if cancel.is_cancelled()
                        && team
                            .lock()
                            .residents
                            .get(&session)
                            .is_none_or(|slot| slot.ending.is_none())
                    {
                        team.kill(Ending::Shutdown(FinishCause::Shutdown)).await;
                    }
                    sidecar_lost |= sidecar_handle
                        .as_ref()
                        .is_some_and(|cached| !cached.is_healthy());
                    if sidecar_lost {
                        sidecar_tools = Arc::from([]);
                        sidecar_hooks = None;
                        let user_stopped = team
                            .lock()
                            .residents
                            .get(&session)
                            .is_some_and(|slot| slot.user_stopped);
                        let interrupted = loss_interrupted_turn
                            || (!user_stopped
                                && matches!(
                                    turn_result,
                                    Ok(TurnOutcome::Cancelled {
                                        cause: None | Some(FinishCause::Other),
                                        ..
                                    }) | Err(CoreError::Cancelled)
                                ));
                        if interrupted
                            && matches!(
                                turn_result,
                                Ok(TurnOutcome::Cancelled { .. }) | Err(CoreError::Cancelled)
                            )
                        {
                            let Some(claim) = claim.as_ref() else {
                                unreachable!("only member episodes have sidecars");
                            };
                            finish_episode(
                                &team,
                                claim,
                                Ending::Failed("activation sidecar lost".into()),
                                &mut sidecar_handle,
                                None,
                            )
                            .await;
                            return;
                        }
                        let ending = team
                            .lock()
                            .residents
                            .get(&session)
                            .is_some_and(|slot| slot.ending.is_some());
                        if !ending && let Some(error) = stop_sidecar(&mut sidecar_handle).await {
                            tracing::warn!(%session, %error, "idle sidecar did not stop");
                        }
                    }
                    if !is_main
                        && matches!(
                            turn_result,
                            Ok(TurnOutcome::Cancelled {
                                mail_owed: true,
                                ..
                            })
                        )
                    {
                        let mut state = team.lock();
                        if let Some(slot) = state
                            .residents
                            .get_mut(&session)
                            .filter(|slot| slot.claim == claim && slot.ending.is_none())
                        {
                            slot.pending = true;
                        }
                    }
                    // Flip the slot idle BEFORE recording the roster activity:
                    // observers gate on "roster says idle" (the report gate),
                    // so that observation must imply the slot is idle too.
                    team.finish_run(session, task_claim.as_ref());
                    match turn_result {
                        Ok(TurnOutcome::NotStarted) => {}
                        Ok(TurnOutcome::Finished { inbox_through }) => {
                            if let Some(claim) = claim.as_ref() {
                                if let Err(error) = team
                                    .engine
                                    .commit_resident_mutation(
                                        claim,
                                        team.root,
                                        vec![
                                            Event::ResidentWorkFinished {
                                                session: team.root,
                                                handle: handle.clone(),
                                                epoch: claim.epoch,
                                                inbox_through,
                                            },
                                            Event::AgentActivityChanged {
                                                session: team.root,
                                                handle,
                                                status: RosterStatus::Idle,
                                                current_task: None,
                                            },
                                        ],
                                    )
                                    .await
                                {
                                    finish_episode(
                                        &team,
                                        claim,
                                        Ending::Failed(format!("work completion error: {error}")),
                                        &mut sidecar_handle,
                                        None,
                                    )
                                    .await;
                                    return;
                                }
                            } else {
                                let _ = team
                                    .record_activity(None, handle, RosterStatus::Idle, None)
                                    .await;
                            }
                        }
                        Ok(TurnOutcome::Cancelled { .. }) | Err(CoreError::Cancelled) => {
                            // A user-stopped episode stays live and durably idle;
                            // a forced ending owns its terminal activity instead.
                            let ending = team
                                .lock()
                                .residents
                                .get(&session)
                                .is_some_and(|slot| slot.ending.is_some());
                            if !ending {
                                let _ = team
                                    .record_activity(
                                        claim.as_ref(),
                                        handle,
                                        RosterStatus::Idle,
                                        None,
                                    )
                                    .await;
                            }
                        }
                        Err(err) if is_main => {
                            // The lead is never archived: a failed lead turn
                            // ended with `finish: error`, its team was told
                            // to wrap up, and the session stays live and
                            // resumable. The slot parks until the user
                            // resumes the lead.
                            let _ = team
                                .record_activity(
                                    claim.as_ref(),
                                    handle,
                                    RosterStatus::Idle,
                                    Some(format!("turn failed: {err}")),
                                )
                                .await;
                        }
                        Err(err) => {
                            let Some(claim) = claim.as_ref() else {
                                return;
                            };
                            finish_episode(
                                &team,
                                claim,
                                Ending::Failed(format!("turn error: {err}")),
                                &mut sidecar_handle,
                                None,
                            )
                            .await;
                            return;
                        }
                    }
                    // An accepted report executes at rest before any
                    // follow-up turn: mail the turn already steered in owes
                    // no new episode, and genuinely unread mail fails the
                    // at-rest gate re-check, which re-arms the wake.
                    if team.pending_report(session, task_claim.as_ref()).is_some()
                        || team
                            .lock()
                            .residents
                            .get(&session)
                            .is_some_and(|slot| slot.ending.is_some())
                    {
                        break;
                    }
                }
                Action::End(ending) => {
                    if let Some(claim) = task_claim.as_ref() {
                        finish_episode(&team, claim, ending, &mut sidecar_handle, None).await;
                    } else {
                        team.complete_episode(session, None, Ok(()));
                    }
                    return;
                }
                Action::Idle {
                    handle,
                    claim,
                    became_idle,
                } => {
                    if became_idle {
                        let _ = team
                            .record_activity(claim.as_ref(), handle, RosterStatus::Idle, None)
                            .await;
                    }
                    if let Err(error) = team.settle_activation(session, task_claim.as_ref()).await {
                        let Some(claim) = claim.as_ref() else {
                            return;
                        };
                        finish_episode(
                            &team,
                            claim,
                            Ending::Failed(format!("activation completion error: {error}")),
                            &mut sidecar_handle,
                            None,
                        )
                        .await;
                        return;
                    }
                    break;
                }
            }
        }
    }
}

pub(super) async fn await_episode(mut done: EpisodeDone) -> Result<(), CoreError> {
    loop {
        if let Some(result) = done.borrow().clone() {
            return result;
        }
        done.changed()
            .await
            .map_err(|_| CoreError::Invalid("episode exited without its ending result".into()))?;
    }
}

impl TeamActor {
    /// Open `session`'s member row on its parent's log (`running`) as its
    /// turn starts, unless the row already is: one transition per episode
    /// (the first turn after the spawn, or after a revival reopened a row a
    /// report or archive closed), not one per wake. The row folds from the
    /// log, so a restart re-reads it instead of re-emitting. Observability
    /// only: a failed append is logged and never fails the turn.
    async fn mark_member_running(&self, session: SessionId, claim: Option<&ActorClaim>) {
        let Ok(child_projection) = self.engine.read_projection_shared(session).await else {
            return;
        };
        let Some(parent) = child_projection.session.parent else {
            return;
        };
        let Ok(parent_projection) = self.engine.read_projection_shared(parent).await else {
            return;
        };
        let Some(row) = parent_projection
            .session
            .members
            .iter()
            .find(|row| row.child == Some(session))
        else {
            return;
        };
        if row.status == MemberRunStatus::Running {
            return;
        }
        let member = row.member;
        let result = match claim {
            Some(claim) => {
                self.engine
                    .commit_resident_mutation(
                        claim,
                        parent,
                        vec![Event::MemberStatusChanged {
                            session: parent,
                            member,
                            status: MemberRunStatus::Running,
                        }],
                    )
                    .await
            }
            None => {
                self.engine
                    .record_member_status(parent, member, MemberRunStatus::Running)
                    .await
            }
        };
        if let Err(error) = result {
            tracing::warn!("resident member row stayed {:?}: {error}", row.status);
        }
    }

    /// Run exactly one turn for `session`: coalesce durable mail and the
    /// synthesis directive (main, on quiescence), and every inbox message since the
    /// cursor, then advance the cursor and stream one turn. All of this is coalesced
    /// into a single turn — many queued messages produce one turn, never several.
    ///
    /// Returns `NotStarted` when a wake has nothing left to deliver:
    /// mail that arrived during the lead's own turn and was
    /// already steered into it needs no follow-up turn.
    async fn run_one_turn(
        &self,
        session: SessionId,
        plan: RunPlan,
        sidecar_tools: Arc<[ResolvedTool]>,
    ) -> Result<TurnOutcome, CoreError> {
        let RunPlan {
            agent,
            binding,
            agents,
            resources,
            guidance,
            route,
            sidecar_factory: _,
            handle,
            is_main,
            cursor,
            synth,
            claim,
            cancel,
            lease,
        } = plan;
        // Admission hooks already belong to the leased turn, including a
        // user stop that arrives before run_turn binds its cancellation token.
        let cancel = lease.bind_cancel(&cancel);
        // Snapshot new inbox mail for this handle (folded before its wake, so it is
        // already visible here). The durable cursor leads the in-memory slot
        // cursor when steer already surfaced mail inside a tool result (or a
        // `wait` returned it) — sync so the wake never re-injects delivered
        // messages as user prompts. The plan's cursor was captured before
        // this sync, so it advances here too.
        let projection = self.engine.read_projection(self.root).await?;
        if !self.owns_slot(session, claim.as_ref()) {
            return Ok(TurnOutcome::NotStarted);
        }
        let mut cursor = cursor;
        let mut durable_cursor = 0;
        if let Some(entry) = projection.team.roster.get(&handle) {
            let durable = usize::try_from(entry.resident_cursor).unwrap_or(usize::MAX);
            durable_cursor = durable;
            cursor = cursor.max(durable);
            let mut st = self.lock();
            if let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            {
                slot.cursor = slot.cursor.max(durable);
            }
        }
        let mut resumed_prompt = None;
        let mut prompt_answered = false;
        if claim.is_some()
            && let Some(work) = projection
                .team
                .roster
                .get(&handle)
                .and_then(|entry| entry.resident_work)
        {
            cursor = cursor.max(usize::try_from(work.inbox_through).unwrap_or(usize::MAX));
            // The old prompt has already been hooked. Only new mail or a
            // continuation needs hooks; begin_work owns the transcript decision.
            let actor = self.engine.read_projection(session).await?;
            resumed_prompt = Some(
                actor
                    .session
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == hya_proto::Role::User)
                    .ok_or_else(|| {
                        CoreError::Invalid("resident work has no admitted prompt".into())
                    })?
                    .id,
            );
            prompt_answered = actor
                .session
                .messages
                .iter()
                .rev()
                .find(|message| {
                    message.role != hya_proto::Role::Assistant || !message.parts.is_empty()
                })
                .is_some_and(|message| Some(message.id) != resumed_prompt);
        }
        let inbox_len = projection
            .team
            .inboxes
            .get(&handle)
            .map_or(0, |inbox| inbox.len());
        let mail_owed = resumed_prompt.is_some()
            || projection
                .team
                .unread_incoming_mail(&handle, durable_cursor as u64)
                .next()
                .is_some();
        let channel_policy = ChannelPolicy::from_binding(&binding)?;
        let new_mail: Vec<(String, String)> = projection
            .team
            .inboxes
            .get(&handle)
            .map(|inbox| {
                inbox
                    .iter()
                    .skip(cursor)
                    .filter(|message| {
                        if message.from == handle {
                            return false;
                        }
                        if message.from == scope::HARNESS_HANDLE {
                            return true;
                        }
                        let (kind, role) = recipient_channel_role(
                            &projection,
                            &message.to,
                            &handle,
                            &message.from,
                        );
                        resident_delivery_allowed(&channel_policy, agent.name.as_str(), kind, role)
                    })
                    .map(|m| (m.from.clone(), m.body.clone()))
                    .collect()
            })
            .unwrap_or_default();

        if resumed_prompt.is_none() && !synth && new_mail.is_empty() {
            if cancel.is_cancelled() {
                return Ok(TurnOutcome::NotStarted);
            }
            if inbox_len > cursor {
                self.engine
                    .emit_for_actor(
                        claim.as_ref(),
                        self.root,
                        Event::MailConsumed {
                            session: self.root,
                            handle: handle.clone(),
                            through: inbox_len as u64,
                        },
                    )
                    .await?;
            }

            let mut st = self.lock();
            if let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            {
                slot.cursor = slot.cursor.max(inbox_len);
            }
            return Ok(TurnOutcome::NotStarted);
        }

        if cancel.is_cancelled() {
            return Ok(TurnOutcome::Cancelled {
                cause: self.engine.turn_cancel_cause(session),
                mail_owed,
            });
        }

        // An empty notification is not a turn and consumes no team budget.
        let exceeded = {
            let mut state = self.lock();
            state.turns_used = state.turns_used.saturating_add(1);
            self.engine.governor().is_some_and(|gov| {
                let limit = gov.limits().per_team_turn_budget;
                limit != 0 && state.turns_used > limit
            })
        };
        if exceeded {
            self.kill(Ending::Killed("per-team turn budget exceeded".into()))
                .await;
            return Ok(TurnOutcome::Cancelled {
                cause: self.engine.turn_cancel_cause(session),
                mail_owed,
            });
        }
        let task_label = if synth && is_main {
            "synthesizing".to_string()
        } else if let Some((from, _)) = new_mail.first() {
            format!("mail from {from}")
        } else {
            "working".to_string()
        };
        let mut sections = Vec::new();
        if synth && is_main {
            sections.push(SYNTHESIS_DIRECTIVE.to_string());
        }
        sections.extend(
            new_mail
                .iter()
                .map(|(from, body)| format!("[mail from {from}] {body}")),
        );
        if sections.is_empty() && prompt_answered {
            sections.push("Continue the unfinished work.".into());
        }
        let text = if sections.is_empty() {
            String::new()
        } else {
            // Hooks see the whole wake before any delivery or consumption commits.
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(TurnOutcome::Cancelled { cause: self.engine.turn_cancel_cause(session), mail_owed }),
                text = self.engine.user_text_after_hooks(&binding, agent.name.as_str(), session, sections.join("\n\n")) => text,
            }
        };
        if cancel.is_cancelled() {
            return Ok(TurnOutcome::Cancelled {
                cause: self.engine.turn_cancel_cause(session),
                mail_owed,
            });
        }
        let prompt = match resumed_prompt {
            Some(message) => hya_store::ResidentWorkPrompt::Resume {
                message,
                append: text,
            },
            None => hya_store::ResidentWorkPrompt::Text(text),
        };
        let (admitted_prompt, envelopes) = self
            .engine
            .store()
            .begin_work(
                claim.as_ref(),
                self.root,
                &handle,
                u64::try_from(inbox_len).unwrap_or(u64::MAX),
                prompt,
                task_label,
            )
            .await?;
        self.engine.update_session_channel_policy(
            session,
            channel_policy.snapshot_for(agent.name.as_str()),
        );
        for envelope in envelopes {
            self.engine.publish_envelope(envelope);
        }
        if !self.owns_slot(session, claim.as_ref()) {
            return Ok(TurnOutcome::NotStarted);
        }
        if !is_main {
            self.mark_member_running(session, claim.as_ref()).await;
        }
        self.heartbeat_turn_start(session, claim.as_ref(), &handle)
            .await;
        // Advance the cursor so a follow-up turn never re-injects the same mail.
        {
            let mut st = self.lock();
            let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim == claim)
            else {
                return Ok(TurnOutcome::NotStarted);
            };
            slot.cursor = slot.cursor.max(inbox_len);
        }
        // Exactly one turn, under the team-wide cancel so a budget kill stops it.
        let finish = match (claim.as_ref(), agents, resources) {
            (Some(claim), Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools_for_actor_and_workflow_route(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        TurnRequestContext::new(cancel.clone(), &[], guidance, Some(claim), route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
            (None, Some(agents), Some(resources)) => {
                self.engine
                    .run_resolved_turn_with_sidecar_tools_and_workflow_route(
                        session,
                        &agent,
                        (binding, agents, resources, Arc::clone(&sidecar_tools)),
                        TurnRequestContext::new(cancel.clone(), &[], guidance, None, route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
            (Some(claim), _, _) => {
                self.engine
                    .run_bound_turn_for_actor_with_workflow_route(
                        session,
                        &agent,
                        binding,
                        TurnRequestContext::new(cancel.clone(), &[], guidance, Some(claim), route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
            (None, _, _) => {
                self.engine
                    .run_bound_turn_with_workflow_route(
                        session,
                        &agent,
                        binding,
                        TurnRequestContext::new(cancel, &[], guidance, None, route)
                            .with_admitted_prompt(Some(admitted_prompt))
                            .with_lease(lease),
                    )
                    .await
            }
        };
        let finish = match finish {
            Ok(finish) => finish,
            Err(CoreError::Cancelled) => FinishReason::Cancelled,
            Err(error) => return Err(error),
        };
        // Turn finished: a fresh boundary heartbeat closes the window even
        // when the turn's own engine events never tripped the throttle.
        self.heartbeat_turn_finish(session, claim.as_ref(), &handle)
            .await;
        Ok(if finish == FinishReason::Cancelled {
            let projection = self.engine.read_projection(session).await?;
            let root = self.engine.read_projection(self.root).await?;
            let mail_owed = root.team.roster.get(&handle).is_some_and(|entry| {
                entry.resident_work.is_some()
                    || root
                        .team
                        .unread_incoming_mail(&handle, entry.resident_cursor)
                        .next()
                        .is_some()
            });
            TurnOutcome::Cancelled {
                mail_owed,
                cause: projection
                    .session
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == hya_proto::Role::Assistant)
                    .and_then(|message| message.cause),
            }
        } else {
            TurnOutcome::Finished {
                inbox_through: u64::try_from(inbox_len).unwrap_or(u64::MAX),
            }
        })
    }

    pub(super) async fn settle_activation(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
    ) -> Result<(), CoreError> {
        let eligible = |slot: &Episode| {
            slot.claim.as_ref() == claim
                && slot.status == SlotStatus::Idle
                && !slot.has_work()
                && !slot.user_stopped
                && slot.ending.is_none()
                && slot.report.is_none()
                && slot
                    .activation
                    .as_ref()
                    .is_some_and(|activation| slot.cursor >= activation.boundary)
        };
        if !self.lock().residents.get(&session).is_some_and(eligible) {
            return Ok(());
        }
        let projection = self.engine.read_projection(session).await?;
        let final_text = projection
            .session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == hya_proto::Role::Assistant)
            .map(|message| {
                message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        let root = self.engine.read_projection(self.root).await?;
        let mut state = self.lock();
        if let Some(slot) = state
            .residents
            .get_mut(&session)
            .filter(|slot| eligible(slot))
            .filter(|slot| {
                root.team.roster.get(&slot.handle).is_some_and(|entry| {
                    entry.resident_work.is_none()
                        && slot.activation.as_ref().is_some_and(|activation| {
                            entry.resident_cursor >= activation.boundary as u64
                        })
                        && root
                            .team
                            .unread_incoming_mail(&slot.handle, entry.resident_cursor)
                            .next()
                            .is_none()
                        && !root
                            .team
                            .roster
                            .keys()
                            .any(|path| scope::parent_path(path) == Some(slot.handle.as_str()))
                })
            })
        {
            slot.route = None;
            if let Some(activation) = slot.activation.take() {
                let _ = activation
                    .done
                    .send(Ok(ActivationResult::Settled { final_text }));
            }
        }
        Ok(())
    }

    fn pending_report(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
    ) -> Option<PendingReport> {
        self.lock()
            .residents
            .get(&session)
            .filter(|slot| slot.claim.as_ref() == claim && slot.ending.is_none())
            .and_then(|slot| slot.report.clone())
    }

    pub(super) fn take_episode(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
    ) -> Option<Episode> {
        let done = {
            let mut state = self.lock();
            let std::collections::hash_map::Entry::Occupied(entry) = state.residents.entry(session)
            else {
                return None;
            };
            if entry.get().claim.as_ref() != claim {
                return None;
            }
            let slot = entry.remove();
            if slot.status == SlotStatus::Busy {
                state.busy = state.busy.saturating_sub(1);
            }
            if state.main_session == Some(session) {
                state.main_session = None;
            }
            self.maybe_fire_quiescence(&mut state);
            slot
        };
        self.wake_activated_parent(&done.handle);
        self.forget_episode(session, claim);
        Some(done)
    }

    pub(super) fn wake_activated_parent(&self, child: &str) {
        if let Some(parent) = scope::parent_path(child)
            && let Some(slot) = self
                .lock()
                .residents
                .values()
                .find(|slot| slot.handle == parent && slot.activation.is_some())
        {
            slot.notify.notify_one();
        }
    }

    fn complete_episode(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        result: Result<(), CoreError>,
    ) {
        if let Some(slot) = self.take_episode(session, claim) {
            slot.done.send_replace(Some(result));
        }
    }
}
