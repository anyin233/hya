use ratatui::style::Color;

use crate::ConnectorState;
use crate::theme::Theme;

pub fn saved_tokens(tokens: u64) -> String {
    format!("~{} tokens", format_number(tokens))
}

pub fn format_number(number: u64) -> String {
    let mut out = String::new();
    for (idx, ch) in number.to_string().chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

pub fn format_basis_points(basis_points: u16) -> String {
    let tenths = u32::from(basis_points).saturating_add(5) / 10;
    format!("{}.{:01}%", tenths / 10, tenths % 10)
}

pub fn used_percent(current: u64, window: u64) -> u64 {
    if window == 0 {
        return 0;
    }
    current.saturating_mul(100).saturating_div(window).min(100)
}

pub fn workdir_label(workdir: Option<&str>) -> String {
    let raw = workdir.unwrap_or("worktree n/a");
    if raw == "."
        && let Ok(current) = std::env::current_dir()
    {
        return current.to_string_lossy().into_owned();
    }
    raw.to_string()
}

pub fn connector_color(state: &ConnectorState, theme: &Theme) -> Color {
    match state {
        ConnectorState::Connected => theme.success,
        ConnectorState::Failed(_) => theme.error,
        ConnectorState::NeedsAuth => theme.warning,
        ConnectorState::Disabled => theme.muted,
    }
}
