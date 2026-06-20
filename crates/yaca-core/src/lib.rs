//! `yaca-core` — SessionEngine, the agent turn loop, and the in-process EventBus.
//! Team orchestration and the completion (goal + loop) engines land in later phases.

pub mod bus;
pub mod completion;
pub mod engine;
pub mod error;

pub use bus::EventBus;
pub use completion::{
    GoalEvaluator, IterationDriver, ModelGoalEvaluator, RunOutcome, SafetyCaps, Verdict, run_goal,
};
pub use engine::{AgentSpec, CreateSession, SessionEngine};
pub use error::CoreError;
