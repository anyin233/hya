use super::*;

impl SessionEngine {
    /// Recover durable resident state after restart and report next work.
    ///
    /// # Errors
    /// Returns store/validation failures.
    pub async fn recover_resident_actor(
        &self,
        recovered: &hya_store::RecoveredActorClaim,
        root: SessionId,
        handle: &str,
    ) -> Result<ResidentRecoveryReport, CoreError> {
        let work = self
            .recover_resident_actor_durable(recovered, root, handle)
            .await?;
        Ok(ResidentRecoveryReport {
            work: map_recovered_resident_work(work),
        })
    }
}

fn map_recovered_resident_work(work: hya_store::RecoveredResidentWork) -> ResidentRecovery {
    match work {
        hya_store::RecoveredResidentWork::Idle => ResidentRecovery::Idle,
        hya_store::RecoveredResidentWork::Queued { inbox_cursor } => {
            ResidentRecovery::Queued { inbox_cursor }
        }
        hya_store::RecoveredResidentWork::AbortedRunning {
            inbox_cursor,
            queued_after,
        } => ResidentRecovery::AbortedRunning {
            inbox_cursor,
            queued_after,
        },
    }
}

impl Drop for ResidentSupervisor {
    fn drop(&mut self) {
        self.listener_stop.cancel();
        let teams = self
            .teams
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let episodes = self
            .episodes
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for team in teams.values().chain(episodes.values()) {
            if !team.lock().phase.killed() {
                team.request_end(Ending::Shutdown(FinishCause::Shutdown));
            }
        }
    }
}

/// Single-active-turn delivery: when any turn on a team slot's session ends,
/// the owning team re-wakes that slot if work was deferred behind the turn and
/// re-checks quiescence.
impl TurnBoundaryObserver for ResidentSupervisor {
    fn mail_mark(&self, session: SessionId) -> Option<u64> {
        self.team_with_resident(session)
            .map(|team| team.lock().work_seq)
    }

    fn mail_consumed(&self, session: SessionId, mark: u64) {
        if let Some(team) = self.team_with_resident(session) {
            team.on_mail_consumed(session, mark);
        }
    }

    fn turn_released(&self, session: SessionId) {
        if let Some(team) = self.team_with_resident(session) {
            team.on_turn_released(session);
        }
    }

    fn turn_started(&self, session: SessionId) {
        if let Some(team) = self.teams().get(&session).cloned() {
            team.on_lead_turn_started(session);
        }
    }

    fn turn_failed(&self, session: SessionId) {
        if let Some(team) = self.teams().get(&session).cloned() {
            team.on_lead_turn_failed(session);
        }
    }
}

/// A downward mail to an archived direct child revives it (ADR-0015): the
/// supervisor is the engine's [`ArchiveReviver`].
#[async_trait::async_trait]
impl ArchiveReviver for ResidentSupervisor {
    fn has_episode(&self, session: SessionId) -> bool {
        self.team_with_resident(session).is_some_and(|team| {
            team.lock()
                .residents
                .get(&session)
                .is_some_and(|slot| slot.claim.is_some())
        })
    }

    async fn reconcile_orphan(
        &self,
        root: SessionId,
        handle: &str,
        archive: hya_store::ResidentArchive,
    ) -> Result<hya_store::ResidentArchiveOutcome, CoreError> {
        let _write = self.claim_writes.lock().await;
        if self.team_with_resident(archive.actor).is_some() {
            return Err(CoreError::Invalid(
                "a live episode owns this archive; request its ending".into(),
            ));
        }
        let (outcome, envelopes) = self
            .engine
            .store()
            .finalize_resident_archive(
                hya_store::ResidentArchiveOwner::Takeover,
                root,
                handle,
                archive,
            )
            .await?;
        self.engine.publish_resident_finalization(envelopes);
        Ok(outcome)
    }

    async fn archive_member(
        &self,
        caller: SessionId,
        target: &str,
        reason: &str,
    ) -> Result<ArchiveReceipt, CoreError> {
        ResidentSupervisor::archive_member(self, caller, target, reason).await
    }

    async fn archive_member_for_user(
        &self,
        root: SessionId,
        target: &str,
    ) -> Result<ArchiveReceipt, CoreError> {
        self.archive_member_noted(root, target, "archived by the user".to_string())
            .await
    }

    fn stop_for_user(&self, session: SessionId) -> Vec<SessionId> {
        self.team_of(session)
            .map(|team| team.stop_for_user(session))
            .unwrap_or_default()
    }

    fn resume_for_user(&self, session: SessionId) {
        if let Some(team) = self.team_of(session) {
            team.resume_for_user(session);
        }
    }

    fn stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        let team = self.teams().get(&root).cloned();
        team.is_some_and(|team| team.handle_stopped_by_user(handle))
    }

    fn session_stopped_by_user(&self, session: SessionId) -> bool {
        self.team_of(session).is_some_and(|team| {
            team.lock()
                .residents
                .get(&session)
                .is_some_and(|slot| slot.user_stopped)
        })
    }

    async fn teardown_root(&self, root: SessionId) -> Result<(), CoreError> {
        let team = self.teams().get(&root).cloned();
        let mut done = match team {
            Some(team) => team.kill(Ending::Teardown).await,
            None => Vec::new(),
        };
        // Wait for any claim commit already in flight to install into its
        // killed team, then request all generations, including old killed teams.
        {
            let _write = self.claim_writes.lock().await;
            let teams = self.episode_teams(root);
            for team in teams {
                done.extend(team.kill(Ending::Teardown).await);
            }
        }
        let results = futures::future::join_all(done.into_iter().map(await_episode)).await;
        // A failed ending has become an orphan. Reconcile it under the same
        // claim-write boundary; never take a member's turn lease from outside.
        self.reconcile_root(root, Ending::Teardown).await?;
        for result in results {
            if let Err(error) = result {
                tracing::error!(%root, %error, "root teardown reconciled a failed ending");
            }
        }
        Ok(())
    }
}

impl ResidentSupervisor {
    /// Build the supervisor and spawn its bus listener. The returned `Arc` is the
    /// registration handle used by the spawn path.
    ///
    /// The bus is subscribed *synchronously* here, before returning, so any mail
    /// published after `start` is guaranteed to be observed (no lost-wake race with
    /// the listener task's startup).
    #[must_use]
    pub fn start(engine: Arc<SessionEngine>) -> Arc<Self> {
        Self::start_with_owner(engine, OwnerRunId::new())
    }

    #[must_use]
    /// Start a supervisor owned by `owner_run_id` (cancels with the owner).
    pub fn start_with_owner(engine: Arc<SessionEngine>, owner_run_id: OwnerRunId) -> Arc<Self> {
        let rx = engine.bus().subscribe();
        let supervisor = Arc::new_cyclic(|self_ref| Self {
            engine,
            owner_run_id,
            teams: Mutex::new(HashMap::new()),
            root_drivers: Mutex::new(HashMap::new()),
            episodes: Mutex::new(HashMap::new()),
            claim_writes: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            registration_pause: Mutex::new(None),
            #[cfg(test)]
            ending_pause: Mutex::new(None),
            #[cfg(test)]
            activation_lock_pause: Mutex::new(None),
            #[cfg(test)]
            revive_ready_pause: Mutex::new(None),
            #[cfg(test)]
            mail_pause: Mutex::new(None),
            self_ref: self_ref.clone(),
            recovered: Mutex::new(Vec::new()),
            tasks: TaskTracker::new(),
            listener_stop: CancellationToken::new(),
            listener: Mutex::new(None),
            wait_stalls: Mutex::new(HashMap::new()),
        });
        // The supervisor is the engine's revive seam (ADR-0015): wire it before
        // the listener task starts so no mail send can race the installation.
        supervisor.engine.set_reviver(Arc::downgrade(&supervisor));
        // Turn boundaries deliver wakes deferred by the single-active-turn
        // invariant (mail/quiescence owed to a session whose turn was running).
        let observer: Arc<dyn TurnBoundaryObserver> = supervisor.clone();
        supervisor
            .engine
            .set_turn_observer(Arc::downgrade(&observer));
        let listener = Arc::downgrade(&supervisor);
        let stop = supervisor.listener_stop.clone();
        *supervisor
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(tokio::spawn(Self::run_bus(listener, stop, rx)));
        supervisor
    }

    pub(super) fn teams(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Arc<TeamActor>>> {
        match self.teams.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The bus listener only routes; each team processes its FIFO independently.
    /// Lag recovery enters those same queues, behind already routed envelopes.
    async fn run_bus(
        supervisor: Weak<Self>,
        stop: CancellationToken,
        mut rx: tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
    ) {
        loop {
            let received = tokio::select! {
                biased;
                () = stop.cancelled() => break,
                received = rx.recv() => received,
            };
            let Some(supervisor) = supervisor.upgrade() else {
                break;
            };
            match received {
                Ok(envelope) => supervisor.route_envelope(envelope),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    for team in supervisor.teams().values() {
                        team.enqueue(TeamEvent::Recover);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    pub(super) fn route_envelope(&self, envelope: hya_proto::Envelope) {
        match envelope.event {
            Event::MailSent {
                session: root,
                from,
                to,
                ..
            } => {
                // Select and enqueue under the map lock, excluding retirement.
                if let Some(team) = self.teams().get(&root) {
                    team.enqueue(TeamEvent::Mail {
                        seq: envelope.seq,
                        from,
                        to,
                    });
                }
            }
            Event::AgentHeartbeat { .. } => {}
            event => {
                if let Some(actor) = event.session()
                    && let Some(team) = self.team_with_resident(actor)
                {
                    team.enqueue(TeamEvent::Activity(actor));
                }
            }
        }
    }

    /// Look up a member by episode identity (including killed generations),
    /// or a lead by its current team's root. Neither lookup scans teams.
    pub(super) fn team_with_resident(&self, session: SessionId) -> Option<Arc<TeamActor>> {
        self.episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .cloned()
            .or_else(|| self.teams().get(&session).cloned())
    }

    /// Reserve under the map lock so retirement cannot cancel a team between
    /// selection and episode installation. Explicit kills still end registrations.
    pub(super) async fn team_for(&self, root: SessionId) -> Result<TeamRegistration, CoreError> {
        if let Some(team) = self.teams().get(&root).cloned() {
            return Ok(TeamRegistration::new(team));
        }
        let main = self.resolve_main_activation(root).await?;
        let mut teams = self.teams();
        let team = teams
            .entry(root)
            .or_insert_with(|| self.new_team(root, main));
        Ok(TeamRegistration::new(team.clone()))
    }

    /// Hold sole ownership of root continuations until the returned guard drops.
    pub async fn root_driver_activity(
        &self,
        session: SessionId,
    ) -> Result<RootDriverActivity, CoreError> {
        let (root, _) = self.engine.session_lineage(session).await?;
        let team = self.team_for(root).await?;
        {
            let state = team.lock();
            if state.root_driver.runs.load(Ordering::Acquire) == 0 && !self.engine.turn_active(root)
            {
                state.root_driver.autonomous.store(false, Ordering::Release);
            }
            state.root_driver.runs.fetch_add(1, Ordering::AcqRel);
        }
        Ok(RootDriverActivity(team))
    }

    fn new_team(&self, root: SessionId, main: MainActivation) -> Arc<TeamActor> {
        let root_driver = {
            let mut drivers = self
                .root_drivers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = drivers.entry(root).or_default();
            if let Some(active) = entry.upgrade() {
                active
            } else {
                let active = Arc::new(RootDriverState {
                    runs: AtomicUsize::new(0),
                    autonomous: AtomicBool::new(true),
                });
                *entry = Arc::downgrade(&active);
                active
            }
        };
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let team = Arc::new(TeamActor::new(
            root,
            self.engine.clone(),
            self.self_ref.clone(),
            root_driver,
            self.tasks.clone(),
            events,
        ));
        self.tasks.spawn(team.clone().run_events(receiver));
        team.install_main(main);
        team
    }

    fn team_for_with_main(&self, root: SessionId, main: MainActivation) {
        let mut teams = self.teams();
        let team = teams
            .entry(root)
            .or_insert_with(|| self.new_team(root, main));
        let state = team.lock();
        if state.root_driver.runs.load(Ordering::Acquire) == 0 {
            state.root_driver.autonomous.store(true, Ordering::Release);
        }
    }

    pub(super) fn retire_team(&self, team: &TeamActor) {
        let mut teams = self.teams();
        if !teams
            .get(&team.root)
            .is_some_and(|current| std::ptr::eq(current.as_ref(), team))
        {
            return;
        }
        let mut state = team.lock();
        if state.phase.killed()
            || state.busy != 0
            || state.registrations != 0
            || !state.routing_mail.is_empty()
            || state.queued_wakes != 0
            || self.engine.turn_active(team.root)
            || state
                .residents
                .values()
                .any(|slot| !slot.is_main || slot.has_work() || slot.report.is_some())
        {
            return;
        }
        teams.remove(&team.root);
        for (session, slot) in &mut state.residents {
            slot.request_end(
                &self.engine,
                *session,
                Ending::Shutdown(FinishCause::Shutdown),
            );
        }
        team.cancel.cancel();
    }

    fn episode_teams(&self, root: SessionId) -> Vec<Arc<TeamActor>> {
        let mut teams = Vec::new();
        for team in self
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            if team.root == root && !teams.iter().any(|current| Arc::ptr_eq(current, team)) {
                teams.push(team.clone());
            }
        }
        teams
    }

    pub(super) async fn resolve_main_activation(
        &self,
        root: SessionId,
    ) -> Result<MainActivation, CoreError> {
        let projection = self.engine.read_projection_shared(root).await?;
        let session = &projection.session;
        let invalid =
            || CoreError::Invalid(format!("root session {root} lacks activation context"));
        let mut base = AgentSpec {
            name: session.agent.clone().ok_or_else(invalid)?,
            model: session.model.clone().ok_or_else(invalid)?,
            workdir: session.workdir.clone().ok_or_else(invalid)?.into(),
            system_prompt: self.engine.base_system_prompt().into(),
            reasoning: None,
        };
        drop(projection);
        let binding = self
            .engine
            .bind_session_runtime(root, &base.workdir)
            .await?;
        if self
            .engine
            .fall_back_from_missing_agent(root, Some(&base.name), &binding)
            .await?
        {
            let projection = self.engine.read_projection_shared(root).await?;
            base.name = projection.session.agent.clone().ok_or_else(invalid)?;
        }
        let agent = self
            .engine
            .agent_spec_for_binding(&binding, &base, base.name.as_str())?;
        let agents = self
            .engine
            .agent_roster_for_binding(&binding, base.name.as_str())?;
        let resources = self
            .engine
            .agent_resource_policy_for_binding(&binding, base.name.as_str())?;
        Ok(MainActivation {
            agent,
            binding,
            agents,
            resources,
            guidance: None,
        })
    }

    /// Graceful stop (SIGINT/SIGTERM, user cancel, end of a one-shot run,
    /// `serve` shutdown): cancel every in-flight turn in every session with
    /// `cause` and wait up to `deadline` for them to close their messages
    /// ([`SessionEngine::drain_turns`]), then make every team member terminal
    /// and park each lead. New turns are refused from the first step on.
    pub async fn drain(&self, cause: FinishCause, deadline: Duration) -> TurnDrainReport {
        self.engine.begin_drain(cause);
        let endings = async {
            let teams: Vec<Arc<TeamActor>> = self.teams().values().cloned().collect();
            let mut roots = teams.iter().map(|team| team.root).collect::<Vec<_>>();
            for team in self
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
            {
                if !roots.contains(&team.root) {
                    roots.push(team.root);
                }
            }
            let mut orphans = Vec::new();
            match self.engine.store().active_actor_ids().await {
                Ok(actors) => {
                    for actor in actors {
                        if self.team_with_resident(actor).is_some() {
                            continue;
                        }
                        match self.engine.session_lineage(actor).await {
                            Ok((root, _)) => match self.engine.read_projection_shared(root).await {
                                Ok(projection) => {
                                    if let Some((handle, _)) =
                                        projection.team.roster.iter().find(|(handle, entry)| {
                                            handle.as_str() != scope::ROOT_HANDLE
                                                && entry.session == actor
                                        })
                                    {
                                        orphans.push((root, handle.clone(), actor));
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(%root, %error, "shutdown could not read orphan roster")
                                }
                            },
                            Err(error) => {
                                tracing::error!(%actor, %error, "shutdown could not resolve orphan root")
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "shutdown could not read active actor claims")
                }
            }
            let mut done = Vec::new();
            for team in teams {
                done.extend(team.kill(Ending::Shutdown(cause)).await);
            }
            for root in &roots {
                for team in self.episode_teams(*root) {
                    done.extend(team.kill(Ending::Shutdown(cause)).await);
                }
            }
            for result in futures::future::join_all(done.into_iter().map(await_episode)).await {
                if let Err(error) = result {
                    tracing::error!(%error, "resident shutdown ending failed");
                }
            }
            // Only actors already orphaned at entry are reconciled here. A
            // failed write in this drain leaves an orphan for the next request.
            for (root, handle, actor) in orphans {
                if let Err(error) = self
                    .end_orphan(root, &handle, actor, &Ending::Shutdown(cause))
                    .await
                {
                    tracing::error!(%root, %actor, %error, "resident shutdown orphan reconciliation failed");
                }
            }
        };
        // The turn drain and each episode's own cancellation deadline run
        // together, so shutdown pays one drain deadline, not two in series.
        let (report, ()) = tokio::join!(self.engine.drain_turns(cause, deadline), endings);
        self.listener_stop.cancel();
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(listener) = listener
            && let Err(error) = listener.await
        {
            tracing::error!(%error, "resident bus listener failed");
        }
        self.tasks.close();
        self.tasks.wait().await;
        report
    }

    /// Number of in-memory member episodes (the claim-less lead is excluded).
    /// A failed terminal write releases this ownership even while its durable
    /// claim remains active for later reconciliation.
    #[must_use]
    pub fn episode_count(&self) -> usize {
        self.episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether the resident slot for `session` has work in flight: a running
    /// turn, owed mail or directive, or an accepted report still to execute.
    /// `None` when this process tracks no such slot.
    pub(crate) fn member_busy(&self, root: SessionId, session: SessionId) -> Option<bool> {
        let team = self
            .team_with_resident(session)
            .filter(|team| team.root == root)?;
        let state = team.lock();
        let slot = state.residents.get(&session)?;
        Some(
            slot.status == SlotStatus::Busy
                || slot.has_work()
                || slot.report.is_some()
                || slot.ending.is_some()
                || self.engine.turn_active(session),
        )
    }

    /// Whether `session`'s slot is committing its archive (a report executing
    /// or an `archive` in progress): its report mail may already be out while
    /// the archive marker is not.
    pub(crate) fn member_archiving(&self, root: SessionId, session: SessionId) -> bool {
        self.team_with_resident(session)
            .filter(|team| team.root == root)
            .is_some_and(|team| {
                team.lock()
                    .residents
                    .get(&session)
                    .is_some_and(|slot| slot.ending.is_some() || slot.report.is_some())
            })
    }

    /// Whether the user stopped `session`'s slot (see
    /// [`ArchiveReviver::stop_for_user`]).
    pub(crate) fn member_stopped_by_user(&self, root: SessionId, session: SessionId) -> bool {
        self.team_with_resident(session)
            .filter(|team| team.root == root)
            .is_some_and(|team| {
                team.lock()
                    .residents
                    .get(&session)
                    .is_some_and(|slot| slot.user_stopped)
            })
    }

    /// The team `session` belongs to: the one it leads, or the one it is a
    /// member of.
    fn team_of(&self, session: SessionId) -> Option<Arc<TeamActor>> {
        let led = self.teams().get(&session).cloned();
        led.or_else(|| self.team_with_resident(session))
    }

    pub(crate) fn forget_wait_stalls(&self, session: SessionId) {
        self.wait_stalls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(caller, member), _| *caller != session && *member != session);
    }

    pub(crate) fn member_claim(&self, member: SessionId) -> Option<ActorClaim> {
        self.team_with_resident(member)?
            .lock()
            .residents
            .get(&member)?
            .claim
    }

    /// Whether this episode's stopped turn has already been reported to the caller.
    pub(crate) fn wait_stall_reported(
        &self,
        caller: SessionId,
        claim: ActorClaim,
        last_answer: Option<hya_proto::MessageId>,
    ) -> bool {
        self.wait_stalls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(caller, claim.actor_id))
            .is_some_and(|stall| stall.claim == claim && stall.last_answer == last_answer)
    }

    /// Record a snapshot only while its exact episode still owns the member.
    pub(crate) fn record_wait_stall(
        &self,
        caller: SessionId,
        claim: ActorClaim,
        last_answer: Option<hya_proto::MessageId>,
    ) {
        let episodes = self
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if episodes.get(&claim.actor_id).is_some_and(|team| {
            team.lock()
                .residents
                .get(&claim.actor_id)
                .is_some_and(|slot| slot.claim == Some(claim))
        }) {
            self.wait_stalls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((caller, claim.actor_id), WaitStall { claim, last_answer });
        }
    }

    /// The team-wide cancellation token for `root`, if the team is tracked. Exposed
    /// so the runtime/TUI can observe or force a team shutdown.
    #[must_use]
    pub fn team_cancel(&self, root: SessionId) -> Option<CancellationToken> {
        self.teams().get(&root).and_then(|team| {
            let state = team.lock();
            (!state.residents.is_empty()).then(|| team.cancel.clone())
        })
    }

    /// Prepare outside the claim-write lock, then commit and install a revived
    /// episode under the same serialization as every archive/Takeover decision.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn revive_episode(
        &self,
        root: SessionId,
        parent: &str,
        target: &str,
        channel: String,
        kind: hya_proto::MailKind,
        body: String,
        parent_claim: Option<&ActorClaim>,
        policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
    ) -> Result<usize, CoreError> {
        self.revive_episode_with_activation(
            root,
            parent,
            target,
            channel,
            kind,
            body,
            parent_claim,
            policy,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn revive_episode_with_activation(
        &self,
        root: SessionId,
        parent: &str,
        target: &str,
        channel: String,
        kind: hya_proto::MailKind,
        body: String,
        parent_claim: Option<&ActorClaim>,
        policy: Option<hya_proto::mail::ChannelPolicySnapshot>,
        activation: Option<WorkflowActivation>,
    ) -> Result<usize, CoreError> {
        let projection = self.engine.read_projection_shared(root).await?;
        // A concurrent sender may already have installed the new episode.
        let child = projection
            .team
            .roster
            .get(target)
            .map(|entry| entry.session)
            .or_else(|| {
                projection
                    .team
                    .archived
                    .get(target)
                    .map(|entry| entry.session)
            })
            .ok_or_else(|| CoreError::Invalid(format!("resident not found: {target}")))?;
        let parent_session = projection
            .team
            .roster
            .get(parent)
            .map_or(root, |entry| entry.session);
        drop(projection);
        let (agent, (binding, agents, resources, sidecar_factory)) =
            if let Some(activation) = &activation {
                activation.resolved.clone()
            } else {
                let child_projection = self.engine.read_projection_shared(child).await?;
                let stable =
                    child_projection.session.agent.clone().ok_or_else(|| {
                        CoreError::Invalid("resident has no recorded agent".into())
                    })?;
                let workdir = child_projection
                    .session
                    .workdir
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| CoreError::Invalid(format!("session not found: {child}")))?;
                let base = AgentSpec {
                    name: stable.clone(),
                    model: child_projection
                        .session
                        .model
                        .clone()
                        .ok_or_else(|| CoreError::Invalid("resident has no model".into()))?,
                    system_prompt: self.engine.base_system_prompt().into(),
                    workdir,
                    reasoning: None,
                };
                drop(child_projection);
                let binding = self
                    .engine
                    .bind_session_runtime(child, &base.workdir)
                    .await?;
                self.engine
                    .resolve_resident_runtime(binding, &base, &stable)?
            };
        let channel_policy =
            ChannelPolicy::from_binding(&binding)?.snapshot_for(agent.name.as_str());
        let mut team = self.team_for(root).await?;
        #[cfg(test)]
        {
            let pause = self
                .registration_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let mut write = self.claim_writes.lock().await;
        let live = loop {
            let current = self.engine.read_projection_shared(root).await?;
            // A live roster wins: never wait for a concurrent revival's episode.
            if current.team.roster.contains_key(target) {
                break true;
            }
            let previous = self.team_with_resident(child).and_then(|previous| {
                previous
                    .lock()
                    .residents
                    .get(&child)
                    .map(|slot| slot.done.subscribe())
            });
            let Some(done) = previous else {
                break false;
            };
            drop(current);
            drop(write);
            let _ = await_episode(done).await;
            team = self.team_for(root).await?;
            write = self.claim_writes.lock().await;
        };
        // Reserve the existing turn seam before checking idle: unrelated mail
        // cannot start work until guidance, route and completion are installed.
        let activation_lease = activation
            .as_ref()
            .map(|_| self.engine.try_begin_turn(child))
            .transpose()?;
        if activation.is_some() {
            let state = team.lock();
            if let Some(slot) = state.residents.get(&child) {
                if slot.status != SlotStatus::Idle
                    || slot.has_work()
                    || slot.activation.is_some()
                    || slot.ending.is_some()
                    || slot.report.is_some()
                    || slot.user_stopped
                {
                    return Err(CoreError::Invalid(
                        "resident Workflow activation requires an idle episode".into(),
                    ));
                }
            } else if live {
                return Err(CoreError::Invalid(
                    "resident session is not registered".into(),
                ));
            }
        }
        let (claim, envelopes, recipients) = self
            .engine
            .store()
            .revive_episode(
                root,
                self.owner_run_id,
                self.capacity(),
                parent_claim,
                child,
                parent.into(),
                channel,
                kind,
                body,
                hya_provider::COMPACT_CONTEXT_MARKER,
                policy,
            )
            .await?;
        let activation = if let Some(activation) = activation {
            let projection = self.engine.read_projection_shared(root).await?;
            let boundary = projection.team.inboxes[target].len();
            Some((activation, boundary))
        } else {
            None
        };
        let Some((claim, cursor)) = claim else {
            if let Some((activation, boundary)) = activation {
                let mut state = team.lock();
                let slot = state.residents.get_mut(&child).ok_or_else(|| {
                    CoreError::Invalid("resident episode disappeared during activation".into())
                })?;
                slot.guidance = Some(activation.guidance);
                slot.route = activation.route;
                slot.activation = Some(ActivationCompletion {
                    boundary,
                    done: activation.done,
                });
                slot.pending = true;
            }
            for envelope in envelopes {
                self.engine.publish_envelope(envelope);
            }
            return Ok(recipients);
        };
        let activated = activation.is_some();
        let (guidance, route, completion) = match activation {
            Some((activation, boundary)) => (
                Some(activation.guidance),
                activation.route,
                Some(ActivationCompletion {
                    boundary,
                    done: activation.done,
                }),
            ),
            None => (None, None, None),
        };
        self.engine
            .update_session_channel_policy(child, channel_policy);
        let (ready, initialized) = oneshot::channel();
        let (publication, published) = oneshot::channel();
        let (child_envelopes, parent_envelopes): (Vec<_>, Vec<_>) = envelopes
            .into_iter()
            .partition(|envelope| envelope.event.session() == Some(child));
        let init = EpisodeInit {
            binding: binding.clone(),
            agent: agent.name.clone(),
            parent: parent_session,
            envelopes: child_envelopes,
            published,
            ready,
        };
        team.install_episode(
            child,
            Episode {
                handle: target.into(),
                agent,
                binding,
                agents: Some(agents),
                resources: Some(resources),
                guidance,
                route,
                activation: completion,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending: true,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: Some(claim),
                cancel: team.cancel.child_token(),
                ending: None,
                done: watch::channel(None).0,
                cursor: usize::try_from(cursor).unwrap_or(usize::MAX),
                last_activity: None,
                last_heartbeat_emit: None,
                notify: Arc::new(Notify::new()),
            },
            Some(init),
        );
        for envelope in parent_envelopes {
            self.engine.publish_envelope(envelope);
        }
        let _ = publication.send(());
        drop(write);
        drop(activation_lease);
        if activated {
            return Ok(recipients);
        }
        initialized
            .await
            .map_err(|_| CoreError::Invalid("resident initialization task exited".into()))??;
        #[cfg(test)]
        {
            let pause = self
                .revive_ready_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        Ok(recipients)
    }

    /// Register the team root as the main actor so child mail (and quiescence) wake
    /// it. Idempotent: a second call for the same root is a no-op. The root is
    /// registered on the roster as `main` (transient mode — it is the root, not a
    /// resident subagent) if not already present.
    ///
    /// Callers must supply the already-resolved **team root** `AgentSpec`, can_spawn
    /// roster, and resource policy from the same captured [`TurnBinding`] as the
    /// triggering spawn (exact root stable `AgentName`, not the nested caller).
    /// `guidance` remains optional in-process-only triggering-turn text. Recovery
    /// invents nothing. Later `ensure_main` calls for the same root leave the first
    /// registration intact (first-wins).
    pub async fn ensure_main(
        &self,
        root: SessionId,
        agent: AgentSpec,
        resolved: (TurnBinding, Arc<[AgentDef]>, AgentResourcePolicy),
        actor_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(), CoreError> {
        let (binding, agents, resources) = resolved;
        self.engine
            .ensure_root_registered_for_actor(root, actor_claim)
            .await?;
        self.team_for_with_main(
            root,
            MainActivation {
                agent,
                binding,
                agents,
                resources,
                guidance,
            },
        );
        Ok(())
    }

    /// Spawn a brand-new resident under `parent`: create its session, assign a
    /// stable team-scoped handle, register it (mode = resident), announce it in the
    /// parent tree, and give it an initial wake so it runs its first turn on
    /// `directive`, then idles. Returns the new session + its handle.
    ///
    /// Non-blocking: this returns as soon as the resident is registered and armed;
    /// the caller (parent) does NOT wait for the resident's turn.
    ///
    /// `guidance` is the immutable triggering-turn Arc stored only in-process for
    /// every activation of this slot; it is not written to the resident definition.
    pub async fn spawn_resident(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String), CoreError> {
        let (session, handle, _) = self
            .spawn_resident_inner(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(
                    directive.clone(),
                    Some(directive),
                    parent_claim,
                    guidance,
                ),
            )
            .await?;
        Ok((session, handle))
    }

    /// [`spawn_resident`](Self::spawn_resident) for a `task` spawn: the handle
    /// prefix is the sanitized `subagent_type` (the resolved agent id the call
    /// named, see [`hya_tool::sanitize_handle_prefix`]) rather than the spec's
    /// agent name, which an inline overlay may replace. The handle becomes
    /// `<parent path>/<subagent_type>-<operator>`; see [`crate::handle_naming`].
    /// The member row on the parent log carries the call's description and id.
    ///
    /// # Errors
    /// Every [`spawn_resident`](Self::spawn_resident) failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_resident_typed(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        directive: String,
        task: TaskSpawnOrigin,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String), CoreError> {
        let (session, handle, _) = self
            .spawn_resident_inner(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(
                    directive.clone(),
                    Some(directive),
                    parent_claim,
                    guidance,
                )
                .with_task(task),
            )
            .await?;
        Ok((session, handle))
    }

    /// Create and register one resident without an implicit first turn.
    ///
    /// The caller must deliver work through durable mail after this returns.
    /// `registration_directive` is provenance only and must not contain the work
    /// payload or input values. Returns the committed Session, handle and member
    /// id before initializer hooks complete; the episode runs those before work.
    pub async fn spawn_resident_parked(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        registration_directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        let (root, prepared, member) = self
            .prepare_resident(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(registration_directive, None, parent_claim, guidance),
            )
            .await?;
        let session = prepared.session;
        let handle = scope::join_path(&prepared.parent_path, &prepared.leaf);
        // Return the committed identity before initialization can wait on a
        // plugin. Activation and cancellation then belong to the controller.
        self.register_episodes(root, vec![prepared], None).await?;
        Ok((session, handle, member))
    }

    async fn spawn_resident_inner(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        context: ResidentSpawnContext,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        let (root, prepared, member) = self
            .prepare_resident(parent, agent, resolved, context)
            .await?;
        let session = prepared.session;
        let handle = scope::join_path(&prepared.parent_path, &prepared.leaf);
        let initialized = self.register_episodes(root, vec![prepared], None).await?;
        let _ = futures::future::join_all(initialized).await;
        Ok((session, handle, member))
    }

    /// Register all resolved task members or none, then install every committed
    /// episode under the claim-write boundary before awaiting initialization.
    pub async fn spawn_residents_typed(
        &self,
        parent: SessionId,
        members: Vec<TaskResidentSpawn>,
        main: MainActivation,
        parent_claim: Option<&ActorClaim>,
    ) -> Result<Vec<(SessionId, String)>, CoreError> {
        let (root, _) = self.engine.session_lineage(parent).await?;
        let mut prepared = Vec::with_capacity(members.len());
        let mut identities = Vec::with_capacity(members.len());
        for member in members {
            let (_, registration, _) = self
                .prepare_resident(
                    parent,
                    member.agent,
                    member.runtime,
                    ResidentSpawnContext::new(
                        member.directive.clone(),
                        Some(member.directive),
                        parent_claim,
                        member.guidance,
                    )
                    .with_task(member.task),
                )
                .await?;
            identities.push((
                registration.session,
                scope::join_path(&registration.parent_path, &registration.leaf),
            ));
            prepared.push(registration);
        }
        let initialized = self.register_episodes(root, prepared, Some(main)).await?;
        let _ = futures::future::join_all(initialized).await;
        Ok(identities)
    }

    pub(super) async fn prepare_resident(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        context: ResidentSpawnContext,
    ) -> Result<(SessionId, PreparedResident, MemberId), CoreError> {
        let ResidentSpawnContext {
            directive,
            initial,
            parent_claim,
            guidance,
            task,
        } = context;
        let prefix = hya_tool::sanitize_handle_prefix(
            task.as_ref()
                .map_or(agent.name.as_str(), |task| task.subagent_type.as_str()),
        );
        let (binding, agents, resources, sidecar_factory) = resolved;
        let (root, parent_depth) = self.engine.session_lineage(parent).await?;
        let session = SessionId::new();
        let parent_projection = self.engine.read_projection_shared(parent).await?;
        let mut events = vec![Event::SessionCreated {
            session,
            parent: Some(parent),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: parent_projection.session.project,
            kind: parent_projection.session.kind,
        }];
        drop(parent_projection);
        if parent_depth.saturating_add(1) > crate::MAX_SUBAGENT_DEPTH {
            return Err(CoreError::Invalid("maximum subagent depth exceeded".into()));
        }
        let parent_path = if parent == root {
            scope::ROOT_HANDLE.into()
        } else {
            self.engine.resolve_handle(root, parent).await?
        };
        let reservation = {
            // A commit may release a reservation only after this snapshot has
            // observed its durable identity; stale snapshots cannot reuse it.
            let _write = self.claim_writes.lock().await;
            self.engine.mint_member_leaf(root, &prefix).await?
        };
        let leaf = reservation.leaf.clone();
        let member = MemberId::new();
        let description = task
            .as_ref()
            .map(|task| task.description.trim())
            .filter(|description| !description.is_empty())
            .map_or_else(|| directive.chars().take(80).collect(), ToString::to_string);
        let tool_call = task.as_ref().and_then(|task| task.tool_call);
        events.push(Event::MemberSpawned {
            session: parent,
            member,
            child: Some(session),
            subagent_type: agent.name.clone(),
            description,
            depth: parent_depth.saturating_add(1),
            directive,
            tool_call,
        });
        Ok((
            root,
            PreparedResident {
                reservation: Some(reservation),
                session,
                parent_path,
                leaf,
                agent,
                runtime: ResidentRuntimeContext::Resolved {
                    binding,
                    agents,
                    resources,
                },
                activation: ResidentActivation {
                    events,
                    parent_claim,
                    initial,
                    guidance,
                    route: None,
                    sidecar_factory,
                },
            },
            member,
        ))
    }

    /// Arm an idle member (or revive an archived one) with durable parent mail.
    /// Guidance, route and the post-mail inbox boundary are installed before
    /// the turn reservation is released. The episode alone resolves completion.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn activate(
        &self,
        parent: SessionId,
        member: SessionId,
        resolved: (AgentSpec, ResolvedResidentRuntime),
        directive: String,
        guidance: Arc<str>,
        route: Option<WorkflowTurnRoute>,
        parent_claim: Option<&ActorClaim>,
    ) -> Result<ActivationWaiter, CoreError> {
        if directive.trim().is_empty() {
            return Err(CoreError::Invalid(
                "Workflow activation directive must not be blank".into(),
            ));
        }
        let (root, _) = self.engine.session_lineage(parent).await?;
        let from = self.engine.resolve_handle(root, parent).await?;
        #[cfg(test)]
        {
            let pause = self
                .activation_lock_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let write = self.claim_writes.lock().await;
        let projection = self.engine.read_projection_shared(root).await?;
        let target = projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == member)
            .map(|entry| entry.handle.clone())
            .or_else(|| {
                projection
                    .team
                    .archived
                    .values()
                    .find(|entry| entry.session == member)
                    .map(|entry| entry.handle.clone())
            })
            .ok_or_else(|| CoreError::Invalid("resident member is not registered".into()))?;
        let dm = crate::engine::dm_channel_between(&projection, &from, &target)
            .ok_or_else(|| CoreError::Invalid("resident member has no parent DM channel".into()))?;
        let (done, receiver) = oneshot::channel();
        let live = projection.team.roster.contains_key(&target);
        drop(projection);
        if live {
            let lease = self.engine.try_begin_turn(member)?;
            let team = self
                .team_with_resident(member)
                .ok_or_else(|| CoreError::Invalid("resident session is not registered".into()))?;
            {
                let state = team.lock();
                let slot = &state.residents[&member];
                if slot.status != SlotStatus::Idle
                    || slot.has_work()
                    || slot.activation.is_some()
                    || slot.ending.is_some()
                    || slot.report.is_some()
                    || slot.user_stopped
                {
                    return Err(CoreError::Invalid(
                        "resident Workflow activation requires an idle episode".into(),
                    ));
                }
            }
            let envelope = self
                .engine
                .store()
                .append_channel_mail(
                    root,
                    from,
                    dm.clone(),
                    hya_proto::MailKind::Message,
                    directive,
                    parent_claim,
                    None,
                )
                .await?
                .0;
            let boundary =
                self.engine.read_projection_shared(root).await?.team.inboxes[&target].len();
            {
                let mut state = team.lock();
                let slot = state.residents.get_mut(&member).ok_or_else(|| {
                    CoreError::Invalid("resident episode disappeared during activation".into())
                })?;
                slot.guidance = Some(guidance);
                slot.route = route;
                slot.activation = Some(ActivationCompletion { boundary, done });
                slot.pending = true;
            }
            self.engine.publish_envelope(envelope);
            drop(write);
            drop(lease);
            return Ok(ActivationWaiter { done: receiver });
        }
        drop(write);
        self.revive_episode_with_activation(
            root,
            &from,
            &target,
            dm,
            hya_proto::MailKind::Message,
            directive,
            parent_claim,
            None,
            Some(WorkflowActivation {
                resolved,
                guidance,
                route,
                done,
            }),
        )
        .await?;
        Ok(ActivationWaiter { done: receiver })
    }

    /// Register an already-created `session` as a resident of team `root`, arm it,
    /// and (when `initial` is set) give it a first wake. Used by
    /// [`spawn_resident`](Self::spawn_resident); also the seam tests drive directly.
    pub async fn register_existing_resident(
        &self,
        root: SessionId,
        session: SessionId,
        handle: String,
        agent: AgentSpec,
        initial: Option<String>,
    ) -> Result<(), CoreError> {
        let binding = self
            .engine
            .bind_session_runtime(session, &agent.workdir)
            .await?;
        let parent_path = self.engine.parent_agent_path(root, session).await;
        let leaf = scope::leaf(&handle).to_string();
        self.register_episode(
            root,
            session,
            parent_path,
            leaf,
            agent,
            ResidentRuntimeContext::Bound(binding),
            ResidentActivation {
                events: Vec::new(),
                parent_claim: None,
                initial,
                guidance: None,
                route: None,
                sidecar_factory: None,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn register_episode(
        &self,
        root: SessionId,
        session: SessionId,
        parent_path: String,
        leaf: String,
        agent: AgentSpec,
        runtime: ResidentRuntimeContext,
        activation: ResidentActivation,
    ) -> Result<(), CoreError> {
        let initialized = self
            .register_episodes(
                root,
                vec![PreparedResident {
                    reservation: None,
                    session,
                    parent_path,
                    leaf,
                    agent,
                    runtime,
                    activation,
                }],
                None,
            )
            .await?;
        let _ = futures::future::join_all(initialized).await;
        Ok(())
    }

    fn capacity(&self) -> u64 {
        self.engine
            .governor()
            .map_or(u64::MAX, |governor| governor.limits().per_run_budget)
    }

    pub(super) async fn register_episodes(
        &self,
        root: SessionId,
        members: Vec<PreparedResident>,
        main: Option<MainActivation>,
    ) -> Result<Vec<oneshot::Receiver<Result<(), CoreError>>>, CoreError> {
        let projection = self.engine.read_projection_shared(root).await?;
        let parent_claim = members
            .first()
            .and_then(|member| member.activation.parent_claim);
        let mut registrations = Vec::with_capacity(members.len());
        let mut installs = Vec::with_capacity(members.len());
        let mut reservations = Vec::with_capacity(members.len());
        for member in members {
            let PreparedResident {
                reservation,
                session,
                parent_path,
                leaf,
                agent,
                runtime,
                activation,
            } = member;
            reservations.push(reservation);
            let ResidentActivation {
                mut events,
                parent_claim: _,
                initial,
                guidance,
                route,
                sidecar_factory,
            } = activation;
            let (binding, agents, resources) = match runtime {
                ResidentRuntimeContext::Bound(binding) => (binding, None, None),
                ResidentRuntimeContext::Resolved {
                    binding,
                    agents,
                    resources,
                } => (binding, Some(agents), Some(resources)),
            };
            let handle = scope::join_path(&parent_path, &leaf);
            // The store resolves the unit's group channel under the writer lock;
            // simultaneous first siblings must join the same broadcast channel.
            let group_channel = hya_proto::mint_channel_id(ChannelKind::Group);
            let dm_channel = hya_proto::mint_channel_id(ChannelKind::Dm);
            events.extend([
                Event::AgentRegistered {
                    session: root,
                    agent_session: session,
                    handle: leaf,
                    parent: Some(parent_path.clone()),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                },
                // The unit's leader-only broadcast channel: minted with the
                // leader as founding member on first registration, joined
                // by every child thereafter.
                Event::ChannelCreated {
                    session: root,
                    channel: group_channel,
                    kind: ChannelKind::Group,
                    unit: Some(parent_path.clone()),
                    members: vec![parent_path.clone(), handle.clone()],
                },
                // The persistent parent-child DM pair: the revival address
                // that outlives archive cycles.
                Event::ChannelCreated {
                    session: root,
                    channel: dm_channel.clone(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec![parent_path.clone(), handle.clone()],
                },
            ]);
            let initial = initial.filter(|text| !text.trim().is_empty());
            let pending = initial.is_some();
            if let Some(text) = initial {
                events.push(Event::MailSent {
                    session: root,
                    from: parent_path.clone(),
                    to: MailEndpoint::Channel(dm_channel),
                    kind: hya_proto::MailKind::Message,
                    body: text,
                });
            }
            let channel_policy =
                ChannelPolicy::from_binding(&binding)?.snapshot_for(agent.name.as_str());

            let parent_session = projection
                .team
                .roster
                .get(&parent_path)
                .map_or(root, |entry| entry.session);
            registrations.push(hya_store::EpisodeRegistration {
                actor: session,
                events,
            });
            let episode = Episode {
                handle,
                agent,
                binding,
                agents,
                resources,
                guidance,
                route,
                activation: None,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: None,
                cancel: CancellationToken::new(),
                ending: None,
                done: watch::channel(None).0,
                cursor: 0,
                last_activity: None,
                last_heartbeat_emit: None,
                notify: Arc::new(Notify::new()),
            };
            installs.push((session, parent_session, episode, channel_policy));
        }
        let archived: std::collections::HashSet<_> = projection
            .team
            .archived
            .values()
            .map(|entry| entry.session)
            .collect();
        drop(projection);
        let mut team = if let Some(main) = main {
            let mut teams = self.teams();
            let team = teams
                .entry(root)
                .or_insert_with(|| self.new_team(root, main));
            {
                let state = team.lock();
                if state.root_driver.runs.load(Ordering::Acquire) == 0 {
                    state.root_driver.autonomous.store(true, Ordering::Release);
                }
            }
            TeamRegistration::new(team.clone())
        } else {
            self.team_for(root).await?
        };
        #[cfg(test)]
        {
            let pause = self
                .registration_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let mut write = self.claim_writes.lock().await;
        loop {
            let previous = registrations.iter().find_map(|registration| {
                self.team_with_resident(registration.actor)
                    .and_then(|team| {
                        team.lock()
                            .residents
                            .get(&registration.actor)
                            .map(|slot| (registration.actor, slot.done.subscribe()))
                    })
            });
            let Some((actor, done)) = previous else { break };
            if !archived.contains(&actor) {
                return Err(CoreError::Invalid(
                    "resident already has a live episode".into(),
                ));
            }
            drop(write);
            let _ = await_episode(done).await;
            team = self.team_for(root).await?;
            write = self.claim_writes.lock().await;
        }
        let registered = self
            .engine
            .store()
            .register_episodes(
                root,
                self.owner_run_id,
                parent_claim.as_ref(),
                self.capacity(),
                registrations,
            )
            .await?;
        // Committed identities now belong to durable history. Release while
        // claim_writes still excludes name allocation from a stale snapshot.
        drop(reservations);
        let mut initialized_members = Vec::with_capacity(installs.len());
        let mut publications = Vec::with_capacity(installs.len());
        for ((claim, envelopes), (session, parent, mut episode, channel_policy)) in
            registered.into_iter().zip(installs)
        {
            self.engine
                .update_session_channel_policy(session, channel_policy);
            let (ready, initialized) = oneshot::channel();
            let (publication, published) = oneshot::channel();
            let (child_envelopes, parent_envelopes): (Vec<_>, Vec<_>) = envelopes
                .into_iter()
                .partition(|envelope| envelope.event.session() == Some(session));
            let init = EpisodeInit {
                binding: episode.binding.clone(),
                agent: episode.agent.name.clone(),
                parent,
                envelopes: child_envelopes,
                published,
                ready,
            };
            episode.claim = Some(claim);
            episode.cancel = team.cancel.child_token();
            team.install_episode(session, episode, Some(init));
            initialized_members.push(initialized);
            publications.push((parent_envelopes, publication));
        }
        for (envelopes, _) in &publications {
            for envelope in envelopes {
                self.engine.publish_envelope(envelope.clone());
            }
        }
        for (_, publication) in publications {
            let _ = publication.send(());
        }
        drop(write);
        // Commit is the batch's success boundary. Each episode owns initializer
        // errors and its ending; none can retract these committed identities.
        Ok(initialized_members)
    }

    /// Stage a fenced actor without creating a team or starting any work.
    pub fn defer_recovered_resident(
        &self,
        root: SessionId,
        handle: String,
        agent: AgentSpec,
        runtime: ResolvedResidentRuntime,
        claim: hya_store::RecoveredActorClaim,
        disposition: ResidentRecovery,
    ) {
        self.recovered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RecoveredResident {
                root,
                handle,
                agent,
                runtime,
                claim,
                disposition,
            });
    }

    /// Start a headless runtime's recovered actors under external root ownership.
    /// Keep these guards until the headless command has drained its runtime.
    pub async fn start_recovered_headless(
        &self,
        session: SessionId,
    ) -> Result<Vec<RootDriverActivity>, CoreError> {
        let mut roots = self
            .recovered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|member| member.root)
            .collect::<HashSet<_>>();
        roots.insert(self.engine.session_lineage(session).await?.0);
        let mut drivers = Vec::with_capacity(roots.len());
        for root in roots {
            drivers.push(self.root_driver_activity(root).await?);
        }
        self.start_recovered().await?;
        Ok(drivers)
    }

    /// Install recovered episodes after the server has admitted restore prompts.
    /// Repeated calls have no effects once the staged actors have been installed.
    pub async fn start_recovered(&self) -> Result<usize, CoreError> {
        let recovered = std::mem::take(
            &mut *self
                .recovered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let mut count = 0;
        for member in recovered {
            match self
                .register_recovered_resident(
                    member.root,
                    member.handle,
                    member.agent,
                    member.runtime,
                    member.claim,
                    member.disposition,
                )
                .await
            {
                Ok(()) => count += 1,
                // Restore tools may end or replace a staged claim before installation.
                Err(CoreError::Store(hya_store::StoreError::StaleActorClaim { .. })) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(count)
    }

    /// Re-register a recovered resident into the live supervisor map.
    pub async fn register_recovered_resident(
        &self,
        root: SessionId,
        handle: String,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        recovered: hya_store::RecoveredActorClaim,
        disposition: ResidentRecovery,
    ) -> Result<(), CoreError> {
        let (binding, agents, resources, sidecar_factory) = resolved;
        let team = self.team_for(root).await?;
        // Transfer liveness to recovery before a stale claim can drop registration.
        team.enqueue(TeamEvent::Recover);
        let _write = self.claim_writes.lock().await;
        self.engine
            .store()
            .validate_actor_claim(&recovered.claim)
            .await?;
        let projection = self.engine.read_projection_shared(root).await?;
        let cursor = projection
            .team
            .roster
            .get(&handle)
            .filter(|entry| entry.session == recovered.claim.actor_id)
            .map_or(0, |entry| entry.resident_cursor);
        let pending = matches!(
            disposition,
            ResidentRecovery::Queued { .. }
                | ResidentRecovery::AbortedRunning {
                    queued_after: true,
                    ..
                }
        ) || projection
            .team
            .unread_incoming_mail(&handle, cursor)
            .next()
            .is_some();
        if team
            .lock()
            .residents
            .contains_key(&recovered.claim.actor_id)
        {
            return Err(CoreError::Invalid("resident already has an episode".into()));
        }
        let notify = Arc::new(Notify::new());
        team.install_episode(
            recovered.claim.actor_id,
            Episode {
                handle,
                agent,
                binding,
                agents: Some(agents),
                resources: Some(resources),
                // Ephemeral guidance is not durable; recovery invents nothing.
                guidance: None,
                route: None,
                activation: None,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: Some(recovered.claim),
                cancel: team.cancel.child_token(),
                ending: None,
                done: watch::channel(None).0,
                cursor: usize::try_from(cursor).unwrap_or(usize::MAX),
                last_activity: None,
                last_heartbeat_emit: None,
                notify: notify.clone(),
            },
            None,
        );
        Ok(())
    }
}
