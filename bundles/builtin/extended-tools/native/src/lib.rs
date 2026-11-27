//! Native implementation of the extended built-in tool family.

use std::sync::Arc;

use hya_tool::Tool;

mod agents;
mod archive;
mod lsp;
mod plan;
mod project_activity;
mod search_agent;
mod skill;
mod task;
mod workflow;

hya_tool::export_native_tool_bundle!(|out| {
    let tools: [Arc<dyn Tool>; 10] = [
        Arc::new(lsp::LspTool),
        Arc::new(skill::SkillTool),
        Arc::new(agents::ListAgentsTool),
        Arc::new(task::TaskTool),
        Arc::new(workflow::WorkflowTool),
        Arc::new(project_activity::ProjectActivityTool),
        Arc::new(search_agent::SearchAgentTool),
        Arc::new(archive::ArchiveTool),
        Arc::new(plan::PlanExitTool),
        Arc::new(hya_tool::lifecycle::WaitTool::new(false)),
    ];
    out.extend(tools);
});
