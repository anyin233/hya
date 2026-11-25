//! Native implementations of the ten base built-in tools.

use std::sync::Arc;

use hya_tool::Tool;

mod apply_patch;
mod ask_user;
mod edit;
mod file_diff;
mod fs_tools;
mod grep;
mod hashline;
mod lsp_post_edit;
mod read;
mod read_media;
mod shell;
mod shell_output;
mod utf8_bom;
mod write;

hya_tool::export_native_tool_bundle!(|out| {
    let hashline_runtime = Arc::new(hashline::HashlineRuntime::new());
    let tools: [Arc<dyn Tool>; 10] = [
        Arc::new(read::ReadTool::new(Arc::clone(&hashline_runtime))),
        Arc::new(write::WriteTool::new(Arc::clone(&hashline_runtime))),
        Arc::new(edit::EditTool::new(Arc::clone(&hashline_runtime))),
        Arc::new(fs_tools::LsTool),
        Arc::new(fs_tools::GlobTool),
        Arc::new(fs_tools::FindTool),
        Arc::new(grep::GrepTool::with_runtime(Arc::clone(&hashline_runtime))),
        Arc::new(ask_user::AskUserTool),
        Arc::new(shell::ShellTool),
        Arc::new(apply_patch::ApplyPatchTool),
    ];
    out.extend(tools);
});
