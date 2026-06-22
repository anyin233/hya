use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::sidebar_format::workdir_label;
use crate::AppState;
use crate::theme::Theme;

pub fn sidebar_footer_lines(state: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(meta(
        workdir_label(state.projection.session.workdir.as_deref()),
        theme.muted,
    ));
    if let Some(branch) = &state.branch_label {
        lines.push(meta(branch.clone(), theme.text));
    }
    lines.push(Line::from(vec![
        Span::styled("  • ", Style::default().fg(theme.success)),
        Span::styled(
            format!("yaca {}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(theme.muted),
        ),
    ]));
    lines
}

fn meta(text: impl Into<String>, color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(text.into(), Style::default().fg(color)),
    ])
}
