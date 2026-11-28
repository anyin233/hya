//! Native `tool.visibility` hook for `hya/plan`.
//!
//! Hides `plan_exit` whenever the active session has no interactive question
//! channel (for example headless `hya exec`, `-p`, or `hya loop` runs).

use async_trait::async_trait;
use hya_runtime_abi::{
    HookPoint, HostError, NativeHook, ToolVisibilityInput, ToolVisibilityOutcome,
};

use crate::plan_exit::PLAN_EXIT_TOOL_NAME;

/// Stable hook identifier within `hya/plan`.
pub const PLAN_EXIT_VISIBILITY_HOOK_ID: &str = "plan_exit_visibility";

/// Hides `plan_exit` when no interactive user-question channel is connected.
pub struct PlanExitVisibilityHook;

#[async_trait]
impl NativeHook for PlanExitVisibilityHook {
    fn id(&self) -> &str {
        PLAN_EXIT_VISIBILITY_HOOK_ID
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::ToolVisibility]
    }

    async fn tool_visibility(
        &self,
        input: ToolVisibilityInput,
    ) -> Result<ToolVisibilityOutcome, HostError> {
        if input.interactive {
            return Ok(ToolVisibilityOutcome::Preserve);
        }
        Ok(ToolVisibilityOutcome::Hide {
            tools: vec![PLAN_EXIT_TOOL_NAME.to_string()],
        })
    }
}
