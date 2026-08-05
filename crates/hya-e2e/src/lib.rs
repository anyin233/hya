//! Process-level E2E harness for hya.
//!
//! Spawns a real `hya-backend` against a temp XDG config and a local scripted
//! OpenAI-compatible FakeLlm. Product code under test is the production binary
//! path (config → HttpProvider → SessionEngine → tools → HTTP API).

#![allow(missing_docs)]

mod backend;
mod error;
mod fake_llm;
mod scenario;

pub use backend::{BackendProcess, BackendSpec};
pub use error::E2eError;
pub use fake_llm::{
    FakeLlm, FakeLlmHandle, ScriptStep, ToolCallStep, text_step, tool_step, tools_step,
};
pub use scenario::{E2eEnv, E2eEnvBuilder, wait_until};
