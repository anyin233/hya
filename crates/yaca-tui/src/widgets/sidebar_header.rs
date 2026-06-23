use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::AppState;
use crate::theme::Theme;

pub(super) fn push_header(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    let title = if state.session_label.trim().is_empty() {
        "context"
    } else {
        state.session_label.trim()
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "GUI",
            Style::default()
                .fg(theme.text)
                .bg(theme.panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {title}"), Style::default().fg(theme.muted)),
    ]));
    push_model_identity(lines, state, theme);
}

fn push_model_identity(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    let model = state.model.trim();
    let provider = state.model_provider_label.as_deref().map(str::trim);
    let Some(provider) = provider.filter(|provider| !provider.is_empty()) else {
        return;
    };
    if model.is_empty() {
        return;
    }
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("model ", Style::default().fg(theme.muted)),
        Span::styled(model.to_string(), Style::default().fg(theme.accent)),
        Span::styled(format!(" {provider}"), Style::default().fg(theme.muted)),
    ]));
}
