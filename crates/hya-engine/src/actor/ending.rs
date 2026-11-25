use hya_bundle::HandoffPolicy;

use super::*;

/// The first terminal request owns the ending; later callers only wait.
#[derive(Clone)]
pub(super) enum Ending {
    Reported(PendingReport),
    Failed(String),
    Archived(String),
    Killed(String),
    Teardown,
    Shutdown(FinishCause),
}

impl Ending {
    pub(super) fn cause(&self) -> FinishCause {
        match self {
            Self::Archived(_) | Self::Teardown => FinishCause::Archived,
            Self::Shutdown(cause) => *cause,
            Self::Killed(_) => FinishCause::Other,
            Self::Failed(_) => FinishCause::Other,
            Self::Reported(_) => FinishCause::UserCancel,
        }
    }

    fn forced_fields(&self) -> (ArchiveReason, MemberRunStatus, String) {
        match self {
            Self::Archived(note) => (
                ArchiveReason::ArchivedByParent,
                MemberRunStatus::Cancelled,
                note.clone(),
            ),
            Self::Killed(note) => (ArchiveReason::Killed, MemberRunStatus::Failed, note.clone()),
            Self::Teardown => (
                ArchiveReason::RootTeardown,
                MemberRunStatus::Cancelled,
                "root turn teardown".into(),
            ),
            Self::Shutdown(cause) => (
                ArchiveReason::Shutdown,
                MemberRunStatus::Cancelled,
                match cause {
                    FinishCause::UserCancel => "stopped: cancelled by the user",
                    FinishCause::LeaderFailed => "stopped: the team lead failed",
                    _ => "stopped: shutdown",
                }
                .into(),
            ),
            Self::Reported(_) | Self::Failed(_) => unreachable!("report is not a forced ending"),
        }
    }
}

pub(super) async fn commit_episode_ending(
    team: &TeamActor,
    claim: &ActorClaim,
    ending: &Ending,
    sidecar_error: Option<String>,
) -> Result<(hya_store::ResidentArchiveOutcome, EpisodeFinalization), CoreError> {
    let (handle, child, cancel, handoff_policy) = {
        let state = team.lock();
        let slot = state
            .residents
            .get(&claim.actor_id)
            .filter(|slot| slot.claim == Some(*claim))
            .ok_or_else(|| CoreError::Invalid("episode ownership changed".into()))?;
        (
            slot.handle.clone(),
            claim.actor_id,
            slot.cancel.clone(),
            slot.binding
                .handoff_policy(slot.agent.name.as_str())
                .map_err(CoreError::bundle)?,
        )
    };
    let projection = team.engine.read_projection_shared(team.root).await?;
    let expected_work = projection
        .team
        .roster
        .get(&handle)
        .map(|entry| (entry.resident_cursor, entry.resident_work));
    drop(projection);
    let handoff = match ending {
        Ending::Reported(_) if handoff_policy == HandoffPolicy::Model => {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(CoreError::Cancelled),
                handoff = team.engine.terminal_handoff_claimed(child, Some(claim)) => handoff,
            }
        }
        _ => team.engine.degraded_terminal_handoff(child).await,
    };
    let (reason, status, mut report, mut note, check_gate) = match ending {
        Ending::Reported(pending) => (
            ArchiveReason::Reported,
            match pending.outcome {
                ReportOutcome::Done => MemberRunStatus::Done,
                ReportOutcome::Failed => MemberRunStatus::Failed,
            },
            Some((pending.outcome, pending.report.clone())),
            pending.report.clone(),
            true,
        ),
        Ending::Failed(note) => (
            ArchiveReason::Reported,
            MemberRunStatus::Failed,
            Some((ReportOutcome::Failed, note.clone())),
            note.clone(),
            false,
        ),
        ending => {
            let (reason, status, note) = ending.forced_fields();
            (reason, status, None, note, false)
        }
    };
    if let Some(error) = sidecar_error {
        note.push_str(&format!("; sidecar did not stop: {error}"));
        if let Some((_, report)) = &mut report {
            report.clone_from(&note);
        }
    }
    let (outcome, envelopes) = team
        .engine
        .store()
        .finalize_resident_archive(
            hya_store::ResidentArchiveOwner::Claim(claim),
            team.root,
            &handle,
            hya_store::ResidentArchive {
                actor: child,
                generation: handoff.generation,
                doc: handoff.doc,
                degraded: handoff.degraded,
                reason,
                note,
                member_status: status,
                report,
                check_gate,
                expected_work,
            },
        )
        .await?;
    if outcome == hya_store::ResidentArchiveOutcome::Refused {
        team.engine.publish_resident_finalization(envelopes);
        Ok((outcome, Vec::new()))
    } else {
        Ok((outcome, envelopes))
    }
}

/// Finish once, even on a failed store write. The durable active claim then
/// belongs to an orphan, which a later archive reconciles with Takeover.
pub(super) async fn finish_episode(
    team: &TeamActor,
    claim: &ActorClaim,
    mut ending: Ending,
    sidecar: &mut Option<Box<dyn SidecarHandle>>,
    committed: Option<Result<EpisodeFinalization, CoreError>>,
) {
    let sidecar_error = stop_sidecar(sidecar).await;
    if committed.is_some()
        && let Some(error) = &sidecar_error
    {
        tracing::error!(session = %claim.actor_id, %error, "reported episode sidecar did not stop");
    }
    if committed.is_none() {
        let mut state = team.lock();
        if let Some(slot) = state
            .residents
            .get_mut(&claim.actor_id)
            .filter(|slot| slot.claim == Some(*claim))
        {
            ending = slot.ending.get_or_insert(ending).clone();
        }
    }
    let result = match committed {
        Some(result) => result,
        None => commit_episode_ending(team, claim, &ending, sidecar_error)
            .await
            .map(|(_, finalization)| finalization),
    };
    if let Err(error) = &result {
        tracing::error!(session = %claim.actor_id, ?claim, %error, "episode terminal transition failed; leaving orphan");
    }
    // No archive envelope is observable while this episode is installed. Hold
    // the claim-write boundary until publication and done, so revival cannot
    // publish a new registration ahead of the old archive.
    let supervisor = team.supervisor.upgrade();
    let _write = match &supervisor {
        Some(supervisor) => Some(supervisor.claim_writes.lock().await),
        None => None,
    };
    if let Ok(envelopes) = &result {
        team.lock()
            .routing_mail
            .extend(envelopes.iter().filter_map(|envelope| {
                matches!(envelope.event, Event::MailSent { .. }).then_some(envelope.seq.0)
            }));
    }
    if let Some(slot) = team.take_episode(claim.actor_id, Some(claim)) {
        let activation = slot.activation.map(|activation| {
            let activation_result = result
                .as_ref()
                .map(|envelopes| {
                    if let Some((outcome, text)) =
                        envelopes.iter().find_map(|envelope| match &envelope.event {
                            Event::SubagentReported {
                                child,
                                outcome,
                                report,
                                ..
                            } if *child == claim.actor_id => Some((*outcome, report.clone())),
                            _ => None,
                        })
                    {
                        ActivationResult::Reported { outcome, text }
                    } else {
                        let (reason, status, mut note) = ending.forced_fields();
                        if let Some(summary) =
                            envelopes.iter().find_map(|envelope| match &envelope.event {
                                Event::MemberFinished {
                                    child: Some(child),
                                    summary,
                                    ..
                                } if *child == claim.actor_id => Some(summary),
                                _ => None,
                            })
                        {
                            note.clone_from(summary);
                        }
                        ActivationResult::Ended {
                            reason,
                            status,
                            note,
                        }
                    }
                })
                .map_err(Clone::clone);
            (activation, activation_result)
        });
        drop(slot.route);
        let result =
            result.map(|finalization| team.engine.publish_resident_finalization(finalization));
        if let Some((activation, activation_result)) = activation {
            let _ = activation.done.send(activation_result);
        }
        slot.done.send_replace(Some(result));
    }
}

/// A lost or ending sidecar cannot hold the episode beyond its stop deadline.
const SIDECAR_STOP_DEADLINE: Duration = Duration::from_secs(5);

pub(super) async fn stop_sidecar(sidecar: &mut Option<Box<dyn SidecarHandle>>) -> Option<String> {
    let mut handle = sidecar.take()?;
    match tokio::time::timeout(SIDECAR_STOP_DEADLINE, handle.terminate()).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("deadline exceeded".into()),
    }
}

/// Resolve a model-supplied subagent reference — canonical handle, a leaf
/// relative to the caller (`hya-worker-exusiai`), a unique leaf, or a session id
/// (`hysec_…`) — against the team's live roster and archive. The lead
/// resolves too (`main` or the root session id); callers decide what that
/// means. The error lists the caller's live subagents.
pub(crate) fn resolve_member_target(
    projection: &hya_proto::Projection,
    root: SessionId,
    caller: &str,
    raw: &str,
) -> Result<MemberTarget, CoreError> {
    let token = raw.trim().trim_start_matches('@');
    let live = |handle: &str| {
        projection
            .team
            .roster
            .get(handle)
            .map(|entry| MemberTarget {
                handle: handle.to_string(),
                session: entry.session,
                live: true,
            })
    };
    let archived = |handle: &str| {
        projection
            .team
            .archived
            .get(handle)
            .map(|entry| MemberTarget {
                handle: handle.to_string(),
                session: entry.session,
                live: false,
            })
    };
    let found = if token.is_empty() {
        None
    } else if let Ok(session) = token.parse::<SessionId>() {
        if session == root {
            Some(MemberTarget {
                handle: scope::ROOT_HANDLE.to_string(),
                session: root,
                live: true,
            })
        } else {
            projection
                .team
                .roster
                .values()
                .find(|entry| entry.session == session)
                .map(|entry| MemberTarget {
                    handle: entry.handle.clone(),
                    session,
                    live: true,
                })
                .or_else(|| {
                    projection
                        .team
                        .archived
                        .values()
                        .find(|entry| entry.session == session)
                        .map(|entry| MemberTarget {
                            handle: entry.handle.clone(),
                            session,
                            live: false,
                        })
                })
        }
    } else if token == scope::ROOT_HANDLE {
        Some(MemberTarget {
            handle: scope::ROOT_HANDLE.to_string(),
            session: root,
            live: true,
        })
    } else {
        let relative = scope::join_path(caller, token);
        live(token)
            .or_else(|| archived(token))
            .or_else(|| live(&relative))
            .or_else(|| archived(&relative))
            .or_else(|| {
                let mut leaves = projection
                    .team
                    .roster
                    .keys()
                    .filter(|key| scope::leaf(key) == token);
                match (leaves.next(), leaves.next()) {
                    (Some(only), None) => live(only),
                    _ => None,
                }
            })
            .or_else(|| {
                let mut leaves = projection
                    .team
                    .archived
                    .keys()
                    .filter(|key| scope::leaf(key) == token);
                match (leaves.next(), leaves.next()) {
                    (Some(only), None) => archived(only),
                    _ => None,
                }
            })
    };
    found.ok_or_else(|| {
        let prefix = format!("{caller}{}", scope::PATH_SEPARATOR);
        let mine: Vec<&str> = projection
            .team
            .roster
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .map(String::as_str)
            .collect();
        let listed = if mine.is_empty() {
            "none".to_string()
        } else {
            mine.join(", ")
        };
        CoreError::Invalid(format!(
            "no subagent `{token}` on your team; your live subagents: {listed}. \
             Name one by the handle or session id the `task` result returned"
        ))
    })
}

impl TeamActor {
    /// Request every ending before publishing any terminal activity. The team
    /// leaves the live map immediately; episode owners remain indexed until done.
    pub(super) fn request_end(&self, ending: Ending) -> (bool, Vec<EpisodeDone>) {
        let mut state = self.lock();
        let first = !state.phase.killed();
        if first {
            state.phase = TeamPhase::Killed(ending.clone());
        }
        let TeamPhase::Killed(ending) = state.phase.clone() else {
            unreachable!()
        };
        if matches!(ending, Ending::Teardown | Ending::Shutdown(_)) {
            self.engine.cancel_turn(self.root, ending.cause());
        }
        let done = state
            .residents
            .iter_mut()
            .map(|(session, slot)| slot.request_end(&self.engine, *session, ending.clone()))
            .collect();
        self.cancel.cancel();
        (first, done)
    }

    pub(super) async fn kill(&self, ending: Ending) -> Vec<EpisodeDone> {
        let Some(supervisor) = self.supervisor.upgrade() else {
            return Vec::new();
        };
        let (first, done) = supervisor.request_team_end(self, ending.clone());
        if first {
            let (status, note) = match ending {
                Ending::Killed(note) => (RosterStatus::Failed, Some(note)),
                _ => (RosterStatus::Idle, None),
            };
            if let Err(error) = self
                .record_activity(None, scope::ROOT_HANDLE.into(), status, note)
                .await
            {
                tracing::error!(root = %self.root, %error, "team lead terminal activity failed");
            }
        }
        done
    }
}

impl ResidentSupervisor {
    /// The `archive` tool (0.41.0, replaces `kill`): stop one of `caller`'s
    /// subagents and archive it. Any live descendants of the target archive
    /// first (deepest first). For each member the in-flight turn is cancelled
    /// with `cause: archived` and waited for (up to [`DRAIN_DEADLINE`]; a turn
    /// still open then is closed like a drain straggler), then the member
    /// archives with reason `archived_by_parent`: degraded handoff, member row
    /// cancelled, claim released, off the live roster. Its session log and
    /// channel history stay readable, and mail to its handle (or its DM
    /// channel) wakes it under the same handle and session.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] with an actionable message when the target is
    /// unknown (listing the caller's live subagents), already archived, the
    /// team lead, or not a descendant of the caller.
    pub async fn archive_member(
        &self,
        caller: SessionId,
        target: &str,
        reason: &str,
    ) -> Result<ArchiveReceipt, CoreError> {
        let note = if reason.trim().is_empty() {
            "archived by parent".to_string()
        } else {
            format!("archived by parent: {}", reason.trim())
        };
        self.archive_member_noted(caller, target, note).await
    }

    fn request_team_end(&self, team: &TeamActor, ending: Ending) -> (bool, Vec<EpisodeDone>) {
        let mut teams = self.teams();
        if teams
            .get(&team.root)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), team))
        {
            teams.remove(&team.root);
        }
        team.request_end(ending)
    }

    pub(super) async fn reconcile_root(
        &self,
        root: SessionId,
        ending: Ending,
    ) -> Result<(), CoreError> {
        let projection = self.engine.read_projection_shared(root).await?;
        let mut members = projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| path.as_str() != scope::ROOT_HANDLE)
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect::<Vec<_>>();
        drop(projection);
        members.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        for (handle, member) in members {
            self.end_orphan(root, &handle, member, &ending).await?;
        }
        Ok(())
    }

    pub(super) async fn end_orphan(
        &self,
        root: SessionId,
        handle: &str,
        actor: SessionId,
        ending: &Ending,
    ) -> Result<(), CoreError> {
        let _write = self.claim_writes.lock().await;
        if self.team_with_resident(actor).is_some() {
            return Err(CoreError::Invalid(
                "a live episode owns this archive; request its ending".into(),
            ));
        }
        let handoff = self.engine.degraded_terminal_handoff(actor).await;
        let (reason, status, note) = ending.forced_fields();
        let (_, envelopes) = self
            .engine
            .store()
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                root,
                handle,
                hya_store::ResidentArchive {
                    actor,
                    generation: handoff.generation,
                    doc: handoff.doc,
                    degraded: handoff.degraded,
                    reason,
                    note,
                    member_status: status,
                    report: None,
                    check_gate: false,
                    expected_work: None,
                },
            )
            .await?;
        self.engine.publish_resident_finalization(envelopes);
        Ok(())
    }

    /// Request an archive and wait for the episode's own ending. Dropping
    /// this wait never cancels the episode task.
    pub async fn end_member(&self, root: SessionId, handle: &str) -> Result<(), CoreError> {
        let prefix = format!("{handle}{}", scope::PATH_SEPARATOR);
        // Capture completion handles before any await: deletion may remove both
        // the roster and the episodes while we discover durable descendants.
        let (live, mut captured) = {
            let episodes = self
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut live = None;
            let mut captured = HashMap::new();
            for (session, team) in episodes.iter().filter(|(_, team)| team.root == root) {
                let mut state = team.lock();
                if let Some(slot) = state.residents.get_mut(session)
                    && (slot.handle == handle || slot.handle.starts_with(&prefix))
                {
                    // Suppress later wakes without letting the parent archive
                    // before its descendants. Its claim fences in-flight spawns.
                    slot.user_stopped = true;
                    self.engine.cancel_turn(*session, FinishCause::Archived);
                    if slot.handle == handle {
                        live = Some(*session);
                    }
                    captured.insert(slot.handle.clone(), (*session, slot.done.subscribe()));
                }
            }
            (live, captured)
        };
        let projection = self.engine.read_projection_shared(root).await?;
        let child = live.or_else(|| {
            projection
                .team
                .roster
                .get(handle)
                .map(|entry| entry.session)
        });
        let mut descendants: BTreeMap<_, _> = projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect();
        let archived = projection.team.archived.contains_key(handle);
        drop(projection);
        descendants.extend(
            captured
                .iter()
                .filter(|(path, _)| path.starts_with(&prefix))
                .map(|(path, (session, _))| (path.clone(), *session)),
        );
        let mut descendants: Vec<_> = descendants.into_iter().collect();
        descendants.sort_by_key(|(path, _)| {
            std::cmp::Reverse(path.matches(scope::PATH_SEPARATOR).count())
        });
        #[cfg(test)]
        {
            let pause = self
                .ending_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        for (path, session) in descendants {
            self.stop_and_archive_live(
                root,
                &path,
                session,
                "Workflow run cancelled",
                captured.remove(&path).map(|(_, done)| done),
            )
            .await?;
        }
        if let Some(child) = child {
            self.stop_and_archive_live(
                root,
                handle,
                child,
                "Workflow run cancelled",
                captured.remove(handle).map(|(_, done)| done),
            )
            .await?;
        } else if !archived {
            return Err(CoreError::Invalid(format!("unknown resident `{handle}`")));
        }
        // Parent claim release fences new direct children. Sweep anything that
        // committed before release, including descendants of those late children.
        loop {
            let projection = self.engine.read_projection_shared(root).await?;
            let mut descendants: Vec<_> = projection
                .team
                .roster
                .iter()
                .filter(|(path, _)| path.starts_with(&prefix))
                .map(|(path, entry)| (path.clone(), entry.session))
                .collect();
            drop(projection);
            if descendants.is_empty() {
                break;
            }
            descendants.sort_by_key(|(path, _)| {
                std::cmp::Reverse(path.matches(scope::PATH_SEPARATOR).count())
            });
            for (path, session) in descendants {
                self.stop_and_archive_live(root, &path, session, "Workflow run cancelled", None)
                    .await?;
            }
        }
        Ok(())
    }

    /// [`archive_member`](Self::archive_member) with the archive `note`
    /// recorded on the member row and handoff, naming who archived it.
    pub(super) async fn archive_member_noted(
        &self,
        caller: SessionId,
        target: &str,
        note: String,
    ) -> Result<ArchiveReceipt, CoreError> {
        let (root, _) = self.engine.session_lineage(caller).await?;
        let caller_path = self.engine.resolve_handle(root, caller).await?;
        let projection = self.engine.read_projection_shared(root).await?;
        let resolved = resolve_member_target(&projection, root, &caller_path, target)?;
        if resolved.session == root || resolved.handle == scope::ROOT_HANDLE {
            return Err(CoreError::Invalid(
                "`main` is the team lead; the lead is never archived".to_string(),
            ));
        }
        if !resolved.live {
            return Err(CoreError::Invalid(format!(
                "`{}` is already archived; send it mail (`send` to `{}`) to wake it",
                resolved.handle, resolved.handle
            )));
        }
        let prefix = format!("{}{}", resolved.handle, scope::PATH_SEPARATOR);
        if !resolved
            .handle
            .starts_with(&format!("{caller_path}{}", scope::PATH_SEPARATOR))
        {
            let parent = scope::parent_path(&resolved.handle).unwrap_or(scope::ROOT_HANDLE);
            return Err(CoreError::Invalid(format!(
                "`{}` is not one of your subagents; only its parent `{parent}` or an ancestor can archive it",
                resolved.handle
            )));
        }
        let mut descendants: Vec<(String, SessionId)> = projection
            .team
            .roster
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(path, entry)| (path.clone(), entry.session))
            .collect();
        descendants.sort_by_key(|(path, _)| {
            std::cmp::Reverse(path.matches(scope::PATH_SEPARATOR).count())
        });
        drop(projection);
        for (handle, session) in &descendants {
            self.stop_and_archive_live(root, handle, *session, &note, None)
                .await?;
        }
        let cancelled_turn = self
            .stop_and_archive_live(root, &resolved.handle, resolved.session, &note, None)
            .await?;
        Ok(ArchiveReceipt {
            handle: resolved.handle,
            session: resolved.session,
            cancelled_turn,
            descendants: descendants.into_iter().map(|(handle, _)| handle).collect(),
        })
    }

    /// Stop one live member and archive it (`archived_by_parent`); `true`
    /// when an in-flight turn was cancelled.
    async fn stop_and_archive_live(
        &self,
        root: SessionId,
        handle: &str,
        child: SessionId,
        note: &str,
        captured: Option<EpisodeDone>,
    ) -> Result<bool, CoreError> {
        if child == root || handle == scope::ROOT_HANDLE {
            return Err(CoreError::Invalid("the team lead is never archived".into()));
        }
        let write = self.claim_writes.lock().await;
        let requested = self.team_with_resident(child).and_then(|team| {
            let mut state = team.lock();
            state.residents.get_mut(&child).map(|slot| {
                let cancelled = self.engine.turn_active(child);
                (
                    slot.request_end(&self.engine, child, Ending::Archived(note.into())),
                    cancelled,
                )
            })
        });
        if let Some((done, cancelled)) = requested {
            drop(write);
            return await_episode(done).await.map(|()| cancelled);
        }
        if let Some(done) = captured {
            drop(write);
            return await_episode(done).await.map(|()| false);
        }
        let handoff = self.engine.degraded_terminal_handoff(child).await;
        let (_, envelopes) = self
            .engine
            .store()
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                root,
                handle,
                hya_store::ResidentArchive {
                    actor: child,
                    generation: handoff.generation,
                    doc: handoff.doc,
                    degraded: handoff.degraded,
                    reason: ArchiveReason::ArchivedByParent,
                    member_status: MemberRunStatus::Cancelled,
                    note: note.into(),
                    report: None,
                    check_gate: false,
                    expected_work: None,
                },
            )
            .await?;
        self.engine.publish_resident_finalization(envelopes);
        if let Some(team) = self.teams().get(&root).cloned() {
            team.wake_activated_parent(handle);
        }
        Ok(false)
    }
}
