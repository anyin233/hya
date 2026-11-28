use super::*;

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
        archive: crate::store_bridge::ResidentArchive,
    ) -> Result<crate::store_bridge::ResidentArchiveOutcome, CoreError> {
        use crate::store_bridge::{ResidentArchiveOwner, SessionStoreExt as _};
        let _write = self.claim_writes.lock().await;
        if self.team_with_resident(archive.actor).is_some() {
            return Err(CoreError::Invalid(
                "a live episode owns this archive; request its ending".into(),
            ));
        }
        let (outcome, envelopes) = self
            .engine
            .store()
            .finalize_resident_archive(ResidentArchiveOwner::Takeover, root, handle, archive)
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
        match ExtensionEvent::from_event(&envelope.event) {
            Some(ExtensionEvent::MailSent {
                session: root,
                from,
                to,
                ..
            }) => {
                // Select and enqueue under the map lock, excluding retirement.
                if let Some(team) = self.teams().get(&root) {
                    team.enqueue(TeamEvent::Mail {
                        seq: envelope.seq,
                        from,
                        to,
                    });
                }
            }
            Some(ExtensionEvent::AgentHeartbeat { .. }) => {}
            _ => {
                if let Some(actor) = envelope.event.session()
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

    pub(super) fn new_team(&self, root: SessionId, main: MainActivation) -> Arc<TeamActor> {
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

    pub(super) fn team_for_with_main(&self, root: SessionId, main: MainActivation) {
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
            use crate::store_bridge::SessionStoreExt as _;
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
                                        hya_tool::extension::team_projection(&projection)
                                            .roster
                                            .iter()
                                            .find(|(handle, entry)| {
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

    /// Arm an idle member (or revive an archived one) with durable parent mail.
    /// Guidance, route and the post-mail inbox boundary are installed before
    /// the turn reservation is released. The episode alone resolves completion.
    #[allow(clippy::too_many_arguments)]
    pub async fn activate(
        &self,
        parent: SessionId,
        member: SessionId,
        resolved: (AgentSpec, ResolvedResidentRuntime),
        directive: String,
        guidance: Arc<str>,
        route: Option<ExplicitTurnRoute>,
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
        use crate::store_bridge::SessionStoreExt as _;
        let write = self.claim_writes.lock().await;
        let projection = self.engine.read_projection_shared(root).await?;
        let team_proj = hya_tool::extension::team_projection(&projection);
        let target = team_proj
            .roster
            .values()
            .find(|entry| entry.session == member)
            .map(|entry| entry.handle.clone())
            .or_else(|| {
                team_proj
                    .archived
                    .values()
                    .find(|entry| entry.session == member)
                    .map(|entry| entry.handle.clone())
            })
            .ok_or_else(|| CoreError::Invalid("resident member is not registered".into()))?;
        let dm = crate::engine::dm_channel_between(&projection, &from, &target)
            .ok_or_else(|| CoreError::Invalid("resident member has no parent DM channel".into()))?;
        let (done, receiver) = oneshot::channel();
        let live = team_proj.roster.contains_key(&target);
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
                    parent_claim,
                    from,
                    dm.clone(),
                    hya_proto::MailKind::Message,
                    directive,
                    None,
                )
                .await?
                .0;
            let root_proj = self.engine.read_projection_shared(root).await?;
            let boundary = hya_tool::extension::team_projection(&root_proj).inboxes[&target].len();
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
}
