//! Native implementation of the channel built-in tool family.

use std::sync::Arc;

use hya_tool::Tool;

mod mailbox_tools;
mod report;

hya_tool::export_native_tool_bundle!(|out| {
    let tools: [Arc<dyn Tool>; 4] = [
        Arc::new(mailbox_tools::SendTool),
        Arc::new(mailbox_tools::ListChannelTool),
        Arc::new(report::ReportTool),
        Arc::new(hya_tool::lifecycle::WaitTool::new(true)),
    ];
    out.extend(tools);
});
