//! Wall-clock budget of a headless run (`hya exec --time-budget`).
//!
//! An outside harness kills the whole process at the budget, so the deadline
//! is a fact of the process, not of a session: [`install`] once at start,
//! [`current`] anywhere. The remaining time is split into stages that decide
//! which harness notices the agents get and how long a tool call may block.
//! A native tool bundle links its own copy of this crate; the loader hands it
//! the host's cell ([`crate::native_bundle::share_deadline_v1`]).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};
use std::time::Duration;

// Tokio's clock, so a paused test runtime moves the stages with its timers.
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use hya_proto::SessionId;

/// Shortest timeout a capped tool call still gets.
pub const MIN_TOOL_SECS: u64 = 5;

/// How close the run is to its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// No notice.
    Normal,
    /// Stop exploring; make the deliverables complete.
    WrapUp,
    /// Write what is missing now and end.
    Final,
    /// Within the grace before the kill: no new turn can finish.
    Over,
}

/// The run's start and budget, and what each session was last told.
#[derive(Debug)]
pub struct RunDeadline {
    start: Instant,
    budget: Duration,
    notified: Mutex<HashMap<SessionId, Told>>,
    background: BackgroundJobs,
}

#[derive(Debug, Default)]
struct BackgroundJobs {
    shutdown: CancellationToken,
    jobs: Mutex<Vec<oneshot::Receiver<()>>>,
}

/// The stage a session was last told of, and the tenth of the budget that
/// had passed when it last read the clock.
#[derive(Clone, Copy, Debug)]
struct Told {
    stage: Stage,
    tenth: u64,
}

impl Told {
    const START: Self = Self {
        stage: Stage::Normal,
        tenth: 0,
    };
}

static DEADLINE: OnceLock<RunDeadline> = OnceLock::new();
/// In a native bundle's copy of this crate: the host's [`DEADLINE`].
static HOST: OnceLock<&'static OnceLock<RunDeadline>> = OnceLock::new();

/// Start the run's clock with `budget`.
///
/// # Panics
/// When called a second time: one process runs one budgeted run.
pub fn install(budget: Duration) {
    assert!(
        DEADLINE
            .set(RunDeadline::new(Instant::now(), budget))
            .is_ok(),
        "run deadline installed twice"
    );
}

/// The run's deadline, when it has a budget.
#[must_use]
pub fn current() -> Option<&'static RunDeadline> {
    HOST.get().copied().unwrap_or(&DEADLINE).get()
}

/// Cancel yielded jobs and wait for their supervisors before leaving exec.
pub async fn shutdown_background_jobs() {
    if let Some(deadline) = current() {
        deadline.shutdown_background_jobs().await;
    } else {
        UNBUDGETED_JOBS.shutdown().await;
    }
}

/// Dropping the supervisor's sender closes its completion receiver.
#[must_use]
pub struct BackgroundJobGuard {
    _completion: oneshot::Sender<()>,
}

/// This copy's cell, for the loader to hand to a bundle.
pub(crate) fn cell() -> &'static OnceLock<RunDeadline> {
    &DEADLINE
}

/// Read the host's cell from now on (in a bundle's copy, once at load).
pub(crate) fn adopt(host: &'static OnceLock<RunDeadline>) {
    assert!(HOST.set(host).is_ok(), "host deadline adopted twice");
}

fn clamp_secs(value: f64, low: f64, high: f64) -> Duration {
    Duration::from_secs_f64(value.clamp(low, high))
}

static UNBUDGETED_JOBS: LazyLock<BackgroundJobs> = LazyLock::new(BackgroundJobs::default);

#[cfg(unix)]
pub(crate) fn register_unbudgeted_job() -> (CancellationToken, BackgroundJobGuard) {
    UNBUDGETED_JOBS.register()
}

impl BackgroundJobs {
    /// Register a yielded job before handing it to its supervisor.
    fn register(&self) -> (CancellationToken, BackgroundJobGuard) {
        let (tx, rx) = oneshot::channel();
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.retain_mut(|job| matches!(job.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
        jobs.push(rx);
        (
            self.shutdown.clone(),
            BackgroundJobGuard { _completion: tx },
        )
    }

    /// Called after tool producers have drained. Join every remaining supervisor.
    async fn shutdown(&self) {
        self.shutdown.cancel();
        let jobs = std::mem::take(&mut *self.jobs.lock().unwrap_or_else(PoisonError::into_inner));
        for job in jobs {
            let _ = job.await;
        }
    }
}

impl RunDeadline {
    /// Measure a run's budget from the supplied clock instant.
    #[must_use]
    pub fn new(start: Instant, budget: Duration) -> Self {
        Self {
            start,
            budget,
            notified: Mutex::new(HashMap::new()),
            background: BackgroundJobs::default(),
        }
    }

    /// Register a yielded job before starting its supervisor.
    pub fn register_background_job(&self) -> (CancellationToken, BackgroundJobGuard) {
        self.background.register()
    }

    /// Stop and join yielded jobs before exiting.
    pub async fn shutdown_background_jobs(&self) {
        self.background.shutdown().await;
    }

    /// The whole budget.
    #[must_use]
    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// Time since the run started.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Time left before the kill.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.budget.saturating_sub(self.elapsed())
    }

    /// Remaining time at which [`Stage::WrapUp`] starts: early enough that a
    /// long model step (p99 ~120 s, max ~480 s recorded) still fits after it,
    /// at most an hour before the end and never in the first half.
    #[must_use]
    pub fn wrap_up_window(&self) -> Duration {
        let budget = self.budget.as_secs_f64();
        clamp_secs(0.35 * budget, 300.0, 3600.0).min(Duration::from_secs_f64(0.5 * budget))
    }

    /// Remaining time at which [`Stage::Final`] starts: one step plus one
    /// file write.
    #[must_use]
    pub fn final_window(&self) -> Duration {
        clamp_secs(0.12 * self.budget.as_secs_f64(), 90.0, 600.0)
    }

    /// Remaining time at which [`Stage::Over`] starts; also kept free of
    /// tool calls by [`RunDeadline::cap`].
    #[must_use]
    pub fn grace(&self) -> Duration {
        clamp_secs(0.05 * self.budget.as_secs_f64(), 30.0, 120.0)
    }

    /// Least time a model step that has started reasoning gets before a
    /// checkpoint may cut it: about a short step, and at most half of what is
    /// left before [`Stage::Over`].
    #[must_use]
    pub fn reasoning_grace(&self) -> Duration {
        clamp_secs(0.03 * self.budget.as_secs_f64(), 15.0, 30.0)
            .min(self.remaining().saturating_sub(self.grace()) / 2)
    }

    /// The stage now.
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage_at(self.remaining())
    }

    fn stage_at(&self, remaining: Duration) -> Stage {
        if remaining <= self.grace() {
            Stage::Over
        } else if remaining <= self.final_window() {
            Stage::Final
        } else if remaining <= self.wrap_up_window() {
            Stage::WrapUp
        } else {
            Stage::Normal
        }
    }

    /// The timeout a tool call may use now: `requested` (`None` = none)
    /// capped to what is left after a reserve for the step that reads the
    /// result, and whether it was capped. The reserve is the grace, or half
    /// of what is left once that is less: near the end a call may still use
    /// as much time as it leaves behind. Never under [`MIN_TOOL_SECS`]. A
    /// call without a timeout is capped too: the kill would end it anyway,
    /// with nothing returned.
    #[must_use]
    pub fn cap(&self, requested: Option<Duration>) -> (Duration, bool) {
        let remaining = self.remaining();
        let limit = remaining
            .saturating_sub(self.grace().min(remaining / 2))
            .max(Duration::from_secs(MIN_TOOL_SECS));
        match requested {
            Some(requested) if requested <= limit => (requested, false),
            _ => (limit, true),
        }
    }

    /// The next stage `session` has not been told of, and how long until it
    /// starts (zero once it has); `None` once it was told of
    /// [`Stage::Final`]. A model stream that has shown only reasoning is cut
    /// at this point.
    #[must_use]
    pub fn next_checkpoint(&self, session: SessionId) -> Option<(Duration, Stage)> {
        let told = self
            .notified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session)
            .map_or(Stage::Normal, |told| told.stage);
        if told >= Stage::Final {
            return None;
        }
        let now = self.stage().min(Stage::Final);
        if now > told {
            return Some((Duration::ZERO, now));
        }
        let (window, stage) =
            if told == Stage::Normal && self.wrap_up_window() > self.final_window() {
                (self.wrap_up_window(), Stage::WrapUp)
            } else {
                (self.final_window(), Stage::Final)
            };
        let at = self.budget.saturating_sub(window);
        Some((at.saturating_sub(self.elapsed()), stage))
    }

    /// The stage to announce to `session` now, at most once per stage per
    /// session and recorded as told; [`Stage::Over`] is announced as
    /// [`Stage::Final`].
    pub fn take_notice(&self, session: SessionId) -> Option<Stage> {
        let stage = match self.stage() {
            Stage::Normal => return None,
            Stage::WrapUp => Stage::WrapUp,
            Stage::Final | Stage::Over => Stage::Final,
        };
        let tenth = self.tenth();
        let mut notified = self.notified.lock().unwrap_or_else(PoisonError::into_inner);
        let told = notified.entry(session).or_insert(Told::START);
        (told.stage < stage).then(|| {
            *told = Told { stage, tenth };
            stage
        })
    }

    /// Record that `session` was told of `stage` another way (a lead
    /// continuation notice), so no in-turn notice repeats it.
    pub fn mark_notified(&self, session: SessionId, stage: Stage) {
        let tenth = self.tenth();
        let mut notified = self.notified.lock().unwrap_or_else(PoisonError::into_inner);
        let told = notified.entry(session).or_insert(Told::START);
        *told = Told {
            stage: told.stage.max(stage),
            tenth,
        };
    }

    /// The clock line for `session` when another tenth of the budget has
    /// passed since it last read the time (the opening, a stage notice, or
    /// an earlier clock line). An agent has no other clock, and a notice per
    /// result would sit in every file it reads.
    pub fn take_clock(&self, session: SessionId) -> Option<String> {
        let tenth = self.tenth();
        let mut notified = self.notified.lock().unwrap_or_else(PoisonError::into_inner);
        let told = notified.entry(session).or_insert(Told::START);
        (told.tenth < tenth).then(|| {
            told.tenth = tenth;
            format!(
                "HEADLESS RUN — clock: {}s of {}s used, {}s left.",
                self.elapsed().as_secs(),
                self.budget.as_secs(),
                self.remaining().as_secs()
            )
        })
    }

    /// Whole tenths of the budget used so far.
    fn tenth(&self) -> u64 {
        let budget = self.budget.as_millis().max(1);
        u64::try_from(self.elapsed().as_millis() * 10 / budget).unwrap_or(u64::MAX)
    }

    /// The first notice of a budgeted run.
    #[must_use]
    pub fn opening(&self) -> String {
        format!(
            "HEADLESS RUN — time budget: {}s wall clock for this whole run; at the limit the \
             process is killed and only files on disk count. Put a working deliverable in place \
             early. Watch for deadline notices as the run goes.",
            self.budget.as_secs()
        )
    }

    /// The notice for `stage` (`WrapUp` or later); `subagent` selects the
    /// text for an agent that delivers by `report`.
    #[must_use]
    pub fn notice(&self, stage: Stage, subagent: bool) -> String {
        let (elapsed, budget, left) = (
            self.elapsed().as_secs(),
            self.budget.as_secs(),
            self.remaining().as_secs(),
        );
        match (stage, subagent) {
            (Stage::Normal | Stage::WrapUp, false) => format!(
                "HEADLESS RUN — TIME: {elapsed}s of {budget}s used, {left}s left. At the limit \
                 the run is killed and only files on disk count. Stop exploring and polishing: \
                 make sure every requested deliverable exists at its requested path and passes \
                 the request's own checks, then finish. Start only commands that finish well \
                 within the time left."
            ),
            (Stage::Final | Stage::Over, false) => format!(
                "HEADLESS RUN — FINAL {left}s: {elapsed}s of {budget}s used. Write any missing \
                 deliverable now from the work you have, commit if the request asks for a \
                 commit, then end your turn. Do not edit a file you have already delivered in \
                 place: if it needs a change, change a copy, run the check the file passed, and \
                 replace the file only if the copy passes. Start only commands that finish well \
                 within the time left."
            ),
            (Stage::Normal | Stage::WrapUp, true) => format!(
                "HEADLESS RUN — TIME: {elapsed}s of {budget}s used, {left}s left in the whole \
                 run; at the limit every agent is stopped. Finish the piece in hand and call \
                 `report` with what you have."
            ),
            (Stage::Final | Stage::Over, true) => {
                format!("HEADLESS RUN — FINAL {left}s: call `report` now with what you have.")
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn started(budget: u64, elapsed: u64) -> RunDeadline {
        let start = Instant::now()
            .checked_sub(Duration::from_secs(elapsed))
            .expect("monotonic clock far enough from boot");
        RunDeadline::new(start, Duration::from_secs(budget))
    }

    fn session() -> SessionId {
        SessionId::new()
    }

    /// D1: the windows of the plan's table (budget → W1, W2, G).
    #[test]
    fn d1_windows_follow_the_budget() {
        for (budget, wrap_up, last, grace) in [
            (900, 315, 108, 45),
            (1200, 420, 144, 60),
            (1800, 630, 216, 90),
            (3600, 1260, 432, 120),
            (28800, 3600, 600, 120),
            (40, 20, 90, 30),
        ] {
            let deadline = started(budget, 0);
            assert_eq!(
                deadline.wrap_up_window().as_secs(),
                wrap_up,
                "W1 for {budget}"
            );
            assert_eq!(deadline.final_window().as_secs(), last, "W2 for {budget}");
            assert_eq!(deadline.grace().as_secs(), grace, "G for {budget}");
        }
        let deadline = started(900, 0);
        let stage = |left: u64| deadline.stage_at(Duration::from_secs(left));
        assert_eq!(stage(316), Stage::Normal);
        assert_eq!(stage(315), Stage::WrapUp);
        assert_eq!(stage(108), Stage::Final);
        assert_eq!(stage(45), Stage::Over);
        assert_eq!(started(40, 0).stage(), Stage::Final);
        assert_eq!(started(30, 0).stage(), Stage::Over);
    }

    /// D2: a tool call keeps the grace free, or half of what is left near
    /// the end, and never gets under 5 s.
    #[test]
    fn d2_cap_keeps_a_reserve_free() {
        // B = 900, G = 45: R = 200 leaves 155 s.
        let deadline = started(900, 700);
        let (timeout, capped) = deadline.cap(Some(Duration::from_secs(300)));
        assert!(capped);
        assert!((154..=155).contains(&timeout.as_secs()), "{timeout:?}");
        assert_eq!(
            deadline.cap(Some(Duration::from_secs(10))),
            (Duration::from_secs(10), false)
        );
        let (none, capped) = deadline.cap(None);
        assert!(capped && none.as_secs() <= 155);
        // B = 3600, G = 120, R = 121: a 60 s command runs whole.
        assert_eq!(
            started(3600, 3479).cap(Some(Duration::from_secs(60))),
            (Duration::from_secs(60), false)
        );
        // R = 40 < G = 45: half of what is left.
        let (timeout, capped) = started(900, 860).cap(Some(Duration::from_secs(300)));
        assert!(capped);
        assert!((19..=20).contains(&timeout.as_secs()), "{timeout:?}");
        // R = 6: the floor.
        assert_eq!(
            started(900, 894).cap(Some(Duration::from_secs(300))),
            (Duration::from_secs(MIN_TOOL_SECS), true)
        );
    }

    /// D4: the clock is told once per tenth of the budget, and a stage
    /// notice counts as reading it.
    #[test]
    fn d4_the_clock_is_told_once_per_tenth() {
        let lead = session();
        assert_eq!(started(900, 80).take_clock(lead), None);
        let third = started(900, 300);
        let clock = third.take_clock(lead).expect("three tenths passed");
        assert!(
            clock.starts_with("HEADLESS RUN — clock: 300s of 900s used, "),
            "{clock}"
        );
        assert_eq!(third.take_clock(lead), None);
        let wrap_up = started(900, 600);
        assert_eq!(wrap_up.take_notice(lead), Some(Stage::WrapUp));
        assert_eq!(wrap_up.take_clock(lead), None);
        let member = session();
        wrap_up.mark_notified(member, Stage::WrapUp);
        assert_eq!(wrap_up.take_clock(member), None);
    }

    #[test]
    fn final_notice_text_for_lead_and_subagent() {
        // Before the clock starts, elapsed time is zero for both notices.
        let deadline = RunDeadline::new(
            Instant::now() + Duration::from_secs(3600),
            Duration::from_secs(40),
        );
        assert_eq!(
            deadline.notice(Stage::Final, false),
            "HEADLESS RUN — FINAL 40s: 0s of 40s used. Write any missing deliverable now from \
             the work you have, commit if the request asks for a commit, then end your turn. \
             Do not edit a file you have already delivered in place: if it needs a change, \
             change a copy, run the check the file passed, and replace the file only if the \
             copy passes. Start only commands that finish well within the time left."
        );
        assert_eq!(
            deadline.notice(Stage::Final, true),
            "HEADLESS RUN — FINAL 40s: call `report` now with what you have."
        );
    }

    /// D5: the next checkpoint follows what the session was told, and
    /// nothing follows a Final notice.
    #[test]
    fn d5_next_checkpoint_follows_what_was_told() {
        // B = 900: WrapUp at 585 s used, Final at 792 s.
        let lead = session();
        let early = started(900, 100);
        let (delay, stage) = early.next_checkpoint(lead).expect("not told of Final");
        assert_eq!(stage, Stage::WrapUp);
        assert!((484..=485).contains(&delay.as_secs()), "{delay:?}");
        let wrap_up = started(900, 600);
        assert_eq!(
            wrap_up.next_checkpoint(lead),
            Some((Duration::ZERO, Stage::WrapUp)),
            "a stage nobody announced is due now"
        );
        wrap_up.mark_notified(lead, Stage::WrapUp);
        let (delay, stage) = wrap_up.next_checkpoint(lead).expect("not told of Final");
        assert_eq!(stage, Stage::Final);
        assert!((191..=192).contains(&delay.as_secs()), "{delay:?}");
        // Told of WrapUp early, as a lead continuation notice does.
        let member = session();
        early.mark_notified(member, Stage::WrapUp);
        assert_eq!(
            early.next_checkpoint(member).map(|(_, stage)| stage),
            Some(Stage::Final)
        );
        // A stream that ran from Normal past Final reports Final; so does Over.
        assert_eq!(
            started(900, 800).next_checkpoint(session()),
            Some((Duration::ZERO, Stage::Final))
        );
        assert_eq!(
            started(900, 890).next_checkpoint(session()),
            Some((Duration::ZERO, Stage::Final))
        );
        // A budget whose WrapUp window is not wider than Final's goes to Final.
        // B = 150: WrapUp 75 s, Final 90 s.
        let (delay, stage) = started(150, 0)
            .next_checkpoint(session())
            .expect("not told of Final");
        assert_eq!(stage, Stage::Final);
        assert!((59..=60).contains(&delay.as_secs()), "{delay:?}");
        wrap_up.mark_notified(lead, Stage::Final);
        assert_eq!(wrap_up.next_checkpoint(lead), None);
        let over = started(900, 890);
        over.mark_notified(member, Stage::Final);
        assert_eq!(over.next_checkpoint(member), None);
    }

    /// D6: the reasoning grace is 3 % of the budget within 15–30 s, and at
    /// most half of what is left before Over.
    #[test]
    fn d6_reasoning_grace_follows_budget_and_time_left() {
        let grace = |budget, elapsed| started(budget, elapsed).reasoning_grace().as_millis();
        // B = 900: 27 s early, at WrapUp and at Final; G = 45 s.
        assert_eq!(grace(900, 0), 27_000);
        assert_eq!(grace(900, 585), 27_000);
        assert_eq!(grace(900, 792), 27_000);
        // R = 50: (50 - 45) / 2.
        assert!((2_499..=2_500).contains(&grace(900, 850)));
        assert_eq!(grace(900, 860), 0, "Over");
        assert_eq!(grace(1800, 0), 30_000);
        assert_eq!(grace(28800, 0), 30_000);
        // B = 30 starts in Over: no grace.
        assert_eq!(grace(30, 0), 0);
    }

    /// D3: once per stage per session, and only upward.
    #[test]
    fn d3_each_stage_is_told_once() {
        let (lead, member) = (session(), session());
        assert_eq!(started(900, 0).take_notice(lead), None);
        let wrap_up = started(900, 600);
        assert_eq!(wrap_up.take_notice(lead), Some(Stage::WrapUp));
        assert_eq!(wrap_up.take_notice(lead), None);
        assert_eq!(wrap_up.take_notice(member), Some(Stage::WrapUp));
        let over = started(900, 890);
        assert_eq!(over.take_notice(lead), Some(Stage::Final));
        assert_eq!(over.take_notice(lead), None);
        over.mark_notified(member, Stage::Final);
        assert_eq!(over.take_notice(member), None);
        over.mark_notified(member, Stage::WrapUp);
        assert_eq!(over.take_notice(member), None, "stages never go back");
    }
}
