//! `yaca-tool` — Tool trait + registry + the allow/ask/deny permission plane.

mod apply_patch;
mod edit;
mod edit_replace;
mod file_diff;
mod formatter;
mod formatter_catalog;
mod formatter_command;
mod formatter_definition;
pub mod interaction;
mod invalid;
mod lsp;
mod lsp_path;
mod lsp_plane;
mod lsp_post_edit;
pub mod permission;
mod plan;
mod question;
mod read;
mod read_media;
mod read_text;
mod shell;
mod skill;
pub mod spawn;
mod task;
pub mod todo;
pub mod tool;
mod utf8_bom;
mod webfetch;
mod websearch;
mod websearch_response;
mod write;

pub use formatter::{
    BuiltinFormatterProvider, FormatterConfig, FormatterEntry, FormatterError, FormatterPlane,
    FormatterProvider, FormatterStatus,
};
pub use interaction::{
    InteractionError, InteractionPlane, QuestionAnswer, QuestionInfo, QuestionKind, QuestionOption,
    QuestionPrompt, QuestionReply, QuestionRequest,
};
pub use lsp_plane::{LspError, LspOperation, LspPlane, LspProvider, LspRequest};
pub use permission::{
    Action, AskRequest, Decision, Mode, PermissionError, PermissionInterceptor, PermissionPlane,
    PermissionRules, Resource, Rule, glob_match,
};
pub use skill::SkillPlane;
pub use spawn::{MemberOutcome, SpawnError, SpawnMember, SpawnRequest, SpawnerPlane};
pub use todo::{TodoItem, TodoPlane, TodoPriority, TodoStatus};
pub use tool::{DuplicateName, Tool, ToolCtx, ToolError, ToolRegistry};
pub use websearch::{WebSearchPlane, WebSearchProvider};
