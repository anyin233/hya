//! `yaca-tool` — Tool trait + registry + the allow/ask/deny permission plane.

pub mod interaction;
pub mod permission;
pub mod tool;

pub use interaction::{
    InteractionError, InteractionPlane, QuestionAnswer, QuestionKind, QuestionRequest,
};
pub use permission::{
    Action, AskRequest, Decision, Mode, PermissionError, PermissionPlane, PermissionRules,
    Resource, Rule, glob_match,
};
pub use tool::{Tool, ToolCtx, ToolError, ToolRegistry};
