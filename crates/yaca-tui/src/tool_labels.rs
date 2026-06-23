use crate::view_model::ToolStatus;

pub(crate) fn status_symbol(name: &str, status: &ToolStatus) -> &'static str {
    match status {
        ToolStatus::Error { .. } if name == "task" => "✗",
        ToolStatus::Error { .. } => "×",
        ToolStatus::Pending | ToolStatus::Running if name == "task" => "•",
        ToolStatus::Completed { .. } if name == "task" => "✓",
        ToolStatus::Pending | ToolStatus::Running | ToolStatus::Completed { .. } => {
            action_symbol(name)
        }
    }
}

pub(crate) fn action_symbol(name: &str) -> &'static str {
    match name {
        "edit" | "write" => "←",
        "find" | "grep" | "glob" => "✱",
        "todowrite" => "#",
        "webfetch" => "%",
        "websearch" => "◈",
        _ => "→",
    }
}

pub(crate) fn action_label(name: &str) -> String {
    match name {
        "bash" | "shell" => "Shell".to_string(),
        "read" => "Read".to_string(),
        "edit" => "Edit".to_string(),
        "write" => "Write".to_string(),
        "ls" => "List".to_string(),
        "find" => "Find".to_string(),
        "grep" => "Grep".to_string(),
        "glob" => "Glob".to_string(),
        "ask_user" => "Asked".to_string(),
        "task" => "Task".to_string(),
        "todowrite" => "Todos".to_string(),
        "webfetch" => "WebFetch".to_string(),
        "websearch" => "Web Search".to_string(),
        other => title_case_ascii(other),
    }
}

pub(crate) fn websearch_provider_label(provider: Option<&str>) -> String {
    match provider {
        Some("parallel") => "Parallel Web Search".to_string(),
        Some("exa") => "Exa Web Search".to_string(),
        _ => action_label("websearch"),
    }
}

fn title_case_ascii(input: &str) -> String {
    let mut chars = input.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}
