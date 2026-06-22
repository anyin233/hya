use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::identity::active_agent_label;
use super::transcript_metadata::format_duration;
use crate::AppState;
use crate::theme::Theme;
use crate::view_model::latest_assistant_duration_ms;

pub fn render_runtime_status(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(runtime_status_line(state, theme)).style(theme.base()),
        area,
    );
}

fn runtime_status_line(state: &AppState, theme: &Theme) -> Line<'static> {
    let model = if state.model.is_empty() {
        "offline"
    } else {
        state.model.as_str()
    };
    let mut spans = vec![
        Span::styled("  ▣ ", Style::default().fg(theme.primary)),
        Span::styled(active_agent_label(state), Style::default().fg(theme.info)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(model.to_string(), Style::default().fg(theme.text)),
    ];
    if let Some(state_label) = runtime_state_label(state) {
        let state_color = if state.running {
            theme.warning
        } else {
            theme.muted
        };
        spans.extend([
            Span::styled(" · ", Style::default().fg(theme.muted)),
            Span::styled(state_label, Style::default().fg(state_color)),
        ]);
    }
    if state.running {
        spans.extend([
            Span::styled("   ", Style::default().fg(theme.muted)),
            Span::styled("ctrl+x down ", Style::default().fg(theme.text)),
            Span::styled("view subagents", Style::default().fg(theme.muted)),
        ]);
    }
    Line::from(spans)
}

fn runtime_state_label(state: &AppState) -> Option<String> {
    if state.running {
        Some("streaming".to_string())
    } else {
        latest_assistant_duration_ms(&state.projection).map(format_duration)
    }
}
