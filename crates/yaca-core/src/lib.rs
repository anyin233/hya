//! `yaca-core` — SessionEngine, the agent turn loop, and the in-process EventBus.
//! Team orchestration and the completion (goal + loop) engines land in later phases.

pub mod bus;
pub mod category;
pub mod completion;
pub mod engine;
pub mod error;
pub mod loop_mode;
pub mod subagent;
pub mod team;

pub use bus::EventBus;
pub use category::{
    CategoryEntry, CategoryRegistry, ResolvedCategory, build_member_agent, inject_skills,
};
pub use completion::{
    GoalEvaluator, IterationDriver, ModelGoalEvaluator, RunOutcome, SafetyCaps, Verdict, run_goal,
};
pub use engine::{AgentSpec, CreateSession, SessionEngine};
pub use error::CoreError;
pub use loop_mode::{
    EvidenceQuality, LoopConfig, LoopPlanner, LoopVerifier, PlannerOutput, VerifierVerdict,
    cost_preflight, drive_loop, run_loop,
};
pub use subagent::{
    MemberEvidence, MemberSpec, MemberStatus, TeamEvidenceEnvelope, project_envelope, run_team,
};
pub use team::{
    MailEndpoint, MailKind, MemberState, TaskStatus, TeamControlPlane, TeamError, TeamState,
    team_transition,
};
