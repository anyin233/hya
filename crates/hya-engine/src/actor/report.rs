use super::*;

/// A report accepted from the `report` tool, executed once the actor is at
/// rest (ADR-0015): the gate was checked at call time; the archive runs after
/// the current turn ends and re-checks.
#[derive(Clone)]
pub(super) struct PendingReport {
    pub(super) outcome: ReportOutcome,
    pub(super) report: String,
}

/// Projection-level report gate (ADR-0015): live roster row, inbox drained,
/// no live children. Pure read; shared by the tool pre-check, the direct
/// archive path, and the at-rest pending-report execution.
pub(super) async fn report_gate_projection(
    engine: &Arc<SessionEngine>,
    root: SessionId,
    canonical: &str,
) -> Result<Arc<hya_proto::Projection>, CoreError> {
    let projection = engine.read_projection_shared(root).await?;
    let team = hya_tool::extension::team_projection(&projection);
    let entry = team.roster.get(canonical).ok_or_else(|| {
        CoreError::Invalid(format!("resident `{canonical}` is not on the roster"))
    })?;
    // Mail the current resident wake injected into this turn was read there.
    let read_through = entry.resident_work.map_or(entry.resident_cursor, |work| {
        entry.resident_cursor.max(work.inbox_through)
    });
    if team
        .unread_incoming_mail(canonical, read_through)
        .next()
        .is_some()
    {
        return Err(CoreError::Invalid(unread_mail_rejection(
            &team,
            canonical,
            read_through,
        )));
    }
    let live_children = team
        .roster
        .keys()
        .any(|path| scope::parent_path(path) == Some(canonical));
    if live_children {
        return Err(CoreError::Invalid(format!(
            "report rejected: `{canonical}` still has live children; archive them first"
        )));
    }
    Ok(projection)
}

/// Actionable unread-mail gate error: names each channel holding unread mail
/// and the exact call that reads it, then the reply and `wait` paths. A real
/// run's subagent guessed `#main/scout-1`, `#mail`, `#inbox`… because the old
/// error named no channel.
fn unread_mail_rejection(
    team: &hya_tool::extension::TeamProjection,
    canonical: &str,
    read_through: u64,
) -> String {
    let unread: Vec<_> = team
        .unread_incoming_mail(canonical, read_through)
        .collect();
    let mut per_channel: BTreeMap<String, usize> = BTreeMap::new();
    let mut notices = 0usize;
    for message in &unread {
        let channel = match &message.to {
            MailEndpoint::Channel(channel) => Some(channel.clone()),
            MailEndpoint::Handle(_) => None,
        };
        match channel {
            Some(channel) => *per_channel.entry(channel).or_default() += 1,
            None => notices += 1,
        }
    }
    let mut holders: Vec<String> = per_channel
        .iter()
        .map(|(channel, count)| format!("#{channel} ({count})"))
        .collect();
    if notices > 0 {
        holders.push(format!("{notices} harness notice(s)"));
    }
    let reads: Vec<String> = per_channel
        .iter()
        .map(|(channel, count)| {
            if *count > 1 {
                format!("`read channel://{channel}?last={count}`")
            } else {
                format!("`read channel://{channel}`")
            }
        })
        .collect();
    let read_hint = if reads.is_empty() {
        "Call `list_channel`: pending notices arrive with the next tool result as `[NEW MAIL]`"
            .to_string()
    } else {
        format!(
            "Read it with {} (`list_channel` lists every channel with unread counts)",
            reads.join(" and ")
        )
    };
    format!(
        "report rejected: `{canonical}` has {} unread mail message(s) on {}; answer them first. \
         {read_hint}, reply with `send` if the sender needs an answer, then call `report` \
         again — or call `wait` to block until more mail arrives.",
        unread.len(),
        holders.join(", ")
    )
}

impl ResidentSupervisor {
    /// Projection-only report-gate check (ADR-0015): the handle must be live,
    /// its inbox drained, and it must have no live children. The tool surface
    /// calls this for fast feedback; the episode re-checks it at rest.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] for each gate failure.
    pub async fn report_gate(&self, root: SessionId, handle: &str) -> Result<String, CoreError> {
        let canonical = handle.to_string();
        self.report_gate_state(root, &canonical).await?;
        Ok(canonical)
    }

    /// Shared gate body over an already-canonical handle; returns the
    /// projection it validated.
    async fn report_gate_state(
        &self,
        root: SessionId,
        canonical: &str,
    ) -> Result<Arc<hya_proto::Projection>, CoreError> {
        report_gate_projection(&self.engine, root, canonical).await
    }

    /// The `report` tool entry (ADR-0015): gate feedback now, archive once the
    /// actor is at rest. Works mid-turn — the accepted report parks on the
    /// slot and executes after the current turn ends (gate re-checked there).
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] for gate failures or a dead handle.
    pub async fn submit_report(
        &self,
        root: SessionId,
        handle: &str,
        outcome: ReportOutcome,
        report: String,
    ) -> Result<(), CoreError> {
        // Fast gate feedback against durable truth (unread mail / children).
        let canonical = self.report_gate(root, handle).await?;
        let team =
            self.teams().get(&root).cloned().ok_or_else(|| {
                CoreError::Invalid(format!("resident `{handle}` has no live team"))
            })?;
        {
            let mut state = team.lock();
            let slot = state
                .residents
                .iter_mut()
                .find(|(_, slot)| slot.handle == canonical)
                .map(|(_, slot)| slot)
                .ok_or_else(|| CoreError::Invalid(format!("resident `{handle}` is not live")))?;
            if slot.is_main {
                return Err(CoreError::Invalid(
                    "the main actor cannot report".to_string(),
                ));
            }
            slot.report = Some(PendingReport { outcome, report });
            slot.notify.notify_one();
        }
        Ok(())
    }
}
