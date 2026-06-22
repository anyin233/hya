use crate::view_model::ToolStatus;

pub(super) fn pending_tool_label(name: &str, status: &ToolStatus) -> Option<&'static str> {
    if !matches!(status, ToolStatus::Pending | ToolStatus::Running) {
        return None;
    }

    match name {
        "bash" | "shell" => Some("Writing command..."),
        "read" => Some("Reading file..."),
        "write" => Some("Preparing write..."),
        "edit" => Some("Preparing edit..."),
        "glob" => Some("Finding files..."),
        "grep" => Some("Searching content..."),
        "webfetch" => Some("Fetching from the web..."),
        "websearch" => Some("Searching web..."),
        "task" => Some("Delegating..."),
        "todowrite" => Some("Updating todos..."),
        "ask_user" => Some("Asking questions..."),
        "skill" => Some("Loading skill..."),
        _ => None,
    }
}
