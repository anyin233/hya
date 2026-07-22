//! `hya-tool` — Tool trait + registry + the allow/ask/deny permission plane.

mod agents;
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
pub mod mailbox;
mod output_cap;
pub mod permission;
mod plan;
mod question;
mod read;
mod read_media;
mod read_text;
mod shell;
mod skill;
mod skill_catalog;
pub mod spawn;
mod task;
pub mod todo;
pub mod tool;
mod utf8_bom;
mod webfetch;
mod websearch;
mod websearch_response;
mod write;

pub use agents::{AgentCatalogPlane, AgentDef};
pub use formatter::{
    BuiltinFormatterProvider, FormatterConfig, FormatterEntry, FormatterError, FormatterPlane,
    FormatterProvider, FormatterStatus,
};
pub use interaction::{
    InteractionError, InteractionPlane, QuestionAnswer, QuestionInfo, QuestionKind, QuestionOption,
    QuestionPrompt, QuestionReply, QuestionRequest,
};
pub use lsp_plane::{LspError, LspOperation, LspPlane, LspProvider, LspRequest};
pub use mailbox::{ChannelInfo, MailReceipt, MailboxError, MailboxPlane, MailboxRequest};
pub use output_cap::{MAX_TOOL_OUTPUT_CHARS, cap_tool_output};
pub use permission::{
    Action, AskRequest, Decision, ExactSubject, Invocation, InvocationDecision, InvocationPolicy,
    InvocationRule, Mode, PermissionError, PermissionInterceptor, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, RememberScope, Resource, Rule, glob_match,
};
pub use skill::SkillPlane;
pub use skill_catalog::{
    ParsedSkill, SkillCatalogEntry, discover_skills, discover_skills_from_dirs, parse_skill,
    skill_dirs_for_workdir, skills_section,
};
pub use spawn::{InlineAgent, MemberOutcome, SpawnError, SpawnMember, SpawnRequest, SpawnerPlane};
pub use todo::{TodoItem, TodoPlane, TodoPriority, TodoStatus};
pub use tool::{
    DuplicateName, ResolvedTool, Tool, ToolCtx, ToolError, ToolPermission, ToolRegistry,
};
pub use websearch::{WebSearchConfig, WebSearchPlane, WebSearchProvider};
