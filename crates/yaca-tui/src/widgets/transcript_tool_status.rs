use ratatui::style::{Color, Modifier, Style};

use crate::theme::Theme;
use crate::view_model::ToolStatus;

pub(super) fn status_label(status: &ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Completed { .. } => "completed",
        ToolStatus::Error { .. } => "error",
    }
}

pub(super) fn status_color(status: &ToolStatus, theme: &Theme) -> Color {
    match status {
        ToolStatus::Pending | ToolStatus::Running => theme.warning,
        ToolStatus::Completed {
            exit_code: Some(exit_code),
            ..
        } if *exit_code != 0 => theme.error,
        ToolStatus::Completed { .. } => theme.muted,
        ToolStatus::Error { message } if is_denied_error_message(message) => theme.muted,
        ToolStatus::Error { .. } => theme.error,
    }
}

pub(super) fn inline_status_text(status: &ToolStatus) -> Option<String> {
    match status {
        ToolStatus::Pending | ToolStatus::Running => {
            Some(format!("{}{}", status_label(status), status_suffix(status)))
        }
        ToolStatus::Error { .. } => None,
        ToolStatus::Completed {
            time_ms,
            exit_code: Some(exit_code),
            ..
        } if *exit_code != 0 => Some(format!("exit {exit_code} ✗ {time_ms}ms")),
        ToolStatus::Completed { .. } => None,
    }
}

pub(super) fn is_denied_error(status: &ToolStatus) -> bool {
    match status {
        ToolStatus::Error { message } => is_denied_error_message(message),
        ToolStatus::Pending | ToolStatus::Running | ToolStatus::Completed { .. } => false,
    }
}

pub(super) fn tool_style(fg: Color, selected: bool, theme: &Theme, crossed_out: bool) -> Style {
    let bg = if selected {
        theme.block
    } else {
        theme.background
    };
    let style = Style::default().fg(fg).bg(bg);
    if crossed_out {
        style.add_modifier(Modifier::CROSSED_OUT)
    } else {
        style
    }
}

fn is_denied_error_message(message: &str) -> bool {
    [
        "QuestionRejectedError",
        "rejected permission",
        "specified a rule",
        "user dismissed",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn status_suffix(status: &ToolStatus) -> String {
    match status {
        ToolStatus::Pending | ToolStatus::Running => " …".to_string(),
        ToolStatus::Completed {
            time_ms,
            exit_code: Some(exit_code),
            ..
        } => format!(" (exit {exit_code}) ✓ {time_ms}ms"),
        ToolStatus::Completed { time_ms, .. } => format!(" ✓ {time_ms}ms"),
        ToolStatus::Error { .. } => " ✗".to_string(),
    }
}
