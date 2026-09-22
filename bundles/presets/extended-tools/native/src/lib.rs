//! Native implementation of the extended built-in tool family.

use std::sync::Arc;

use hya_tool::Tool;

mod agents;
mod invalid;
mod kill;
mod lsp;
mod lsp_path;
mod plan;
mod search_agent;
mod skill;
mod task;
mod workflow;

/// Report the lockstep Rust tool ABI before any Rust object crosses the library boundary.
///
/// # Safety
///
/// `out` must point to a writable array of at least 32 bytes, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_abi_v1(out: *mut u8) {
    if out.is_null() {
        return;
    }
    let digest = hya_tool::native_bundle::abi_digest_v1();
    // SAFETY: the caller supplies a writable 32-byte output array.
    unsafe { std::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len()) };
}

/// Register this bundle's concrete tools with the host's tool registry loader.
///
/// # Safety
///
/// `out` must point to a live `Vec<Arc<dyn Tool>>` built with the matching
/// hya-tool ABI, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_register_v1(out: *mut Vec<Arc<dyn Tool>>) {
    // SAFETY: the host calls this only after the ABI digest matches.
    if let Some(out) = unsafe { out.as_mut() } {
        let tools: [Arc<dyn Tool>; 9] = [
            Arc::new(invalid::InvalidTool),
            Arc::new(lsp::LspTool),
            Arc::new(skill::SkillTool),
            Arc::new(agents::ListAgentsTool),
            Arc::new(task::TaskTool),
            Arc::new(workflow::WorkflowTool),
            Arc::new(search_agent::SearchAgentTool),
            Arc::new(kill::KillTool),
            Arc::new(plan::PlanExitTool),
        ];
        out.extend(tools);
    }
}
