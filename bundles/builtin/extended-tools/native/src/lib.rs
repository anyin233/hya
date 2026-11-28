//! Native implementation of the extended built-in tool family.

use std::sync::Arc;

use hya_tool::Tool;

mod agents;
mod archive;
mod search_agent;
mod task;
mod workflow;

hya_tool::export_native_tool_bundle!(|out| {
    let tools: [Arc<dyn Tool>; 6] = [
        Arc::new(agents::ListAgentsTool),
        Arc::new(task::TaskTool),
        Arc::new(workflow::WorkflowTool),
        Arc::new(search_agent::SearchAgentTool),
        Arc::new(archive::ArchiveTool),
        Arc::new(hya_tool::lifecycle::WaitTool::new(false)),
    ];
    out.extend(tools);
});
