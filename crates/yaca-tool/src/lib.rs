//! `yaca-tool` — Tool trait + registry + the allow/ask/deny permission plane.

pub mod interaction;
pub mod permission;
pub mod spawn;
pub mod tool;

pub use interaction::{
    InteractionError, InteractionPlane, QuestionAnswer, QuestionKind, QuestionRequest,
};
pub use permission::{
    Action, AskRequest, Decision, Mode, PermissionError, PermissionPlane, PermissionRules,
    Resource, Rule, glob_match,
};
pub use spawn::{MemberOutcome, SpawnError, SpawnMember, SpawnRequest, SpawnerPlane};
pub use tool::{DuplicateName, Tool, ToolCtx, ToolError, ToolRegistry};
