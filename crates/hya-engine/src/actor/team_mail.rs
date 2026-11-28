use super::inbox::Inbox;
use super::*;

impl TeamActor {
    /// Resolve a `MailSent`'s recipient sessions, EXCLUDING the sender's own handle
    /// (self-wake avoidance) and any handle that is not a registered resident.
    async fn recipients(
        &self,
        from: &str,
        to: &MailEndpoint,
    ) -> Vec<(SessionId, Option<ActorClaim>)> {
        let Ok(projection) = self.engine.read_projection_shared(self.root).await else {
            return Vec::new();
        };
        let team = hya_tool::extension::team_projection(&projection);
        let handles = hya_agent_channels::mail_recipient_handles(&team, from, to);
        let st = self.lock();
        handles
            .into_iter()
            .filter_map(|handle| {
                team.roster
                    .get(&handle)
                    .map(|entry| entry.session)
                    .and_then(|session| {
                        st.residents.get(&session).map(|slot| (session, slot.claim))
                    })
            })
            .collect()
    }

    /// Handle one `MailSent` for this team: charge the message budget, count the
    /// work, and deliver a wake to every resident recipient (never the sender).
    pub(super) async fn on_mail(&self, from: &str, to: &MailEndpoint) {
        #[cfg(test)]
        {
            let pause = self.supervisor.upgrade().and_then(|supervisor| {
                let mut pause = supervisor
                    .mail_pause
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if pause
                    .as_ref()
                    .is_some_and(|(root, _, _)| *root == self.root)
                {
                    pause.take()
                } else {
                    None
                }
            });
            if let Some((_, entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let exceeded = {
            let mut state = self.lock();
            if state.phase.killed() {
                return;
            }
            state.messages_used = state.messages_used.saturating_add(1);
            self.engine.governor().is_some_and(|gov| {
                let limit = gov.limits().per_team_message_budget;
                limit != 0 && state.messages_used > limit
            })
        };
        if exceeded {
            self.kill(Ending::Killed("per-team message budget exceeded".into()))
                .await;
            return;
        }
        {
            let mut st = self.lock();
            if st.phase.killed() {
                return;
            }
            st.work_seq = st.work_seq.saturating_add(1);
        }
        let recipients = self.recipients(from, to).await;
        let Ok(projection) = self.engine.read_projection_shared(self.root).await else {
            return;
        };
        let mut consumed = Vec::new();
        {
            let mut st = self.lock();
            for (session, claim) in recipients {
                if let Some(slot) = st
                    .residents
                    .get_mut(&session)
                    .filter(|slot| slot.claim == claim)
                {
                    let inbox = Inbox::new(&projection, &slot.handle);
                    let inbox_len = inbox.len();
                    let cursor = slot.cursor.max(inbox.durable_cursor().unwrap_or(0));
                    let allowed =
                        crate::channel_policy::from_binding(&slot.binding).is_ok_and(|policy| {
                            inbox
                                .resident_mail(cursor, slot.agent.name.as_str(), Some(&policy))
                                .next()
                                .is_some()
                        });
                    if allowed {
                        slot.pending = true;
                        slot.notify.notify_one();
                    } else if !slot.pending && inbox_len > cursor {
                        consumed.push((session, slot.claim, slot.handle.clone(), inbox_len));
                    }
                }
            }
        }
        drop(projection);
        for (session, claim, handle, through) in consumed {
            if self
                .engine
                .emit_for_actor(
                    claim.as_ref(),
                    self.root,
                    ExtensionEvent::MailConsumed {
                        session: self.root,
                        handle: handle.clone(),
                        through: through as u64,
                    }
                    .into_event(),
                )
                .await
                .is_ok()
                && let Some(slot) = self.lock().residents.get_mut(&session)
                && slot.handle == handle
                && slot.claim == claim
            {
                slot.cursor = slot.cursor.max(through);
                if slot.activation.is_some() {
                    slot.notify.notify_one();
                }
            }
        }
    }

    /// Recover from a bus lag: a dropped `MailSent` never ran [`on_mail`], so the
    /// affected slot's `pending` was never set and a bare notify would just idle
    /// again. Instead, compare each resident's inbox length against its cursor and
    /// re-arm (set `pending` + notify) any slot with genuinely unconsumed mail — so
    /// no wake is lost, and quiescent slots with nothing new are left parked.
    pub(super) async fn recover(&self) {
        let Ok(projection) = self.engine.read_projection_shared(self.root).await else {
            return;
        };
        let mut consumed = Vec::new();
        {
            let mut st = self.lock();
            if st.phase.killed() {
                return;
            }
            st.routing_mail.retain(|seq| *seq > projection.last_seq);
            for slot in st.residents.values_mut() {
                let inbox = Inbox::new(&projection, &slot.handle);
                if inbox.len() <= slot.cursor {
                    continue;
                }
                let policy = crate::channel_policy::from_binding(&slot.binding).ok();
                let has_deliverable = inbox
                    .resident_mail(slot.cursor, slot.agent.name.as_str(), policy.as_ref())
                    .next()
                    .is_some();
                if has_deliverable {
                    slot.pending = true;
                    slot.notify.notify_one();
                } else {
                    consumed.push((slot.claim, slot.handle.clone(), inbox.len()));
                }
            }
        }
        drop(projection);
        for (claim, handle, through) in consumed {
            if self
                .engine
                .emit_for_actor(
                    claim.as_ref(),
                    self.root,
                    ExtensionEvent::MailConsumed {
                        session: self.root,
                        handle: handle.clone(),
                        through: through as u64,
                    }
                    .into_event(),
                )
                .await
                .is_ok()
                && let Some(slot) = self
                    .lock()
                    .residents
                    .values_mut()
                    .find(|slot| slot.handle == handle && slot.claim == claim)
            {
                slot.cursor = slot.cursor.max(through);
                if slot.activation.is_some() {
                    slot.notify.notify_one();
                }
            }
        }
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor.retire_team(self);
        }
    }
}
