use super::*;

impl TeamActor {
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
        let final_text = last_assistant_text(&*self.engine.read_projection_shared(session).await?);
        let root = self.engine.read_projection_shared(self.root).await?;
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

    /// A member whose turn ended normally with a text answer, no accepted
    /// report and nothing queued for it reports that answer (outcome `done`)
    /// through the same pending-report path as the `report` tool. Called
    /// before the slot goes idle, so a parent's `wait` never sees a stall in
    /// between. Unread mail or live children (the report gate), an empty
    /// answer, or a Workflow activation (which settles on its final text)
    /// leave the member idle as before.
    pub(super) async fn report_final_answer(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        handle: &str,
    ) -> Result<(), CoreError> {
        let eligible = |slot: &Episode| {
            slot.claim.as_ref() == claim
                && !slot.is_main
                && !slot.user_stopped
                && slot.ending.is_none()
                && slot.report.is_none()
                && slot.activation.is_none()
        };
        if !self.lock().residents.get(&session).is_some_and(eligible) {
            return Ok(());
        }
        let answer = last_assistant_text(&*self.engine.read_projection_shared(session).await?);
        let answer = answer.trim();
        if answer.is_empty() {
            return Ok(());
        }
        match super::report::report_gate_projection(&self.engine, self.root, handle).await {
            Ok(_) => {}
            Err(CoreError::Invalid(_)) => return Ok(()),
            Err(error) => return Err(error),
        }
        if let Some(slot) = self
            .lock()
            .residents
            .get_mut(&session)
            .filter(|slot| eligible(slot))
        {
            slot.report = Some(PendingReport {
                outcome: ReportOutcome::Done,
                report: answer.to_string(),
            });
        }
        Ok(())
    }

    pub(super) fn pending_report(
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

    pub(super) fn complete_episode(
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

/// The text parts of the session's last assistant message ("" when none).
fn last_assistant_text(projection: &hya_proto::Projection) -> String {
    projection
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
        .unwrap_or_default()
}
