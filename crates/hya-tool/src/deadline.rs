//! Wall-clock budget of a headless run (`hya exec --time-budget`).
//!
//! An outside harness kills the whole process at the budget, so the deadline
//! is a fact of the process, not of a session: [`install`] once at start,
//! [`current`] anywhere. The remaining time is split into stages that decide
//! which harness notices the agents get and how long a tool call may block.
//! A native tool bundle links its own copy of this crate; the loader hands it
//! the host's cell ([`crate::native_bundle::share_deadline_v1`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

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

/// The run's start and budget, and the stage each session was last told of.
#[derive(Debug)]
pub struct RunDeadline {
    start: Instant,
    budget: Duration,
    notified: Mutex<HashMap<SessionId, Stage>>,
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

impl RunDeadline {
    fn new(start: Instant, budget: Duration) -> Self {
        Self {
            start,
            budget,
            notified: Mutex::new(HashMap::new()),
        }
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
    /// capped to `max(remaining - grace, MIN_TOOL_SECS)`, and whether it
    /// was capped. A call without a timeout is capped too: the kill would
    /// end it anyway, with nothing returned.
    #[must_use]
    pub fn cap(&self, requested: Option<Duration>) -> (Duration, bool) {
        let limit = self
            .remaining()
            .saturating_sub(self.grace())
            .max(Duration::from_secs(MIN_TOOL_SECS));
        match requested {
            Some(requested) if requested <= limit => (requested, false),
            _ => (limit, true),
        }
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
        let mut notified = self.notified.lock().unwrap_or_else(PoisonError::into_inner);
        let told = notified.entry(session).or_insert(Stage::Normal);
        (*told < stage).then(|| {
            *told = stage;
            stage
        })
    }

    /// Record that `session` was told of `stage` another way (a lead
    /// continuation notice), so no in-turn notice repeats it.
    pub fn mark_notified(&self, session: SessionId, stage: Stage) {
        let mut notified = self.notified.lock().unwrap_or_else(PoisonError::into_inner);
        let told = notified.entry(session).or_insert(Stage::Normal);
        *told = (*told).max(stage);
    }

    /// The first notice of a budgeted run.
    #[must_use]
    pub fn opening(&self) -> String {
        format!(
            "HEADLESS RUN — time budget: {}s wall clock for this whole run; at the limit the \
             process is killed and only files on disk count. Put a working deliverable in place \
             early.",
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
                 the request's own checks, then finish. Give long commands a timeout well under \
                 the time left."
            ),
            (Stage::Final | Stage::Over, false) => format!(
                "HEADLESS RUN — FINAL {left}s: write any missing deliverable now from the work \
                 you have, remove scratch files you created in the requested location, commit \
                 if the request asks for a commit, then end your turn. Start no new install, \
                 build, training or test run."
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
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

    /// D2: a tool call keeps the grace free, but never gets under 5 s.
    #[test]
    fn d2_cap_keeps_the_grace_free() {
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
        // R = 40 < G: the floor.
        assert_eq!(
            started(900, 860).cap(Some(Duration::from_secs(300))),
            (Duration::from_secs(MIN_TOOL_SECS), true)
        );
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
