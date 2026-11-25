use super::*;

/// Minimum spacing between harness heartbeats for one busy slot (ADR-0002
/// liveness). A busy child that is really progressing emits fresh engine
/// events; the supervisor folds those observations into `AgentHeartbeat` at
/// most this often so the parent can tell progress from a stall.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(super) enum TeamPhase {
    Live,
    Killed(Ending),
}

impl TeamPhase {
    pub(super) fn killed(&self) -> bool {
        matches!(self, Self::Killed(_))
    }
}

/// The mutable, lock-guarded state of one team.
pub(super) struct TeamState {
    pub(super) residents: HashMap<SessionId, Episode>,
    /// Number of non-idle residents (incl. main). Quiescence = this is 0.
    pub(super) busy: usize,
    pub(super) registrations: usize,
    // Terminal mail keeps main alive between episode removal and bus routing.
    pub(super) routing_mail: HashSet<u64>,
    pub(super) queued_wakes: usize,
    /// Active Workflow/headless root drivers suppress autonomous root turns.
    pub(super) root_driver: Arc<RootDriverState>,
    pub(super) phase: TeamPhase,
    pub(super) turns_used: u64,
    pub(super) messages_used: u64,
    /// Monotonic counter bumped on every unit of new work (mail, registration).
    /// Quiescence re-fires only when this advances past the last synthesis.
    pub(super) work_seq: u64,
    /// `work_seq` captured at the last synthesis wake. Initialized to `u64::MAX`
    /// so the FIRST quiescence always fires (any real `work_seq` differs from it).
    pub(super) last_synth_work_seq: u64,
    pub(super) main_session: Option<SessionId>,
    /// The lead's (main actor's) last turn failed. While set the harness
    /// starts no main turn on its own — no quiescence synthesis, no mail
    /// wake — until a turn on the lead starts from outside the supervisor
    /// (the user resumes it). Queued mail stays in the lead's inbox.
    pub(super) lead_failed: bool,
}

/// A single team's actor group: its residents, budgets, and cancellation.
pub(super) struct TeamActor {
    pub(super) root: SessionId,
    pub(super) engine: Arc<SessionEngine>,
    pub(super) cancel: CancellationToken,
    state: Mutex<TeamState>,
    pub(super) supervisor: Weak<ResidentSupervisor>,
    pub(super) tasks: TaskTracker,
    pub(super) events: tokio::sync::mpsc::UnboundedSender<TeamEvent>,
}

#[cfg_attr(test, derive(Debug))]
pub(super) enum TeamEvent {
    #[cfg(test)]
    Flush(oneshot::Sender<()>),
    Mail {
        seq: hya_proto::EventSeq,
        from: String,
        to: MailEndpoint,
    },
    Activity(SessionId),
    Recover,
}

impl TeamEvent {
    fn keeps_team_live(&self) -> bool {
        match self {
            Self::Mail { .. } => true,
            Self::Activity(_) => false,
            Self::Recover => true,
            #[cfg(test)]
            Self::Flush(_) => false,
        }
    }
}

/// Current wall-clock as non-zero epoch milliseconds for a heartbeat value.
fn heartbeat_now_ms() -> u64 {
    u64::try_from(hya_proto::now_millis()).unwrap_or(0).max(1)
}

impl TeamActor {
    pub(super) fn new(
        root: SessionId,
        engine: Arc<SessionEngine>,
        supervisor: Weak<ResidentSupervisor>,
        root_driver: Arc<RootDriverState>,
        tasks: TaskTracker,
        events: tokio::sync::mpsc::UnboundedSender<TeamEvent>,
    ) -> Self {
        Self {
            root,
            engine,
            cancel: CancellationToken::new(),
            tasks,
            events,
            supervisor,
            state: Mutex::new(TeamState {
                residents: HashMap::new(),
                busy: 0,
                registrations: 0,
                routing_mail: HashSet::new(),
                queued_wakes: 0,
                root_driver,
                phase: TeamPhase::Live,
                turns_used: 0,
                messages_used: 0,
                work_seq: 0,
                last_synth_work_seq: u64::MAX,
                main_session: None,
                lead_failed: false,
            }),
        }
    }

    /// Main is installed before a fresh team is published to the supervisor.
    pub(super) fn install_main(self: &Arc<Self>, context: MainActivation) {
        let notify = Arc::new(Notify::new());
        let cancel = self.cancel.child_token();
        {
            let mut state = self.lock();
            if state.residents.contains_key(&self.root) {
                return;
            }
            state.main_session = Some(self.root);
            state.residents.insert(
                self.root,
                Episode {
                    handle: scope::ROOT_HANDLE.into(),
                    agent: context.agent,
                    binding: context.binding,
                    agents: Some(context.agents),
                    resources: Some(context.resources),
                    guidance: context.guidance,
                    route: None,
                    activation: None,
                    sidecar_factory: None,
                    is_main: true,
                    status: SlotStatus::Idle,
                    pending: false,
                    report: None,
                    user_stopped: false,
                    synth_pending: false,
                    claim: None,
                    cancel: cancel.clone(),
                    ending: None,
                    done: watch::channel(None).0,
                    cursor: 0,
                    last_activity: None,
                    last_heartbeat_emit: None,
                    notify: notify.clone(),
                },
            );
        }
        self.tasks.spawn(episode_task(
            self.clone(),
            self.root,
            None,
            notify,
            cancel,
            None,
        ));
    }

    pub(super) fn install_episode(
        self: &Arc<Self>,
        session: SessionId,
        mut slot: Episode,
        init: Option<EpisodeInit>,
    ) {
        let claim = slot.claim;
        let cancel = slot.cancel.clone();
        let notify = slot.notify.clone();
        let pending = slot.has_work();
        {
            let mut state = self.lock();
            assert!(
                !state.residents.contains_key(&session),
                "episode installed twice"
            );
            if let TeamPhase::Killed(ending) = &state.phase {
                slot.request_end(&self.engine, session, ending.clone());
            }
            state.work_seq = state.work_seq.saturating_add(1);
            state.residents.insert(session, slot);
        }
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(session, self.clone());
        }
        self.tasks.spawn(episode_task(
            self.clone(),
            session,
            claim,
            notify.clone(),
            cancel,
            init,
        ));
        if pending {
            notify.notify_one();
        }
    }

    pub(super) fn enqueue(&self, event: TeamEvent) {
        let mut state = self.lock();
        if self.cancel.is_cancelled() {
            return;
        }
        if event.keeps_team_live() {
            state.queued_wakes += 1;
        }
        let _ = self.events.send(event);
    }

    pub(super) async fn run_events(
        self: Arc<Self>,
        mut events: tokio::sync::mpsc::UnboundedReceiver<TeamEvent>,
    ) {
        loop {
            let event = tokio::select! {
                biased;
                () = self.cancel.cancelled() => break,
                event = events.recv() => event,
            };
            let Some(event) = event else { break };
            let wake = event.keeps_team_live();
            match event {
                TeamEvent::Mail { seq, from, to } => {
                    self.on_mail(&from, &to).await;
                    self.lock().routing_mail.remove(&seq.0);
                    self.note_activity(self.root).await;
                }
                TeamEvent::Activity(actor) => self.note_activity(actor).await,
                TeamEvent::Recover => self.recover().await,
                #[cfg(test)]
                TeamEvent::Flush(done) => {
                    let _ = done.send(());
                }
            }
            if wake {
                self.lock().queued_wakes -= 1;
                if let Some(supervisor) = self.supervisor.upgrade() {
                    supervisor.retire_team(&self);
                }
            }
        }
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, TeamState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(super) async fn record_activity(
        &self,
        claim: Option<&ActorClaim>,
        handle: String,
        status: RosterStatus,
        current_task: Option<String>,
    ) -> Result<(), CoreError> {
        match claim {
            Some(claim) => {
                self.engine
                    .commit_resident_mutation(
                        claim,
                        self.root,
                        vec![Event::AgentActivityChanged {
                            session: self.root,
                            handle,
                            status,
                            current_task,
                        }],
                    )
                    .await
            }
            None => {
                self.engine
                    .record_agent_activity(self.root, handle, status, current_task)
                    .await
            }
        }
    }

    /// Append one harness heartbeat to the team-root log. A failed append is a
    /// dropped liveness hint, never an error path: a fenced claim racing a
    /// stop/kill simply loses that heartbeat.
    async fn record_heartbeat(&self, claim: Option<&ActorClaim>, handle: &str, heartbeat_ms: u64) {
        let event = Event::AgentHeartbeat {
            session: self.root,
            handle: handle.to_string(),
            heartbeat_ms,
        };
        let _ = self.engine.emit_for_actor(claim, self.root, event).await;
    }

    /// Record that `session`'s actor produced real engine work (an event the
    /// supervisor observed on the bus, owned by that session) and emit a
    /// throttled `AgentHeartbeat` while the slot is busy.
    async fn note_activity(&self, session: SessionId) {
        let Some((handle, claim, heartbeat_ms)) = self.touch_activity(session) else {
            return;
        };
        self.record_heartbeat(claim.as_ref(), &handle, heartbeat_ms)
            .await;
    }

    /// Lock-scoped activity accounting: stamps the slot's last activity and
    /// decides — busy state plus the 30-second window — whether this
    /// observation is the one that emits. The window is consumed here so at
    /// most one heartbeat lands per interval per slot.
    fn touch_activity(&self, session: SessionId) -> Option<(String, Option<ActorClaim>, u64)> {
        let mut st = self.lock();
        let slot = st.residents.get_mut(&session)?;
        let heartbeat_ms = heartbeat_now_ms();
        slot.last_activity = Some(heartbeat_ms);
        if slot.status != SlotStatus::Busy {
            return None;
        }
        let due = slot
            .last_heartbeat_emit
            .is_none_or(|last| Instant::now().duration_since(last) >= HEARTBEAT_INTERVAL);
        if !due {
            return None;
        }
        slot.last_heartbeat_emit = Some(Instant::now());
        Some((slot.handle.clone(), slot.claim, heartbeat_ms))
    }

    /// Turn-start heartbeat: stamped "now" while the claim owns the slot — work is
    /// beginning this instant. It also re-arms the throttle window so mid-turn
    /// bus events inside the window stay quiet.
    pub(super) async fn heartbeat_turn_start(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        handle: &str,
    ) {
        let heartbeat_ms = heartbeat_now_ms();
        {
            let mut st = self.lock();
            let Some(slot) = st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim.as_ref() == claim)
            else {
                return;
            };
            slot.last_activity = Some(heartbeat_ms);
            slot.last_heartbeat_emit = Some(Instant::now());
        }
        self.record_heartbeat(claim, handle, heartbeat_ms).await;
    }

    /// Turn-finish heartbeat: stamped with the owning slot's LAST
    /// observed activity rather than "now" — a turn that went quiet long before
    /// finishing must not overstate liveness, and the max-fold makes an older
    /// stamp safe. Re-arms the throttle window for the next turn.
    pub(super) async fn heartbeat_turn_finish(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
        handle: &str,
    ) {
        let now_ms = heartbeat_now_ms();
        let heartbeat_ms = {
            let mut st = self.lock();
            match st
                .residents
                .get_mut(&session)
                .filter(|slot| slot.claim.as_ref() == claim)
            {
                Some(slot) => {
                    slot.last_heartbeat_emit = Some(Instant::now());
                    slot.last_activity.unwrap_or(now_ms)
                }
                None => return,
            }
        };
        self.record_heartbeat(claim, handle, heartbeat_ms).await;
    }

    pub(super) fn owns_slot(&self, session: SessionId, claim: Option<&ActorClaim>) -> bool {
        self.lock()
            .residents
            .get(&session)
            .is_some_and(|slot| slot.claim.as_ref() == claim)
    }

    /// Decide atomically what this claim does next, charging the turn budget.
    pub(super) async fn next_action(
        &self,
        session: SessionId,
        claim: Option<&ActorClaim>,
    ) -> Action {
        let mut st = self.lock();
        let lead_failed = st.lead_failed
            || st.root_driver.runs.load(Ordering::Acquire) != 0
            || !st.root_driver.autonomous.load(Ordering::Acquire);
        let Some(slot) = st
            .residents
            .get_mut(&session)
            .filter(|slot| slot.claim.as_ref() == claim)
        else {
            unreachable!("episode task lost its slot");
        };
        if let Some(ending) = slot.ending.clone() {
            return Action::End(ending);
        }
        // A failed lead is never woken by the harness: its owed work (mail)
        // waits for the user to resume it.
        // Nor is a slot the user stopped: its work waits for the user too.
        if slot.has_work() && !(slot.is_main && lead_failed) && !slot.user_stopped {
            // Single active turn per session: when this session already has a
            // turn (e.g. the lead's own interactive turn while it is the main
            // actor), keep the owed work queued. The turn-boundary observer
            // re-wakes this slot when that turn's lease is released.
            let lease = match self.engine.try_begin_turn(session) {
                Ok(lease) => lease,
                Err(_) => {
                    let handle = slot.handle.clone();
                    let claim = slot.claim;
                    let became_idle = slot.status == SlotStatus::Busy;
                    if became_idle {
                        slot.status = SlotStatus::Idle;
                        st.busy = st.busy.saturating_sub(1);
                        self.maybe_fire_quiescence(&mut st);
                    }
                    return Action::Idle {
                        handle,
                        claim,
                        became_idle,
                    };
                }
            };
            let synth = slot.synth_pending;
            let claim = slot.claim;
            let cursor = slot.cursor;
            let agent = slot.agent.clone();
            let binding = slot.binding.clone();
            let agents = slot.agents.clone();
            let resources = slot.resources.clone();
            let guidance = slot.guidance.clone();
            let route = slot.route.clone();
            let sidecar_factory = slot.sidecar_factory.clone();
            let handle = slot.handle.clone();
            let is_main = slot.is_main;
            let cancel = slot.cancel.child_token();
            slot.pending = false;
            slot.synth_pending = false;
            // Reserve the start window before published registration events
            // can be observed as busy activity and race the boundary heartbeat.
            slot.last_heartbeat_emit = Some(Instant::now());
            if slot.status == SlotStatus::Idle {
                slot.status = SlotStatus::Busy;
                st.busy += 1;
            }
            Action::Run(Box::new(RunPlan {
                agent,
                binding,
                agents,
                resources,
                route,
                guidance,
                sidecar_factory,
                handle,
                is_main,
                cursor,
                synth,
                claim,
                cancel,
                lease,
            }))
        } else {
            let handle = slot.handle.clone();
            let claim = slot.claim;
            let became_idle = slot.status == SlotStatus::Busy;
            if became_idle {
                slot.status = SlotStatus::Idle;
                st.busy = st.busy.saturating_sub(1);
                self.maybe_fire_quiescence(&mut st);
            }
            Action::Idle {
                handle,
                claim,
                became_idle,
            }
        }
    }

    pub(super) fn finish_run(&self, session: SessionId, claim: Option<&ActorClaim>) {
        let mut state = self.lock();
        let has_work = match state
            .residents
            .get_mut(&session)
            .filter(|slot| slot.claim.as_ref() == claim)
        {
            Some(slot) => {
                slot.status = SlotStatus::Idle;
                slot.has_work()
            }
            None => return,
        };
        state.busy = state.busy.saturating_sub(1);
        if !has_work {
            self.maybe_fire_quiescence(&mut state);
        }
        drop(state);
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor.retire_team(self);
        }
    }

    pub(super) fn forget_episode(&self, session: SessionId, claim: Option<&ActorClaim>) {
        if let Some(supervisor) = self.supervisor.upgrade() {
            let mut episodes = supervisor
                .episodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if episodes
                .get(&session)
                .is_some_and(|team| std::ptr::eq(team.as_ref(), self))
            {
                episodes.remove(&session);
            }
            drop(episodes);
            supervisor.forget_wait_stalls(session);
            if claim.is_some() {
                supervisor.retire_team(self);
            }
        }
    }

    /// If the team just went fully idle with new work since the last synthesis,
    /// wake the main actor to synthesize. Pure in-memory + `Notify`, safe under
    /// the lock. Not firing when `work_seq` is unchanged is what guarantees
    /// termination: main's own synthesis (which produces no new work) leaves
    /// `work_seq` equal, so the next idle transition is a no-op (the team is done).
    pub(super) fn maybe_fire_quiescence(&self, st: &mut TeamState) {
        // A failed lead gets no synthesis turn: the lead is dead until the
        // user resumes it (the synthesis re-fires after its next turn).
        if st.phase.killed()
            || st.busy != 0
            || st
                .residents
                .values()
                .any(|slot| !slot.is_main && slot.has_work())
            || st.lead_failed
            || st.root_driver.runs.load(Ordering::Acquire) != 0
            || !st.root_driver.autonomous.load(Ordering::Acquire)
        {
            return;
        }
        // Nor does a lead the user stopped: the user, not the harness, says
        // what happens next (the synthesis re-fires after its next turn).
        if st
            .main_session
            .and_then(|main| st.residents.get(&main))
            .is_some_and(|slot| slot.user_stopped)
        {
            return;
        }
        // A member (usually the lead) whose own turn is still running is not
        // idle, even when the supervisor did not start that turn. Quiescence
        // is re-checked when that turn's lease is released.
        if st
            .residents
            .keys()
            .any(|session| self.engine.turn_active(*session))
        {
            return;
        }
        if st.work_seq == st.last_synth_work_seq {
            return; // nothing new since last synthesis → team is done, parked idle
        }
        st.last_synth_work_seq = st.work_seq;
        if let Some(main_session) = st.main_session
            && let Some(main_slot) = st.residents.get_mut(&main_session)
        {
            main_slot.synth_pending = true;
            main_slot.pending = true;
            main_slot.notify.notify_one();
        }
    }

    /// In-turn steering (or an explicit read, or `wait`) consumed the lead's
    /// queued mail, as of the inbox snapshot taken after `mark` (the team's
    /// `work_seq` then). Do not replay that already-seen work as a quiescence
    /// synthesis wake when the lease ends. Work counted after `mark` was not in
    /// that snapshot: it keeps its wake and its synthesis.
    pub(super) fn on_mail_consumed(&self, session: SessionId, mark: u64) {
        let mut st = self.lock();
        if st.main_session != Some(session) {
            return;
        }
        if st.work_seq == mark
            && let Some(slot) = st.residents.get_mut(&session)
        {
            slot.pending = false;
        }
        // Never move the synthesis mark backwards past a synthesis that
        // already fired (`u64::MAX`: none has yet).
        if st.last_synth_work_seq == u64::MAX || st.last_synth_work_seq < mark {
            st.last_synth_work_seq = mark;
        }
    }

    /// A turn on `session` ended (its lease was released). Deliver work that
    /// was deferred while it ran — mail or a synthesis owed to that slot — and
    /// re-check quiescence, which that turn was holding back.
    pub(super) fn on_turn_released(&self, session: SessionId) {
        let mut st = self.lock();
        if st.phase.killed() {
            return;
        }
        if let Some(slot) = st.residents.get(&session)
            && slot.has_work()
        {
            slot.notify.notify_one();
        }
        self.maybe_fire_quiescence(&mut st);
        drop(st);
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor.retire_team(self);
        }
    }

    /// The lead's turn failed: hold back every harness-started lead turn
    /// (synthesis, mail wakes) until the lead is resumed from outside.
    pub(super) fn on_lead_turn_failed(&self, session: SessionId) {
        let mut st = self.lock();
        if st.main_session != Some(session) {
            return;
        }
        st.lead_failed = true;
        if let Some(slot) = st.residents.get_mut(&session) {
            slot.synth_pending = false;
        }
    }

    /// A turn on the lead started. While the lead is marked failed the
    /// supervisor starts none, so this is the user resuming it.
    pub(super) fn on_lead_turn_started(&self, session: SessionId) {
        let mut st = self.lock();
        if st.main_session == Some(session) {
            st.lead_failed = false;
            if st.root_driver.runs.load(Ordering::Acquire) == 0 {
                st.root_driver.autonomous.store(true, Ordering::Release);
            }
        }
    }

    /// The sessions of `session`'s slot and every slot below it: the whole
    /// team for the lead (or the root when it has no main slot).
    pub(super) fn subtree(st: &TeamState, root: SessionId, session: SessionId) -> Vec<SessionId> {
        let handle = st
            .residents
            .get(&session)
            .filter(|slot| !slot.is_main && session != root)
            .map(|slot| slot.handle.clone());
        if handle.is_none() && session != root {
            return Vec::new();
        }
        st.residents
            .iter()
            .filter(|(_, slot)| {
                handle.as_deref().is_none_or(|handle| {
                    slot.handle == handle
                        || slot
                            .handle
                            .strip_prefix(handle)
                            .is_some_and(|rest| rest.starts_with(scope::PATH_SEPARATOR))
                })
            })
            .map(|(member, _)| *member)
            .collect()
    }

    /// The user cancelled `session`'s turn: stop its slot and every slot below
    /// it. Their queued mail wakes stay owed but held (a pending synthesis is
    /// dropped): the harness starts none of their turns until
    /// [`resume_for_user`](Self::resume_for_user) delivers them. Returns the
    /// stopped sessions, whose in-flight turns the caller cancels — none, and
    /// nothing is stopped, when neither `session` nor any slot below it is
    /// running or owes a turn (there is nothing to interrupt).
    pub(super) fn stop_for_user(&self, session: SessionId) -> Vec<SessionId> {
        let mut st = self.lock();
        if st.phase.killed() {
            return Vec::new();
        }
        let stopped = Self::subtree(&st, self.root, session);
        let running = self.engine.turn_active(session)
            || stopped.iter().any(|member| {
                self.engine.turn_active(*member)
                    || st.residents.get(member).is_some_and(|slot| {
                        slot.status == SlotStatus::Busy || (slot.has_work() && !slot.user_stopped)
                    })
            });
        if !running {
            return Vec::new();
        }
        for member in &stopped {
            if let Some(slot) = st.residents.get_mut(member) {
                slot.user_stopped = true;
                slot.synth_pending = false;
            }
        }
        stopped
    }

    /// The user acted on `session` again (a prompt or a steer): lift the stop
    /// from its slot and every slot below it, waking those with mail queued
    /// before or during the stop.
    pub(super) fn resume_for_user(&self, session: SessionId) {
        let mut st = self.lock();
        for member in Self::subtree(&st, self.root, session) {
            if let Some(slot) = st.residents.get_mut(&member)
                && std::mem::take(&mut slot.user_stopped)
                && slot.has_work()
            {
                slot.notify.notify_one();
            }
        }
    }

    /// Whether the user stopped the live slot at `handle`.
    pub(super) fn handle_stopped_by_user(&self, handle: &str) -> bool {
        self.lock()
            .residents
            .values()
            .any(|slot| slot.handle == handle && slot.user_stopped)
    }
}
