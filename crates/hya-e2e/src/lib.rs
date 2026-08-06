//! Process-level E2E harness for hya.
//!
//! Spawns a real `hya-backend` against a temp XDG config and a local scripted
//! OpenAI-compatible FakeLlm. Product code under test is the production binary
//! path (config → HttpProvider → SessionEngine → tools → HTTP API).

mod backend;
mod error;
mod fake_llm;
mod scenario;

pub use backend::{
    BackendProcess, BackendSpec, MCP_ECHO_SCRIPT_REL, McpFixture, default_backend_bin,
    materialize_public_bundle, mcp_echo_command, mcp_echo_script, public_bundle_fixture,
    public_bundle_source,
};
pub use error::E2eError;
pub use fake_llm::{
    FakeLlm, FakeLlmHandle, ScriptStep, ToolCallStep, text_step, tool_step, tools_step,
};
pub use scenario::{
    E2eEnv, E2eEnvBuilder, fake_requests_from, tree_children, tree_max_depth, tree_session_ids,
    tree_subagent_types, wait_until,
};
